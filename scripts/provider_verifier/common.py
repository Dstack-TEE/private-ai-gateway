"""Shared helpers for the provider-verifier bridge modules."""

from __future__ import annotations

import asyncio
import base64
import contextlib
import gzip
import hashlib
import json
import os
import secrets
import sys
import time
import urllib.parse
import urllib.request
from typing import Any


def emit(obj: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")))


def verifier_id_for(provider: str) -> str:
    if provider == "near-ai":
        return "private-ai-verifier/near-ai-gateway/v1"
    return f"private-ai-verifier/{provider}/v1"


def failed(provider: str, reason: str, **extra: Any) -> None:
    emit(
        {
            "result": "failed",
            "verifier_id": verifier_id_for(provider),
            "reason": reason,
            **extra,
        }
    )


def sha256_json(value: Any) -> str:
    body = json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def sha256_json_prefixed(value: Any) -> str:
    return f"sha256:{sha256_json(value)}"


def sha256_bytes_prefixed(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def json_bytes(value: Any) -> bytes:
    return json.dumps(value, separators=(",", ":"), default=str).encode("utf-8")


def data_uri(data: bytes, content_type: str) -> str:
    return f"data:{content_type};base64,{base64.b64encode(data).decode('ascii')}"


def evidence_bundle(
    data: bytes,
    source_url: str | None = None,
    content_type: str = "application/octet-stream",
) -> dict[str, Any]:
    bundle = {
        "digest": sha256_bytes_prefixed(data),
        "data": data_uri(data, content_type),
    }
    if source_url:
        bundle["source_url"] = source_url
    return bundle


def json_evidence_bundle(value: Any, source_url: str | None = None) -> dict[str, Any]:
    return evidence_bundle(json_bytes(value), source_url, "application/json")


def raw_http_item(name: str, source_url: str, content_type: str, body: bytes) -> dict[str, Any]:
    return {
        "name": name,
        "source_url": source_url,
        "sha256": sha256_bytes_prefixed(body),
        "content_type": content_type,
        "body": body,
    }


def response_content_type(response: Any) -> str:
    return str(response.headers.get("content-type") or "application/octet-stream")


def raw_http_bundle_evidence(
    items: list[dict[str, Any]],
    *,
    source_url: str | None = None,
) -> dict[str, Any]:
    boundary = "aci-evidence-" + hashlib.sha256(
        b"".join(item["body"] for item in items)
    ).hexdigest()[:24]
    chunks: list[bytes] = []
    for item in items:
        headers = [
            f"--{boundary}",
            f"Content-Type: {item['content_type']}",
            f"Content-Location: {item['source_url']}",
            f"Content-ID: <{item['name']}>",
            f"Digest: sha-256={base64.b64encode(hashlib.sha256(item['body']).digest()).decode('ascii')}",
            "",
            "",
        ]
        chunks.append("\r\n".join(headers).encode("utf-8"))
        chunks.append(item["body"])
        chunks.append(b"\r\n")
    chunks.append(f"--{boundary}--\r\n".encode("utf-8"))
    return evidence_bundle(
        b"".join(chunks),
        source_url,
        f"multipart/mixed;boundary={boundary}",
    )


def sha256_base64_key(value: str) -> str:
    return hashlib.sha256(base64.b64decode(value.strip())).hexdigest()


def model_dump(model: Any) -> dict[str, Any]:
    if hasattr(model, "model_dump"):
        return model.model_dump(mode="json")
    return model.dict()


def is_uuid_like(value: str) -> bool:
    return (
        len(value) == 36
        and value.count("-") == 4
        and all(char == "-" or char in "0123456789abcdefABCDEF" for char in value)
    )


def provider_options(request: dict[str, Any]) -> dict[str, str]:
    value = request.get("provider_options") or {}
    if not isinstance(value, dict):
        raise ValueError("provider_options must be an object")
    return {str(key): str(item) for key, item in value.items()}


def request_timeout_seconds(request: dict[str, Any], default: int) -> int:
    value = request.get("timeout_seconds")
    if value is None:
        return default
    timeout = int(value)
    if timeout <= 0:
        raise ValueError("timeout_seconds must be positive")
    return timeout


def tdx_debug_enabled(quote_bytes: bytes) -> bool:
    """True if a TDX v4 quote's TD runs in debug/untrusted mode.

    TD_ATTRIBUTES is 8 bytes at quote offset 168 (header 48 + body offset 120).
    Byte 0 is the little-endian TUD (TD Under Debug) group; DEBUG is bit 0. Per
    dcap-qvl `validate_td10`, any TUD bit set means the TD is untrusted (CPU state
    and private memory are accessible to the host), so we reject a non-zero TUD
    byte. (The previous big-endian `int(hex) & 1` read byte 7 and missed it.)
    """
    td_attributes = quote_bytes[48 + 120 : 48 + 128]
    if len(td_attributes) != 8:
        raise ValueError(f"invalid TDX td_attributes length: {len(td_attributes)}")
    return td_attributes[0] != 0

