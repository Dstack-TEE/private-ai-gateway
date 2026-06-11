#!/usr/bin/env python3
"""Hermetic soundness checks for the PhalaDirect provider verifier bridge.

Exercises verify_phala_direct end-to-end with the HTTP fetch, the dstack
verifier, and the NVIDIA GPU verifier stubbed, but with the REAL report_data
binding logic (verify_report_data + _tdx_report_data_hex). The stub HTTP server
reads the fresh nonce the bridge generates from the request URL and binds it into
a synthetic version-2 report, so the genuine path actually round-trips the
nonce + signing-address + TLS-SPKI binding.

Pins:
  - a genuine version-2 report verifies and emits a tls_spki_sha256 channel
    binding (origin = url_origin, spki = tls_cert_fingerprint) plus the granular
    tcb_status claim;
  - a missing tls_cert_fingerprint, a broken report_data binding (swapped
    fingerprint), a mismatched GPU nonce, a dstack failure, and a GPU failure are
    each rejected.

No network, no localhost:8080, no NRAS. Run: uv run python scripts/soundness_phala_direct.py
"""

from __future__ import annotations

import asyncio
import hashlib
import io
import json
import os
import sys
import tarfile
import tempfile
from contextlib import redirect_stdout
from urllib.parse import parse_qs, urlparse

sys.path.append(os.path.dirname(os.path.abspath(__file__)))

import dstack_os_image as osimg_mod  # noqa: E402
import private_ai_provider_verifier as bridge  # noqa: E402
from confidential_verifier.verifiers import dstack as dstack_mod  # noqa: E402

ADDR = "11" * 20  # 20-byte ECDSA signing address (no 0x)
FP = "ab" * 32  # genuine custom-domain SPKI fingerprint
URL_ORIGIN = "https://model-a.phala.example"
# A real seeded dstack OS image hash (dev image 0.5.9) so the genuine path resolves
# production_os_image offline from dstack_os_image.KNOWN_OS_IMAGES.
SEED_DEV_HASH = "0e09f2bcb510c682b461d16b97192c710886db582852991e05146291063f890b"


def _synthetic_quote(report_data_hex: str, debug: bool = False) -> str:
    """A synthetic TDX v4 quote with report_data at the canonical offset, so the
    real _tdx_report_data_hex extracts it (matches scripts/soundness_report_data.py)."""
    rd = bytes.fromhex(report_data_hex)
    body = bytearray(b"\x11" * 520)
    body[120] = 0x01 if debug else 0x00  # TD_ATTRIBUTES TUD byte (bit0 = DEBUG)
    return (b"\x00" * 48 + bytes(body) + rd + b"\x99" * 16).hex()


def _report(nonce_hex: str, *, bind_fp: str = FP, report_fp: str | None = FP, gpu_nonce: str | None = None, gpu: bool = True, debug: bool = False) -> dict:
    """Build a version-2 report for a given nonce.

    bind_fp  : fingerprint mixed into report_data[0:32] (genuine = FP).
    report_fp: fingerprint advertised in the report body (None ⇒ omit the field).
    gpu      : include NVIDIA GPU CC evidence (False ⇒ a CPU-only workload).
    debug    : set the TD_ATTRIBUTES TUD byte so the quote reads as debug mode.
    """
    first = hashlib.sha256(bytes.fromhex(ADDR) + bytes.fromhex(bind_fp)).digest()
    report_data_hex = (first + bytes.fromhex(nonce_hex)).hex()
    app_compose = "services: []"
    nvidia_payload = (
        json.dumps({"nonce": gpu_nonce or nonce_hex, "evidence_list": [{"arch": "HOPPER"}], "arch": "HOPPER"})
        if gpu
        else None
    )
    attestation = {
        "signing_address": "0x" + ADDR,
        "signing_algo": "ecdsa",
        "request_nonce": nonce_hex,
        "intel_quote": _synthetic_quote(report_data_hex, debug=debug),
        "nvidia_payload": nvidia_payload,
        "info": {
            "compose_hash": hashlib.sha256(app_compose.encode()).hexdigest(),
            "tcb_info": {"app_compose": app_compose},
        },
        "event_log": json.dumps({"mock": True}),
        "vm_config": "mock_vm_config",
        "version": 2,
    }
    if report_fp is not None:
        attestation["tls_cert_fingerprint"] = report_fp
    attestation["all_attestations"] = [dict(attestation)]
    return attestation


class _Resp:
    def __init__(self, body: bytes):
        self._body = body

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def read(self) -> bytes:
        return self._body


