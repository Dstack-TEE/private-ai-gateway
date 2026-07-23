from __future__ import annotations

import json
import os
import shlex
import socket
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

import requests


ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_ROOT = ROOT.parent
DEFAULT_ENV_FILE = WORKSPACE_ROOT / ".env"
DEFAULT_PRIVATE_AI_VERIFIER_DIR = WORKSPACE_ROOT / "private-ai-verifier"
DEFAULT_DSTACK_ENDPOINT = "unix:/tmp/aci-dstack-sock-dev.dstack.sock"
DEFAULT_DSTACK_VERIFIER_URL = "http://localhost:18080"
DEFAULT_ARTIFACT_DIR = Path("/tmp/private-ai-gateway-live-e2e")


@dataclass(frozen=True)
class Provider:
    name: str
    provider: str
    base_url: str
    public_model: str
    upstream_model: str
    api_key_env: str
    binding: str
    capabilities: tuple[str, ...]
    requires: tuple[str, ...]
    structured_output_max_tokens: int
    verification_refresh_seconds: int | None
    session_refresh_seconds: int | None
    chutes_e2ee_api_base: str | None
    chutes_chute_ids: dict[str, str]
    chutes_e2ee_discovery_rounds: int | None
    chutes_e2ee_discovery_interval_seconds: int | None
    privatemode_manifest_path: str | None
    privatemode_manifest_sha256: str | None
    privatemode_proxy_image_digest: str | None

    @classmethod
    def from_json(cls, value: dict[str, Any]) -> "Provider":
        return cls(
            name=require_str(value, "name"),
            provider=require_str(value, "provider"),
            base_url=require_str(value, "base_url").rstrip("/"),
            public_model=require_str(value, "public_model"),
            upstream_model=require_str(value, "upstream_model"),
            api_key_env=require_str(value, "api_key_env"),
            binding=require_str(value, "binding"),
            capabilities=tuple(value.get("capabilities") or ()),
            requires=tuple(value.get("requires") or ()),
            structured_output_max_tokens=int(value.get("structured_output_max_tokens") or 512),
            verification_refresh_seconds=optional_int(
                value, "verification_refresh_seconds"
            ),
            session_refresh_seconds=optional_int(value, "session_refresh_seconds"),
            chutes_e2ee_api_base=optional_str(value, "chutes_e2ee_api_base"),
            chutes_chute_ids=optional_str_map(value, "chutes_chute_ids"),
            chutes_e2ee_discovery_rounds=optional_int(
                value, "chutes_e2ee_discovery_rounds"
            ),
            chutes_e2ee_discovery_interval_seconds=optional_int(
                value, "chutes_e2ee_discovery_interval_seconds"
            ),
            privatemode_manifest_path=optional_str(
                value, "privatemode_manifest_path"
            ),
            privatemode_manifest_sha256=optional_str(
                value, "privatemode_manifest_sha256"
            ),
            privatemode_proxy_image_digest=optional_str(
                value, "privatemode_proxy_image_digest"
            ),
        )

    def has_capability(self, capability: str) -> bool:
        return capability in self.capabilities


def require_str(value: dict[str, Any], key: str) -> str:
    item = value.get(key)
    if not isinstance(item, str) or not item:
        raise ValueError(f"provider entry is missing string field {key!r}")
    return item


def optional_str(value: dict[str, Any], key: str) -> str | None:
    item = value.get(key)
    if item is None:
        return None
    if not isinstance(item, str) or not item:
        raise ValueError(f"provider entry field {key!r} must be a non-empty string")
    return item


def optional_int(value: dict[str, Any], key: str) -> int | None:
    item = value.get(key)
    if item is None:
        return None
    if isinstance(item, bool) or not isinstance(item, int):
        raise ValueError(f"provider entry field {key!r} must be an integer")
    return item


def optional_str_map(value: dict[str, Any], key: str) -> dict[str, str]:
    item = value.get(key)
    if item is None:
        return {}
    if not isinstance(item, dict):
        raise ValueError(f"provider entry field {key!r} must be an object")
    out: dict[str, str] = {}
    for map_key, map_value in item.items():
        if not isinstance(map_key, str) or not map_key:
            raise ValueError(f"provider entry field {key!r} has an empty key")
        if not isinstance(map_value, str) or not map_value:
            raise ValueError(
                f"provider entry field {key!r}[{map_key!r}] must be a non-empty string"
            )
        out[map_key] = map_value
    return out


