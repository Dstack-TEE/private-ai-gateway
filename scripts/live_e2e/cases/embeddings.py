from __future__ import annotations

import secrets
from pathlib import Path
from typing import Any

from ..common import (
    Provider,
    audit_aci_artifacts,
    json_bytes,
    request_json,
    write_bytes,
    write_json,
)
from .attested_sessions import assert_upstream_attested_sessions


PROBE_INPUT = "Reply with exactly one short sentence confirming ACI embeddings lifecycle."


def run_embeddings_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
    inference_token: str,
) -> dict[str, Any]:
    provider_dir = artifact_dir / provider.name / "embeddings"
    body = {
        "model": provider.public_model,
        "input": PROBE_INPUT,
    }
    request_body = json_bytes(body)
    request_path = provider_dir / "request.json"
    write_bytes(request_path, request_body)
    status, headers, response_body, parsed = request_json(
        "POST",
        f"{base_url}/v1/embeddings",
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
            f"{provider.name} embeddings request failed with HTTP {status}: "
            f"{response_body.decode('utf-8', errors='replace')[:600]}"
        )
    if not isinstance(parsed, dict):
        raise RuntimeError(f"{provider.name} embeddings response is not JSON")
    assert_embeddings_shape(provider, parsed)
    receipt_id = headers.get("x-receipt-id")
    if not receipt_id:
        raise RuntimeError(f"{provider.name} embeddings response missing x-receipt-id")

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

    # Embeddings responses carry no upstream `id`, so the receipt is
    # addressed by receipt_id (spec §7.1).
    receipt_status, _, receipt_body, receipt = request_json(
        "GET",
        f"{base_url}/v1/aci/receipts/{receipt_id}",
        headers={"Authorization": f"Bearer {inference_token}"},
        timeout=120,
    )
    receipt_path = provider_dir / "receipt.json"
    write_bytes(receipt_path, receipt_body)
    if receipt_status != 200 or not isinstance(receipt, dict):
        raise RuntimeError(
            f"{provider.name} embeddings receipt fetch failed: HTTP {receipt_status}"
        )

    assert_embeddings_receipt_log(provider, receipt)
    attested_sessions = assert_upstream_attested_sessions(
        base_url=base_url,
        provider=provider,
        receipt=receipt,
        artifact_dir=provider_dir,
    )
    if len(attested_sessions) != 1:
        raise RuntimeError(f"{provider.name} expected one serving attested session")
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
        "receipt_id": receipt_id,
        "status": status,
        "embedding_dim": embedding_dim(parsed),
        "checks": {
            check.get("id"): check.get("status")
            for check in audit_summary.get("checks") or []
            if isinstance(check, dict)
        },
        "attested_sessions": attested_sessions,
    }


def assert_embeddings_shape(provider: Provider, response: dict[str, Any]) -> None:
    if response.get("object") != "list":
        raise RuntimeError(
            f"{provider.name} embeddings response object must be 'list', "
            f"got {response.get('object')!r}"
        )
    data = response.get("data")
    if not isinstance(data, list) or not data:
        raise RuntimeError(f"{provider.name} embeddings response.data must be non-empty list")
    first = data[0]
    if not isinstance(first, dict):
        raise RuntimeError(f"{provider.name} embeddings data[0] is not an object")
    embedding = first.get("embedding")
    if not isinstance(embedding, list) or not embedding:
        raise RuntimeError(f"{provider.name} embeddings data[0].embedding must be non-empty array")
    if not any(
        isinstance(component, (int, float)) and component != 0 for component in embedding
    ):
        raise RuntimeError(
            f"{provider.name} embeddings data[0].embedding is all-zero or non-numeric"
        )


def embedding_dim(response: dict[str, Any]) -> int:
    try:
        return len(response["data"][0]["embedding"])
    except (KeyError, IndexError, TypeError):
        return 0


def assert_embeddings_receipt_log(provider: Provider, receipt: dict[str, Any]) -> None:
    if receipt.get("endpoint") != "/v1/embeddings":
        raise RuntimeError(
            f"{provider.name} receipt endpoint must be /v1/embeddings, got {receipt.get('endpoint')!r}"
        )
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
