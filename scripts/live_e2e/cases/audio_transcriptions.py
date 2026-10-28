from __future__ import annotations

import hashlib
import secrets
from pathlib import Path
from typing import Any

import requests
from openai.types.audio import Transcription, TranscriptionVerbose

from ..common import Provider, request_json, run_pap_audit, write_bytes, write_json
from .attested_sessions import assert_upstream_attested_sessions
from .lifecycle import assert_receipt_log


REQUESTER_TOKEN = "live-e2e-requester"
FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "transcription.wav"
EXPECTED_WORDS = {"quick", "brown", "fox", "jumps", "lazy", "dog"}


def run_audio_transcriptions_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
) -> dict[str, Any]:
    return {
        "provider": provider.name,
        "formats": {
            response_format: run_audio_format(
                base_url=base_url,
                provider=provider,
                artifact_dir=artifact_dir,
                response_format=response_format,
            )
            for response_format in ("json", "verbose_json")
        },
    }


def run_audio_format(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
    response_format: str,
) -> dict[str, Any]:
    provider_dir = artifact_dir / provider.name / "audio_transcriptions" / response_format
    prepared = requests.Request(
        "POST", f"{base_url}/v1/audio/transcriptions",
        data={"model": provider.public_model, "response_format": response_format, "language": "en"},
        files={"file": ("audio.wav", FIXTURE.read_bytes(), "audio/wav")},
    ).prepare()
    request_body = prepared.body
    if not isinstance(request_body, bytes):
        raise RuntimeError("multipart encoder did not return immutable bytes")
    request_path = provider_dir / "request.multipart"
    write_bytes(request_path, request_body)
    status, headers, response_body, parsed = request_json(
        "POST",
        f"{base_url}/v1/audio/transcriptions",
        headers={
            "Authorization": f"Bearer {REQUESTER_TOKEN}",
            "Content-Type": prepared.headers["Content-Type"],
            "X-ACI-Provider": '{"aci_verified":true}',
        },
        body=request_body,
        timeout=300,
    )
    response_path = provider_dir / "response.json"
    write_bytes(response_path, response_body)
    if not 200 <= status < 300:
        raise RuntimeError(
            f"{provider.name} audio_transcriptions request failed with HTTP {status}: "
            f"{response_body.decode('utf-8', errors='replace')[:600]}"
        )
    if not isinstance(parsed, dict):
        raise RuntimeError(f"{provider.name} audio_transcriptions response is not JSON")
    assert_audio_shape(provider, parsed, response_format)
    receipt_id = headers.get("x-receipt-id")
    if not receipt_id:
        raise RuntimeError(f"{provider.name} audio_transcriptions response missing x-receipt-id")

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

    # Transcription receipts carry no chat id, so the receipt is
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
            f"{provider.name} audio_transcriptions receipt fetch failed: HTTP {receipt_status}"
        )

    assert_audio_receipt_log(provider, receipt)
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
        "transcript": parsed["text"],
        "duration_seconds": parsed["usage"]["seconds"],
        "fixture_sha256": hashlib.sha256(FIXTURE.read_bytes()).hexdigest(),
        "verified": (verifier_summary.get("verdict") or {}).get("verified") is True,
        "attested_sessions": attested_sessions,
    }


def assert_audio_shape(
    provider: Provider, response: dict[str, Any], response_format: str = "json",
) -> None:
    text = response.get("text")
    if not isinstance(text, str):
        raise RuntimeError(f"{provider.name} transcription text must be a string")
    words = set(text.lower().replace(".", "").replace(",", "").split())
    if len(words & EXPECTED_WORDS) < 4:
        raise RuntimeError(f"{provider.name} transcript does not match spoken fixture")
    usage = response.get("usage")
    seconds = usage.get("seconds") if isinstance(usage, dict) else None
    if isinstance(seconds, bool) or not isinstance(seconds, int) or not 0 < seconds < 30:
        raise RuntimeError(f"{provider.name} transcription billable seconds must be a positive integer below 30s")
    if usage.get("type") != "duration" or set(usage) - {"type", "seconds", "cost"}:
        raise RuntimeError(f"{provider.name} transcription client usage must describe duration only")
    allowed = {"text", "usage", "languages"}
    if response_format == "verbose_json":
        allowed |= {"language", "duration", "segments", "words", "task"}
    if set(response) - allowed:
        raise RuntimeError(f"{provider.name} {response_format} transcription response contains extra fields")
    if "languages" in response:
        languages = response["languages"]
        if not isinstance(languages, list) or any(
            not isinstance(item, dict) or set(item) != {"code"}
            or not isinstance(item["code"], str) for item in languages
        ):
            raise RuntimeError(f"{provider.name} transcription languages must contain only string codes")
    # The SDK's strict response path validates these same generated models.
    model = Transcription if response_format == "json" else TranscriptionVerbose
    model.model_validate(response)


def assert_audio_receipt_log(provider: Provider, receipt: dict[str, Any]) -> None:
    if receipt.get("endpoint") != "/v1/audio/transcriptions":
        raise RuntimeError(
            f"{provider.name} receipt endpoint must be /v1/audio/transcriptions, got {receipt.get('endpoint')!r}"
        )
    if receipt.get("chat_id") is not None:
        raise RuntimeError(f"{provider.name} audio_transcriptions receipt chat_id must be null")
    assert_receipt_log(provider, receipt)
