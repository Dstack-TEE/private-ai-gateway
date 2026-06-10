#!/usr/bin/env python3
"""Hermetic contract check: the provider-verifier bridge vs. the verifier package.

`private_ai_provider_verifier.py` reaches into the vendored `confidential_verifier`
package (see scripts/confidential_verifier/VENDOR.md). That coupling once broke
silently when the bridge called `NearAICloudVerifier.verify_gateway_component`, a
method that only existed as an uncommitted local edit — every clean checkout failed
with `'NearAICloudVerifier' object has no attribute 'verify_gateway_component'`.

This check asserts that every class, method, and constructor argument the bridge
depends on actually exists in the verifier package it will import. It runs offline
(no network, no keys), so `cargo test` and CI catch drift before it reaches a live
request. Run directly with:

    uv run python scripts/contract_verifier_bridge.py

It checks the vendored package by default; set PRIVATE_AI_VERIFIER_DIR to check an
external verifier checkout instead (the same override the bridge honors).
"""

from __future__ import annotations

import importlib
import inspect
import os
import sys


def _setup_path() -> str:
    """Mirror the bridge's import resolution: vendored by default, env override wins."""
    script_dir = os.path.dirname(os.path.abspath(__file__))
    if script_dir not in sys.path:
        sys.path.append(script_dir)
    override = os.environ.get("PRIVATE_AI_VERIFIER_DIR")
    if override:
        sys.path.insert(0, override)
        return override
    return os.path.join(script_dir, "confidential_verifier")


def _get_attr(obj, name: str, failures: list[str], ctx: str):
    attr = getattr(obj, name, None)
    if attr is None:
        failures.append(f"{ctx}: missing `{name}`")
    return attr


def _require_callable(obj, name: str, failures: list[str], ctx: str, *, coroutine: bool = False):
    attr = _get_attr(obj, name, failures, ctx)
    if attr is None:
        return
    if not callable(attr):
        failures.append(f"{ctx}.{name} is not callable")
        return
    if coroutine and not inspect.iscoroutinefunction(attr):
        failures.append(f"{ctx}.{name} must be `async def` (bridge awaits it)")


def _require_param(cls, name: str, param: str, failures: list[str], ctx: str):
    func = getattr(cls, name, None)
    if func is None:
        failures.append(f"{ctx}: missing `{name}`")
        return
    try:
        sig = inspect.signature(func)
    except (TypeError, ValueError):
        return
    if param not in sig.parameters and not any(
        p.kind == inspect.Parameter.VAR_KEYWORD for p in sig.parameters.values()
    ):
        failures.append(f"{ctx}.{name} must accept `{param}=` (bridge passes it)")


def check() -> list[str]:
    failures: list[str] = []

    # --- chutes: bridge uses dcap_qvl directly (HTTP evidence + online verify) ---
    try:
        dcap_qvl = importlib.import_module("dcap_qvl")
        _require_callable(dcap_qvl, "get_collateral_and_verify", failures, "chutes/dcap_qvl",
                          coroutine=True)
    except Exception as exc:  # noqa: BLE001
        failures.append(f"chutes: cannot import dcap_qvl ({exc})")

    # --- tinfoil: bridge uses the official tinfoil SDK (SecureClient + document) ---
    try:
        tinfoil_mod = importlib.import_module("tinfoil")
        sc = _get_attr(tinfoil_mod, "SecureClient", failures, "tinfoil/SecureClient")
        if sc is not None:
            _require_callable(sc, "verify", failures, "tinfoil/SecureClient")
            _require_callable(sc, "get_verification_document", failures, "tinfoil/SecureClient")
        vd = _get_attr(tinfoil_mod, "VerificationDocument", failures, "tinfoil/VerificationDocument")
        if vd is not None:
            import dataclasses

            field_names = (
                {f.name for f in dataclasses.fields(vd)}
                if dataclasses.is_dataclass(vd)
                else set(dir(vd))
            )
            for needed in ("security_verified", "tls_public_key", "steps"):
                if needed not in field_names:
                    failures.append(f"tinfoil/VerificationDocument: missing `{needed}`")
    except Exception as exc:  # noqa: BLE001
        failures.append(f"tinfoil: cannot import tinfoil SDK ({exc})")

    # --- near-ai: bridge uses NearaiProvider + NearAICloudVerifier.verify_gateway_component ---
    try:
        prov_mod = importlib.import_module("confidential_verifier.providers.nearai")
        provider = _get_attr(prov_mod, "NearaiProvider", failures, "near-ai/providers.nearai")
        if provider is not None:
            _require_callable(provider, "fetch_report", failures, "near-ai/NearaiProvider")
            _require_param(provider, "__init__", "include_tls_fingerprint", failures,
                           "near-ai/NearaiProvider")
    except Exception as exc:  # noqa: BLE001
        failures.append(f"near-ai: cannot import providers.nearai ({exc})")

    try:
        ver_mod = importlib.import_module("confidential_verifier.verifiers.nearai")
        verifier = _get_attr(ver_mod, "NearAICloudVerifier", failures, "near-ai/verifiers.nearai")
        if verifier is not None:
            # The exact method whose absence broke NEAR AI verification.
            _require_callable(verifier, "verify_gateway_component", failures,
                              "near-ai/NearAICloudVerifier", coroutine=True)
    except Exception as exc:  # noqa: BLE001
        failures.append(f"near-ai: cannot import verifiers.nearai ({exc})")

    return failures


def main() -> int:
    source = _setup_path()
    failures = check()
    if failures:
        print("VERIFIER CONTRACT DRIFT — the provider-verifier bridge depends on symbols")
        print(f"that are missing from: {source}")
        for f in failures:
            print(f"  - {f}")
        print("\nFix the vendored package (scripts/confidential_verifier, see VENDOR.md)")
        print("or update the bridge to match. Do not ship with the bridge and verifier out of sync.")
        return 1
    print(f"verifier bridge contract OK ({source})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
