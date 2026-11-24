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
    tdx_session_material,
)

ACCEPTED_SUBJECT_OPTION = "phala_direct_accepted_subject:"
COMPOSE_SUBJECT_PREFIX = "dstack-compose:"


def _phala_direct_compose_hash_ok(
    info: dict[str, Any], dstack_result: dict[str, Any]
) -> tuple[bool, str]:
    """Verify SHA256(app_compose) == the compose-hash event the dstack verifier
    replayed into RTMR3. Returns (ok, reason)."""
    tcb_info = info.get("tcb_info") or {}
    if isinstance(tcb_info, str):
        try:
            tcb_info = json.loads(tcb_info)
        except json.JSONDecodeError:
            tcb_info = {}
    app_compose = tcb_info.get("app_compose")
    if not app_compose:
        return False, "PhalaDirect report is missing app_compose"
    details = dstack_result.get("details") or {}
    app_info = details.get("app_info") or {}
    measured = app_info.get("compose_hash")
    if not measured:
        return False, "dstack verifier returned no event-log compose hash"
    calculated = hashlib.sha256(app_compose.encode("utf-8")).hexdigest()
    if calculated.lower() != str(measured).lower():
        return False, "PhalaDirect app_compose does not match the RTMR3 compose hash"
    return True, ""


async def verify_phala_direct(request: dict[str, Any]) -> None:
    """Verify a Phala Inference Guard or dstack-vllm-proxy endpoint directly.

    Verify the version 2 report's TDX quote, report_data binding (signing
    address, request nonce and custom-domain TLS SPKI), and measured compose.
    Return the attested TLS SPKI for the forwarding backend to pin. Record
    GPU evidence, TCB status and OS classification as supplemental claims.
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
        failed(
            provider,
            "PhalaDirect upstream is missing url_origin",
            failure="verifier_misconfigured",
            verifier_id=verifier_id,
        )
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

    def _fetch_report() -> requests.Response:
        response = requests.get(
            attestation_url, params=params, headers=headers, timeout=timeout
        )
        response.raise_for_status()
        return response

    try:
        # requests is blocking; run it off the event loop (as with the dstack call,
        # and consistent with the rest of the bridge's HTTP).
        response = await asyncio.to_thread(_fetch_report)
    except Exception as exc:  # noqa: BLE001
        failed(
            provider,
            f"failed to fetch PhalaDirect attestation report: {exc}",
            failure="evidence_unavailable",
            verifier_id=verifier_id,
        )
        return
    # A report that arrived but is not JSON was fetched: it did not verify.
    try:
        report = response.json()
    except ValueError as exc:
        failed(
            provider,
            f"PhalaDirect attestation report is not JSON: {exc}",
            failure="evidence_invalid",
            verifier_id=verifier_id,
        )
        return

    evidence = json_evidence_bundle(report, attestation_url)

    # Both producers expose the per-model attestation at the top level, and may
    # repeat it in all_attestations. Use the top-level fields.
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
        failed(
            provider,
            "PhalaDirect report missing intel_quote",
            failure="evidence_invalid",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return
    if not signing_address:
        failed(
            provider,
            "PhalaDirect report missing signing_address",
            failure="evidence_invalid",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return
    if not tls_cert_fingerprint:
        failed(
            provider,
            "PhalaDirect report did not include tls_cert_fingerprint; the producer "
            "must serve attestation version 2 (custom-domain TLS SPKI binding)",
            failure="evidence_invalid",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return
    if report_nonce is not None and str(report_nonce).lower() != nonce.lower():
        failed(
            provider,
            "PhalaDirect report nonce did not match request nonce",
            failure="binding_mismatch",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # Reject a TD running in debug/untrusted mode: its CPU state and private
    # memory are accessible to the host, so the TEE guarantee does not hold.
    try:
        if tdx_debug_enabled(bytes.fromhex(intel_quote)):
            failed(
                provider,
                "PhalaDirect TDX quote is in debug mode (TD_ATTRIBUTES TUD set)",
                failure="policy_rejected",
                evidence=evidence,
                verifier_id=verifier_id,
            )
            return
    except ValueError as exc:
        failed(
            provider,
            f"PhalaDirect intel_quote is not valid TDX quote bytes: {exc}",
            failure="evidence_invalid",
            evidence=evidence,
            verifier_id=verifier_id,
        )
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
            failure="quote_invalid",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # 2. Compose hash: SHA256(app_compose) == the RTMR3-measured compose hash.
    compose_ok, compose_reason = _phala_direct_compose_hash_ok(info, dstack_result)
    if not compose_ok:
        failed(
            provider,
            compose_reason,
            failure="provenance_not_accepted",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    dstack_details = dstack_result["details"]
    app_info = dstack_details["app_info"]
    compose_hash = app_info["compose_hash"]
    compose_pins = {
        key[len(ACCEPTED_SUBJECT_OPTION):]
        for key, value in options.items()
        if key.startswith(ACCEPTED_SUBJECT_OPTION + COMPOSE_SUBJECT_PREFIX) and value == "true"
    }
    subject = f"{COMPOSE_SUBJECT_PREFIX}sha256:{str(compose_hash).lower()}"
    if compose_pins and subject not in compose_pins:
        failed(
            provider,
            "PhalaDirect measured compose hash is not in accepted_subjects",
            failure="provenance_not_accepted",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # 3. report_data binding: nonce + signing address + TLS SPKI. Parse
    # report_data from the quote bytes the dstack verifier just proved authentic
    # (it does not surface report_data itself).
    report_data_hex = dstack_result.get("report_data") or _tdx_report_data_hex(intel_quote)
    if not report_data_hex:
        failed(
            provider,
            "could not obtain report_data from PhalaDirect quote; cannot verify nonce/address/TLS binding",
            failure="evidence_invalid",
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
            failure="binding_mismatch",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # 4. GPU (NVIDIA confidential computing) evidence — SUPPLEMENTAL, never a gate.
    # The NVIDIA path decodes JWTs without signature verification. Its outcome
    # does not authenticate GPU identity or prove continued use of any device.
    # The CPU TEE quote + report_data binding + compose integrity above are the
    # trust gate. Record the supplemental GPU outcome and nonce matching as
    # metadata; neither establishes binding to the serving CPU or request.
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

    # The verified OS image hash binds the published metadata, including is_dev.
    # Known hashes resolve offline. Unresolved hashes leave the OS claim unknown.
    tcb_status = dstack_details.get("tcb_status")
    os_image_hash = app_info.get("os_image_hash") or dstack_details.get("os_image_hash")

    os_image = resolve_os_image(os_image_hash) if os_image_hash else None
    if os_image is not None:
        os_image_is_dev = bool(os_image.get("is_dev"))
        os_image_version = os_image.get("version")
        production_os_image = not os_image_is_dev
    else:
        os_image_is_dev = None
        os_image_version = None
        production_os_image = None

    material = tdx_session_material(intel_quote)
    if material:
        material["session_material_version"] = "phala-direct/1"

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
                **material,
                "trust_boundary": "phala-dstack-cvm",
                "evidence_scope": "model_instance",
                "canonical_model_id": request["model_id"],
                "attestation_version": 2,
                "tls_spki_from_report_data": True,
                "signing_address": signing_address,
                "report_data_nonce_matched": True,
                "compose_hash": compose_hash,
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

