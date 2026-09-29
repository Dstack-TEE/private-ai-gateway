#!/usr/bin/env python3
"""Live streaming smoke for the receipt-hashing path.

The standard live_e2e cases are all buffered, so they never exercise
`ReceiptFinalizingStream`. This drives a real provider's *streaming* chat
through the gateway and checks that the receipt's `response.returned` event
hashes the exact wire bytes we received back.

Usage (run from scripts/, with the provider's API key in the env):
    uv run python live_e2e/streaming_smoke.py near-ai-live
"""
from __future__ import annotations

import hashlib
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import requests  # noqa: E402

from live_e2e.common import load_providers  # noqa: E402
from live_e2e.launch_aggregator import AggregatorProcess  # noqa: E402

PROVIDERS_FILE = Path(__file__).resolve().parent / "providers.json"


def main() -> int:
    name = sys.argv[1] if len(sys.argv) > 1 else "near-ai-live"
    providers = load_providers(PROVIDERS_FILE, [name])
    if not providers:
        print(f"provider {name!r} not found in {PROVIDERS_FILE}")
        return 2
    provider = providers[0]
    if not os.environ.get(provider.api_key_env):
        print(f"missing {provider.api_key_env} in environment")
        return 2

    port = 18086
    with AggregatorProcess([provider], port=port) as agg:
        base = agg.base_url
        body = {
            "model": provider.public_model,
            "messages": [
                {"role": "user", "content": "Reply with exactly: hello from the smoke test."}
            ],
            "stream": True,
            "max_tokens": 64,
        }
        print(f"POST {base}/v1/chat/completions  (stream=true, model={provider.public_model})")
        resp = requests.post(
            f"{base}/v1/chat/completions", json=body, stream=True, timeout=180
        )
        raw = bytearray()
        for chunk in resp.iter_content(chunk_size=None):
            if chunk:
                raw += chunk
        content_type = resp.headers.get("content-type", "")
        receipt_id = resp.headers.get("x-receipt-id")
        print(
            f"  status={resp.status_code} content-type={content_type!r} "
            f"x-receipt-id={receipt_id} wire_bytes={len(raw)}"
        )
        if resp.status_code != 200:
            print("  body:", bytes(raw[:600]))
            return 1
        if "text/event-stream" not in content_type:
            print("  FAIL: response was not an SSE stream")
            return 1
        if not receipt_id:
            print("  FAIL: no x-receipt-id header")
            return 1

        r2 = requests.get(f"{base}/v1/aci/receipts/{receipt_id}", timeout=30)
        if r2.status_code != 200:
            print(f"  FAIL: receipt fetch {r2.status_code}: {r2.text[:300]}")
            return 1
        payload = r2.json() or {}
        # The receipt endpoint returns the receipt object directly (event_log at
        # top level); tolerate a {"receipt": {...}} envelope too.
        receipt = payload.get("receipt") if "event_log" not in payload else payload
        events = (receipt or {}).get("event_log", [])

        # Per-router check: the upstream.verified event (which feeds the attested
        # session) must be the gateway *channel* only — no model attestation
        # folded in. (NEAR AI fix: model TD quotes stay out of the session.)
        import base64 as _b64
        import json as _json

        uv = next((e for e in events if isinstance(e, dict) and e.get("type") == "upstream.verified"), None)
        if uv is not None:
            model_keys = {
                "model_attestations_sha256",
                "model_attestation_count",
                "model_evidence_present",
                "nested_model_attestations_checked_by_gateway",
                "canonical_model_id",
                "model_attestations_nonce_matched",
            }
            pc = uv.get("provider_claims") or {}
            leaked_claims = model_keys & set(pc)
            ev = uv.get("evidence") or {}
            data_uri = ev.get("data") or ""
            ev_obj = {}
            if ";base64," in data_uri:
                try:
                    ev_obj = _json.loads(_b64.b64decode(data_uri.split(";base64,", 1)[1]))
                except Exception:
                    ev_obj = {}
            ev_has_model = "model_attestations" in ev_obj or "model_id" in ev_obj
            print(f"  [per-router] provider_claims keys: {sorted(pc)}")
            print(f"  [per-router] evidence top-level keys: {sorted(ev_obj)}")
            if leaked_claims or ev_has_model:
                print(f"  PER-ROUTER FAIL: model attestation leaked into session "
                      f"(claims={sorted(leaked_claims)}, evidence_has_model={ev_has_model})")
                return 1
            print("  [per-router] OK: session evidence/claims are gateway-channel only")
        types = [e.get("type") for e in events if isinstance(e, dict)]
        rr = next((e for e in events if isinstance(e, dict) and e.get("type") == "response.returned"), None)
        if not rr:
            print(f"  FAIL: receipt has no response.returned event; events={types}")
            return 1

        cleartext_hash = rr.get("cleartext_hash")
        wire_hash = rr.get("wire_hash")
        observed = "sha256:" + hashlib.sha256(bytes(raw)).hexdigest()
        has_upstream_verified = "upstream.verified" in types
        print(f"  receipt.cleartext_hash = {cleartext_hash}")
        print(f"  receipt.wire_hash      = {wire_hash}")
        print(f"  sha256(received wire)  = {observed}")
        print(f"  upstream.verified present = {has_upstream_verified}")

        ok = observed == wire_hash and has_upstream_verified
        if ok:
            print(
                "\nPASS: the streamed wire bytes hash to the receipt's wire_hash, "
                "and the receipt carries upstream.verified."
            )
            print(f"  (cleartext_hash == wire_hash: {cleartext_hash == wire_hash})")
            return 0
        print("\nFAIL: streamed bytes do not match the receipt hash")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
