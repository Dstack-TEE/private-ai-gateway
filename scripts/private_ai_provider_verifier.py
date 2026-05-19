#!/usr/bin/env python3
"""Bridge from Rust provider adapters to private-ai-verifier.

The Rust aggregator owns provider selection and forwarding. This script only
runs provider-specific attestation verification and returns a small, stable JSON
result with binding material the Rust forwarding path can enforce.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import gzip
import hashlib
import json
import os
import secrets
import sys
import time
import urllib.request
from typing import Any


def emit(obj: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")))


def verifier_id_for(provider: str) -> str:
    if provider == "near-ai":
        return "private-ai-verifier/near-ai-gateway/v1"
    return f"private-ai-verifier/{provider}/v1"


def failed(provider: str, reason: str, **extra: Any) -> None:
    emit(
        {
            "result": "failed",
            "verifier_id": verifier_id_for(provider),
            "reason": reason,
            **extra,
        }
    )


def sha256_json(value: Any) -> str:
    body = json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def sha256_json_prefixed(value: Any) -> str:
    return f"sha256:{sha256_json(value)}"


def sha256_base64_key(value: str) -> str:
    return hashlib.sha256(base64.b64decode(value.strip())).hexdigest()


def model_dump(model: Any) -> dict[str, Any]:
    if hasattr(model, "model_dump"):
        return model.model_dump(mode="json")
    return model.dict()


def tinfoil_report_data(raw: dict[str, Any], intel_quote: str) -> bytes:
    fmt = raw.get("format", "")
    if raw.get("body"):
        body = gzip.decompress(base64.b64decode(raw["body"]))
    else:
        body = bytes.fromhex(intel_quote)
    if "sev-snp" in fmt:
        report_data = body[0x50:0x90]
    elif "tdx" in fmt:
        report_data = body[48 + 520 : 48 + 584]
    else:
        raise ValueError(f"unsupported Tinfoil attestation format: {fmt!r}")
    if len(report_data) != 64:
        raise ValueError(f"invalid Tinfoil report_data length: {len(report_data)}")
    return report_data


def is_uuid_like(value: str) -> bool:
    return (
        len(value) == 36
        and value.count("-") == 4
        and all(char == "-" or char in "0123456789abcdefABCDEF" for char in value)
    )


def provider_options(request: dict[str, Any]) -> dict[str, str]:
    value = request.get("provider_options") or {}
    if not isinstance(value, dict):
        raise ValueError("provider_options must be an object")
    return {str(key): str(item) for key, item in value.items()}


def chutes_headers(api_key: str) -> dict[str, str]:
    return {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }


def chutes_api_base(options: dict[str, str]) -> str:
    value = (
        options.get("chutes_e2ee_api_base")
        or "https://api.chutes.ai"
    )
    return value.strip().rstrip("/")


def request_timeout_seconds(request: dict[str, Any], default: int) -> int:
    value = request.get("timeout_seconds")
    if value is None:
        return default
    timeout = int(value)
    if timeout <= 0:
        raise ValueError("timeout_seconds must be positive")
    return timeout


def chutes_resolve_id(
    model_id: str,
    headers: dict[str, str],
    timeout: int,
    api_base: str,
    options: dict[str, str],
) -> str:
    if is_uuid_like(model_id):
        return model_id
    pinned = (
        options.get(f"chutes_chute_id:{model_id}")
        or options.get("chutes_chute_id")
        or ""
    ).strip()
    if pinned:
        if not is_uuid_like(pinned):
            raise ValueError(f"configured chute_id for {model_id} is not UUID-like")
        return pinned

    import requests

    response = requests.get(
        f"{api_base}/chutes/",
        params={"include_public": "true", "name": model_id},
        headers=headers,
        timeout=timeout,
    )
    response.raise_for_status()
    items = response.json().get("items") or []
    if not items:
        raise ValueError(f"Chute not found: {model_id}")
    for item in items:
        if item.get("name") == model_id and item.get("chute_id"):
            return item["chute_id"]
    raise ValueError(f"Chute lookup did not return an exact chute_id match for {model_id}")


def chutes_report_data(quote_bytes: bytes) -> bytes:
    report_data = quote_bytes[48 + 520 : 48 + 584]
    if len(report_data) != 64:
        raise ValueError(f"invalid Chutes TDX report_data length: {len(report_data)}")
    return report_data


def chutes_debug_enabled(quote_bytes: bytes) -> bool:
    td_attributes = quote_bytes[48 + 120 : 48 + 128]
    if len(td_attributes) != 8:
        raise ValueError(f"invalid Chutes td_attributes length: {len(td_attributes)}")
    return bool(int(td_attributes.hex(), 16) & 1)


def chutes_discovery_rounds(options: dict[str, str]) -> int:
    value = options.get("chutes_e2ee_discovery_rounds", "3")
    try:
        rounds = int(value)
    except ValueError as exc:
        raise ValueError("chutes_e2ee_discovery_rounds must be an integer") from exc
    if rounds < 1 or rounds > 10:
        raise ValueError("chutes_e2ee_discovery_rounds must be between 1 and 10")
    return rounds


def chutes_discovery_interval_seconds(options: dict[str, str]) -> float:
    value = options.get("chutes_e2ee_discovery_interval_seconds", "0")
    try:
        interval = float(value)
    except ValueError as exc:
        raise ValueError("chutes_e2ee_discovery_interval_seconds must be a number") from exc
    if interval < 0:
        raise ValueError("chutes_e2ee_discovery_interval_seconds must be non-negative")
    return interval


def chutes_measurement_name(
    dcap_result: dict[str, Any],
    measurements: list[dict[str, Any]],
) -> str | None:
    td10 = ((dcap_result.get("report") or {}).get("TD10") or {})
    mrtd = str(td10.get("mr_td") or "").lower()
    rtmrs = {
        "RTMR0": str(td10.get("rt_mr0") or "").lower(),
        "RTMR1": str(td10.get("rt_mr1") or "").lower(),
        "RTMR2": str(td10.get("rt_mr2") or "").lower(),
        "RTMR3": str(td10.get("rt_mr3") or "").lower(),
    }
    if not mrtd or not all(rtmrs.values()):
        return None
    for profile in measurements:
        if str(profile.get("mrtd") or "").lower() != mrtd:
            continue
        expected = profile.get("runtime_rtmrs") or {}
        if all(str(expected.get(k) or "").lower() == v for k, v in rtmrs.items()):
            return str(profile.get("name") or "unnamed")
    return None


def chutes_verify_gpu(
    gpu_evidence: list[Any],
    expected_report_data: str,
    timeout: int,
) -> None:
    if not gpu_evidence:
        return

    import jwt
    import requests

    first = gpu_evidence[0]
    arch = first.get("arch") if isinstance(first, dict) else None
    if not arch:
        raise ValueError("Chutes GPU evidence is missing arch")

    response = requests.post(
        "https://nras.attestation.nvidia.com/v3/attest/gpu",
        json={
            "evidence_list": gpu_evidence,
            "nonce": expected_report_data,
            "arch": arch,
        },
        headers={"accept": "application/json", "content-type": "application/json"},
        timeout=timeout,
    )
    if response.status_code != 200:
        raise ValueError(f"NRAS responded with status {response.status_code}")

    tokens = response.json()
    if not tokens or not isinstance(tokens, list):
        raise ValueError("NRAS response did not include tokens")
    platform = tokens[0]
    if not isinstance(platform, list) or len(platform) < 2 or platform[0] != "JWT":
        raise ValueError("NRAS platform token has invalid shape")
    claims = jwt.decode(
        platform[1],
        options={"verify_signature": False},
        algorithms=["RS256", "ES256", "ES384", "PS256"],
    )
    if claims.get("x-nvidia-overall-att-result") is not True:
        raise ValueError("NVIDIA attestation result is false")
    if claims.get("eat_nonce") != expected_report_data:
        raise ValueError("NVIDIA eat_nonce does not match Chutes report_data binding")


async def chutes_verify_instance(
    evidence: dict[str, Any],
    nonce: str,
    e2e_pubkey: str,
    measurements: list[dict[str, Any]],
    timeout: int,
) -> dict[str, Any]:
    import dcap_qvl

    instance_id = evidence.get("instance_id")
    quote_b64 = evidence.get("quote")
    if not instance_id or not quote_b64:
        raise ValueError("Chutes evidence is missing instance_id or quote")

    quote_bytes = base64.b64decode(quote_b64)
    expected_report_data = hashlib.sha256((nonce + e2e_pubkey).encode()).hexdigest()
    report_data = chutes_report_data(quote_bytes)
    if report_data[:32].hex() != expected_report_data:
        raise ValueError("Chutes E2EE key binding does not match report_data")
    if chutes_debug_enabled(quote_bytes):
        raise ValueError("Chutes TDX quote has debug mode enabled")

    verified_report = await dcap_qvl.get_collateral_and_verify(quote_bytes)
    dcap_result = json.loads(verified_report.to_json())
    if dcap_result.get("status") != "UpToDate":
        raise ValueError(f"Chutes TDX status is not UpToDate: {dcap_result.get('status')}")
    measurement = chutes_measurement_name(dcap_result, measurements)
    if not measurement:
        raise ValueError("Chutes quote measurements do not match a public profile")

    await asyncio.to_thread(
        chutes_verify_gpu,
        evidence.get("gpu_evidence") or [],
        expected_report_data,
        timeout,
    )

    return {
        "instance_id": instance_id,
        "measurement": measurement,
        "public_key_sha256": sha256_base64_key(e2e_pubkey),
    }


async def verify_tinfoil(request: dict[str, Any]) -> None:
    from confidential_verifier import TeeVerifier

    provider = "tinfoil"
    verifier = TeeVerifier()
    with contextlib.redirect_stdout(sys.stderr):
        report = await verifier.fetch_report(provider, request["model_id"])
        result = await verifier.verify(report)
    report_obj = model_dump(report)
    result_obj = model_dump(result)
    evidence_ref = request.get("url_origin")
    if evidence_ref:
        evidence_ref = f"{evidence_ref.rstrip('/')}/.well-known/tinfoil-attestation"
    else:
        evidence_ref = "https://inference.tinfoil.sh/.well-known/tinfoil-attestation"
    if not result.model_verified:
        failed(
            provider,
            result.error or "Tinfoil verification failed",
            evidence_digest=sha256_json(report_obj),
            evidence_ref=evidence_ref,
        )
        return
    report_data = tinfoil_report_data(report.raw or {}, report.intel_quote)
    emit(
        {
            "result": "verified",
            "verifier_id": "private-ai-verifier/tinfoil/v1",
            "evidence_digest": sha256_json({"report": report_obj, "result": result_obj}),
            "evidence_ref": evidence_ref,
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": request.get("url_origin"),
                    "spki_sha256": report_data[:32].hex(),
                }
            ],
        }
    )


async def verify_nearai(request: dict[str, Any]) -> None:
    from confidential_verifier.providers.nearai import NearaiProvider
    from confidential_verifier.verifiers.nearai import NearAICloudVerifier

    provider = "near-ai"
    verifier_id = verifier_id_for(provider)
    near_provider = NearaiProvider(include_tls_fingerprint=True)
    dstack_verifier_url = os.getenv("DSTACK_VERIFIER_URL", "http://localhost:8080")
    with contextlib.redirect_stdout(sys.stderr):
        report = await asyncio.to_thread(near_provider.fetch_report, request["model_id"])
        verifier = NearAICloudVerifier(dstack_verifier_url)
        gateway = (report.raw or {}).get("gateway_attestation") or {}
        gateway_result = await verifier.verify_gateway_component(
            report.raw or {},
            report.request_nonce,
        )
    report_obj = model_dump(report)
    evidence_ref = "https://cloud-api.near.ai/v1/attestation/report"
    if not gateway:
        failed(
            provider,
            "NEAR AI report did not include gateway_attestation",
            evidence_digest=sha256_json(report_obj),
            evidence_ref=evidence_ref,
            verifier_id=verifier_id,
        )
        return
    spki = gateway.get("tls_cert_fingerprint")
    if not spki:
        failed(
            provider,
            "NEAR AI report did not include TLS SPKI binding",
            evidence_digest=sha256_json(report_obj),
            evidence_ref=evidence_ref,
            verifier_id=verifier_id,
        )
        return
    if not gateway_result.get("is_valid"):
        failed(
            provider,
            "; ".join(gateway_result.get("errors") or [])
            or "NEAR AI gateway verification failed",
            evidence_digest=sha256_json({"report": report_obj, "gateway": gateway_result}),
            evidence_ref=evidence_ref,
            verifier_id=verifier_id,
        )
        return

    raw_report = report.raw or {}
    model_attestations = raw_report.get("model_attestations") or []
    if not isinstance(model_attestations, list) or not model_attestations:
        failed(
            provider,
            "NEAR AI model-scoped report did not include model_attestations",
            evidence_digest=sha256_json(report_obj),
            evidence_ref=evidence_ref,
            verifier_id=verifier_id,
        )
        return

    for index, item in enumerate(model_attestations):
        if not isinstance(item, dict) or not item.get("intel_quote"):
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] did not include intel_quote",
                evidence_digest=sha256_json(report_obj),
                evidence_ref=evidence_ref,
                verifier_id=verifier_id,
            )
            return
        item_nonce = item.get("request_nonce")
        if item_nonce is not None and str(item_nonce).lower() != str(report.request_nonce).lower():
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] nonce did not match request nonce",
                evidence_digest=sha256_json(report_obj),
                evidence_ref=evidence_ref,
                verifier_id=verifier_id,
            )
            return

    model_evidence_digest = sha256_json_prefixed(model_attestations)
    provider_claims = {
        "trust_boundary": "near-ai-gateway",
        "gateway_verified": True,
        "gateway_tls_spki_sha256": spki,
        "model_evidence_present": True,
        "model_attestation_count": len(model_attestations),
        "model_attestations_sha256": model_evidence_digest,
        "nested_model_attestations_checked_by_gateway": False,
        "canonical_model_id": report.model_id,
    }
    if all(isinstance(item, dict) and item.get("request_nonce") for item in model_attestations):
        provider_claims["model_attestations_nonce_matched"] = True

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "evidence_digest": sha256_json(
                {
                    "report": report_obj,
                    "gateway_result": gateway_result,
                    "provider_claims": provider_claims,
                }
            ),
            "evidence_ref": evidence_ref,
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": request.get("url_origin"),
                    "spki_sha256": spki,
                }
            ],
            "provider_claims": provider_claims,
        }
    )
    return


async def verify_chutes(request: dict[str, Any]) -> None:
    provider = "chutes"
    options = provider_options(request)
    api_key = (options.get("chutes_api_key") or "").strip()
    api_base = chutes_api_base(options)
    if not api_key:
        evidence_ref = f"{api_base}/servers/tee/measurements"
        try:
            with urllib.request.urlopen(evidence_ref, timeout=15) as response:
                measurements = json.loads(response.read().decode("utf-8"))
            evidence_digest = sha256_json(measurements)
        except Exception:
            evidence_digest = None
        failed(
            provider,
            "Chutes bearer_token is required to fetch per-instance E2EE attestation evidence",
            evidence_digest=evidence_digest,
            evidence_ref=evidence_ref,
        )
        return

    import requests

    timeout = request_timeout_seconds(request, 60)
    headers = chutes_headers(api_key)
    chute_id = chutes_resolve_id(
        request["model_id"],
        headers,
        timeout,
        api_base,
        options,
    )
    evidence_ref = f"{api_base}/chutes/{chute_id}/evidence"

    measurements_response = requests.get(
        f"{api_base}/servers/tee/measurements", timeout=timeout
    )
    measurements_response.raise_for_status()
    measurements = measurements_response.json()

    nonce = secrets.token_hex(32)
    evidence_response = requests.get(
        evidence_ref,
        params={"nonce": nonce},
        headers=headers,
        timeout=timeout,
    )
    evidence_response.raise_for_status()
    evidence_data = evidence_response.json()
    evidence_items = evidence_data.get("evidence") or []

    discovery_rounds = chutes_discovery_rounds(options)
    discovery_interval = chutes_discovery_interval_seconds(options)
    pubkeys_responses = []
    pubkey_items: dict[str, dict[str, Any]] = {}
    nonce_expires_in = None
    for round_index in range(discovery_rounds):
        if round_index > 0 and discovery_interval > 0:
            time.sleep(discovery_interval)
        pubkeys_response = requests.get(
            f"{api_base}/e2e/instances/{chute_id}",
            headers=headers,
            timeout=timeout,
        )
        pubkeys_response.raise_for_status()
        pubkeys_data = pubkeys_response.json()
        pubkeys_responses.append(pubkeys_data)
        if pubkeys_data.get("nonce_expires_in") is not None:
            nonce_expires_in = (
                pubkeys_data["nonce_expires_in"]
                if nonce_expires_in is None
                else min(nonce_expires_in, pubkeys_data["nonce_expires_in"])
            )
        for item in pubkeys_data.get("instances", []):
            instance_id = item.get("instance_id")
            e2e_pubkey = item.get("e2e_pubkey")
            if not instance_id or not e2e_pubkey:
                continue
            existing = pubkey_items.setdefault(
                instance_id,
                {
                    "instance_id": instance_id,
                    "e2e_pubkey": e2e_pubkey,
                    "nonces": [],
                },
            )
            if existing["e2e_pubkey"] != e2e_pubkey:
                existing["e2e_pubkey"] = e2e_pubkey
                existing["nonces"] = []
            seen = set(existing["nonces"])
            for nonce_token in item.get("nonces") or []:
                if nonce_token not in seen:
                    existing["nonces"].append(nonce_token)
                    seen.add(nonce_token)
    pubkeys = {
        instance_id: item["e2e_pubkey"]
        for instance_id, item in pubkey_items.items()
    }
    if not pubkeys:
        failed(
            provider,
            "Chutes did not return any E2EE public keys for this chute",
            evidence_digest=sha256_json(
                {"measurements": measurements, "pubkeys": pubkeys_responses}
            ),
            evidence_ref=f"{api_base}/e2e/instances/{chute_id}",
        )
        return

    tasks = []
    skipped_without_key = []
    for evidence in evidence_items:
        instance_id = evidence.get("instance_id")
        e2e_pubkey = pubkeys.get(instance_id)
        if not e2e_pubkey:
            if instance_id:
                skipped_without_key.append(instance_id)
            continue
        tasks.append(chutes_verify_instance(evidence, nonce, e2e_pubkey, measurements, timeout))

    results = await asyncio.gather(*tasks, return_exceptions=True)
    verified = [result for result in results if isinstance(result, dict)]
    errors = [str(result) for result in results if isinstance(result, Exception)]
    bindings = [
        {
            "type": "e2ee_public_key_sha256",
            "provider": "chutes",
            "key_id": item["instance_id"],
            "algorithm": "chutes-ml-kem-768",
            "public_key_sha256": item["public_key_sha256"],
        }
        for item in verified
    ]
    if not bindings:
        failed(
            provider,
            "Chutes verification did not produce any verified E2EE key binding"
            + (f": {'; '.join(errors)}" if errors else ""),
            evidence_digest=sha256_json(
                {
                    "measurements": measurements,
                    "pubkeys": pubkeys_responses,
                    "evidence": evidence_data,
                    "errors": errors,
                    "skipped_without_key": skipped_without_key,
                }
            ),
            evidence_ref=evidence_ref,
        )
        return
    emit(
        {
            "result": "verified",
            "verifier_id": "private-ai-verifier/chutes/v1",
            "evidence_digest": sha256_json(
                {
                    "measurements": measurements,
                    "pubkeys": pubkeys_responses,
                    "evidence": evidence_data,
                    "verified": verified,
                    "errors": errors,
                    "skipped_without_key": skipped_without_key,
                }
            ),
            "evidence_ref": evidence_ref,
            "channel_bindings": bindings,
            "chutes_session": {
                "chute_id": chute_id,
                "nonce_expires_in": nonce_expires_in,
                "instances": [
                    {
                        "instance_id": item["instance_id"],
                        "e2e_pubkey": pubkey_items[item["instance_id"]]["e2e_pubkey"],
                        "public_key_sha256": item["public_key_sha256"],
                        "nonces": pubkey_items[item["instance_id"]]["nonces"],
                    }
                    for item in verified
                    if item["instance_id"] in pubkey_items
                ],
            },
        }
    )


async def main() -> None:
    request = json.loads(sys.stdin.read())
    private_ai_dir = os.environ.get("PRIVATE_AI_VERIFIER_DIR")
    if private_ai_dir:
        sys.path.insert(0, private_ai_dir)
    provider = request.get("provider")
    try:
        if provider == "tinfoil":
            await verify_tinfoil(request)
        elif provider == "near-ai":
            await verify_nearai(request)
        elif provider == "chutes":
            await verify_chutes(request)
        else:
            failed(str(provider), f"unsupported provider: {provider!r}")
    except Exception as exc:
        failed(str(provider), str(exc))


if __name__ == "__main__":
    asyncio.run(main())
