from __future__ import annotations

from pathlib import Path
from typing import Any

from ..common import Provider, json_bytes
from .lifecycle import post_request, verify_served_request


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
    response_path = provider_dir / "response.audio"
    status, headers, response_body, _ = post_request(
        base_url=base_url, provider=provider, endpoint="/v1/audio/speech",
        request_body=request_body, request_path=request_path, response_path=response_path,
    )
    if not response_body or not headers.get("content-type", "").lower().startswith("audio/"):
        raise RuntimeError(f"{provider.name} speech response is not buffered audio")
    summary = verify_served_request(
        base_url=base_url, provider=provider, endpoint="/v1/audio/speech", headers=headers,
        provider_dir=provider_dir, request_path=request_path, response_path=response_path,
        status=status,
        receipt_check=lambda receipt: _check_receipt(provider, receipt),
    )
    return {**summary, "audio_bytes": len(response_body), "content_type": headers.get("content-type")}


def _check_receipt(provider: Provider, receipt: dict[str, Any]) -> None:
    usage = [event.get("usage") for event in receipt.get("event_log", []) if event.get("type") == "usage.reported"]
    if usage != [{"type": "characters", "characters": len(PROBE_INPUT)}]:
        raise RuntimeError(f"{provider.name} speech receipt has incorrect character usage")
