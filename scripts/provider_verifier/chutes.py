"""Chutes provider verification."""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import secrets
import time
import urllib.request
from typing import Any

from .common import (
    emit,
    evidence_bundle,
    failed,
    is_uuid_like,
    provider_options,
    raw_http_bundle_evidence,
    raw_http_item,
    request_timeout_seconds,
    response_content_type,
    sha256_base64_key,
    tdx_debug_enabled,
)


def chutes_headers(api_key: str, authorization_scheme: str = "Bearer") -> dict[str, str]:
    return {
        "Authorization": f"{authorization_scheme} {api_key}",
        "Content-Type": "application/json",
    }


def chutes_api_base(options: dict[str, str]) -> str:
    value = (
        options.get("chutes_e2ee_api_base")
        or "https://api.chutes.ai"
    )
    return value.strip().rstrip("/")


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
    return tdx_debug_enabled(quote_bytes)


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
) -> dict[str, Any]:
    """Check the NVIDIA confidential-computing GPU evidence as SUPPLEMENTAL
    metadata, never a gate (mirrors the PhalaDirect verifier).

    We still run the check: when it succeeds it authenticates the GPU model and
    related info we surface, which the CPU TEE quote alone does not vouch for. But
    a real TEE GPU is already established by the measured serving software inside
    that quote, and a standalone NRAS check only proves a CC-capable GPU exists
    for a nonce — not that it is bound to this CPU TEE or served this request. So
    a missing/invalid GPU result is recorded, not fatal: as long as the workload
    is secure and the info we report is accurate, a failed GPU check never
    rejects the session, and the typed gpu_attested claim stays Unknown. The CPU
    TEE quote, report_data binding, debug bit and measurement match remain the
    hard gates."""
    result: dict[str, Any] = {
        "gpu_evidence_present": bool(gpu_evidence),
        "gpu_verified": False,
        "gpu_evidence_nonce_matched": None,
        "gpu_arch": None,
    }
    if not gpu_evidence:
        return result

    import jwt
    import requests

    first = gpu_evidence[0]
    arch = first.get("arch") if isinstance(first, dict) else None
    result["gpu_arch"] = arch
    if not arch:
        return result

    try:
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
            return result
        tokens = response.json()
        if not tokens or not isinstance(tokens, list):
            return result
        platform = tokens[0]
        if not isinstance(platform, list) or len(platform) < 2 or platform[0] != "JWT":
            return result
        claims = jwt.decode(
            platform[1],
            options={"verify_signature": False},
            algorithms=["RS256", "ES256", "ES384", "PS256"],
        )
        nonce_matched = claims.get("eat_nonce") == expected_report_data
        result["gpu_evidence_nonce_matched"] = nonce_matched
        result["gpu_verified"] = (
            claims.get("x-nvidia-overall-att-result") is True and nonce_matched
        )
    except Exception:  # noqa: BLE001 - supplemental; a GPU error is never fatal
        return result
    return result


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
    # TCB freshness is recorded, not gated: a stale TCB surfaces as a refuted
    # tcb_up_to_date claim in the session layer rather than failing the instance.
    # The quote signature/collateral, report_data binding, debug-mode bit and
    # measurement match above and below remain hard gates.
    tcb_status = dcap_result.get("status")
    measurement = chutes_measurement_name(dcap_result, measurements)
    if not measurement:
        raise ValueError("Chutes quote measurements do not match a public profile")

    gpu = await asyncio.to_thread(
        chutes_verify_gpu,
        evidence.get("gpu_evidence") or [],
        expected_report_data,
        timeout,
    )

    return {
        "instance_id": instance_id,
        "measurement": measurement,
        "public_key_sha256": sha256_base64_key(e2e_pubkey),
        "tcb_status": tcb_status,
        "gpu": gpu,
    }


