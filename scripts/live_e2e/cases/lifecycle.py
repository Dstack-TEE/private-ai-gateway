from __future__ import annotations

import secrets
from pathlib import Path
from typing import Any

from ..common import Provider, json_bytes, request_json, run_cmd_json, write_bytes, write_json
from .attested_sessions import assert_upstream_attested_sessions


REQUESTER_TOKEN = "live-e2e-requester"


def run_lifecycle_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
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
            "Authorization": f"Bearer {REQUESTER_TOKEN}",
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

    nonce = secrets.token_hex(16)
    report_status, _, report_body, report_json = request_json(
        "GET",
        f"{base_url}/v1/attestation/report?nonce={nonce}",
        timeout=120,
    )
    report_path = provider_dir / "report.json"
    write_bytes(report_path, report_body)
    if report_status != 200 or not isinstance(report_json, dict):
        raise RuntimeError(f"{provider.name} attestation report fetch failed: {report_status}")

    receipt_status, _, receipt_body, receipt_json = request_json(
        "GET",
        f"{base_url}/v1/signature/{chat_id}",
        headers={"Authorization": f"Bearer {REQUESTER_TOKEN}"},
        timeout=120,
    )
    receipt_path = provider_dir / "receipt.json"
    write_bytes(receipt_path, receipt_body)
    if receipt_status != 200 or not isinstance(receipt_json, dict):
        raise RuntimeError(f"{provider.name} receipt fetch failed: {receipt_status}")

    verifier_summary = run_cmd_json(
        [
            "cargo",
            "run",
            "--quiet",
            "--example",
            "verify_aci_artifacts",
            "--",
            "--report",
            str(report_path),
            "--receipt",
            str(receipt_path),
            "--nonce",
            nonce,
            "--request-body",
            str(request_path),
            "--response-body",
            str(response_path),
        ],
        timeout=240,
    )
    write_json(provider_dir / "user-verification-summary.json", verifier_summary)
    receipt = receipt_json.get("receipt") or {}
    assert_receipt_log(provider, receipt)
    attested_sessions = assert_upstream_attested_sessions(
        base_url=base_url,
        provider=provider,
        receipt=receipt,
        artifact_dir=provider_dir,
    )
    return {
        "provider": provider.name,
        "chat_id": chat_id,
        "receipt_id": receipt_id,
        "status": status,
        "verified": verifier_summary.get("verified") is True,
        "upstream_events": verifier_summary.get("upstream_events"),
        "attested_sessions": attested_sessions,
        "transparency_events": verifier_summary.get("transparency_events"),
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
        bindings = event.get("channel_bindings")
        if not isinstance(bindings, list) or not bindings:
            raise RuntimeError(f"{provider.name} upstream event missing channel binding")
        if provider.binding not in {binding.get("type") for binding in bindings}:
            raise RuntimeError(f"{provider.name} upstream event missing {provider.binding}")
    if provider.public_model != provider.upstream_model:
        request_modified = any(
            isinstance(event, dict)
            and event.get("type") == "transparency.request_modified"
            for event in events
        )
        if not request_modified:
            raise RuntimeError(
                f"{provider.name} receipt missing transparency.request_modified"
            )
