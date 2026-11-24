"""NEAR AI provider verification."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import re
import sys
import time
from collections import Counter
from concurrent.futures import Future
from threading import Thread
from typing import Any

from requests.exceptions import ConnectionError, HTTPError, Timeout

from .common import (
    ResponseTooLarge,
    emit,
    evidence_bundle,
    failed,
    json_bytes,
    json_evidence_bundle,
    verifier_id_for,
    tdx_session_material,
)


ATTESTATION_URL = "https://cloud-api.near.ai/v1/attestation/report"
MODEL_VERIFICATION_BUDGET = 8.0
MAX_MODEL_CANDIDATES = 8
MAX_MODEL_EVIDENCE_BYTES = 2 * 1024 * 1024


def reported_model_name(entry: dict[str, Any]) -> str | None:
    if (
        "model_name" not in entry
        and not entry.get("intel_quote")
        and entry.get("provider") == "chutes"
    ):
        # The public Chutes report uses the canonical ID in "model".
        # This permits only gateway evidence, never a model TDX candidate.
        return entry.get("model")
    return entry.get("model_name")


async def verify_model_candidates(
    verifier: Any,
    entries: list[dict[str, Any]],
    nonce: str,
) -> dict[str, int]:
    # A shared key is ambiguous even if one quote fails. If no unique key
    # exists, there is no single evidence entry to reference.
    addresses = []
    for entry in entries:
        address = entry.get("signing_address")
        quote = entry.get("intel_quote")
        fingerprint = entry.get("tls_cert_fingerprint")
        if (
            not isinstance(address, str)
            or not re.fullmatch(r"0x[0-9a-fA-F]{40}", address)
            or entry.get("signing_algo") != "ecdsa"
            or not isinstance(quote, str)
            or not quote
            or not isinstance(fingerprint, str)
            or not re.fullmatch(r"(?:0x)?[0-9a-fA-F]{64}", fingerprint)
        ):
            return {}
        addresses.append(address.lower())

    counts = Counter(addresses)
    if 1 not in counts.values():
        return {}

    verified = {}
    for index, (address, entry) in enumerate(zip(addresses, entries)):
        payload = entry.get("nvidia_payload")
        if payload:
            try:
                payload = json.loads(payload) if isinstance(payload, str) else payload
            except ValueError:
                return {}
            if not isinstance(payload, dict) or payload.get("nonce") != nonce:
                return {}
        result = await verifier._verify_component("model", entry, nonce)
        details = result.get("details") or {}
        dstack = (details.get("dstack") or {}).get("details") or {}
        # Use the compose identity authenticated by event-log replay, and
        # require that it is also the hash of the supplied compose.
        compose_hash = (dstack.get("app_info") or {}).get("compose_hash")
        if (
            not result.get("is_valid")
            or not details.get("compose_verified")
            or not compose_hash
            or compose_hash != (entry.get("info") or {}).get("compose_hash")
        ):
            return {}
        if counts[address] == 1:
            verified[address] = index
    return verified


async def bounded_model_verification(
    verifier: Any,
    entries: list[dict[str, Any]],
    nonce: str,
    budget: float,
) -> dict[str, int]:
    if budget <= 0:
        return {}
    # dstack and NRAS use blocking HTTP. The worker lives only as long as this
    # single verifier subprocess, so optional checks cannot hold up gateway
    # evidence or outlive the process deadline.
    outcome: Future[dict[str, int]] = Future()

    def verify() -> None:
        if outcome.set_running_or_notify_cancel():
            try:
                outcome.set_result(asyncio.run(verify_model_candidates(verifier, entries, nonce)))
            except Exception as exc:
                outcome.set_exception(exc)

    Thread(target=verify, daemon=True).start()
    try:
        return await asyncio.wait_for(asyncio.wrap_future(outcome), timeout=budget)
    except Exception as exc:
        print(f"NEAR AI model attestation skipped: {type(exc).__name__}", file=sys.stderr)
        return {}


async def verify_nearai(request: dict[str, Any]) -> None:
    from confidential_verifier.providers.nearai import NearaiProvider
    from confidential_verifier.verifiers.nearai import NearAICloudVerifier

    provider = "near-ai"
    verifier_id = verifier_id_for(provider)
    # Fail loudly on bridge/verifier contract drift instead of letting a missing
    # method surface as a cryptic AttributeError mid-verification.
    if not (
        hasattr(NearAICloudVerifier, "verify_gateway_component")
        and hasattr(NearaiProvider, "fetch_raw_report")
    ):
        failed(
            provider,
            "verifier contract drift: NEAR AI verifier lacks "
            "raw report fetch or gateway verification; the confidential_verifier package is out of sync "
            "with this bridge (see scripts/confidential_verifier/VENDOR.md)",
            failure="verifier_misconfigured",
            verifier_id=verifier_id,
        )
        return
    provider_options = request.get("provider_options") or {}
    near_provider = NearaiProvider(
        include_tls_fingerprint=True,
        api_key=provider_options.get("near_ai_api_key"),
    )
    dstack_verifier_url = os.getenv("DSTACK_VERIFIER_URL", "http://localhost:8080")
    model_id = request["model_id"]
    started = time.monotonic()
    try:
        with contextlib.redirect_stdout(sys.stderr):
            raw, nonce = await asyncio.to_thread(near_provider.fetch_raw_report, model_id)
    except (ConnectionError, HTTPError, Timeout) as exc:
        failed(
            provider,
            f"NEAR AI attestation report fetch failed: {exc}",
            failure="evidence_unavailable",
        )
        return
    except ResponseTooLarge as exc:
        failed(provider, str(exc), failure="evidence_invalid")
        return
    except ValueError:
        failed(provider, "NEAR AI attestation report is malformed", failure="evidence_invalid")
        return
    attestations = raw.get("model_attestations")
    if not isinstance(attestations, list) or not attestations:
        failed(provider, "NEAR AI report missing model_attestations", failure="evidence_invalid")
        return
    if any(
        not isinstance(entry, dict) or reported_model_name(entry) != model_id
        for entry in attestations
    ):
        failed(
            provider,
            "NEAR AI model attestation model_name does not match the requested model ID",
            failure="binding_mismatch",
        )
        return
    with contextlib.redirect_stdout(sys.stderr):
        verifier = NearAICloudVerifier(dstack_verifier_url)
        gateway = raw.get("gateway_attestation")
    if not isinstance(gateway, dict) or not gateway:
        failed(
            provider,
            "NEAR AI report did not include gateway_attestation",
            failure="evidence_invalid",
            evidence=json_evidence_bundle(raw, ATTESTATION_URL),
            verifier_id=verifier_id,
        )
        return
    spki = gateway.get("tls_cert_fingerprint")
    if not spki:
        failed(
            provider,
            "NEAR AI report did not include TLS SPKI binding",
            failure="evidence_invalid",
            evidence=json_evidence_bundle(raw, ATTESTATION_URL),
            verifier_id=verifier_id,
        )
        return
    with contextlib.redirect_stdout(sys.stderr):
        gateway_result = await verifier.verify_gateway_component(raw, nonce)
    if not gateway_result.get("is_valid"):
        failed(
            provider,
            "; ".join(gateway_result.get("errors") or [])
            or "NEAR AI gateway verification failed",
            evidence=json_evidence_bundle(raw, ATTESTATION_URL),
            verifier_id=verifier_id,
        )
        return

    # The model_name check guards model selection. The resulting evidence and
    # binding cover the shared gateway TEE channel.
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
        "request_nonce": nonce,
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

    evidence = json_evidence_bundle(channel_evidence, ATTESTATION_URL)
    budget = min(
        MODEL_VERIFICATION_BUDGET,
        request.get("timeout_seconds", 60) - (time.monotonic() - started) - 2,
    )
    if (
        budget > 0
        and len(attestations) <= MAX_MODEL_CANDIDATES
        and all(entry.get("intel_quote") for entry in attestations)
        and hasattr(verifier, "_verify_component")
    ):
        model_evidence = json_bytes({**channel_evidence, "model_attestations": attestations})
        if len(model_evidence) > MAX_MODEL_EVIDENCE_BYTES:
            model_evidence = None
        with contextlib.redirect_stdout(sys.stderr):
            candidates = (
                await bounded_model_verification(verifier, attestations, nonce, budget)
                if model_evidence is not None
                else {}
            )
        if candidates:
            evidence["near_model"] = {
                **evidence_bundle(model_evidence, ATTESTATION_URL, "application/json"),
                "verified_candidates": candidates,
            }

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "attested_scope": "router",
            "evidence": evidence,
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