async def verify_chutes(request: dict[str, Any]) -> None:
    provider = "chutes"
    options = provider_options(request)
    api_key = (options.get("chutes_api_key") or "").strip()
    authorization_scheme = (
        "Basic"
        if options.get("chutes_authorization_scheme", "Bearer").lower() == "basic"
        else "Bearer"
    )
    api_base = chutes_api_base(options)
    if not api_key:
        measurements_url = f"{api_base}/servers/tee/measurements"
        evidence = None
        try:
            with urllib.request.urlopen(measurements_url, timeout=15) as response:
                body = response.read()
                json.loads(body.decode("utf-8"))
                evidence = evidence_bundle(
                    body,
                    measurements_url,
                    response_content_type(response),
                )
        except Exception:
            pass
        failed(
            provider,
            "Chutes bearer_token is required to fetch per-instance E2EE attestation evidence",
            evidence=evidence,
        )
        return

    import requests

    timeout = request_timeout_seconds(request, 60)
    headers = chutes_headers(api_key, authorization_scheme)
    chute_id = chutes_resolve_id(
        request["model_id"],
        headers,
        timeout,
        api_base,
        options,
    )
    attestation_url = f"{api_base}/chutes/{chute_id}/evidence"

    measurements_response = requests.get(
        f"{api_base}/servers/tee/measurements", timeout=timeout
    )
    measurements_response.raise_for_status()
    measurements_body = measurements_response.content
    measurements = json.loads(measurements_body.decode("utf-8"))
    raw_items = [
        raw_http_item(
            "chutes.measurements",
            f"{api_base}/servers/tee/measurements",
            response_content_type(measurements_response),
            measurements_body,
        )
    ]

    nonce = secrets.token_hex(32)
    evidence_response = requests.get(
        attestation_url,
        params={"nonce": nonce},
        headers=headers,
        timeout=timeout,
    )
    evidence_response.raise_for_status()
    evidence_body = evidence_response.content
    evidence_data = json.loads(evidence_body.decode("utf-8"))
    evidence_items = evidence_data.get("evidence") or []
    raw_items.append(
        raw_http_item(
            "chutes.attestation_evidence",
            f"{attestation_url}?nonce={nonce}",
            response_content_type(evidence_response),
            evidence_body,
        )
    )

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
        pubkeys_body = pubkeys_response.content
        pubkeys_data = json.loads(pubkeys_body.decode("utf-8"))
        pubkeys_responses.append(pubkeys_data)
        raw_items.append(
            raw_http_item(
                f"chutes.e2ee_instances.{round_index}",
                f"{api_base}/e2e/instances/{chute_id}",
                response_content_type(pubkeys_response),
                pubkeys_body,
            )
        )
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
            evidence=raw_http_bundle_evidence(
                raw_items,
                source_url=f"{api_base}/e2e/instances/{chute_id}",
            ),
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
            evidence=raw_http_bundle_evidence(
                raw_items,
                source_url=attestation_url,
            ),
        )
        return
    # Surface per-instance TDX TCB status and a single fleet-level status for the
    # tri-state tcb_up_to_date claim: UpToDate only if every verified instance is
    # UpToDate, otherwise a representative stale status so the claim refutes.
    instance_tcb_statuses = {
        item["instance_id"]: item.get("tcb_status") for item in verified
    }
    present_statuses = [s for s in instance_tcb_statuses.values() if s]
    if present_statuses and all(s == "UpToDate" for s in present_statuses):
        fleet_tcb_status: str | None = "UpToDate"
    else:
        fleet_tcb_status = next((s for s in present_statuses if s != "UpToDate"), None)
    # GPU evidence is supplemental metadata, never a trust gate (see
    # chutes_verify_gpu). Surface a per-instance map and a fleet summary so the
    # deep audit sees the outcome; the typed gpu_attested claim stays Unknown.
    instance_gpu = {item["instance_id"]: item.get("gpu") for item in verified}
    gpus = [g for g in instance_gpu.values() if isinstance(g, dict)]
    # Per-instance measurement profile (stable hardware identity, no nonce) so a
    # single instance's session can bind its own CPU evidence.
    instance_measurements = {item["instance_id"]: item["measurement"] for item in verified}
    provider_claims = {
        "trust_boundary": "model_instance",
        "evidence_scope": "model_instance",
        "chute_id": chute_id,
        "canonical_model_id": request["model_id"],
        "verified_instance_count": len(verified),
        "verified_instance_ids": [item["instance_id"] for item in verified],
        "verified_public_key_sha256": [item["public_key_sha256"] for item in verified],
        "tcb_status": fleet_tcb_status,
        "instance_tcb_statuses": instance_tcb_statuses,
        "instance_measurements": instance_measurements,
        "gpu_verified": bool(gpus) and all(g.get("gpu_verified") for g in gpus),
        "gpu_evidence_present": any(g.get("gpu_evidence_present") for g in gpus),
        "instance_gpu": instance_gpu,
    }
    if nonce_expires_in is not None:
        provider_claims["nonce_expires_in"] = nonce_expires_in
    if evidence_data.get("failed_instance_ids"):
        provider_claims["failed_instance_ids"] = evidence_data["failed_instance_ids"]
    if skipped_without_key:
        provider_claims["attested_instances_without_e2ee_key"] = skipped_without_key
    emit(
        {
            "result": "verified",
            "verifier_id": "private-ai-verifier/chutes/v1",
            "evidence": raw_http_bundle_evidence(
                raw_items,
                source_url=attestation_url,
            ),
            "channel_bindings": bindings,
            "provider_claims": provider_claims,
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

