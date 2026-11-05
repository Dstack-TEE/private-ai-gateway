from __future__ import annotations

from pathlib import Path
from typing import Any

import requests

from ..common import Provider, request_json, write_bytes
from .lifecycle import verify_served_request
from .images import assert_images_shape


REQUESTER_TOKEN = "live-e2e-requester"


def run_images_edits_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
) -> dict[str, Any]:
    provider_dir = artifact_dir / provider.name / "images_edits"
    fixture = (Path(__file__).resolve().parents[1] / "fixtures" / "edit.png").read_bytes()
    prepared = requests.Request(
        "POST", f"{base_url}/v1/images/edits",
        data={"model": provider.public_model, "prompt": "Make the image blue.", "size": "512x512", "response_format": "b64_json"},
        files={"image": ("image.png", fixture, "image/png")},
    ).prepare()
    request_body = prepared.body
    if not isinstance(request_body, bytes):
        raise RuntimeError("multipart encoder did not return immutable bytes")
    request_path = provider_dir / "request.multipart"
    write_bytes(request_path, request_body)
    status, headers, response_body, parsed = request_json(
        "POST", f"{base_url}/v1/images/edits",
        headers={"Authorization": f"Bearer {REQUESTER_TOKEN}", "Content-Type": prepared.headers["Content-Type"], "X-ACI-Provider": '{"aci_verified":true}'},
        body=request_body, timeout=300,
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
    summary = verify_served_request(
        base_url=base_url, provider=provider, endpoint="/v1/images/edits", headers=headers,
        provider_dir=provider_dir, request_path=request_path, response_path=response_path,
        status=status, requester_token=REQUESTER_TOKEN,
        receipt_check=lambda receipt: _check_receipt(provider, receipt),
    )
    return {**summary, "image_count": len(parsed["data"])}


def _check_receipt(provider: Provider, receipt: dict[str, Any]) -> None:
    if receipt.get("chat_id") is not None:
        raise RuntimeError(f"{provider.name} images receipt chat_id must be null")
