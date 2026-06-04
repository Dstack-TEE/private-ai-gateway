from __future__ import annotations

from pathlib import Path
from typing import Any

from ..common import Provider, request_json, write_bytes, write_json


def assert_upstream_attested_sessions(
    *,
    base_url: str,
    provider: Provider,
    receipt: dict[str, Any],
    artifact_dir: Path,
) -> list[dict[str, Any]]:
    events = receipt.get("event_log")
    if not isinstance(events, list):
        raise RuntimeError(f"{provider.name} receipt missing event_log")
    verified_events = [
        event
        for event in events
        if isinstance(event, dict)
        and event.get("type") == "upstream.verified"
        and event.get("result") == "verified"
    ]
    if not verified_events:
        raise RuntimeError(f"{provider.name} receipt missing verified upstream event")

    summaries = []
    for index, event in enumerate(verified_events):
        summaries.append(
            assert_upstream_attested_session(
                base_url=base_url,
                provider=provider,
                event=event,
                artifact_dir=artifact_dir,
                index=index,
            )
        )
    return summaries


def assert_upstream_attested_session(
    *,
    base_url: str,
    provider: Provider,
    event: dict[str, Any],
    artifact_dir: Path,
    index: int,
) -> dict[str, Any]:
    session_id = event.get("session_id")
    if not isinstance(session_id, str) or not session_id.startswith("as_"):
        raise RuntimeError(f"{provider.name} upstream event missing attested session_id")

    status, _, body, parsed = request_json(
        "GET",
        f"{base_url}/v1/audit/sessions/{session_id}",
        timeout=120,
    )
    write_bytes(artifact_dir / f"attested-session-{index}.json", body)
    if status != 200 or not isinstance(parsed, dict):
        raise RuntimeError(
            f"{provider.name} attested session fetch failed for {session_id}: HTTP {status}"
        )
    write_json(artifact_dir / f"attested-session-{index}.summary.json", parsed_summary(parsed))

    if parsed.get("api_version") != "aci/1":
        raise RuntimeError(f"{provider.name} attested session response has wrong api_version")
    session = parsed.get("session")
    if not isinstance(session, dict):
        raise RuntimeError(f"{provider.name} attested session response missing session")
    if session.get("api_version") != "aci/1":
        raise RuntimeError(f"{provider.name} attested session has wrong api_version")
    if session.get("session_id") != session_id:
        raise RuntimeError(f"{provider.name} attested session id mismatch")
    if session.get("direction") != "upstream":
        raise RuntimeError(f"{provider.name} attested session direction must be upstream")

    upstream = require_object(session, "upstream", provider.name)
    expect_equal(provider, "upstream.provider", upstream.get("provider"), event.get("vendor"))
    expect_equal(provider, "upstream.provider", upstream.get("provider"), provider.name)
    expect_equal(
        provider,
        "upstream.model_id",
        upstream.get("model_id"),
        event.get("model_id"),
    )
    expect_equal(provider, "upstream.model_id", upstream.get("model_id"), provider.upstream_model)
    expect_equal(
        provider,
        "upstream.endpoint_origin",
        upstream.get("endpoint_origin"),
        event.get("url_origin"),
    )
    expect_equal(
        provider,
        "upstream.endpoint_origin",
        upstream.get("endpoint_origin"),
        provider.base_url,
    )

    verification = require_object(session, "verification", provider.name)
    expect_equal(
        provider,
        "verification.verifier_id",
        verification.get("verifier_id"),
        event.get("verifier_id"),
    )
    claims = verification.get("verified_claims")
    if not isinstance(claims, list) or "encrypted-session-verified" not in claims:
        raise RuntimeError(
            f"{provider.name} attested session missing encrypted-session-verified claim"
        )

    event_evidence = require_object(event, "evidence", provider.name)
    session_evidence = require_object(verification, "evidence", provider.name)
    expect_equal(
        provider,
        "verification.evidence.digest",
        session_evidence.get("digest"),
        event_evidence.get("digest"),
    )
    data = session_evidence.get("data")
    if not isinstance(data, str) or not data.startswith("data:"):
        raise RuntimeError(f"{provider.name} attested session evidence missing data URI")

    event_bindings = event.get("channel_bindings")
    session_bindings = session.get("session_binding")
    if not isinstance(event_bindings, list) or not event_bindings:
        raise RuntimeError(f"{provider.name} upstream event missing channel bindings")
    if not isinstance(session_bindings, list) or not session_bindings:
        raise RuntimeError(f"{provider.name} attested session missing session_binding")
    if session_bindings != event_bindings:
        raise RuntimeError(f"{provider.name} attested session binding mismatch")
    binding_types = {
        binding.get("type") for binding in session_bindings if isinstance(binding, dict)
    }
    if provider.binding not in binding_types:
        raise RuntimeError(
            f"{provider.name} attested session missing binding {provider.binding}"
        )

    return {
        "session_id": session_id,
        "provider": upstream.get("provider"),
        "model_id": upstream.get("model_id"),
        "endpoint_origin": upstream.get("endpoint_origin"),
        "verifier_id": verification.get("verifier_id"),
        "verified_claims": claims,
        "binding_count": len(session_bindings),
        "evidence_digest": session_evidence.get("digest"),
        "evidence_has_data_uri": True,
    }


def require_object(value: dict[str, Any], key: str, provider_name: str) -> dict[str, Any]:
    item = value.get(key)
    if not isinstance(item, dict):
        raise RuntimeError(f"{provider_name} missing object {key}")
    return item


def expect_equal(provider: Provider, field: str, actual: Any, expected: Any) -> None:
    if actual != expected:
        raise RuntimeError(
            f"{provider.name} {field} mismatch: expected {expected!r}, got {actual!r}"
        )


def parsed_summary(value: dict[str, Any]) -> dict[str, Any]:
    session = value.get("session")
    if not isinstance(session, dict):
        return value
    verification = session.get("verification")
    if not isinstance(verification, dict):
        verification = {}
    evidence = verification.get("evidence")
    if not isinstance(evidence, dict):
        evidence = {}
    return {
        "api_version": value.get("api_version"),
        "session": {
            "api_version": session.get("api_version"),
            "session_id": session.get("session_id"),
            "direction": session.get("direction"),
            "established_at": session.get("established_at"),
            "expires_at": session.get("expires_at"),
            "upstream": session.get("upstream"),
            "verification": {
                "verifier_id": verification.get("verifier_id"),
                "verified_claims": verification.get("verified_claims"),
                "evidence": {
                    "digest": evidence.get("digest"),
                    "has_data_uri": isinstance(evidence.get("data"), str)
                    and evidence["data"].startswith("data:"),
                },
                "provider_claims": verification.get("provider_claims"),
            },
            "session_binding_count": len(session.get("session_binding") or []),
        },
    }
