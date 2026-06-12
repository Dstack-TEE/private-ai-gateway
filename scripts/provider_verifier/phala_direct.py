"""PhalaDirect provider verification."""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import os
import secrets
import sys
from typing import Any

from .common import (
    emit,
    failed,
    json_evidence_bundle,
    provider_options,
    tdx_debug_enabled,
    verifier_id_for,
)


def _phala_direct_compose_hash_ok(info: dict[str, Any]) -> tuple[bool, str]:
    """Verify SHA256(app_compose) == reported compose_hash, mirroring the dstack
    verifiers. Returns (ok, reason)."""
    tcb_info = info.get("tcb_info") or {}
    if isinstance(tcb_info, str):
        try:
            tcb_info = json.loads(tcb_info)
        except json.JSONDecodeError:
            tcb_info = {}
    app_compose = tcb_info.get("app_compose")
    reported = info.get("compose_hash")
    if not app_compose or not reported:
        return False, "PhalaDirect report is missing app_compose or compose_hash"
    calculated = hashlib.sha256(app_compose.encode("utf-8")).hexdigest()
    if calculated.lower() != str(reported).lower():
        return False, "PhalaDirect compose hash mismatch"
    return True, ""


async def verify_phala_direct(request: dict[str, Any]) -> None:
    """Verify a Phala dstack-vllm-proxy attestation endpoint reached directly.

    Mirrors the NEAR AI path: fetch the per-model attestation report (version 2),
    verify the dstack TDX quote, the report_data binding (signing address +
    request nonce + custom-domain TLS SPKI), the compose hash, and the GPU
    evidence; then return the attested TLS SPKI as the enforceable channel
    binding so the forwarding backend pins the connection.
    """
    import requests

    from confidential_verifier.verifiers.dstack import DstackVerifier, verify_report_data
    from confidential_verifier.verifiers.nearai import _tdx_report_data_hex
    from confidential_verifier.verifiers.nvidia import NvidiaGpuVerifier
    from dstack_os_image import resolve_os_image

    provider = "phala-direct"
    verifier_id = verifier_id_for(provider)
    options = provider_options(request)

    raw_origin = request.get("url_origin")
    if not raw_origin:
        failed(provider, "PhalaDirect upstream is missing url_origin", verifier_id=verifier_id)
        return
    url_origin = raw_origin.rstrip("/")
    bearer = (options.get("phala_direct_bearer_token") or "").strip()
    try:
        timeout = int(request.get("timeout_seconds") or 30)
    except (TypeError, ValueError):
        timeout = 30

    nonce = secrets.token_hex(32)
    attestation_url = f"{url_origin}/v1/attestation/report"
    params = {"signing_algo": "ecdsa", "nonce": nonce, "version": "2"}
    headers = {"Authorization": f"Bearer {bearer}"} if bearer else {}

    def _fetch_report() -> dict[str, Any]:
        response = requests.get(
            attestation_url, params=params, headers=headers, timeout=timeout
        )
        response.raise_for_status()
        return response.json()

    try:
        # requests is blocking; run it off the event loop (as with the dstack call,
        # and consistent with the rest of the bridge's HTTP).
        report = await asyncio.to_thread(_fetch_report)
    except Exception as exc:  # noqa: BLE001
        failed(
            provider,
            f"failed to fetch PhalaDirect attestation report: {exc}",
            verifier_id=verifier_id,
        )
        return

    evidence = json_evidence_bundle(report, attestation_url)

    # A per-model PhalaDirect endpoint returns a single attestation at the top level
    # of the report — use it directly. (all_attestations is a multi-instance gateway
    # shape and is not expected here.)
    attestation = report

    intel_quote = attestation.get("intel_quote") or attestation.get("quote")
    signing_address = attestation.get("signing_address")
    tls_cert_fingerprint = attestation.get("tls_cert_fingerprint")
    event_log = attestation.get("event_log") or ""
    vm_config = attestation.get("vm_config") or ""
    info = attestation.get("info") or {}
    report_nonce = attestation.get("request_nonce")
    nvidia_payload = attestation.get("nvidia_payload")

    if not intel_quote:
        failed(provider, "PhalaDirect report missing intel_quote", evidence=evidence, verifier_id=verifier_id)
        return
    if not signing_address:
        failed(provider, "PhalaDirect report missing signing_address", evidence=evidence, verifier_id=verifier_id)
        return
    if not tls_cert_fingerprint:
        failed(
            provider,
            "PhalaDirect report did not include tls_cert_fingerprint; the proxy "
            "must serve attestation version 2 (custom-domain TLS SPKI binding)",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return
    if report_nonce is not None and str(report_nonce).lower() != nonce.lower():
        failed(provider, "PhalaDirect report nonce did not match request nonce", evidence=evidence, verifier_id=verifier_id)
        return

    # Reject a TD running in debug/untrusted mode: its CPU state and private
    # memory are accessible to the host, so the TEE guarantee does not hold.
    try:
        if tdx_debug_enabled(bytes.fromhex(intel_quote)):
            failed(provider, "PhalaDirect TDX quote is in debug mode (TD_ATTRIBUTES TUD set)", evidence=evidence, verifier_id=verifier_id)
            return
    except ValueError as exc:
        failed(provider, f"PhalaDirect intel_quote is not valid TDX quote bytes: {exc}", evidence=evidence, verifier_id=verifier_id)
        return

    if isinstance(vm_config, (dict, list)):
        vm_config = json.dumps(vm_config)

    dstack_verifier_url = os.getenv("DSTACK_VERIFIER_URL", "http://localhost:8080")

    # 1. dstack TDX quote / TCB / OS-image verification. Wrap only the noisy
    # verifier call in the stdout redirect; emit()/failed() must reach real stdout
    # (the Rust caller reads the result JSON from stdout).
    with contextlib.redirect_stdout(sys.stderr):
        dstack_result = await asyncio.to_thread(
            DstackVerifier(dstack_verifier_url).verify, intel_quote, event_log, vm_config
        )
    if not dstack_result.get("is_valid"):
        failed(
            provider,
            f"dstack quote verification failed: {dstack_result.get('reason', 'unknown')}",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # 2. Compose hash: SHA256(app_compose) == reported compose_hash.
    compose_ok, compose_reason = _phala_direct_compose_hash_ok(info)
    if not compose_ok:
        failed(provider, compose_reason, evidence=evidence, verifier_id=verifier_id)
        return

    # 3. report_data binding: nonce + signing address + TLS SPKI. Parse
    # report_data from the quote bytes the dstack verifier just proved authentic
    # (it does not surface report_data itself).
    report_data_hex = dstack_result.get("report_data") or _tdx_report_data_hex(intel_quote)
    if not report_data_hex:
        failed(
            provider,
            "could not obtain report_data from PhalaDirect quote; cannot verify nonce/address/TLS binding",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return
    binding = verify_report_data(
        report_data_hex,
        signing_address,
        nonce,
        tls_cert_fingerprint=tls_cert_fingerprint,
    )
    if not binding.get("valid"):
        failed(
            provider,
            f"PhalaDirect report_data binding failed: {binding.get('error') or 'mismatch'}",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # 4. GPU (NVIDIA confidential computing) evidence — SUPPLEMENTAL, never a gate.
    # The CPU TEE quote + report_data binding + compose integrity above are the
    # trust gate. A standalone gateway-side NRAS check only proves a CC-capable
    # GPU exists for a nonce; it does not prove that GPU is bound to this CPU TEE
    # or serving this request (that is the measured serving software's job, inside
    # the quote). So we record the GPU outcome as metadata and do not fail on it.
    gpu_evidence_present = False
    gpu_evidence_nonce_matched: bool | None = None
    gpu_attested = False
    gpu_arch = None
    payload = nvidia_payload
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except json.JSONDecodeError:
            payload = None
    if isinstance(payload, dict) and payload.get("evidence_list"):
        gpu_evidence_present = True
        gpu_arch = payload.get("arch")
        gpu_nonce = payload.get("nonce")
        gpu_evidence_nonce_matched = bool(gpu_nonce) and str(gpu_nonce).lower() == nonce.lower()
        try:
            with contextlib.redirect_stdout(sys.stderr):
                gpu_result = await NvidiaGpuVerifier().verify(payload)
            gpu_attested = bool(gpu_result.model_verified) and bool(gpu_evidence_nonce_matched)
        except Exception:  # noqa: BLE001 - supplemental; a GPU error is never fatal
            gpu_attested = False

    # Surface the granular TDX TCB status (e.g. "UpToDate", "OutOfDate") so the
    # session layer can populate a tri-state `tcb_up_to_date` claim instead of
    # only seeing the dstack verifier's overall is_valid.
    dstack_details = dstack_result.get("details") if isinstance(dstack_result, dict) else None
    tcb_status = dstack_details.get("tcb_status") if isinstance(dstack_details, dict) else None

    # OS-image provenance (production-vs-dev decision).
    #
    # The attested os_image_hash (proved by the dstack verifier — is_valid implies
    # os_image_hash_verified) is SHA256(sha256sum.txt) of dstack's published OS
    # image, and that manifest pins SHA256(metadata.json) — so the image's `is_dev`
    # flag is cryptographically bound to the attested hash. resolve_os_image()
    # re-downloads the published image, re-verifies that binding, and reads is_dev
    # (known fleet images are seeded, so the common case is offline). A dev image
    # (dstack-nvidia-dev-*, SSH/serial-console enabled) is NOT a production OS.
    #
    # This is recorded metadata, not a gate (mirrors how GPU evidence is handled):
    # the deployed fleet currently runs dev images, and gating here would reject
    # them. The session layer decides policy. None means "could not resolve"
    # (unknown hash + offline/unreachable) — never silently treated as production.
    app_info = dstack_details.get("app_info") if isinstance(dstack_details, dict) else None
    os_image_hash = app_info.get("os_image_hash") if isinstance(app_info, dict) else None
    if not os_image_hash and isinstance(dstack_details, dict):
        os_image_hash = dstack_details.get("os_image_hash")

    os_image = resolve_os_image(os_image_hash) if os_image_hash else None
    if os_image is not None:
        os_image_is_dev = bool(os_image.get("is_dev"))
        os_image_version = os_image.get("version")
        production_os_image = not os_image_is_dev
    else:
        os_image_is_dev = None
        os_image_version = None
        production_os_image = None

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "evidence": evidence,
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": raw_origin,
                    "spki_sha256": tls_cert_fingerprint,
                }
            ],
            "provider_claims": {
                "trust_boundary": "phala-dstack-cvm",
                "evidence_scope": "model_instance",
                "canonical_model_id": request["model_id"],
                "attestation_version": 2,
                "tls_spki_from_report_data": True,
                "signing_address": signing_address,
                "report_data_nonce_matched": True,
                "compose_hash_verified": True,
                "tdx_debug_mode": False,
                # OS-image provenance, resolved from dstack's published image and
                # bound to the attested os_image_hash. production_os_image is the
                # prod-vs-dev decision (None only if the hash could not be resolved).
                "os_image_hash": os_image_hash,
                "os_image_version": os_image_version,
                "os_image_is_dev": os_image_is_dev,
                "production_os_image": production_os_image,
                # GPU is supplemental metadata, not part of the trust gate.
                "gpu_verified": gpu_attested,
                "gpu_evidence_present": gpu_evidence_present,
                "gpu_evidence_nonce_matched": gpu_evidence_nonce_matched,
                "gpu_arch": gpu_arch,
                "tcb_status": tcb_status,
            },
        }
    )

