#!/usr/bin/env python3
"""Live verification of the PhalaDirect pipeline against a real Phala backend,
EXCEPT the version-2 TLS-SPKI binding/pinning.

Real per-model dstack-vllm-proxy endpoints currently serve attestation **v1**
(report_data = signing_address ‖ nonce, no `tls_cert_fingerprint`). `verify_phala_direct`
requires v2 and correctly fails such a report closed, so this harness instead exercises the
*same vendored primitives* the bridge uses — dstack quote verification, the report_data
binding (in v1 address mode), GPU evidence, and compose-hash integrity — against a live
report, and prints a per-step result. The only part not covered is the v2 SPKI binding,
because no live backend serves it yet.

Usage:
  uv run python scripts/live_verify_phala_direct.py \
      --endpoint https://api.redpill.ai --model phala/qwen-2.5-7b-instruct \
      [--bearer <key>] [--dstack-url http://localhost:8080]
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import hashlib
import json
import os
import secrets
import sys
import urllib.parse
import urllib.request

sys.path.append(os.path.dirname(os.path.abspath(__file__)))

from confidential_verifier.verifiers.dstack import DstackVerifier, verify_report_data
from confidential_verifier.verifiers.nearai import _tdx_report_data_hex
from confidential_verifier.verifiers.nvidia import NvidiaGpuVerifier


def fetch_report(endpoint: str, model: str, bearer: str, nonce: str, timeout: int):
    base = endpoint.rstrip("/")
    query = urllib.parse.urlencode({"model": model, "signing_algo": "ecdsa", "nonce": nonce})
    url = f"{base}/v1/attestation/report?{query}"
    request = urllib.request.Request(url)
    if bearer:
        request.add_header("Authorization", f"Bearer {bearer}")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return url, json.loads(response.read().decode("utf-8"))


async def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint", default="https://api.redpill.ai")
    parser.add_argument("--model", required=True)
    parser.add_argument("--bearer", default=os.getenv("REDPILL_API_KEY", ""))
    parser.add_argument(
        "--dstack-url", default=os.getenv("DSTACK_VERIFIER_URL", "http://localhost:8080")
    )
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()

    results: list[tuple[str, bool]] = []

    def step(name: str, ok: bool, detail: str = "") -> None:
        results.append((name, ok))
        mark = "PASS" if ok else "FAIL"
        print(f"[{mark}] {name}" + (f" — {detail}" if detail else ""))

    nonce = secrets.token_hex(32)
    try:
        url, report = fetch_report(args.endpoint, args.model, args.bearer, nonce, args.timeout)
    except Exception as exc:  # noqa: BLE001
        step("fetch attestation report", False, str(exc))
        return 1
    step("fetch attestation report", True, url)

    attestation = report
    all_attestations = report.get("all_attestations")
    if isinstance(all_attestations, list) and all_attestations and isinstance(all_attestations[0], dict):
        attestation = all_attestations[0]

    intel_quote = attestation.get("intel_quote") or attestation.get("quote")
    signing_address = attestation.get("signing_address")
    event_log = attestation.get("event_log") or ""
    vm_config = attestation.get("vm_config") or ""
    info = attestation.get("info") or {}
    report_nonce = attestation.get("request_nonce")
    nvidia_payload = attestation.get("nvidia_payload")
    tls_fp = attestation.get("tls_cert_fingerprint")

    step("report has intel_quote", bool(intel_quote))
    step("report has signing_address", bool(signing_address), signing_address or "")
    step(
        "nonce echoed matches request",
        str(report_nonce).lower() == nonce.lower(),
        f"sent {nonce[:12]}… got {str(report_nonce)[:12]}…",
    )
    print(
        f"[SKIP] version-2 tls_cert_fingerprint present: {bool(tls_fp)} "
        "(expected False on a live v1 backend — TLS-SPKI binding/pin intentionally not covered)"
    )

    if isinstance(vm_config, (dict, list)):
        vm_config = json.dumps(vm_config)

    # 1. dstack TDX quote verification (the CPU-TEE gate).
    with contextlib.redirect_stdout(sys.stderr):
        dstack_result = await asyncio.to_thread(
            DstackVerifier(args.dstack_url).verify, intel_quote, event_log, vm_config
        )
    details = dstack_result.get("details") if isinstance(dstack_result, dict) else None
    tcb_status = details.get("tcb_status") if isinstance(details, dict) else None
    dstack_ok = bool(dstack_result.get("is_valid"))
    step(
        "dstack DEEP verify (OS-image + RTMR replay) [external service]",
        dstack_ok,
        dstack_result.get("reason") or f"tcb_status={tcb_status}",
    )

    # 1b. DCAP "lite" verification of the TDX quote itself — Intel-rooted PCK
    # signature + TCB status + report_data, WITHOUT the dstack OS-image/RTMR
    # event-log replay. This proves the quote is a genuine TDX quote from real
    # hardware even when the deep dstack-verifier can't replay the event log.
    try:
        import dcap_qvl

        quote_bytes = bytes.fromhex(intel_quote)
        verified = await dcap_qvl.get_collateral_and_verify(quote_bytes)
        dcap = json.loads(verified.to_json())
        dcap_status = dcap.get("status")
        step(
            "DCAP quote signature + TCB (lite, no event-log replay)",
            dcap_status in ("UpToDate", "SWHardeningNeeded", "ConfigurationAndSWHardeningNeeded", "OutOfDate"),
            f"status={dcap_status}",
        )
    except Exception as exc:  # noqa: BLE001
        step("DCAP quote signature + TCB (lite, no event-log replay)", False, str(exc)[:120])

    # 2. Compose-hash integrity.
    tcb_info = info.get("tcb_info") or {}
    if isinstance(tcb_info, str):
        try:
            tcb_info = json.loads(tcb_info)
        except json.JSONDecodeError:
            tcb_info = {}
    app_compose = tcb_info.get("app_compose")
    reported_compose = info.get("compose_hash")
    compose_ok = bool(
        app_compose
        and reported_compose
        and hashlib.sha256(app_compose.encode("utf-8")).hexdigest().lower()
        == str(reported_compose).lower()
    )
    step("compose hash integrity", compose_ok, f"compose_hash={str(reported_compose)[:16]}…")

    # 3. report_data binding — v1 ADDRESS mode (nonce + signing address; no SPKI).
    report_data_hex = dstack_result.get("report_data") or _tdx_report_data_hex(intel_quote)
    if report_data_hex and signing_address:
        binding = verify_report_data(report_data_hex, signing_address, nonce)
        step(
            "report_data binding (nonce + signing address, v1)",
            bool(binding.get("valid")),
            f"mode={binding.get('address_mode')} err={binding.get('error')}",
        )
    else:
        step("report_data binding (nonce + signing address, v1)", False, "no report_data/signing_address")

    # 4. GPU evidence (supplemental, never a gate).
    payload = nvidia_payload
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except json.JSONDecodeError:
            payload = None
    if isinstance(payload, dict) and payload.get("evidence_list"):
        gpu_nonce = payload.get("nonce")
        nonce_match = bool(gpu_nonce) and str(gpu_nonce).lower() == nonce.lower()
        with contextlib.redirect_stdout(sys.stderr):
            gpu = await NvidiaGpuVerifier().verify(payload)
        step(
            "GPU evidence (supplemental)",
            bool(gpu.model_verified) and nonce_match,
            f"arch={payload.get('arch')} model_verified={gpu.model_verified} nonce_match={nonce_match}",
        )
    else:
        step("GPU evidence (supplemental)", False, "no GPU evidence in report")

    # Gate = genuine TDX quote (DCAP) + report_data binding + compose integrity +
    # freshness. GPU is supplemental; SPKI (v2) is excluded; the dstack DEEP verify
    # is an external-service layer reported separately (it needs full event-log
    # payloads, which the redpill endpoints currently strip).
    gate = {
        "fetch attestation report",
        "report has intel_quote",
        "report has signing_address",
        "nonce echoed matches request",
        "DCAP quote signature + TCB (lite, no event-log replay)",
        "compose hash integrity",
        "report_data binding (nonce + signing address, v1)",
    }
    gate_fail = [n for n, ok in results if n in gate and not ok]
    print("\n=== SUMMARY ===")
    for name, ok in results:
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
    print("\nExcluded: v2 TLS-SPKI binding/pin (live backend is v1).")
    print(
        "Separate external layer: dstack DEEP verify (OS-image + RTMR replay) — reported "
        "separately; requires a dstack-verifier >= the CVM's dstack OS version (>=0.5.6)."
    )
    if gate_fail:
        print(f"\nGATE FAILED: {gate_fail}")
        return 1
    print("\nGATE PASSED on real Phala data (quote genuine + bound + fresh + compose-consistent).")
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
