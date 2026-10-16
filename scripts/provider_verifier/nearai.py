"""NEAR AI provider verification."""

from __future__ import annotations

import asyncio
import contextlib
import os
import sys
from typing import Any

from .common import (
    emit,
    failed,
    json_evidence_bundle,
    model_dump,
    verifier_id_for,
    tdx_session_material,
)


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
    provider_options = request.get("provider_options") or {}
    near_provider = NearaiProvider(
        include_tls_fingerprint=True,
        api_key=provider_options.get("near_ai_api_key"),
    )
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

    # NEAR AI is a router: this verifier attests the gateway TEE channel
    # (attested_scope = "router"), shared by every model. Per-model TEE coverage
    # is delegated to the verified gateway, which attests its own backends; the
    # model is only a fetch parameter and is recorded on the receipt as an
    # identifier.
    #
    # Surface the granular gateway TDX TCB status (e.g. "UpToDate", "OutOfDate")
    # so the session layer can populate a tri-state `tcb_up_to_date` claim. The
    # dstack verifier reports TCB freshness separately from its overall is_valid
    # (is_valid covers quote signature / measurement / event-log replay, not TCB
    # freshness), so a stale TCB surfaces here without failing the gateway.
    gateway_dstack = (gateway_result.get("details") or {}).get("dstack") or {}
    gateway_tcb_status = (gateway_dstack.get("details") or {}).get("tcb_status")

    # Channel-scoped, model-independent evidence: the gateway TD attestation and
    # the TLS binding the dstack verifier checked, nonce-bound for freshness.
    channel_evidence = {
        "provider": "nearai",
        "trust_boundary": "near-ai-gateway",
        "request_nonce": report.request_nonce,
        "tls_cert_fingerprint": spki,
        "gateway_attestation": gateway,
    }
    provider_claims = {
        "trust_boundary": "near-ai-gateway",
        "gateway_verified": True,
        "gateway_tls_spki_sha256": spki,
        "tcb_status": gateway_tcb_status,
    }

    # Only the replayed, authenticated event-log identity is eligible. Supplied
    # info.compose_hash is metadata, not the measured compose identity.
    compose_hash = (
        (gateway_dstack.get("details") or {}).get("app_info") or {}
    ).get("compose_hash")
    gpu = (gateway_result.get("details") or {}).get("gpu")
    provider_claims.update(
        {
            "signing_address": gateway.get("signing_address"),
            "compose_hash": compose_hash,
            "gateway_gpu_check": {
                "present": bool(gateway.get("nvidia_payload")),
                "accepted": isinstance(gpu, dict) and gpu.get("model_verified") is True,
            },
        }
    )
    material = tdx_session_material(gateway.get("intel_quote"))
    if material and compose_hash:
        provider_claims.update(material)
        provider_claims["session_material_version"] = "near-ai/1"

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "attested_scope": "router",
            "evidence": json_evidence_bundle(channel_evidence, attestation_url),
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

