"""NEAR AI provider verification."""

from __future__ import annotations

import asyncio
import base64
import contextlib
import gzip
import hashlib
import json
import os
import secrets
import sys
import time
import urllib.parse
import urllib.request
from typing import Any

from .common import *  # noqa: F401,F403


async def verify_nearai(request: dict[str, Any]) -> None:
    from confidential_verifier.providers.nearai import NearaiProvider
    from confidential_verifier.verifiers.nearai import NearAICloudVerifier

    provider = "near-ai"
    verifier_id = verifier_id_for(provider)
    # Fail loudly on bridge/verifier contract drift instead of letting a missing
    # method surface as a cryptic AttributeError mid-verification.
    if not hasattr(NearAICloudVerifier, "verify_gateway_component"):
        failed(
            provider,
            "verifier contract drift: NearAICloudVerifier is missing "
            "verify_gateway_component; the confidential_verifier package is out of sync "
            "with this bridge (see scripts/confidential_verifier/VENDOR.md)",
            verifier_id=verifier_id,
        )
        return
    near_provider = NearaiProvider(include_tls_fingerprint=True)
    dstack_verifier_url = os.getenv("DSTACK_VERIFIER_URL", "http://localhost:8080")
    with contextlib.redirect_stdout(sys.stderr):
        report = await asyncio.to_thread(near_provider.fetch_report, request["model_id"])
        verifier = NearAICloudVerifier(dstack_verifier_url)
        gateway = (report.raw or {}).get("gateway_attestation") or {}
        gateway_result = await verifier.verify_gateway_component(
            report.raw or {},
            report.request_nonce,
        )
    report_obj = model_dump(report)
    attestation_url = "https://cloud-api.near.ai/v1/attestation/report"
    if not gateway:
        failed(
            provider,
            "NEAR AI report did not include gateway_attestation",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return
    spki = gateway.get("tls_cert_fingerprint")
    if not spki:
        failed(
            provider,
            "NEAR AI report did not include TLS SPKI binding",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return
    if not gateway_result.get("is_valid"):
        failed(
            provider,
            "; ".join(gateway_result.get("errors") or [])
            or "NEAR AI gateway verification failed",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return

    raw_report = report.raw or {}
    model_attestations = raw_report.get("model_attestations") or []
    if not isinstance(model_attestations, list) or not model_attestations:
        failed(
            provider,
            "NEAR AI model-scoped report did not include model_attestations",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return

    for index, item in enumerate(model_attestations):
        if not isinstance(item, dict) or not item.get("intel_quote"):
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] did not include intel_quote",
                evidence=json_evidence_bundle(report_obj, attestation_url),
                verifier_id=verifier_id,
            )
            return
        item_nonce = item.get("request_nonce")
        if item_nonce is not None and str(item_nonce).lower() != str(report.request_nonce).lower():
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] nonce did not match request nonce",
                evidence=json_evidence_bundle(report_obj, attestation_url),
                verifier_id=verifier_id,
            )
            return
        # The gateway does not re-verify these nested model quotes through the
        # dstack verifier, so check the TD debug bit here: a debug-mode model TD
        # would otherwise be accepted on the gateway's word alone.
        try:
            if tdx_debug_enabled(bytes.fromhex(item["intel_quote"])):
                failed(
                    provider,
                    f"NEAR AI model_attestations[{index}] TDX quote is in debug mode",
                    evidence=json_evidence_bundle(report_obj, attestation_url),
                    verifier_id=verifier_id,
                )
                return
        except ValueError as exc:
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] intel_quote is not valid TDX bytes: {exc}",
                evidence=json_evidence_bundle(report_obj, attestation_url),
                verifier_id=verifier_id,
            )
            return

    model_attestations_sha256 = sha256_json_prefixed(model_attestations)
    # Surface the granular gateway TDX TCB status (e.g. "UpToDate", "OutOfDate")
    # so the session layer can populate a tri-state `tcb_up_to_date` claim. The
    # dstack verifier reports TCB freshness separately from its overall is_valid
    # (is_valid covers quote signature / measurement / event-log replay, not TCB
    # freshness), so a stale TCB surfaces here without failing the gateway.
    gateway_dstack = (gateway_result.get("details") or {}).get("dstack") or {}
    gateway_tcb_status = (gateway_dstack.get("details") or {}).get("tcb_status")
    provider_claims = {
        "trust_boundary": "near-ai-gateway",
        "gateway_verified": True,
        "gateway_tls_spki_sha256": spki,
        "model_evidence_present": True,
        "model_attestation_count": len(model_attestations),
        "model_attestations_sha256": model_attestations_sha256,
        "nested_model_attestations_checked_by_gateway": False,
        "canonical_model_id": report.model_id,
        "tcb_status": gateway_tcb_status,
    }
    if all(isinstance(item, dict) and item.get("request_nonce") for item in model_attestations):
        provider_claims["model_attestations_nonce_matched"] = True

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "evidence": json_evidence_bundle(report_obj, attestation_url),
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": request.get("url_origin"),
                    "spki_sha256": spki,
                }
            ],
            "provider_claims": provider_claims,
        }
    )
    return

