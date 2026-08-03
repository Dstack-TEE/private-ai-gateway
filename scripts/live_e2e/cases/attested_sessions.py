from __future__ import annotations

import base64
import hashlib
import json
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
    if not isinstance(session_id, str) or len(session_id) != 64:
        raise RuntimeError(f"{provider.name} upstream event missing attested session_id")

    # The path takes the id exactly as the receipt cites it (spec §8.1).
    status, _, body, session = request_json(
        "GET",
        f"{base_url}/v1/aci/sessions/{session_id}",
        timeout=120,
    )
    write_bytes(artifact_dir / f"attested-session-{index}.json", body)
    if status != 200 or not isinstance(session, dict):
        raise RuntimeError(
            f"{provider.name} attested session fetch failed for {session_id}: HTTP {status}"
        )
    write_json(artifact_dir / f"attested-session-{index}.summary.json", parsed_summary(session))

    # §9.3 step 2: the JCS form of the document hashes to the cited id, so
    # the record is provably the one the signed receipt referenced.
    jcs = json.dumps(session, separators=(",", ":"), sort_keys=True).encode()
    recomputed = hashlib.sha256(jcs).hexdigest()
    expect_equal(provider, "session_id (recomputed)", recomputed, session_id)

    # Flat immutable SessionDocument (spec §8.2): {api_version, upstream_name,
    # endpoint, verifier_id, established_at, expires_at, identity?,
    # channel_binding[], claims{}, evidence{digest,data}}. The id is not
    # inside the document.
    if session.get("api_version") != "aci/1":
        raise RuntimeError(f"{provider.name} attested session has wrong api_version")
    if "session_id" in session:
        raise RuntimeError(f"{provider.name} attested session embeds its own id")
    expect_equal(
        provider, "session.upstream_name", session.get("upstream_name"), provider.name
    )
    expect_equal(
        provider,
        "session.endpoint",
        _norm_endpoint(session.get("endpoint")),
        _norm_endpoint(provider.base_url),
    )
    if not session.get("verifier_id"):
        raise RuntimeError(f"{provider.name} attested session missing verifier_id")

    # Typed claim vocabulary (spec §8.3). tee_attested — a genuine CPU TEE
    # with the recorded identity bound — must be asserted for every verified
    # upstream; that is what a verified session means.
    claims = require_object(session, "claims", provider.name)
    tee = require_object(claims, "tee_attested", provider.name)
    if tee.get("status") != "asserted":
        raise RuntimeError(
            f"{provider.name} attested session tee_attested not asserted: "
            f"{tee.get('status')!r}"
        )

    # The full record embeds the exact evidence bytes; digest must match.
    evidence = require_object(session, "evidence", provider.name)
    data = evidence.get("data")
    if not isinstance(data, str) or not data.startswith("data:"):
        raise RuntimeError(f"{provider.name} attested session evidence missing data URI")
    evidence_bytes = base64.b64decode(data.split(",", 1)[1])
    expect_equal(
        provider,
        "evidence.digest",
        "sha256:" + hashlib.sha256(evidence_bytes).hexdigest(),
        evidence.get("digest"),
    )

    bindings = session.get("channel_binding")
    if not isinstance(bindings, list) or not bindings:
        raise RuntimeError(f"{provider.name} attested session missing channel_binding")
    binding_types = {
        binding.get("type") for binding in bindings if isinstance(binding, dict)
    }
    if provider.binding not in binding_types:
        raise RuntimeError(
            f"{provider.name} attested session missing binding {provider.binding}"
        )

    return {
        "session_id": session_id,
        "upstream_name": session.get("upstream_name"),
        "endpoint": session.get("endpoint"),
        "verifier_id": session.get("verifier_id"),
        "claims": {
            name: claim.get("status")
            for name, claim in claims.items()
            if isinstance(claim, dict)
        },
        "binding_count": len(bindings),
        "binding_types": sorted(t for t in binding_types if t),
        "evidence_digest": evidence.get("digest"),
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
        "upstream_name": value.get("upstream_name"),
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
