#!/usr/bin/env python3
"""Resolve a dstack `os_image_hash` to its published image metadata (is_dev / version).

The TDX quote (proved by the dstack verifier, `os_image_hash_verified`) carries an
`app_info.os_image_hash`. dstack publishes the corresponding OS image at
`https://download.dstack.org/os-images/mr_{os_image_hash}.tar.gz`. That archive
contains a `metadata.json` whose `is_dev` flag tells us whether the image is a
development build (SSH/serial-console enabled) or a production build.

The flag is **cryptographically bound to the attested hash**, so the download
server cannot lie about it:

    os_image_hash == SHA256(sha256sum.txt)          # how dstack derives the hash
    sha256sum.txt pins SHA256(metadata.json)        # metadata.json is a listed file
    => flipping is_dev changes metadata.json's digest
       => changes sha256sum.txt
          => changes os_image_hash (no longer matches the quote)

`resolve_os_image()` re-downloads the image and re-checks both equalities before
trusting `is_dev`. Known fleet images are seeded inline so the common case needs
no network. Resolved misses are cached on disk. Every failure path returns None
(undecided) — never a fabricated decision.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
import tarfile
import urllib.request
from typing import Any, Optional

DEFAULT_DOWNLOAD_URL_TEMPLATE = "https://download.dstack.org/os-images/mr_{}.tar.gz"

# Seed map of os_image_hash -> verified metadata, so the deployed fleet resolves
# offline. Each entry was produced by resolve_os_image() (download + re-verify the
# os_image_hash == SHA256(sha256sum.txt) and metadata.json binding). Re-verify with:
#   uv run python scripts/dstack_os_image.py <os_image_hash>
KNOWN_OS_IMAGES: dict[str, dict[str, Any]] = {
    # dstack PROD image 0.5.9 — entire reachable Phala fleet as of 2026-06-11
    # (glm4-7-flash.use2, gemma*/gpt-oss/qwen*/bge-reranker/embedding, …).
    "806a352e16175d90568de97dff563f31f680239e6b90e9b5b2e9141d0955b0d9": {
        "is_dev": False,
        "version": "0.5.9",
        "git_revision": "e3655d1390feee3736476f4bda35c4354b4a12fc",
    },
    # dstack DEV image 0.5.9 — the fleet's pre-flip image (now superseded by the
    # prod build above). Kept as a known-dev vector and for any un-flipped box.
    "0e09f2bcb510c682b461d16b97192c710886db582852991e05146291063f890b": {
        "is_dev": True,
        "version": "0.5.9",
        "git_revision": "e3655d1390feee3736476f4bda35c4354b4a12fc",
    },
    # dstack DEV image 0.5.5 — earlier embedding/reranker image (now superseded).
    "021bf66a7c9fd4a05031b8fa688834948874631c2ad5b9a2d566b4421b817271": {
        "is_dev": True,
        "version": "0.5.5",
        "git_revision": "25c25025c556ab2f797eeda3bab433f38a8ffb7a",
    },
}


def _normalize(os_image_hash: str) -> str:
    h = str(os_image_hash).strip().lower()
    if h.startswith("0x"):
        h = h[2:]
    return h


def verify_and_read_metadata(os_image_hash: str, archive_bytes: bytes) -> dict[str, Any]:
    """Re-verify the os_image_hash -> is_dev binding and return metadata.json.

    Raises ValueError if the archive does not hash to `os_image_hash`, or if its
    metadata.json is not the file pinned by sha256sum.txt.
    """
    h = _normalize(os_image_hash)
    with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:gz") as tf:
        try:
            sha_txt = tf.extractfile("sha256sum.txt").read()  # type: ignore[union-attr]
            meta_bytes = tf.extractfile("metadata.json").read()  # type: ignore[union-attr]
        except (KeyError, AttributeError) as exc:
            raise ValueError(f"image archive missing sha256sum.txt/metadata.json: {exc}")

    # 1. The attested os_image_hash is SHA256 of the file manifest.
    actual = hashlib.sha256(sha_txt).hexdigest()
    if actual != h:
        raise ValueError(
            f"archive sha256sum.txt hashes to {actual}, not the attested os_image_hash {h}"
        )

    # 2. metadata.json (which holds is_dev) must be the file pinned in that manifest.
    pinned: dict[str, str] = {}
    for line in sha_txt.decode("utf-8", "replace").splitlines():
        parts = line.split()
        if len(parts) >= 2:
            pinned[parts[-1].lstrip("*")] = parts[0].lower()
    meta_digest = hashlib.sha256(meta_bytes).hexdigest()
    if pinned.get("metadata.json") != meta_digest:
        raise ValueError("metadata.json is not bound by sha256sum.txt (is_dev unverifiable)")

    meta = json.loads(meta_bytes.decode("utf-8"))
    if "is_dev" not in meta:
        raise ValueError("metadata.json has no is_dev field")
    return meta


def _cache_dir() -> str:
    return (
        os.getenv("DSTACK_OS_IMAGE_CACHE_DIR")
        or os.path.join(
            os.getenv("XDG_CACHE_HOME") or os.path.expanduser("~/.cache"),
            "dstack-os-images",
        )
    )


def resolve_os_image(
    os_image_hash: Optional[str],
    *,
    allow_download: bool = True,
    timeout: int = 60,
) -> Optional[dict[str, Any]]:
    """Resolve os_image_hash -> {is_dev, version, git_revision, verified, source}.

    Order: seed map -> on-disk cache -> verified download. Returns None if the hash
    is unknown and cannot be resolved (offline / unreachable / verification failed):
    callers must treat None as "undecided", never as production.
    """
    if not os_image_hash:
        return None
    h = _normalize(os_image_hash)

    seed = KNOWN_OS_IMAGES.get(h)
    if seed is not None:
        return {**seed, "verified": True, "source": "seed"}

    cache_dir = _cache_dir()
    cache_file = os.path.join(cache_dir, f"{h}.json")
    if os.path.exists(cache_file):
        try:
            with open(cache_file, encoding="utf-8") as fh:
                return {**json.load(fh), "source": "cache"}
        except (OSError, json.JSONDecodeError):
            pass  # fall through to re-download

    if not allow_download or os.getenv("DSTACK_OS_IMAGE_OFFLINE"):
        return None

    template = os.getenv("DSTACK_OS_IMAGE_DOWNLOAD_URL") or DEFAULT_DOWNLOAD_URL_TEMPLATE
    url = template.format(h)
    try:
        # download.dstack.org 403s the default Python-urllib User-Agent, so set one.
        req = urllib.request.Request(url, headers={"User-Agent": "redpill-phala-direct-verifier/1"})
        with urllib.request.urlopen(req, timeout=timeout) as response:
            archive_bytes = response.read()
        meta = verify_and_read_metadata(h, archive_bytes)
    except Exception:  # noqa: BLE001 - resolution is best-effort; failure => undecided
        return None

    entry = {
        "is_dev": bool(meta.get("is_dev")),
        "version": meta.get("version"),
        "git_revision": meta.get("git_revision"),
        "verified": True,
    }
    try:
        os.makedirs(cache_dir, exist_ok=True)
        with open(cache_file, "w", encoding="utf-8") as fh:
            json.dump(entry, fh)
    except OSError:
        pass  # cache is an optimization, not required
    return {**entry, "source": "download"}


def _main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: dstack_os_image.py <os_image_hash>")
        return 2
    result = resolve_os_image(argv[1])
    if result is None:
        print(json.dumps({"os_image_hash": _normalize(argv[1]), "resolved": False}))
        return 1
    print(json.dumps({"os_image_hash": _normalize(argv[1]), "resolved": True, **result}, indent=2))
    return 0


if __name__ == "__main__":
    import sys

    raise SystemExit(_main(sys.argv))
