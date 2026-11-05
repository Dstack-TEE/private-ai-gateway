from __future__ import annotations

import secrets
from pathlib import Path
from typing import Any

from ..common import Provider, json_bytes, request_json, run_pap_audit, write_bytes, write_json
from .attested_sessions import assert_upstream_attested_sessions
from .lifecycle import assert_receipt_log


REQUESTER_TOKEN = "live-e2e-requester"
PROBE_INPUT = "Hello from confidential speech synthesis."


def run_speech_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
) -> dict[str, Any]:
    provider_dir = artifact_dir / provider.name / "speech"
    body = {
        "model": provider.public_model,
        "input": PROBE_INPUT,
        "voice": "ryan",
        "response_format": "wav",
        "speed": 1.2,
    }
    request_body = json_bytes(body)
    request_path = provider_dir / "request.json"
    write_bytes(request_path, request_body)
    status, headers, response_body, _ = request_json(
        "POST",
        f"{base_url}/v1/audio/speech",
        headers={
            "Authorization": f"Bearer {REQUESTER_TOKEN}",
            "Content-Type": "application/json",
        },
        body=request_body,
        timeout=240,
    )
    response_path = provider_dir / "response.audio"
    write_bytes(response_path, response_body)
    if not 200 <= status < 300:
        raise RuntimeError(
            f"{provider.name} speech request failed with HTTP {status}: "
            f"{response_body.decode('utf-8', errors='replace')[:600]}"
        )
    if not response_body or not headers.get("content-type", "").lower().startswith("audio/"):
        raise RuntimeError(f"{provider.name} speech response is not buffered audio")
    receipt_id = headers.get("x-receipt-id")
    if not receipt_id:
        raise RuntimeError(f"{provider.name} speech response missing x-receipt-id")

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

    # Fetch the receipt by its gateway-owned identifier (spec §7.1).
    receipt_status, _, receipt_body, receipt = request_json(
        "GET",
        f"{base_url}/v1/aci/receipts/{receipt_id}",
        headers={"Authorization": f"Bearer {REQUESTER_TOKEN}"},
        timeout=120,
    )
    receipt_path = provider_dir / "receipt.json"
    write_bytes(receipt_path, receipt_body)
    if receipt_status != 200 or not isinstance(receipt, dict):
        raise RuntimeError(
            f"{provider.name} speech receipt fetch failed: HTTP {receipt_status}"
        )

    assert_speech_receipt_log(provider, receipt)
    usage = [event.get("usage") for event in receipt.get("event_log", []) if event.get("type") == "usage.reported"]
    if usage != [{"type": "characters", "characters": len(PROBE_INPUT)}]:
        raise RuntimeError(f"{provider.name} speech receipt has incorrect character usage")
    attested_sessions = assert_upstream_attested_sessions(
        base_url=base_url,
        provider=provider,
        receipt=receipt,
        artifact_dir=provider_dir,
    )
    if len(attested_sessions) != 1:
        raise RuntimeError(f"{provider.name} expected one serving attested session")
    verifier_summary = run_pap_audit(
        report_path,
        receipt_path,
        provider_dir / "attested-session-0.json",
        nonce,
        request_path,
        response_path,
    )
    write_json(provider_dir / "user-verification-summary.json", verifier_summary)
    return {
        "provider": provider.name,
        "receipt_id": receipt_id,
        "status": status,
        "audio_bytes": len(response_body),
        "content_type": headers.get("content-type"),
        "verified": (verifier_summary.get("verdict") or {}).get("verified") is True,
        "attested_sessions": attested_sessions,
    }


def assert_speech_receipt_log(provider: Provider, receipt: dict[str, Any]) -> None:
    if receipt.get("endpoint") != "/v1/audio/speech":
        raise RuntimeError(
            f"{provider.name} receipt endpoint must be /v1/audio/speech, got {receipt.get('endpoint')!r}"
        )
    assert_receipt_log(provider, receipt)
