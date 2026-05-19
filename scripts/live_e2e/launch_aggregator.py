from __future__ import annotations

import os
import signal
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

import requests

from .common import (
    DEFAULT_DSTACK_ENDPOINT,
    DEFAULT_DSTACK_VERIFIER_URL,
    DEFAULT_PRIVATE_AI_VERIFIER_DIR,
    ROOT,
    Provider,
    public_base_url,
    write_json,
)


class AggregatorProcess:
    def __init__(
        self,
        providers: list[Provider],
        *,
        port: int,
        env: dict[str, str] | None = None,
        artifact_dir: Path | None = None,
    ) -> None:
        self.providers = providers
        self.port = port
        self.base_url = public_base_url(port)
        self.env = {**os.environ, **(env or {})}
        self.artifact_dir = artifact_dir
        self._tmp: tempfile.TemporaryDirectory[str] | None = None
        self._process: subprocess.Popen[bytes] | None = None
        self.config_path: Path | None = None
        self.log_path: Path | None = None

    def __enter__(self) -> "AggregatorProcess":
        self._tmp = tempfile.TemporaryDirectory(prefix="private-ai-gateway-live-e2e-")
        tmp_dir = Path(self._tmp.name)
        self.config_path = tmp_dir / "upstreams.json"
        self.log_path = (
            self.artifact_dir / "aggregator.log"
            if self.artifact_dir
            else tmp_dir / "aggregator.log"
        )
        self.log_path.parent.mkdir(parents=True, exist_ok=True)
        config = build_upstream_config(self.providers, self.env)
        write_json(self.config_path, config, mode=0o600)
        if self.artifact_dir:
            write_json(
                self.artifact_dir / "aggregator-upstreams.redacted.json",
                redact_upstream_config(config),
            )
        child_env = {
            **self.env,
            "PRIVATE_AI_GATEWAY_BIND": f"127.0.0.1:{self.port}",
            "PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH": str(self.config_path),
            "PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT": self.env.get(
                "PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT",
                DEFAULT_DSTACK_ENDPOINT,
            ),
            "PRIVATE_AI_GATEWAY_REPO_URL": self.env.get(
                "PRIVATE_AI_GATEWAY_REPO_URL",
                "https://github.com/Dstack-TEE/private-ai-gateway",
            ),
            "PRIVATE_AI_GATEWAY_REPO_COMMIT": self.env.get(
                "PRIVATE_AI_GATEWAY_REPO_COMMIT",
                "live-e2e",
            ),
            "PRIVATE_AI_GATEWAY_BODY_RETENTION_SECONDS": self.env.get(
                "PRIVATE_AI_GATEWAY_BODY_RETENTION_SECONDS",
                "3600",
            ),
            "PRIVATE_AI_GATEWAY_RECEIPT_TTL_SECONDS": self.env.get(
                "PRIVATE_AI_GATEWAY_RECEIPT_TTL_SECONDS",
                "3600",
            ),
            "PRIVATE_AI_GATEWAY_UPSTREAM_CONNECT_TIMEOUT_SECONDS": self.env.get(
                "PRIVATE_AI_GATEWAY_UPSTREAM_CONNECT_TIMEOUT_SECONDS",
                "10",
            ),
            "PRIVATE_AI_GATEWAY_UPSTREAM_READ_TIMEOUT_SECONDS": self.env.get(
                "PRIVATE_AI_GATEWAY_UPSTREAM_READ_TIMEOUT_SECONDS",
                "600",
            ),
            "PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS": self.env.get(
                "PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS",
                "240",
            ),
            "PRIVATE_AI_VERIFIER_DIR": self.env.get(
                "PRIVATE_AI_VERIFIER_DIR",
                str(DEFAULT_PRIVATE_AI_VERIFIER_DIR),
            ),
            "RUST_LOG": self.env.get("RUST_LOG", "info"),
        }
        if "DSTACK_VERIFIER_URL" not in child_env:
            child_env["DSTACK_VERIFIER_URL"] = DEFAULT_DSTACK_VERIFIER_URL
        for provider in self.providers:
            child_env.pop(provider.api_key_env, None)
        log = self.log_path.open("wb")
        self._process = subprocess.Popen(
            ["cargo", "run", "--bin", "private-ai-gateway"],
            cwd=ROOT,
            env=child_env,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self._wait_ready()
        return self

    def __exit__(self, exc_type: object, exc: object, tb: object) -> None:
        if self._process is not None:
            if self._process.poll() is None:
                os.killpg(self._process.pid, signal.SIGTERM)
                try:
                    self._process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(self._process.pid, signal.SIGKILL)
                    self._process.wait(timeout=10)
        if self._tmp is not None and os.getenv("KEEP_LIVE_E2E") != "1":
            self._tmp.cleanup()

    def _wait_ready(self, timeout_seconds: int = 240) -> None:
        deadline = time.time() + timeout_seconds
        last_error: Exception | None = None
        while time.time() < deadline:
            if self._process and self._process.poll() is not None:
                raise RuntimeError(
                    f"aggregator exited early with status {self._process.returncode}; "
                    f"log: {self.log_path}"
                )
            try:
                response = requests.get(f"{self.base_url}/v1/models", timeout=2)
                if response.status_code == 200:
                    payload = response.json()
                    model_ids = {
                        item.get("id")
                        for item in payload.get("data", [])
                        if isinstance(item, dict)
                    }
                    expected = {provider.public_model for provider in self.providers}
                    missing = expected.difference(model_ids)
                    if missing:
                        raise RuntimeError(
                            f"/v1/models missing public aliases: {sorted(missing)}"
                        )
                    return
            except Exception as exc:  # noqa: BLE001 - surfaced after timeout.
                last_error = exc
            time.sleep(0.75)
        raise TimeoutError(f"aggregator did not become ready: {last_error}; log: {self.log_path}")


def build_upstream_config(
    providers: list[Provider],
    env: dict[str, str],
) -> list[dict[str, Any]]:
    config = []
    for provider in providers:
        token = env.get(provider.api_key_env)
        if not token:
            raise RuntimeError(f"missing API key env var {provider.api_key_env}")
        item = {
            "name": provider.name,
            "provider": provider.provider,
            "base_url": provider.base_url,
            "models": {provider.public_model: provider.upstream_model},
            "bearer_token": token,
            "connect_timeout_seconds": 10,
            "read_timeout_seconds": 600,
            "verifier_request_timeout_seconds": 600
            if provider.provider == "chutes"
            else 120,
        }
        for field in (
            "verification_refresh_seconds",
            "session_refresh_seconds",
            "chutes_e2ee_api_base",
            "chutes_chute_ids",
            "chutes_e2ee_discovery_rounds",
            "chutes_e2ee_discovery_interval_seconds",
        ):
            value = getattr(provider, field)
            if value is not None and value != {}:
                item[field] = value
        config.append(item)
    return config


def redact_upstream_config(config: list[dict[str, Any]]) -> list[dict[str, Any]]:
    out = []
    for item in config:
        redacted = dict(item)
        redacted["bearer_token"] = "<redacted>"
        out.append(redacted)
    return out
