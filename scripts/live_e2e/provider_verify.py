from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

from .common import (
    ROOT,
    Provider,
    run_cmd_json,
    write_json,
)


ZERO_HASH = "sha256:" + ("0" * 64)


def verify_provider(
    provider: Provider,
    *,
    env: dict[str, str] | None = None,
    strict: bool = False,
    refs_dir: Path | None = None,
    timeout: int = 300,
    artifact_dir: Path | None = None,
) -> dict[str, Any]:
    # The bridge runs from the gateway project (cwd=ROOT) so it uses the gateway's
    # own uv env and the vendored confidential_verifier package. An external verifier
    # checkout is selected only when PRIVATE_AI_VERIFIER_DIR is set in the environment.
    request = {
        "api_version": "aci.provider-verifier.request.v1",
        "provider": provider.provider,
        "upstream_name": provider.name,
        "url_origin": provider.base_url,
        "model_id": provider.upstream_model,
        "forwarded_body_hash": ZERO_HASH,
        "required": True,
        "timeout_seconds": timeout,
    }
    options = provider_options(provider, env or os.environ)
    if options:
        request["provider_options"] = options
    output = run_cmd_json(
        [
            "uv",
            "run",
            "python",
            str(ROOT / "scripts" / "private_ai_provider_verifier.py"),
        ],
        cwd=ROOT,
        env={
            **os.environ,
            **(env or {}),
        },
        input_value=request,
        timeout=timeout,
    )
    if artifact_dir:
        write_json(
            artifact_dir / provider.name / "provider-verifier-request.json",
            redact_verifier_request(request, provider.api_key_env),
        )
        write_json(
            artifact_dir / provider.name / "provider-verifier-output.json",
            output,
        )
    assert_verified_provider_output(provider, output)
    if strict:
        assert_strict_reference(provider, output, refs_dir or ROOT / "scripts/live_e2e/provider_refs")
    return output


def assert_verified_provider_output(provider: Provider, output: dict[str, Any]) -> None:
    if output.get("result") != "verified":
        raise RuntimeError(
            f"{provider.name} verifier failed: {output.get('reason') or output}"
        )
    evidence = output.get("evidence")
    data = evidence.get("data") if isinstance(evidence, dict) else None
    if (
        not isinstance(evidence, dict)
        or not evidence.get("digest")
        or not isinstance(data, str)
        or not data.startswith("data:")
    ):
        raise RuntimeError(f"{provider.name} verifier did not return embedded evidence")
    bindings = output.get("channel_bindings")
    if not isinstance(bindings, list) or not bindings:
        raise RuntimeError(f"{provider.name} verifier did not return channel bindings")
    binding_types = {binding.get("type") for binding in bindings if isinstance(binding, dict)}
    if provider.binding not in binding_types:
        raise RuntimeError(
            f"{provider.name} expected binding {provider.binding}, got {sorted(binding_types)}"
        )


def assert_strict_reference(
    provider: Provider,
    output: dict[str, Any],
    refs_dir: Path,
) -> None:
    ref_path = refs_dir / f"{provider.provider}.json"
    if not ref_path.exists():
        raise RuntimeError(f"strict mode missing reference file {ref_path}")
    ref = json.loads(ref_path.read_text(encoding="utf-8"))
    accepted = (ref.get("accepted_models") or {}).get(provider.upstream_model)
    if not accepted:
        raise RuntimeError(
            f"strict mode has no accepted model reference for {provider.upstream_model}"
        )
    expected_binding = accepted.get("expected_binding")
    binding_types = {
        binding.get("type")
        for binding in output.get("channel_bindings") or []
        if isinstance(binding, dict)
    }
    if expected_binding and expected_binding not in binding_types:
        raise RuntimeError(
            f"strict reference expected {expected_binding}, got {sorted(binding_types)}"
        )


def provider_options(provider: Provider, env: dict[str, str]) -> dict[str, str]:
    if provider.provider != "chutes":
        return {}
    options: dict[str, str] = {}
    api_key = env.get(provider.api_key_env)
    if api_key:
        options["chutes_api_key"] = api_key
    if provider.chutes_e2ee_api_base:
        options["chutes_e2ee_api_base"] = provider.chutes_e2ee_api_base
    for model_id, chute_id in provider.chutes_chute_ids.items():
        options[f"chutes_chute_id:{model_id}"] = chute_id
    if provider.chutes_e2ee_discovery_rounds is not None:
        options["chutes_e2ee_discovery_rounds"] = str(
            provider.chutes_e2ee_discovery_rounds
        )
    if provider.chutes_e2ee_discovery_interval_seconds is not None:
        options["chutes_e2ee_discovery_interval_seconds"] = str(
            provider.chutes_e2ee_discovery_interval_seconds
        )
    return options


def redact_verifier_request(request: dict[str, Any], api_key_env: str) -> dict[str, Any]:
    redacted = dict(request)
    provider_options = redacted.get("provider_options")
    if isinstance(provider_options, dict):
        redacted["provider_options"] = dict(provider_options)
        if "chutes_api_key" in redacted["provider_options"]:
            redacted["provider_options"]["chutes_api_key"] = "<redacted>"
    redacted["api_key_env"] = api_key_env
    return redacted
