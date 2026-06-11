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
import types
from contextlib import redirect_stdout
from urllib.parse import parse_qs, urlparse

sys.path.append(os.path.dirname(os.path.abspath(__file__)))

import private_ai_provider_verifier as bridge  # noqa: E402
from confidential_verifier.verifiers import dstack as dstack_mod  # noqa: E402
from confidential_verifier.verifiers import nvidia as nvidia_mod  # noqa: E402

ADDR = "11" * 20  # 20-byte ECDSA signing address (no 0x)
FP = "ab" * 32  # genuine custom-domain SPKI fingerprint
URL_ORIGIN = "https://model-a.phala.example"


def _synthetic_quote(report_data_hex: str, debug: bool = False) -> str:
    """A synthetic TDX v4 quote with report_data at the canonical offset, so the
    real _tdx_report_data_hex extracts it (matches scripts/soundness_report_data.py)."""
    rd = bytes.fromhex(report_data_hex)
    body = bytearray(b"\x11" * 520)
    body[120] = 0x01 if debug else 0x00  # TD_ATTRIBUTES TUD byte (bit0 = DEBUG)
    return (b"\x00" * 48 + bytes(body) + rd + b"\x99" * 16).hex()


def _report(nonce_hex: str, *, bind_fp: str = FP, report_fp: str | None = FP, gpu_nonce: str | None = None, debug: bool = False) -> dict:
    """Build a version-2 report for a given nonce.

    bind_fp  : fingerprint mixed into report_data[0:32] (genuine = FP).
    report_fp: fingerprint advertised in the report body (None ⇒ omit the field).
    debug    : set the TD_ATTRIBUTES TUD byte so the quote reads as debug mode.
    """
    first = hashlib.sha256(bytes.fromhex(ADDR) + bytes.fromhex(bind_fp)).digest()
    report_data_hex = (first + bytes.fromhex(nonce_hex)).hex()
    app_compose = "services: []"
    attestation = {
        "signing_address": "0x" + ADDR,
        "signing_algo": "ecdsa",
        "request_nonce": nonce_hex,
        "intel_quote": _synthetic_quote(report_data_hex, debug=debug),
        "nvidia_payload": json.dumps(
            {"nonce": gpu_nonce or nonce_hex, "evidence_list": [{"arch": "HOPPER"}], "arch": "HOPPER"}
        ),
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
    def __init__(self, url=None, *, is_valid=True):
        self._is_valid = is_valid

    def verify(self, quote, event_log, vm_config):
        if not self._is_valid:
            return {"is_valid": False, "reason": "stub dstack failure"}
        # Intentionally omit report_data so the bridge falls back to parsing it
        # from the quote via the real _tdx_report_data_hex.
        return {"is_valid": True, "details": {"tcb_status": "UpToDate"}}


def _stub_gpu(ok=True):
    class _G:
        async def verify(self, payload):
            return types.SimpleNamespace(
                model_verified=ok, error=None if ok else "stub gpu failure"
            )

    return lambda: _G()


def _run(*, report_builder, dstack_valid=True, gpu_ok=True) -> dict:
    """Run verify_phala_direct with stubs and return the emitted JSON result."""
    orig_urlopen = bridge.urllib.request.urlopen
    orig_dstack = dstack_mod.DstackVerifier
    orig_gpu = nvidia_mod.NvidiaGpuVerifier
    bridge.urllib.request.urlopen = _make_urlopen(report_builder)
    dstack_mod.DstackVerifier = lambda url=None: _StubDstack(url, is_valid=dstack_valid)
    nvidia_mod.NvidiaGpuVerifier = _stub_gpu(gpu_ok)
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
        nvidia_mod.NvidiaGpuVerifier = orig_gpu
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
        if claims.get("gpu_verified") is not True:
            f.append("genuine: expected gpu_verified true for a fresh GPU pass")
        if out.get("verifier_id") != "private-ai-verifier/phala-direct/v1":
            f.append(f"genuine: unexpected verifier_id {out.get('verifier_id')!r}")

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
    # A GPU evidence nonce mismatch still VERIFIES; the outcome is recorded.
    out = _run(report_builder=lambda n: _report(n, gpu_nonce="99" * 32))
    if out.get("result") != "verified":
        f.append(f"gpu-nonce: GPU is supplemental, expected verified, got {out!r}")
    else:
        claims = out.get("provider_claims") or {}
        if claims.get("gpu_evidence_nonce_matched") is not False:
            f.append("gpu-nonce: expected gpu_evidence_nonce_matched=false")
        if claims.get("gpu_verified") is not False:
            f.append("gpu-nonce: stale GPU nonce must not count as gpu_verified")

    # A failed NRAS result still VERIFIES; gpu_verified is recorded false.
    out = _run(report_builder=lambda n: _report(n), gpu_ok=False)
    if out.get("result") != "verified":
        f.append(f"gpu-fail: GPU is supplemental, expected verified, got {out!r}")
    else:
        claims = out.get("provider_claims") or {}
        if claims.get("gpu_verified") is not False:
            f.append("gpu-fail: expected gpu_verified=false")
        if claims.get("gpu_evidence_present") is not True:
            f.append("gpu-fail: expected gpu_evidence_present=true")

    return f


def main() -> int:
    failures = check()
    if failures:
        print("PHALA-DIRECT BRIDGE SOUNDNESS FAILURES:")
        for item in failures:
            print(f"  - {item}")
        return 1
    print("phala-direct bridge soundness checks OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
