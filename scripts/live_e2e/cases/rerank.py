from __future__ import annotations

import secrets
from pathlib import Path
from typing import Any

from ..common import Provider, json_bytes, request_json, run_pap_audit, write_bytes, write_json
from .attested_sessions import assert_upstream_attested_sessions
from .lifecycle import assert_receipt_log


REQUESTER_TOKEN = "live-e2e-requester"
PROBE_INPUT = "What is confidential inference?"


def run_rerank_case(
    *,
    base_url: str,
    provider: Provider,
    artifact_dir: Path,
) -> dict[str, Any]:
    provider_dir = artifact_dir / provider.name / "rerank"
    body = {
        "model": provider.public_model,
        "query": PROBE_INPUT,
        "documents": ["Inference inside a trusted execution environment.", "A recipe for soup."],
        "top_n": 1,
        "return_documents": True,
    }
    request_body = json_bytes(body)
    request_path = provider_dir / "request.json"
    write_bytes(request_path, request_body)
    status, headers, response_body, parsed = request_json(
        "POST",
        f"{base_url}/v1/rerank",
        headers={
            "Authorization": f"Bearer {REQUESTER_TOKEN}",
            "Content-Type": "application/json",
        },
        body=request_body,
        timeout=240,
    )
    response_path = provider_dir / "response.json"
    write_bytes(response_path, response_body)
    if not 200 <= status < 300:
        raise RuntimeError(
            f"{provider.name} rerank request failed with HTTP {status}: "
            f"{response_body.decode('utf-8', errors='replace')[:600]}"
        )
    if not isinstance(parsed, dict):
        raise RuntimeError(f"{provider.name} rerank response is not JSON")
    assert_rerank_shape(provider, parsed)
    receipt_id = headers.get("x-receipt-id")
    if not receipt_id:
        raise RuntimeError(f"{provider.name} rerank response missing x-receipt-id")

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

    # Fetch the receipt by its gateway-owned identifier (spec §7.1).
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
            f"{provider.name} rerank receipt fetch failed: HTTP {receipt_status}"
        )

    assert_rerank_receipt_log(provider, receipt)
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
        "result_count": len(parsed["results"]),
        "verified": (verifier_summary.get("verdict") or {}).get("verified") is True,
        "attested_sessions": attested_sessions,
    }


def assert_rerank_shape(provider: Provider, response: dict[str, Any]) -> None:
    results = response.get("results")
    if not isinstance(results, list) or len(results) != 1:
        raise RuntimeError(f"{provider.name} rerank must return one result")
    result = results[0]
    if result.get("index") not in (0, 1) or not isinstance(result.get("relevance_score"), (int, float)):
        raise RuntimeError(f"{provider.name} rerank result lacks index/score")
    usage = response.get("usage") or {}
    if not isinstance(usage.get("prompt_tokens"), int) or usage["prompt_tokens"] <= 0:
        raise RuntimeError(f"{provider.name} rerank must report billable input tokens")
    if set(response) - {"id", "model", "results", "usage"}:
        raise RuntimeError(f"{provider.name} rerank leaked extra response fields")


def assert_rerank_receipt_log(provider: Provider, receipt: dict[str, Any]) -> None:
    if receipt.get("endpoint") != "/v1/rerank":
        raise RuntimeError(
            f"{provider.name} receipt endpoint must be /v1/rerank, got {receipt.get('endpoint')!r}"
        )
    assert_receipt_log(provider, receipt)