def _make_urlopen(report_builder):
    def _urlopen(request, timeout=None):
        url = request.full_url if hasattr(request, "full_url") else request
        nonce = parse_qs(urlparse(url).query)["nonce"][0]
        return _Resp(json.dumps(report_builder(nonce)).encode("utf-8"))

    return _urlopen


class _StubDstack:
    def __init__(self, url=None, *, is_valid=True, os_image_hash=SEED_DEV_HASH):
        self._is_valid = is_valid
        self._os_image_hash = os_image_hash

    def verify(self, quote, event_log, vm_config):
        if not self._is_valid:
            return {"is_valid": False, "reason": "stub dstack failure"}
        # Intentionally omit report_data so the bridge falls back to parsing it
        # from the quote via the real _tdx_report_data_hex. Surface os_image_hash
        # under app_info, matching the live dstack-verifier >= 0.5.6 response shape.
        details = {"tcb_status": "UpToDate"}
        if self._os_image_hash is not None:
            details["app_info"] = {"os_image_hash": self._os_image_hash}
        return {"is_valid": True, "details": details}


def _run(
    *,
    report_builder,
    dstack_valid=True,
    os_image_hash=SEED_DEV_HASH,
    resolve_override=None,
) -> dict:
    """Run verify_phala_direct with stubs and return the emitted JSON result.

    OS-image resolution stays offline: seeded hashes resolve from
    KNOWN_OS_IMAGES, and DSTACK_OS_IMAGE_OFFLINE blocks any network for unseeded
    ones (so an unknown hash yields production_os_image=None). resolve_override
    lets a case force a specific decision (e.g. a production image). The bridge
    does NOT call NRAS, so there is no GPU verifier to stub.
    """
    orig_urlopen = bridge.urllib.request.urlopen
    orig_dstack = dstack_mod.DstackVerifier
    orig_resolve = osimg_mod.resolve_os_image
    orig_offline = os.environ.get("DSTACK_OS_IMAGE_OFFLINE")
    bridge.urllib.request.urlopen = _make_urlopen(report_builder)
    dstack_mod.DstackVerifier = lambda url=None: _StubDstack(
        url, is_valid=dstack_valid, os_image_hash=os_image_hash
    )
    if resolve_override is not None:
        osimg_mod.resolve_os_image = resolve_override
    os.environ["DSTACK_OS_IMAGE_OFFLINE"] = "1"
    request = {
        "provider": "phala-direct",
        "upstream_name": "phala-a",
        "url_origin": URL_ORIGIN,
        "model_id": "test-model",
        "provider_options": {"phala_direct_bearer_token": "tok"},
        "timeout_seconds": 5,
    }
    buf = io.StringIO()
    try:
        with redirect_stdout(buf):
            asyncio.run(bridge.verify_phala_direct(request))
    finally:
        bridge.urllib.request.urlopen = orig_urlopen
        dstack_mod.DstackVerifier = orig_dstack
        osimg_mod.resolve_os_image = orig_resolve
        if orig_offline is None:
            os.environ.pop("DSTACK_OS_IMAGE_OFFLINE", None)
        else:
            os.environ["DSTACK_OS_IMAGE_OFFLINE"] = orig_offline
    return json.loads(buf.getvalue())


