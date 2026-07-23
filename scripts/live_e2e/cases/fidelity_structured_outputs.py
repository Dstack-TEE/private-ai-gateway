from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from ..common import Provider, json_bytes, request_json, write_bytes, write_json


EMAIL_RE = re.compile(
    r"\b[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@"
    r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?"
    r"(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)+\b"
)
EXPECTED_EMAILS = ["ada.lovelace@example.com", "ops+aci@example.com"]


def run_structured_outputs_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
    inference_token: str,
) -> dict[str, Any]:
    if not provider.has_capability("structured_outputs"):
        return {"provider": provider.name, "status": "skipped", "reason": "capability_absent"}
    provider_dir = artifact_dir / provider.name / "structured_outputs"
    body = {
        "model": provider.public_model,
        "messages": [
            {
                "role": "system",
                "content": (
                    "Extract only valid email addresses from the user's text. "
                    "Return only the JSON object required by the schema."
                ),
            },
            {
                "role": "user",
                "content": (
                    "Contact Ada at ada.lovelace@example.com and ops at "
                    "ops+aci@example.com. Do not include root at localhost or "
                    "alice.example.com."
                ),
            },
        ],
        "temperature": 0,
        "max_tokens": provider.structured_output_max_tokens,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "email_extract",
                "strict": True,
                "schema": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["emails"],
                    "properties": {
                        "emails": {
                            "type": "array",
                            "items": {"type": "string", "format": "email"},
                        }
                    },
                },
            },
        },
    }
    request_body = json_bytes(body)
    write_bytes(provider_dir / "request.json", request_body)
    status, _, response_body, parsed = request_json(
        "POST",
        f"{base_url}/v1/chat/completions",
        headers={
            "Authorization": f"Bearer {inference_token}",
            "Content-Type": "application/json",
        },
        body=request_body,
        timeout=240,
    )
    write_bytes(provider_dir / "response.json", response_body)
    if not 200 <= status < 300:
        raise RuntimeError(
            f"{provider.name} structured-output request failed with HTTP {status}: "
            f"{response_body.decode('utf-8', errors='replace')[:600]}"
        )
    if not isinstance(parsed, dict):
        raise RuntimeError(f"{provider.name} structured-output response is not JSON")
    content = extract_message_content(parsed)
    parsed_content = json.loads(content)
    emails = sorted(set(find_emails(parsed_content)))
    expected = sorted(EXPECTED_EMAILS)
    summary = {
        "provider": provider.name,
        "status": "passed",
        "emails": emails,
        "expected": expected,
        "chat_id": parsed.get("id"),
    }
    write_json(provider_dir / "summary.json", summary)
    if emails != expected:
        raise RuntimeError(
            f"{provider.name} structured output emails mismatch: got {emails}, expected {expected}"
        )
    return summary


def extract_message_content(response: dict[str, Any]) -> str:
    choices = response.get("choices")
    if not isinstance(choices, list) or not choices:
        raise RuntimeError("OpenAI response missing choices")
    message = choices[0].get("message") if isinstance(choices[0], dict) else None
    if not isinstance(message, dict):
        raise RuntimeError("OpenAI response missing message")
    content = message.get("content")
    if not isinstance(content, str) or not content:
        raise RuntimeError("OpenAI response message content is empty")
    return content


def find_emails(value: Any) -> list[str]:
    out: list[str] = []
    if isinstance(value, str):
        out.extend(match.group(0).lower() for match in EMAIL_RE.finditer(value))
    elif isinstance(value, list):
        for item in value:
            out.extend(find_emails(item))
    elif isinstance(value, dict):
        for item in value.values():
            out.extend(find_emails(item))
    return out
