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
        f"{base_url}/v1/aci/sessions/{session_id}",
        timeout=120,
    )
    write_bytes(artifact_dir / f"attested-session-{index}.json", body)
    if status != 200 or not isinstance(parsed, dict):
        raise RuntimeError(
            f"{provider.name} attested session fetch failed for {session_id}: HTTP {status}"
        )
    write_json(artifact_dir / f"attested-session-{index}.summary.json", parsed_summary(parsed))

    # The gateway serves a flat, immutable AttestedSession record (no wrapper):
    # {api_version, session_id, provider, endpoint, verifier_id, established_at,
    #  expires_at, identity?, channel_binding[], claims{...}, evidence{digest,data}}.
    session = parsed
    if session.get("api_version") != "aci/1":
        raise RuntimeError(f"{provider.name} attested session has wrong api_version")
    if session.get("session_id") != session_id:
        raise RuntimeError(f"{provider.name} attested session id mismatch")

    # `provider` is the operator's upstream config name (== event.upstream_name);
    # `provider.provider` ("tinfoil") is the vendor and lives in event.provider.
    expect_equal(provider, "session.provider", session.get("provider"), provider.name)
    expect_equal(
        provider, "session.provider", session.get("provider"), event.get("upstream_name")
    )
    expect_equal(
        provider,
        "session.endpoint",
        _norm_endpoint(session.get("endpoint")),
        _norm_endpoint(event.get("url_origin")),
    )
    expect_equal(
        provider,
        "session.endpoint",
        _norm_endpoint(session.get("endpoint")),
        _norm_endpoint(provider.base_url),
    )
    elif not str(session.get("endpoint") or "").startswith("https://127.0.0.1:"):
        raise RuntimeError(
            f"{provider.name} supervised proxy endpoint is not pinned loopback TLS: "
            f"{session.get('endpoint')!r}"
        )
    expect_equal(
        provider,
        "session.verifier_id",
        session.get("verifier_id"),
        event.get("verifier_id"),
    )

    # Typed claim vocabulary (SessionClaims). The §1 tee_attested claim — a
    # genuine CPU TEE with the workload identity bound — must be `asserted` for
    # every verified upstream; that is what a "fully verified" session means.
    claims = require_object(session, "claims", provider.name)
    tee = require_object(claims, "tee_attested", provider.name)
    if tee.get("status") != "asserted":
        raise RuntimeError(
            f"{provider.name} attested session tee_attested not asserted: "
            f"{tee.get('status')!r}"
        )

    event_evidence = require_object(event, "evidence", provider.name)
    session_evidence = require_object(session, "evidence", provider.name)
    expect_equal(
        provider,
        "evidence.digest",
        session_evidence.get("digest"),
        event_evidence.get("digest"),
    )
    data = session_evidence.get("data")
    if not isinstance(data, str) or not data.startswith("data:"):
        raise RuntimeError(f"{provider.name} attested session evidence missing data URI")

    event_bindings = event.get("channel_bindings")
    session_bindings = session.get("channel_binding")
    if not isinstance(event_bindings, list) or not event_bindings:
        raise RuntimeError(f"{provider.name} upstream event missing channel bindings")
    if not isinstance(session_bindings, list) or not session_bindings:
        raise RuntimeError(f"{provider.name} attested session missing channel_binding")
    session_binding_types = {
        binding.get("type") for binding in session_bindings if isinstance(binding, dict)
    }
    event_binding_types = {
        binding.get("type") for binding in event_bindings if isinstance(binding, dict)
    }
    if session_binding_types != event_binding_types:
        raise RuntimeError(
            f"{provider.name} attested session binding types {session_binding_types} "
            f"!= receipt event binding types {event_binding_types}"
        )
    if provider.binding not in session_binding_types:
        raise RuntimeError(
            f"{provider.name} attested session missing binding {provider.binding}"
        )

    return {
        "session_id": session_id,
        "provider": session.get("provider"),
        "endpoint": session.get("endpoint"),
        "verifier_id": session.get("verifier_id"),
        "claims": {
            name: claim.get("status")
            for name, claim in claims.items()
            if isinstance(claim, dict)
        },
        "binding_count": len(session_bindings),
        "binding_types": sorted(t for t in session_binding_types if t),
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


def _norm_endpoint(value: Any) -> Any:
    return value.rstrip("/") if isinstance(value, str) else value


def parsed_summary(value: dict[str, Any]) -> dict[str, Any]:
    claims = value.get("claims") if isinstance(value.get("claims"), dict) else {}
    evidence = value.get("evidence") if isinstance(value.get("evidence"), dict) else {}
    return {
        "api_version": value.get("api_version"),
        "session_id": value.get("session_id"),
        "provider": value.get("provider"),
        "endpoint": value.get("endpoint"),
        "verifier_id": value.get("verifier_id"),
        "established_at": value.get("established_at"),
        "expires_at": value.get("expires_at"),
        "claims": {
            name: (claim.get("status") if isinstance(claim, dict) else claim)
            for name, claim in claims.items()
        },
        "channel_binding_types": [
            binding.get("type")
            for binding in (value.get("channel_binding") or [])
            if isinstance(binding, dict)
        ],
        "evidence": {
            "digest": evidence.get("digest"),
            "has_data_uri": isinstance(evidence.get("data"), str)
            and evidence["data"].startswith("data:"),
        },
    }