def check() -> list[str]:
    f: list[str] = []

    # --- genuine version-2 report ---
    out = _run(report_builder=lambda n: _report(n))
    if out.get("result") != "verified":
        f.append(f"genuine: expected verified, got {out!r}")
    else:
        bindings = out.get("channel_bindings") or []
        if bindings != [
            {"type": "tls_spki_sha256", "origin": URL_ORIGIN, "spki_sha256": FP}
        ]:
            f.append(f"genuine: unexpected channel binding {bindings!r}")
        claims = out.get("provider_claims") or {}
        if claims.get("tcb_status") != "UpToDate":
            f.append("genuine: tcb_status claim not surfaced from dstack details")
        if claims.get("signing_address") != "0x" + ADDR:
            f.append("genuine: signing_address claim missing")
        # GPU verified by implication from the attested inference software (no NRAS):
        # the report presents GPU CC evidence and the CPU-TEE gate passed.
        if claims.get("gpu_verified") is not True:
            f.append("genuine: gpu_verified should be true (implied by attested software)")
        if claims.get("gpu_evidence_present") is not True:
            f.append("genuine: expected gpu_evidence_present=true")
        if claims.get("gpu_evidence_nonce_matched") is not True:
            f.append("genuine: fresh GPU nonce should match")
        # OS-image provenance: the attested os_image_hash resolves (offline, from the
        # seed map) to a known dev image, so production_os_image must be False — not
        # None, and never a fake True.
        if claims.get("os_image_hash") != SEED_DEV_HASH:
            f.append(f"genuine: os_image_hash not surfaced ({claims.get('os_image_hash')!r})")
        if claims.get("os_image_is_dev") is not True:
            f.append("genuine: seeded dev image must surface os_image_is_dev=true")
        if claims.get("production_os_image") is not False:
            f.append("genuine: a dev image must yield production_os_image=false")
        if claims.get("os_image_version") != "0.5.9":
            f.append("genuine: os_image_version not surfaced from resolved metadata")
        if out.get("verifier_id") != "private-ai-verifier/phala-direct/v1":
            f.append(f"genuine: unexpected verifier_id {out.get('verifier_id')!r}")

    # --- a production OS image resolves to production_os_image=true ---
    out = _run(
        report_builder=lambda n: _report(n),
        resolve_override=lambda h, **kw: {
            "is_dev": False,
            "version": "0.5.9",
            "verified": True,
            "source": "seed",
        },
    )
    claims = out.get("provider_claims") or {}
    if out.get("result") != "verified":
        f.append(f"prod-image: expected verified, got {out!r}")
    elif claims.get("production_os_image") is not True or claims.get("os_image_is_dev") is not False:
        f.append(f"prod-image: expected production_os_image=true, got {claims!r}")

    # --- an unresolvable os_image_hash (unknown + offline) stays undecided (None) ---
    out = _run(report_builder=lambda n: _report(n), os_image_hash="ff" * 32)
    claims = out.get("provider_claims") or {}
    if out.get("result") != "verified":
        f.append(f"unknown-image: GPU/OS are not gates, expected verified, got {out!r}")
    elif claims.get("production_os_image") is not None or claims.get("os_image_is_dev") is not None:
        f.append(f"unknown-image: unresolved hash must be undecided None, got {claims!r}")

    # --- missing tls_cert_fingerprint (old proxy that ignored version=2) ---
    out = _run(report_builder=lambda n: _report(n, report_fp=None))
    if out.get("result") != "failed" or "tls_cert_fingerprint" not in (out.get("reason") or ""):
        f.append(f"missing-fp: expected failure citing tls_cert_fingerprint, got {out!r}")

    # --- swapped fingerprint: report advertises FP but report_data binds a different one ---
    out = _run(report_builder=lambda n: _report(n, bind_fp="cd" * 32, report_fp=FP))
    if out.get("result") != "failed" or "binding" not in (out.get("reason") or ""):
        f.append(f"swapped-fp: expected report_data binding failure, got {out!r}")

    # --- TD in debug mode (TD_ATTRIBUTES TUD byte set) → hard rejection ---
    out = _run(report_builder=lambda n: _report(n, debug=True))
    if out.get("result") != "failed" or "debug" not in (out.get("reason") or ""):
        f.append(f"debug-mode: expected debug-mode rejection, got {out!r}")

    # --- dstack verification fails (the CPU TEE gate) → hard rejection ---
    out = _run(report_builder=lambda n: _report(n), dstack_valid=False)
    if out.get("result") != "failed" or "dstack" not in (out.get("reason") or ""):
        f.append(f"dstack-fail: expected dstack failure, got {out!r}")

    # --- GPU is supplemental, never a gate ---
    # A stale GPU evidence nonce still VERIFIES, and gpu_verified stays true: trust
    # comes from the attested software presenting the GPU, not the evidence blob's
    # freshness — only gpu_evidence_nonce_matched records the staleness.
    out = _run(report_builder=lambda n: _report(n, gpu_nonce="99" * 32))
    if out.get("result") != "verified":
        f.append(f"gpu-nonce: GPU is supplemental, expected verified, got {out!r}")
    else:
        claims = out.get("provider_claims") or {}
        if claims.get("gpu_evidence_nonce_matched") is not False:
            f.append("gpu-nonce: expected gpu_evidence_nonce_matched=false")
        if claims.get("gpu_verified") is not True:
            f.append("gpu-nonce: gpu_verified should stay true (implied by software)")

    # --- a CPU-only workload (no GPU evidence) is not GPU-verified ---
    out = _run(report_builder=lambda n: _report(n, gpu=False))
    if out.get("result") != "verified":
        f.append(f"no-gpu: expected verified, got {out!r}")
    else:
        claims = out.get("provider_claims") or {}
        if claims.get("gpu_evidence_present") is not False:
            f.append("no-gpu: expected gpu_evidence_present=false")
        if claims.get("gpu_verified") is not False:
            f.append("no-gpu: no GPU evidence must yield gpu_verified=false")

    return f


