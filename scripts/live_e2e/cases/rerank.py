from __future__ import annotations

from pathlib import Path
from typing import Any

from ..common import Provider, json_bytes, request_json, write_bytes
from .lifecycle import verify_served_request


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
    summary = verify_served_request(
        base_url=base_url, provider=provider, endpoint="/v1/rerank", headers=headers,
        provider_dir=provider_dir, request_path=request_path, response_path=response_path,
        status=status, requester_token=REQUESTER_TOKEN,
    )
    return {**summary, "result_count": len(parsed["results"])}


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
