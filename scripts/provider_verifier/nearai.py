"""NEAR AI provider verification.

Two layers, both from one nonce-bound report
(`/v1/attestation/report?model=…&provider=near&include_tls_fingerprint=true`):

1. The NEAR gateway (router) TEE and its TLS key. This is the channel binding,
   verified exactly as before; if it fails, the upstream fails closed.
2. Every model enclave NEAR reports for the requested model: TDX quote and event
   log (dstack verifier), compose hash, `report_data` binding of the enclave's
   signing key and TLS fingerprint to the nonce, and NVIDIA GPU evidence (NRAS,
   nonce-matched).

When at least one model enclave verifies, the result is model-scoped and lists
the verified enclave signers (`near_session`). NEAR signs non-streaming
responses from the serving enclave (`provider_tee` signatures over
`<model>:<sha256(request)>:<sha256(response)>`), so the backend can bind each
response to one verified enclave and cite that enclave's own session. With no
verified model enclave, the result falls back to the router scope.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import secrets
import sys
import time
from typing import Any

from .common import (
    emit,
    failed,
    json_evidence_bundle,
    verifier_id_for,
)

ATTESTATION_URL = "https://cloud-api.near.ai/v1/attestation/report"
SIGNATURE_PATH = "/v1/signature/{chat_id}"
FETCH_ATTEMPTS = 5
# (connect, read) seconds. NEAR's report endpoint intermittently stalls on connect
# or mid-body; without timeouts the verifier would hang until the backend's outer
# deadline. Retries back off 2, 4, 6, 8 seconds.
FETCH_TIMEOUT = (15, 60)


def fetch_report(model_id: str, api_key: str | None, nonce: str) -> dict[str, Any]:
    import requests

    params = {
        "model": model_id,
        "signing_algo": "ecdsa",
        "nonce": nonce,
        "include_tls_fingerprint": "true",
        "provider": "near",
    }
    headers = {"Accept": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    last_error: Exception | None = None
    for attempt in range(FETCH_ATTEMPTS):
        try:
            response = requests.get(ATTESTATION_URL, params=params, headers=headers, timeout=FETCH_TIMEOUT)
            response.raise_for_status()
            return response.json()
        except Exception as exc:  # noqa: BLE001 - every failure is retried, then reported
            last_error = exc
            time.sleep(2 * (attempt + 1))
    raise RuntimeError(f"NEAR attestation report fetch failed after {FETCH_ATTEMPTS} attempts: {last_error}")


def dstack_details(component: dict[str, Any]) -> dict[str, Any]:
    dstack = (component.get("details") or {}).get("dstack") or {}
    return dstack.get("details") or {}


def tcb_status_of(component: dict[str, Any]) -> str | None:
    return dstack_details(component).get("tcb_status")


def production_os_image(component: dict[str, Any]) -> bool | None:
    """True when the dstack verifier reproduced the OS image hash and the image
    is a production build; False for a dev image; None when unresolved."""
    details = dstack_details(component)
    if not details.get("os_image_hash_verified"):
        return None
    is_dev = details.get("os_image_is_dev")
    return None if is_dev is None else not is_dev


def worst_tcb_status(statuses: list[str | None]) -> str | None:
    """UpToDate only when every present status is UpToDate; otherwise the first
    stale status, so the tri-state tcb_up_to_date claim refutes."""
    present = [s for s in statuses if s]
    if not present:
        return None
    return next((s for s in present if s != "UpToDate"), "UpToDate")


def enclave_facts(entry: dict[str, Any], component: dict[str, Any]) -> dict[str, Any]:
    info = entry.get("info") or {}
    gpu = (component.get("details") or {}).get("gpu") or {}
    errors = component.get("errors") or []
    return {
        "signing_address": str(entry.get("signing_address", "")).lower(),
        "signing_algo": entry.get("signing_algo"),
        "tcb_status": tcb_status_of(component),
        "gpu_verified": bool(gpu.get("model_verified")),
        "gpu_evidence_present": bool(entry.get("nvidia_payload")),
        "gpu_nonce_matched": not any("GPU nonce mismatch" in e for e in errors),
        "compose_hash_verified": bool((component.get("details") or {}).get("compose_verified")),
        "production_os_image": production_os_image(component),
        "app_id": info.get("app_id"),
        "instance_id": info.get("instance_id"),
        "compose_hash": info.get("compose_hash"),
        "os_image_hash": info.get("os_image_hash"),
    }


async def verify_nearai(request: dict[str, Any]) -> None:
    from confidential_verifier.verifiers.nearai import NearAICloudVerifier

    provider = "near-ai"
    verifier_id = verifier_id_for(provider)
    # Fail loudly on bridge/verifier contract drift instead of letting a missing
    # method surface as a cryptic AttributeError mid-verification.
    for method in ("verify_gateway_component", "_verify_component"):
        if not hasattr(NearAICloudVerifier, method):
            failed(
                provider,
                f"verifier contract drift: NearAICloudVerifier is missing {method}; the "
                "confidential_verifier package is out of sync with this bridge "
                "(see scripts/confidential_verifier/VENDOR.md)",
                verifier_id=verifier_id,
            )
            return

    model_id = request["model_id"]
    api_key = (request.get("provider_options") or {}).get("near_ai_api_key")
    nonce = secrets.token_hex(32)
    dstack_verifier_url = os.getenv("DSTACK_VERIFIER_URL", "http://localhost:8080")
    # The verifier library prints progress to stdout, which carries this bridge's
    # JSON result, so its output is redirected; results are emitted after the block.
    try:
        raw = await asyncio.to_thread(fetch_report, model_id, api_key, nonce)
    except RuntimeError as exc:
        failed(provider, str(exc), verifier_id=verifier_id)
        return
    with contextlib.redirect_stdout(sys.stderr):
        verifier = NearAICloudVerifier(dstack_verifier_url)
        gateway_result = await verifier.verify_gateway_component(raw, nonce)
        entries = [
            entry
            for entry in raw.get("model_attestations") or []
            if entry.get("model_name") in (None, model_id)
        ]
        model_results = [
            await verifier._verify_component(f"model-{i}", entry, nonce)
            for i, entry in enumerate(entries)
        ]

    report_evidence = {"provider": "nearai", "request_nonce": nonce, "report": raw}
    gateway = raw.get("gateway_attestation") or {}
    if not gateway:
        failed(
            provider,
            "NEAR AI report did not include gateway_attestation",
            evidence=json_evidence_bundle(report_evidence, ATTESTATION_URL),
            verifier_id=verifier_id,
        )
        return
    spki = gateway.get("tls_cert_fingerprint")
    if not spki:
        failed(
            provider,
            "NEAR AI report did not include TLS SPKI binding",
            evidence=json_evidence_bundle(report_evidence, ATTESTATION_URL),
            verifier_id=verifier_id,
        )
        return
    if not gateway_result.get("is_valid"):
        failed(
            provider,
            "; ".join(gateway_result.get("errors") or []) or "NEAR AI gateway verification failed",
            evidence=json_evidence_bundle(report_evidence, ATTESTATION_URL),
            verifier_id=verifier_id,
        )
        return

    gateway_tcb_status = tcb_status_of(gateway_result)
    gateway_production_os = production_os_image(gateway_result)
    channel_bindings = [{"type": "tls_spki_sha256", "origin": request.get("url_origin"), "spki_sha256": spki}]
    verified = [
        enclave_facts(entry, result)
        for entry, result in zip(entries, model_results)
        if result.get("is_valid") and entry.get("signing_address")
    ]
    failed_enclaves = {
        str(entry.get("signing_address", f"model-{i}")).lower(): result.get("errors") or ["not valid"]
        for i, (entry, result) in enumerate(zip(entries, model_results))
        if not result.get("is_valid")
    }

    if not verified:
        # Router scope only: the gateway TEE and its TLS key, as before. Model
        # enclaves are unverified, so no model-level claim is made.
        provider_claims = {
            "trust_boundary": "near-ai-gateway",
            "gateway_verified": True,
            "gateway_tls_spki_sha256": spki,
            "tcb_status": gateway_tcb_status,
            "production_os_image": gateway_production_os,
            "model_enclaves_verified": 0,
        }
        if failed_enclaves:
            provider_claims["failed_enclaves"] = failed_enclaves
        emit(
            {
                "result": "verified",
                "verifier_id": verifier_id,
                "attested_scope": "model",
                "evidence": json_evidence_bundle(report_evidence, ATTESTATION_URL),
                "channel_bindings": channel_bindings,
                "provider_claims": provider_claims,
            }
        )
        return

    # The router sees traffic, so its TCB counts: the session's status is the
    # worst of the router and every verified enclave.
    provider_claims = {
        "trust_boundary": "near-ai-model-enclave",
        "evidence_scope": "model_instance",
        "canonical_model_id": model_id,
        "gateway_verified": True,
        "gateway_tls_spki_sha256": spki,
        "gateway_tcb_status": gateway_tcb_status,
        "gateway_production_os_image": gateway_production_os,
        "tcb_status": worst_tcb_status([gateway_tcb_status] + [v["tcb_status"] for v in verified]),
        "model_enclaves_verified": len(verified),
        "verified_signers": [v["signing_address"] for v in verified],
        "instance_tcb_statuses": {v["signing_address"]: v["tcb_status"] for v in verified},
        "instance_gpu": {
            v["signing_address"]: {
                "gpu_verified": v["gpu_verified"],
                "gpu_evidence_present": v["gpu_evidence_present"],
                "gpu_nonce_matched": v["gpu_nonce_matched"],
            }
            for v in verified
        },
        "instance_workloads": {
            v["signing_address"]: {
                key: v[key]
                for key in ("app_id", "instance_id", "compose_hash", "os_image_hash", "compose_hash_verified", "production_os_image")
            }
            for v in verified
        },
        "gpu_verified": all(v["gpu_verified"] for v in verified),
        "gpu_evidence_present": any(v["gpu_evidence_present"] for v in verified),
        "response_signature": {"path": SIGNATURE_PATH, "signing_algo": "ecdsa", "kind": "provider_tee"},
    }
    if failed_enclaves:
        provider_claims["failed_enclaves"] = failed_enclaves
    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "attested_scope": "model",
            "evidence": json_evidence_bundle(report_evidence, ATTESTATION_URL),
            "channel_bindings": channel_bindings,
            "provider_claims": provider_claims,
            "near_session": {
                "model_id": model_id,
                "signers": [
                    {"signing_address": v["signing_address"], "signing_algo": v["signing_algo"]}
                    for v in verified
                ],
            },
        }
    )