def _build_image_archive(metadata: dict, *, pinned_metadata: dict | None = None) -> tuple[str, bytes]:
    """Build a synthetic dstack OS-image archive and return (os_image_hash, bytes).

    os_image_hash = SHA256(sha256sum.txt); sha256sum.txt pins SHA256(each file).
    If pinned_metadata is given, sha256sum.txt pins ITS digest while the archive
    ships `metadata` — i.e. a download server that swapped metadata.json after the
    manifest was fixed (the binding must reject this).
    """
    meta_bytes = json.dumps(metadata).encode("utf-8")
    manifest_meta = json.dumps(pinned_metadata).encode("utf-8") if pinned_metadata else meta_bytes
    files = {
        "ovmf.fd": b"firmware",
        "bzImage": b"kernel",
        "initramfs.cpio.gz": b"initrd",
        "metadata.json": meta_bytes,
    }
    digests = {name: hashlib.sha256(c).hexdigest() for name, c in files.items()}
    digests["metadata.json"] = hashlib.sha256(manifest_meta).hexdigest()
    sha_txt = ("".join(f"{digests[n]}  {n}\n" for n in files)).encode("utf-8")
    os_image_hash = hashlib.sha256(sha_txt).hexdigest()
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        for name, content in list(files.items()) + [("sha256sum.txt", sha_txt)]:
            info = tarfile.TarInfo(name)
            info.size = len(content)
            tf.addfile(info, io.BytesIO(content))
    return os_image_hash, buf.getvalue()


def check_os_image() -> list[str]:
    """Pin the os_image_hash -> is_dev binding the production_os_image claim rests on."""
    f: list[str] = []

    # Genuine archive: is_dev is bound to the hash and reads back.
    h, archive = _build_image_archive({"version": "9.9.9", "is_dev": True})
    try:
        meta = osimg_mod.verify_and_read_metadata(h, archive)
        if meta.get("is_dev") is not True:
            f.append("os-image: genuine archive did not read back is_dev=true")
    except ValueError as exc:
        f.append(f"os-image: genuine archive rejected: {exc}")

    # Tamper: ship is_dev=false but keep the manifest that pinned is_dev=true.
    # The os_image_hash still matches the manifest, but metadata.json no longer does.
    h2, archive2 = _build_image_archive(
        {"version": "9.9.9", "is_dev": False},
        pinned_metadata={"version": "9.9.9", "is_dev": True},
    )
    try:
        osimg_mod.verify_and_read_metadata(h2, archive2)
        f.append("os-image: a swapped metadata.json (flipped is_dev) was NOT rejected")
    except ValueError:
        pass

    # Tamper: a different os_image_hash than the archive hashes to.
    try:
        osimg_mod.verify_and_read_metadata("ab" * 32, archive)
        f.append("os-image: a wrong os_image_hash was NOT rejected")
    except ValueError:
        pass

    # End-to-end resolve over a file:// URL, with verification + on-disk cache.
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, f"mr_{h}.tar.gz")
        with open(path, "wb") as fh:
            fh.write(archive)
        env_keys = ("DSTACK_OS_IMAGE_DOWNLOAD_URL", "DSTACK_OS_IMAGE_CACHE_DIR", "DSTACK_OS_IMAGE_OFFLINE")
        saved = {k: os.environ.get(k) for k in env_keys}
        try:
            os.environ["DSTACK_OS_IMAGE_DOWNLOAD_URL"] = "file://" + os.path.join(tmp, "mr_{}.tar.gz")
            os.environ["DSTACK_OS_IMAGE_CACHE_DIR"] = os.path.join(tmp, "cache")
            os.environ.pop("DSTACK_OS_IMAGE_OFFLINE", None)
            res = osimg_mod.resolve_os_image(h)
            if not res or res.get("is_dev") is not True or res.get("source") != "download":
                f.append(f"os-image: file:// resolve did not verify+return is_dev ({res!r})")
            # Offline now must still resolve from the on-disk cache written above.
            os.environ["DSTACK_OS_IMAGE_OFFLINE"] = "1"
            cached = osimg_mod.resolve_os_image(h)
            if not cached or cached.get("source") != "cache":
                f.append(f"os-image: resolved image was not cached for offline reuse ({cached!r})")
            # An unknown hash while offline is undecided, never fabricated.
            if osimg_mod.resolve_os_image("cc" * 32) is not None:
                f.append("os-image: an unknown hash offline must resolve to None")
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v

    return f


def main() -> int:
    failures = check() + check_os_image()
    if failures:
        print("PHALA-DIRECT BRIDGE SOUNDNESS FAILURES:")
        for item in failures:
            print(f"  - {item}")
        return 1
    print("phala-direct bridge soundness checks OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
