#!/usr/bin/env python3
"""Live check for router channel-keying.

Boots the gateway with ONE NEAR AI upstream mapping two models, sends a request
to each, and asserts both receipts cite the SAME `upstream.verified.session_id`
— i.e. a router yields one attested session per channel, not one per model.

Run from scripts/ with NEARAI_API_KEY in the environment.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import requests  # noqa: E402

from live_e2e.common import (  # noqa: E402
    DEFAULT_DSTACK_ENDPOINT,
    DEFAULT_DSTACK_VERIFIER_URL,
    ROOT,
)

PORT = 18086
BASE = f"http://127.0.0.1:{PORT}"
MODELS = {
    "router-gemma": "google/gemma-4-31B-it",
    "router-deepseek": "deepseek-ai/DeepSeek-V4-Flash",
}


def session_id_for(model: str) -> tuple[int, str | None, str | None]:
    body = {"model": model, "messages": [{"role": "user", "content": "hi"}], "max_tokens": 16}
    resp = requests.post(f"{BASE}/v1/chat/completions", json=body, timeout=180)
    rid = resp.headers.get("x-receipt-id")
    if resp.status_code != 200 or not rid:
        print(f"    {model}: status={resp.status_code} body={resp.text[:300]}")
        return resp.status_code, None, None
    rc = requests.get(f"{BASE}/v1/aci/receipts/{rid}", timeout=30).json()
    receipt = rc.get("receipt") if "event_log" not in rc else rc
    uv = next(
        (e for e in (receipt or {}).get("event_log", []) if e.get("type") == "upstream.verified"),
        {},
    )
    return resp.status_code, uv.get("session_id"), uv.get("model_id")


def main() -> int:
    token = os.environ.get("NEARAI_API_KEY")
    if not token:
        print("missing NEARAI_API_KEY")
        return 2

    config = [
        {
            "name": "near-router",
            "provider": "near-ai",
            "base_url": "https://cloud-api.near.ai",
            "models": MODELS,
            "bearer_token": token,
            "connect_timeout_seconds": 10,
            "read_timeout_seconds": 600,
            "verifier_request_timeout_seconds": 120,
        }
    ]

    with tempfile.TemporaryDirectory(prefix="router-session-smoke-") as tmp:
        cfg_path = Path(tmp) / "upstreams.json"
        cfg_path.write_text(json.dumps(config))
        log_path = Path(tmp) / "aggregator.log"
        env = {
            **os.environ,
            "PRIVATE_AI_GATEWAY_BIND": f"127.0.0.1:{PORT}",
            "PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH": str(cfg_path),
            "PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT": DEFAULT_DSTACK_ENDPOINT,
            "DSTACK_VERIFIER_URL": os.environ.get("DSTACK_VERIFIER_URL", DEFAULT_DSTACK_VERIFIER_URL),
            "PRIVATE_AI_GATEWAY_REPO_URL": "https://github.com/Dstack-TEE/private-ai-gateway",
            "PRIVATE_AI_GATEWAY_REPO_COMMIT": "live-e2e",
            "PRIVATE_AI_GATEWAY_RECEIPT_TTL_SECONDS": "3600",
            "RUST_LOG": "warn",
        }
        env.pop("NEARAI_API_KEY", None)  # lives in the config file, not the env
        log = log_path.open("wb")
        proc = subprocess.Popen(
            ["cargo", "run", "--bin", "private-ai-gateway"],
            cwd=ROOT,
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            deadline = time.time() + 240
            ready = False
            while time.time() < deadline:
                if proc.poll() is not None:
                    print("gateway exited early; log tail:\n", log_path.read_text()[-1200:])
                    return 1
                try:
                    r = requests.get(f"{BASE}/v1/models", timeout=2)
                    if r.status_code == 200:
                        ids = {m.get("id") for m in r.json().get("data", [])}
                        if set(MODELS) <= ids:
                            ready = True
                            break
                except Exception:
                    pass
                time.sleep(0.75)
            if not ready:
                print("gateway not ready; log tail:\n", log_path.read_text()[-1200:])
                return 1

            results = {}
            for model in MODELS:
                status, sid, model_id = session_id_for(model)
                print(f"  {model}: status={status} session_id={sid} model_id={model_id}")
                if status != 200 or not sid:
                    return 1
                results[model] = sid

            sids = set(results.values())
            if len(sids) == 1:
                print(f"\nPASS: both models resolved to ONE channel session: {sids.pop()}")
                return 0
            print(f"\nFAIL: models produced different sessions: {results}")
            return 1
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)


if __name__ == "__main__":
    raise SystemExit(main())
