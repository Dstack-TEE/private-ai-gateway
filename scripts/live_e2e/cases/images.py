from __future__ import annotations

import base64
import binascii
import secrets
from pathlib import Path
from typing import Any

from ..common import Provider, json_bytes, request_json, run_pap_audit, write_bytes, write_json
from .attested_sessions import assert_upstream_attested_sessions
from .lifecycle import assert_receipt_log


REQUESTER_TOKEN = "live-e2e-requester"
PROBE_PROMPT = "A simple blue circle on a white background."


def run_images_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
) -> dict[str, Any]:
    provider_dir = artifact_dir / provider.name / "images"
    body = {
        "model": provider.public_model,
        "prompt": PROBE_PROMPT,
        "n": 1,
        "size": "512x512",
        "response_format": "b64_json",
    }
    request_body = json_bytes(body)
    request_path = provider_dir / "request.json"
    write_bytes(request_path, request_body)
    status, headers, response_body, parsed = request_json(
        "POST",
        f"{base_url}/v1/images/generations",
        headers={
            "Authorization": f"Bearer {REQUESTER_TOKEN}",
            "Content-Type": "application/json",
        },
        body=request_body,
        timeout=300,
    )
    response_path = provider_dir / "response.json"
    write_bytes(response_path, response_body)
    if not 200 <= status < 300:
        raise RuntimeError(
            f"{provider.name} images request failed with HTTP {status}: "
            f"{response_body.decode('utf-8', errors='replace')[:600]}"
        )
    if not isinstance(parsed, dict):
        raise RuntimeError(f"{provider.name} images response is not JSON")
    assert_images_shape(provider, parsed)
    receipt_id = headers.get("x-receipt-id")
    if not receipt_id:
        raise RuntimeError(f"{provider.name} images response missing x-receipt-id")

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

    # Images receipts carry no chat id, so the receipt is
    # addressed by receipt_id (spec §7.1).
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
            f"{provider.name} images receipt fetch failed: HTTP {receipt_status}"
        )

    assert_images_receipt_log(provider, receipt)
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
        "image_count": len(parsed["data"]),
        "verified": (verifier_summary.get("verdict") or {}).get("verified") is True,
        "attested_sessions": attested_sessions,
    }


def assert_images_shape(provider: Provider, response: dict[str, Any]) -> None:
    data = response.get("data")
    if not isinstance(data, list) or len(data) != 1:
        raise RuntimeError(f"{provider.name} images response must contain exactly one image")
    for item in data:
        if not isinstance(item, dict) or "url" in item:
            raise RuntimeError(f"{provider.name} images must be inline, without URLs")
        encoded = item.get("b64_json")
        if not isinstance(encoded, str):
            raise RuntimeError(f"{provider.name} image b64_json must be a string")
        try:
            image = base64.b64decode(encoded, validate=True)
        except (ValueError, binascii.Error) as exc:
            raise RuntimeError(f"{provider.name} image is not valid base64") from exc
        png = image.startswith(b"\x89PNG\r\n\x1a\n")
        jpeg = image.startswith(b"\xff\xd8\xff")
        webp = image.startswith(b"RIFF") and image[8:12] == b"WEBP"
        if not (png or jpeg or webp):
            raise RuntimeError(f"{provider.name} decoded image lacks PNG/JPEG/WebP magic")


def assert_images_receipt_log(provider: Provider, receipt: dict[str, Any]) -> None:
    if receipt.get("endpoint") != "/v1/images/generations":
        raise RuntimeError(
            f"{provider.name} receipt endpoint must be /v1/images/generations, got {receipt.get('endpoint')!r}"
        )
    if receipt.get("chat_id") is not None:
        raise RuntimeError(f"{provider.name} images receipt chat_id must be null")
    assert_receipt_log(provider, receipt)
