#!/usr/bin/env python3
"""Live check that a router upstream's attested session is refreshed.

Boots the gateway with one router upstream (NEAR AI or Tinfoil) and a short
`verification_refresh_seconds`, then — making NO inference request — confirms:

  1. the boot prewarm writes the channel session (it appears in /v1/aci/sessions), and
  2. the background refresh re-verifies it: a session with a newer `established_at`
     appears, with no inference traffic.

We do NOT require the session id to stay constant. A session is an immutable,
content-addressed record of one verification; a refresh is a new (fresh,
nonce-bound) verification, so a provider whose attestation carries a freshness
nonce (NEAR AI) correctly mints a new immutable session each cycle, while one
whose attestation is static (Tinfoil) re-seals the same id. Both are "refreshed".
Superseded sessions are retained until their TTL so receipts can still resolve
them.

Usage (from scripts/, with the provider key in the environment):
    uv run python live_e2e/router_refresh_smoke.py tinfoil
"""
from __future__ import annotations

import argparse
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
PRESETS = {
    "near-ai": ("https://cloud-api.near.ai", "NEARAI_API_KEY", "google/gemma-4-31B-it"),
    "tinfoil": ("https://inference.tinfoil.sh", "TINFOIL_API_KEY", "kimi-k2-6"),
}


def list_sessions(name: str) -> list[dict]:
    try:
        r = requests.get(f"{BASE}/v1/aci/sessions", params={"provider": name}, timeout=5)
        if r.status_code == 200:
            return (r.json() or {}).get("sessions", [])
    except Exception:
        pass
    return []


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("provider", choices=sorted(PRESETS), default="tinfoil", nargs="?")
    args = parser.parse_args()
    base_url, key_env, model = PRESETS[args.provider]

    token = os.environ.get(key_env)
    if not token:
        print(f"missing {key_env}")
        return 2

    name = f"{args.provider}-router"
    config = [
        {
            "name": name,
            "provider": args.provider,
            "base_url": base_url,
            "models": {"router-model": model},
            "bearer_token": token,
            "verification_refresh_seconds": 5,
            "connect_timeout_seconds": 10,
            "read_timeout_seconds": 600,
            "verifier_request_timeout_seconds": 120,
        }
    ]

    with tempfile.TemporaryDirectory(prefix="router-refresh-smoke-") as tmp:
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
        env.pop(key_env, None)
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
            # Wait for readiness.
            deadline = time.time() + 240
            while time.time() < deadline:
                if proc.poll() is not None:
                    print("gateway exited early; log tail:\n", log_path.read_text()[-1200:])
                    return 1
                try:
                    if requests.get(f"{BASE}/v1/models", timeout=2).status_code == 200:
                        break
                except Exception:
                    pass
                time.sleep(0.5)

            # 1. Prewarm writes the session (no inference request made).
            first = None
            deadline = time.time() + 90
            while time.time() < deadline:
                sessions = list_sessions(name)
                if sessions:
                    first = sessions
                    break
                time.sleep(1)
            if not first:
                print("FAIL: prewarm did not write a session; log tail:\n", log_path.read_text()[-1000:])
                return 1
            est0 = max(s.get("established_at", 0) for s in first)
            ids0 = sorted(s.get("session_id") for s in first)
            print(f"  prewarm: {len(first)} session(s), established_at={est0}, ids={ids0}")

            # 2. The refresh loop re-verifies the channel: a session with a newer
            #    established_at appears (id rotates for nonce-bound attestation,
            #    stays for static attestation — both count as refreshed).
            deadline = time.time() + 90
            while time.time() < deadline:
                sessions = list_sessions(name)
                if sessions and max(s.get("established_at", 0) for s in sessions) > est0:
                    est1 = max(s.get("established_at", 0) for s in sessions)
                    ids1 = sorted(s.get("session_id") for s in sessions)
                    # Is the freshest session the prewarm one re-sealed, or a new
                    # immutable session (nonce-bound attestation)?
                    fresh_ids = {s.get("session_id") for s in sessions if s.get("established_at", 0) == est1}
                    re_sealed = fresh_ids <= set(ids0)
                    print(f"  refresh: {len(sessions)} session(s), established_at={est1}, ids={ids1}")
                    print(
                        f"\nPASS: router channel re-verified by the background refresh "
                        f"(established_at {est0} -> {est1}; "
                        f"{'same id, re-sealed' if re_sealed else 'fresh immutable session, prior retained'})."
                    )
                    return 0
                time.sleep(1)
            print("FAIL: channel was not re-verified within the window")
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
