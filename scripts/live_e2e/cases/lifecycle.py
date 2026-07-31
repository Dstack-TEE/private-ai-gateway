from __future__ import annotations

import secrets
from pathlib import Path
from typing import Any

from ..common import (
    Provider,
    audit_aci_artifacts,
    json_bytes,
    request_json,
    verify_aci_report,
    write_bytes,
    write_json,
)
from .attested_sessions import assert_upstream_attested_sessions


def run_lifecycle_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
    inference_token: str,
) -> dict[str, Any]:
    provider_dir = artifact_dir / provider.name / "lifecycle"
    body = {
        "model": provider.public_model,
        "messages": [
            {
                "role": "user",
                "content": "Reply with exactly one short sentence confirming ACI lifecycle.",
            }
        ],
        "temperature": 0,
        "max_tokens": 32,
    }
    request_body = json_bytes(body)
    request_path = provider_dir / "request.json"
    write_bytes(request_path, request_body)
    status, headers, response_body, parsed = request_json(
        "POST",
        f"{base_url}/v1/chat/completions",
        headers={
            "Authorization": f"Bearer {inference_token}",
            "Content-Type": "application/json",
        },
        body=request_body,
        timeout=240,
    )
    response_path = provider_dir / "response.json"
    write_bytes(response_path, response_body)
    if not 200 <= status < 300:
        raise RuntimeError(
            f"{provider.name} lifecycle request failed with HTTP {status}: "
            f"{response_body.decode('utf-8', errors='replace')[:600]}"
        )
    if not isinstance(parsed, dict):
        raise RuntimeError(f"{provider.name} lifecycle response is not JSON")
    chat_id = parsed.get("id")
    if not isinstance(chat_id, str) or not chat_id:
        raise RuntimeError(f"{provider.name} lifecycle response missing id")
    receipt_id = headers.get("x-receipt-id")
    if not receipt_id:
        raise RuntimeError(f"{provider.name} lifecycle response missing x-receipt-id")

    # A nonce is exactly 64 lowercase hex characters (spec §3.2).
    nonce = secrets.token_hex(32)
    report_status, _, report_body, report_json = request_json(
        "GET",
        f"{base_url}/v1/aci/attestation?nonce={nonce}",
        timeout=120,
    )
    report_path = provider_dir / "report.json"
    write_bytes(report_path, report_body)
    if report_status != 200 or not isinstance(report_json, dict):
        raise RuntimeError(f"{provider.name} attestation report fetch failed: {report_status}")

    # The §7.2 receipt document from the canonical endpoint.
    receipt_status, _, receipt_body, receipt = request_json(
        "GET",
        f"{base_url}/v1/aci/receipts/{receipt_id}",
        headers={"Authorization": f"Bearer {inference_token}"},
        timeout=120,
    )
    receipt_path = provider_dir / "receipt.json"
    write_bytes(receipt_path, receipt_body)
    if receipt_status != 200 or not isinstance(receipt, dict):
        raise RuntimeError(f"{provider.name} receipt fetch failed: {receipt_status}")

    # Legacy compatibility spot-check: the inherited dstack-vllm-proxy
    # signature wrapper still serves its contract fields.
    legacy_status, _, _, legacy_json = request_json(
        "GET",
        f"{base_url}/v1/signature/{chat_id}",
        headers={"Authorization": f"Bearer {inference_token}"},
        timeout=120,
    )
    if legacy_status != 200 or not isinstance(legacy_json, dict):
        raise RuntimeError(f"{provider.name} legacy signature fetch failed: {legacy_status}")
    for field in ("text", "signature", "signing_address", "signing_algo"):
        if not legacy_json.get(field):
            raise RuntimeError(f"{provider.name} legacy signature wrapper missing {field}")

    assert_receipt_log(provider, receipt)
    attested_sessions = assert_upstream_attested_sessions(
        base_url=base_url,
        provider=provider,
        receipt=receipt,
        artifact_dir=provider_dir,
    )
    if len(attested_sessions) != 1:
        raise RuntimeError(f"{provider.name} expected one serving attested session")
    verifier_summary = verify_aci_report(base_url, nonce, report_json)
    write_json(provider_dir / "user-verification-summary.json", verifier_summary)
    audit_summary = audit_aci_artifacts(
        report=report_path,
        receipt=receipt_path,
        session=provider_dir / "attested-session-0.json",
        nonce=nonce,
        request_body=request_path,
        response_body=response_path,
    )
    write_json(provider_dir / "receipt-audit.json", audit_summary)
    return {
        "provider": provider.name,
        "chat_id": chat_id,
        "receipt_id": receipt_id,
        "status": status,
        "verified": (verifier_summary.get("verdict") or {}).get("verified") is True,
        "checks": {
            check.get("id"): check.get("status")
            for check in verifier_summary.get("checks") or []
            if isinstance(check, dict)
        },
        "attested_sessions": attested_sessions,
    }


def assert_receipt_log(provider: Provider, receipt: dict[str, Any]) -> None:
    events = receipt.get("event_log")
    if not isinstance(events, list):
        raise RuntimeError(f"{provider.name} receipt missing event_log")
    upstream = [
        event
        for event in events
        if isinstance(event, dict) and event.get("type") == "upstream.verified"
    ]
    if not upstream:
        raise RuntimeError(f"{provider.name} receipt missing upstream.verified event")
    verified = [event for event in upstream if event.get("result") == "verified"]
    if not verified:
        raise RuntimeError(f"{provider.name} receipt has no verified upstream event")
    for event in verified:
        session_id = event.get("session_id")
        if not isinstance(session_id, str) or len(session_id) != 64:
            raise RuntimeError(f"{provider.name} upstream event missing session_id")
    if provider.public_model != provider.upstream_model:
        hashes = {
            event.get("type"): event.get("body_hash")
            for event in events
            if isinstance(event, dict)
            and event.get("type") in {"request.received", "request.forwarded"}
        }
        if not hashes.get("request.forwarded") or hashes.get(
            "request.received"
        ) == hashes.get("request.forwarded"):
            raise RuntimeError(
                f"{provider.name} receipt did not record the model rewrite"
            )