def load_dotenv(path: Path) -> dict[str, str]:
    loaded: dict[str, str] = {}
    if not path.exists():
        return loaded
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip()
        if not key:
            continue
        if (
            len(value) >= 2
            and value[0] == value[-1]
            and value[0] in ("'", '"')
        ):
            value = value[1:-1]
        loaded[key] = value
        os.environ.setdefault(key, value)
    return loaded


def merged_env(extra: dict[str, str] | None = None) -> dict[str, str]:
    env = os.environ.copy()
    if extra:
        env.update(extra)
    return env


def load_providers(path: Path, selected: Iterable[str] | None = None) -> list[Provider]:
    data = json.loads(path.read_text(encoding="utf-8"))
    providers = [Provider.from_json(item) for item in data]
    selected_set = set(selected or [])
    if selected_set:
        providers = [
            provider
            for provider in providers
            if provider.name in selected_set
            or provider.provider in selected_set
            or provider.public_model in selected_set
        ]
        missing = selected_set.difference(
            {provider.name for provider in providers}
            | {provider.provider for provider in providers}
            | {provider.public_model for provider in providers}
        )
        if missing:
            raise ValueError(f"unknown provider selection: {', '.join(sorted(missing))}")
    if not providers:
        raise ValueError("provider selection is empty")
    return providers


def json_bytes(value: Any) -> bytes:
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def write_json(path: Path, value: Any, *, mode: int | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if mode is not None:
        path.chmod(mode)


def write_bytes(path: Path, value: bytes, *, mode: int | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(value)
    if mode is not None:
        path.chmod(mode)


def run_cmd(
    cmd: list[str],
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    input_bytes: bytes | None = None,
    timeout: int = 120,
) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def run_cmd_json(
    cmd: list[str],
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    input_value: Any | None = None,
    timeout: int = 120,
) -> dict[str, Any]:
    input_bytes = None if input_value is None else json_bytes(input_value)
    result = run_cmd(cmd, cwd=cwd, env=env, input_bytes=input_bytes, timeout=timeout)
    if result.returncode != 0:
        printable = " ".join(shlex.quote(part) for part in cmd)
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"command failed ({result.returncode}): {printable}\n{stderr}")
    try:
        parsed = json.loads(result.stdout.decode("utf-8"))
    except json.JSONDecodeError as exc:
        stdout = result.stdout.decode("utf-8", errors="replace")
        raise RuntimeError(f"command did not return JSON: {exc}\n{stdout}") from exc
    if not isinstance(parsed, dict):
        raise RuntimeError("command returned non-object JSON")
    return parsed


def request_json(
    method: str,
    url: str,
    *,
    headers: dict[str, str] | None = None,
    body: bytes | None = None,
    timeout: int = 120,
) -> tuple[int, dict[str, str], bytes, Any]:
    response = requests.request(
        method,
        url,
        headers=headers,
        data=body,
        timeout=timeout,
    )
    payload = response.content
    try:
        parsed = response.json()
    except ValueError:
        parsed = None
    return response.status_code, dict(response.headers), payload, parsed


def wait_http_json(url: str, *, timeout_seconds: int = 120) -> Any:
    deadline = time.time() + timeout_seconds
    last_error: Exception | None = None
    while time.time() < deadline:
        try:
            response = requests.get(url, timeout=2)
            if 200 <= response.status_code < 300:
                return response.json()
        except Exception as exc:  # noqa: BLE001 - surfaced after timeout.
            last_error = exc
        time.sleep(0.5)
    if last_error:
        raise TimeoutError(f"{url} did not become ready: {last_error}") from last_error
    raise TimeoutError(f"{url} did not become ready")


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def assert_port_free(port: int) -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        if sock.connect_ex(("127.0.0.1", port)) == 0:
            raise RuntimeError(f"127.0.0.1:{port} is already in use")


def provider_key(provider: Provider) -> str:
    return f"{provider.provider}:{provider.upstream_model}"


def public_base_url(port: int) -> str:
    return f"http://127.0.0.1:{port}"
