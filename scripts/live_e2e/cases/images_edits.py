from __future__ import annotations

from pathlib import Path
from typing import Any

import requests

from ..common import Provider
from .lifecycle import post_request, verify_served_request
from .images import assert_images_shape


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
        data={"model": provider.public_model, "prompt": "Make the image blue.", "size": "512x512"},
        files={"image": ("image.png", fixture, "image/png")},
    ).prepare()
    request_body = prepared.body
    if not isinstance(request_body, bytes):
        raise RuntimeError("multipart encoder did not return immutable bytes")
    request_path = provider_dir / "request.multipart"
    response_path = provider_dir / "response.json"
    status, headers, response_body, parsed = post_request(
        base_url=base_url, provider=provider, endpoint="/v1/images/edits",
        request_body=request_body, request_path=request_path, response_path=response_path,
        content_type=prepared.headers["Content-Type"],
        headers={"X-ACI-Provider": '{"aci_verified":true}'}, timeout=300,
    )
    if not isinstance(parsed, dict):
        raise RuntimeError(f"{provider.name} image edit response is not JSON")
    assert_images_shape(provider, parsed)
    summary = verify_served_request(
        base_url=base_url, provider=provider, endpoint="/v1/images/edits", headers=headers,
        provider_dir=provider_dir, request_path=request_path, response_path=response_path,
        status=status,
        receipt_check=lambda receipt: _check_receipt(provider, receipt),
    )
    return {**summary, "image_count": len(parsed["data"])}


def _check_receipt(provider: Provider, receipt: dict[str, Any]) -> None:
    if receipt.get("chat_id") is not None:
        raise RuntimeError(f"{provider.name} image edit receipt chat_id must be null")
