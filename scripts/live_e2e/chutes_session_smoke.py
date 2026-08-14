#!/usr/bin/env python3
"""Live check for Chutes per-instance attested sessions (#6).

Boots the gateway with ONE Chutes upstream (which fronts several TEE instances,
each with its own E2EE key), sends a request, and asserts:

  1. the attested-session list has one session per *verified instance*, each
     bound to that instance's E2EE key (`e2ee_public_key_sha256` with a `key_id`),
     not one bundled session, and
  2. the request's receipt cites one of those per-instance sessions — i.e. the
     session of the instance that actually served — so the chain receipt ->
     instance session -> that instance's claims holds.

`chutes_e2ee_discovery_rounds` is set so discovery samples more than one instance
when the chute is served by several; with a single live instance the smoke still
passes (one per-instance session) and reports the count.

Usage (from scripts/, with CHUTES_API_KEY in the environment):
    uv run python live_e2e/chutes_session_smoke.py
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

PORT = 18087
BASE = f"http://127.0.0.1:{PORT}"
NAME = "chutes-tee"
PUBLIC_MODEL = "chutes-model"
UPSTREAM_MODEL = "zai-org/GLM-5.1-TEE"


def list_sessions() -> list[dict]:
    r = requests.get(f"{BASE}/v1/aci/sessions", params={"provider": NAME}, timeout=10)
    if r.status_code != 200:
        return []
    return (r.json() or {}).get("sessions", [])


def instance_binding(session: dict) -> dict | None:
    """The session's single per-instance E2EE binding, if it is one."""
    for binding in session.get("channel_binding", []):
        if binding.get("type") == "e2ee_public_key_sha256" and binding.get("key_id"):
            return binding
    return None


def send_request() -> tuple[int, str | None]:
    body = {
        "model": PUBLIC_MODEL,
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 16,
    }
    resp = requests.post(f"{BASE}/v1/chat/completions", json=body, timeout=180)
    rid = resp.headers.get("x-receipt-id")
    if resp.status_code != 200 or not rid:
        print(f"    request: status={resp.status_code} body={resp.text[:300]}")
        return resp.status_code, None
    rc = requests.get(f"{BASE}/v1/aci/receipts/{rid}", timeout=30).json()
    receipt = rc.get("receipt") if "event_log" not in rc else rc
    uv = next(
        (e for e in (receipt or {}).get("event_log", []) if e.get("type") == "upstream.verified"),
        {},
    )
    return resp.status_code, uv.get("session_id")


def main() -> int:
    token = os.environ.get("CHUTES_API_KEY")
    if not token:
        print("missing CHUTES_API_KEY")
        return 2

    config = [
        {
            "name": NAME,
            "provider": "chutes",
            "base_url": "https://api.chutes.ai",
            "models": {PUBLIC_MODEL: UPSTREAM_MODEL},
            "bearer_token": token,
            "chutes_e2ee_discovery_rounds": 4,
            "connect_timeout_seconds": 10,
            "read_timeout_seconds": 600,
            "verifier_request_timeout_seconds": 120,
        }
    ]

    with tempfile.TemporaryDirectory(prefix="chutes-session-smoke-") as tmp:
        upstream_seed_path = Path(tmp) / "upstreams.seed.json"
        gateway_config_path = Path(tmp) / "gateway.config.json"
        state_dir = Path(tmp) / "state"
        upstream_seed_path.write_text(json.dumps(config))
        gateway_config_path.write_text(
            json.dumps(
                {
                    "bind": f"127.0.0.1:{PORT}",
                    "state_dir": str(state_dir),
                    "upstream_config_seed_path": str(upstream_seed_path),
                    "dstack_endpoint": DEFAULT_DSTACK_ENDPOINT,
                }
            )
        )
        log_path = Path(tmp) / "aggregator.log"
        env = {
            **os.environ,
            "PRIVATE_AI_GATEWAY_CONFIG_PATH": str(gateway_config_path),
            "DSTACK_VERIFIER_URL": os.environ.get("DSTACK_VERIFIER_URL", DEFAULT_DSTACK_VERIFIER_URL),
            "RUST_LOG": "warn",
        }
        env.pop("CHUTES_API_KEY", None)  # lives in the config file, not the env
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
                    if r.status_code == 200 and PUBLIC_MODEL in {
                        m.get("id") for m in r.json().get("data", [])
                    }:
                        ready = True
                        break
                except Exception:
                    pass
                time.sleep(0.75)
            if not ready:
                print("gateway not ready; log tail:\n", log_path.read_text()[-1200:])
                return 1

            status, cited = send_request()
            if status != 200 or not cited:
                return 1

            sessions = list_sessions()
            if not sessions:
                print("FAIL: no attested sessions recorded for Chutes")
                return 1

            # 1. Every Chutes session is a single per-instance E2EE channel.
            key_ids: list[str] = []
            for s in sessions:
                binding = instance_binding(s)
                if binding is None:
                    print(f"FAIL: session {s.get('session_id')} is not a per-instance E2EE channel: "
                          f"{s.get('channel_binding')}")
                    return 1
                key_ids.append(binding["key_id"])
            if len(set(key_ids)) != len(key_ids):
                print(f"FAIL: duplicate instance bindings across sessions: {key_ids}")
                return 1

            ids = {s.get("session_id") for s in sessions}
            print(f"  sessions: {len(sessions)} (one per instance), instances={sorted(key_ids)}")
            print(f"  receipt cites: {cited}")

            # 2. The receipt cites one of the per-instance sessions.
            if cited not in ids:
                print(f"FAIL: receipt session id {cited} is not among the per-instance sessions {sorted(ids)}")
                return 1

            # 3. That session binds the serving instance's own evidence: its CPU
            #    measurement profile, and its GPU verification outcome.
            cited_session = next((s for s in sessions if s.get("session_id") == cited), {})
            claims = cited_session.get("claims", {})
            measurement = claims.get("extra", {}).get("measurement")
            gpu_attested = (claims.get("gpu_attested") or {}).get("status")
            print(f"  cited session binds: measurement={measurement} gpu_attested={gpu_attested}")
            if not measurement:
                print("FAIL: cited session does not bind the instance's CPU measurement")
                return 1

            print(
                f"\nPASS: {len(sessions)} per-instance Chutes session(s); the receipt resolves to the "
                f"serving instance's session ({cited}), which binds its CPU measurement and GPU outcome."
            )
            return 0
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)


if __name__ == "__main__":
    raise SystemExit(main())
