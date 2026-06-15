#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from live_e2e.common import (  # noqa: E402
    DEFAULT_DSTACK_ENDPOINT,
    DEFAULT_DSTACK_VERIFIER_URL,
    DEFAULT_ENV_FILE,
    ROOT,
    Provider,
    assert_port_free,
    load_dotenv,
    load_providers,
    run_cmd,
)


def preflight(
    providers: list[Provider],
    *,
    port: int,
    dstack_endpoint: str = DEFAULT_DSTACK_ENDPOINT,
    env_file: Path = DEFAULT_ENV_FILE,
    check_build: bool = True,
) -> dict[str, object]:
    load_dotenv(env_file)
    os.environ.setdefault("DSTACK_VERIFIER_URL", DEFAULT_DSTACK_VERIFIER_URL)
    missing: list[str] = []
    for provider in providers:
        if not os.getenv(provider.api_key_env):
            missing.append(provider.api_key_env)
        for required in provider.requires:
            if not os.getenv(required):
                missing.append(required)
    if missing:
        raise RuntimeError(f"missing required env vars: {', '.join(sorted(set(missing)))}")

    for binary in ("cargo", "uv"):
        if shutil.which(binary) is None:
            raise RuntimeError(f"required binary not found on PATH: {binary}")

    # The gateway ships a vendored confidential_verifier; an external checkout is
    # only needed when PRIVATE_AI_VERIFIER_DIR explicitly overrides it.
    override_dir = os.getenv("PRIVATE_AI_VERIFIER_DIR")
    if override_dir:
        verifier_dir = Path(override_dir)
        if not verifier_dir.exists():
            raise RuntimeError(f"PRIVATE_AI_VERIFIER_DIR override not found: {verifier_dir}")
    else:
        verifier_dir = ROOT / "scripts" / "confidential_verifier"
        if not (verifier_dir / "__init__.py").exists():
            raise RuntimeError(f"vendored confidential_verifier not found: {verifier_dir}")

    if dstack_endpoint.startswith("unix:"):
        socket_path = Path(dstack_endpoint.removeprefix("unix:"))
        if not socket_path.exists():
            raise RuntimeError(
                f"dstack socket not found: {socket_path}. Start the local dstack simulator tunnel."
            )

    assert_port_free(port)

    if check_build:
        result = run_cmd(["cargo", "build", "--bin", "private-ai-gateway"], timeout=300)
        if result.returncode != 0:
            stderr = result.stderr.decode("utf-8", errors="replace")
            raise RuntimeError(f"aggregator build failed:\n{stderr}")

    return {
        "ok": True,
        "provider_count": len(providers),
        "port": port,
        "env_file": str(env_file),
        "private_ai_verifier_dir": str(verifier_dir),
        "dstack_endpoint": dstack_endpoint,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Run live E2E preflight checks.")
    parser.add_argument("--providers-file", type=Path, default=ROOT / "scripts/live_e2e/providers.json")
    parser.add_argument("--provider", action="append", default=[])
    parser.add_argument("--env-file", type=Path, default=DEFAULT_ENV_FILE)
    parser.add_argument("--port", type=int, default=18086)
    parser.add_argument("--dstack-endpoint", default=DEFAULT_DSTACK_ENDPOINT)
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()
    load_dotenv(args.env_file)
    providers = load_providers(args.providers_file, args.provider)
    summary = preflight(
        providers,
        port=args.port,
        dstack_endpoint=args.dstack_endpoint,
        env_file=args.env_file,
        check_build=not args.no_build,
    )
    print(summary)


if __name__ == "__main__":
    main()
