from __future__ import annotations

import hashlib
import os
import secrets
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
        dstack_endpoint: str = DEFAULT_DSTACK_ENDPOINT,
        env: dict[str, str] | None = None,
        artifact_dir: Path | None = None,
        inference_token: str | None = None,
    ) -> None:
        self.providers = providers
        self.port = port
        self.base_url = public_base_url(port)
        self.dstack_endpoint = dstack_endpoint
        self.env = {**os.environ, **(env or {})}
        self.artifact_dir = artifact_dir
        if inference_token is None:
            inference_token = secrets.token_urlsafe(32)
        if not inference_token or inference_token != inference_token.strip():
            raise ValueError(
                "inference_token must be non-empty and have no surrounding whitespace"
            )
        self.inference_token = inference_token
        self._tmp: tempfile.TemporaryDirectory[str] | None = None
        self._process: subprocess.Popen[bytes] | None = None
        self.gateway_config_path: Path | None = None
        self.upstream_seed_path: Path | None = None
        self.state_dir: Path | None = None
        self.log_path: Path | None = None

    def __enter__(self) -> "AggregatorProcess":
        self._tmp = tempfile.TemporaryDirectory(prefix="private-ai-gateway-live-e2e-")
        tmp_dir = Path(self._tmp.name)
        self.gateway_config_path = tmp_dir / "gateway.config.json"
        self.upstream_seed_path = tmp_dir / "upstreams.seed.json"
        self.state_dir = tmp_dir / "state"
        self.log_path = (
            self.artifact_dir / "aggregator.log"
            if self.artifact_dir
            else tmp_dir / "aggregator.log"
        )
        self.log_path.parent.mkdir(parents=True, exist_ok=True)
        config = build_upstream_config(self.providers, self.env)
        write_json(self.upstream_seed_path, config, mode=0o600)
        privatemode_credential_path = None
        privatemode = [
            provider for provider in self.providers if provider.provider == "privatemode"
        ]
        if privatemode:
            credential = self.env.get(privatemode[0].api_key_env)
            if not credential:
                raise RuntimeError(
                    f"missing API key env var {privatemode[0].api_key_env}"
                )
            privatemode_credential_path = tmp_dir / "privatemode-api-key"
            privatemode_credential_path.write_text(credential, encoding="utf-8")
            privatemode_credential_path.chmod(0o600)
        gateway_config = build_gateway_config(
            self.providers,
            self.env,
            port=self.port,
            state_dir=self.state_dir,
            upstream_seed_path=self.upstream_seed_path,
            dstack_endpoint=self.dstack_endpoint,
            inference_token=self.inference_token,
            privatemode_credential_path=privatemode_credential_path,
        )
        write_json(self.gateway_config_path, gateway_config, mode=0o600)
        if self.artifact_dir:
            write_json(
                self.artifact_dir / "aggregator-upstreams.redacted.json",
                redact_upstream_config(config),
            )
        child_env = {
            **self.env,
            "PRIVATE_AI_GATEWAY_CONFIG_PATH": str(self.gateway_config_path),
            "RUST_LOG": self.env.get("RUST_LOG", "info"),
        }
        if "DSTACK_VERIFIER_URL" not in child_env:
            child_env["DSTACK_VERIFIER_URL"] = DEFAULT_DSTACK_VERIFIER_URL
        # Only forward PRIVATE_AI_VERIFIER_DIR when explicitly set; otherwise the
        # gateway uses its vendored confidential_verifier package.
        verifier_override = self.env.get("PRIVATE_AI_VERIFIER_DIR")
        if verifier_override:
            child_env["PRIVATE_AI_VERIFIER_DIR"] = verifier_override
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


def build_gateway_config(
    providers: list[Provider],
    env: dict[str, str],
    *,
    port: int,
    state_dir: Path,
    upstream_seed_path: Path,
    dstack_endpoint: str,
    inference_token: str,
    privatemode_credential_path: Path | None,
) -> dict[str, Any]:
    gateway_config: dict[str, Any] = {
        "bind": f"127.0.0.1:{port}",
        "state_dir": str(state_dir),
        "upstream_config_seed_path": str(upstream_seed_path),
        "dstack_endpoint": dstack_endpoint,
    }
    privatemode = [
        provider for provider in providers if provider.provider == "privatemode"
    ]
    if not privatemode:
        return gateway_config

    first = privatemode[0]
    credential = env.get(first.api_key_env)
    if not credential:
        raise RuntimeError(f"missing API key env var {first.api_key_env}")
    required = {
        "manifest_path": first.privatemode_manifest_path,
        "manifest_sha256": first.privatemode_manifest_sha256,
        "credential_path": str(privatemode_credential_path)
        if privatemode_credential_path is not None
        else None,
        "credential_sha256": hashlib.sha256(
            credential.encode("utf-8")
        ).hexdigest(),
        "proxy_image_digest": first.privatemode_proxy_image_digest,
    }
    missing = [name for name, value in required.items() if value is None]
    if missing:
        raise RuntimeError(
            f"Privatemode live E2E is missing static fields: {', '.join(missing)}"
        )
    expected = (
        first.base_url,
        first.privatemode_manifest_path,
        first.privatemode_manifest_sha256,
        first.privatemode_proxy_image_digest,
    )
    if any(
        (
            provider.base_url,
            provider.privatemode_manifest_path,
            provider.privatemode_manifest_sha256,
            provider.privatemode_proxy_image_digest,
        )
        != expected
        for provider in privatemode[1:]
    ):
        raise RuntimeError(
            "all Privatemode routes must share one static proxy deployment"
        )
    gateway_config["inference_token_sha256"] = hashlib.sha256(
        inference_token.encode("utf-8")
    ).hexdigest()
    gateway_config["privatemode_proxy"] = {
        "base_url": first.base_url,
        **required,
    }
    return gateway_config


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
            "connect_timeout_seconds": 10,
            "read_timeout_seconds": 600,
            "verifier_request_timeout_seconds": 600
            if provider.provider == "chutes"
            else 120,
        }
        if provider.provider != "privatemode":
            item["bearer_token"] = token
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
        if "bearer_token" in redacted:
            redacted["bearer_token"] = "<redacted>"
        out.append(redacted)
    return out
