#!/usr/bin/env python3
"""Bridge from Rust provider adapters to private-ai-verifier.

The Rust aggregator owns provider selection and forwarding. This script only
runs provider-specific attestation verification and returns a small, stable JSON
result with binding material the Rust forwarding path can enforce.

This is a thin entry point: the provider logic lives in the ``provider_verifier``
package next to this file. The ``verify_*`` functions are re-exported here so the
historical ``import private_ai_provider_verifier as bridge`` (used by
``scripts/soundness_phala_direct.py``) keeps working unchanged.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys

from provider_verifier import (
    verify_chutes,
    verify_nearai,
    verify_phala_direct,
    verify_secret_ai,
    verify_tinfoil,
)
from provider_verifier.common import failed

__all__ = [
    "verify_chutes",
    "verify_nearai",
    "verify_phala_direct",
    "verify_secret_ai",
    "verify_tinfoil",
    "main",
]


async def main() -> None:
    request = json.loads(sys.stdin.read())
    # Default to the vendored `confidential_verifier` package next to this script
    # (see scripts/confidential_verifier/VENDOR.md). An external private-ai-verifier
    # checkout can override via PRIVATE_AI_VERIFIER_DIR, which is inserted ahead of
    # the vendored copy on sys.path.
    script_dir = os.path.dirname(os.path.abspath(__file__))
    if script_dir not in sys.path:
        sys.path.append(script_dir)
    private_ai_dir = os.environ.get("PRIVATE_AI_VERIFIER_DIR")
    if private_ai_dir:
        sys.path.insert(0, private_ai_dir)
    provider = request.get("provider")
    try:
        if provider == "tinfoil":
            await verify_tinfoil(request)
        elif provider == "near-ai":
            await verify_nearai(request)
        elif provider == "phala-direct":
            await verify_phala_direct(request)
        elif provider == "secret-ai":
            await verify_secret_ai(request)
        elif provider == "chutes":
            await verify_chutes(request)
        else:
            failed(str(provider), f"unsupported provider: {provider!r}")
    except Exception as exc:
        failed(str(provider), str(exc))


if __name__ == "__main__":
    asyncio.run(main())
