"""Chutes provider verification."""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import secrets
import time
from collections.abc import Callable
from concurrent.futures import Future
from threading import Thread
from typing import Any

import jwt
import requests

from .common import (
    ResponseTooLarge,
    emit,
    evidence_bundle,
    failed,
    is_uuid_like,
    provider_options,
    raw_http_bundle_evidence,
    raw_http_item,
    read_bounded_body,
    request_timeout_seconds,
    response_content_type,
    sha256_base64_key,
    tdx_debug_enabled,
    tdx_session_material,
)

NRAS_ORIGIN = "https://nras.attestation.nvidia.com"
NRAS_JWKS_URL = f"{NRAS_ORIGIN}/.well-known/jwks.json"
NRAS_ATTEST_URL = f"{NRAS_ORIGIN}/v3/attest/gpu"
NRAS_JWKS_TIMEOUT_SECONDS = 5
NRAS_POST_TIMEOUT_SECONDS = 5
NRAS_GPU_BUDGET_SECONDS = 10
NRAS_GPU_CONCURRENCY = 8
CPU_VERIFICATION_CONCURRENCY = 8
MAX_MEASUREMENTS_BYTES = 1024 * 1024
MAX_EVIDENCE_BYTES = 40 * 1024 * 1024
MAX_PUBLIC_KEYS_BYTES = 40 * 1024 * 1024
VERIFIER_EXIT_MARGIN_SECONDS = 2


def chutes_headers(api_key: str, basic_auth: bool = False) -> dict[str, str]:
    return {
        "Authorization": f"{'Basic' if basic_auth else 'Bearer'} {api_key}",
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
        if "runtime_rtmrs" in profile:
            expected = profile["runtime_rtmrs"]
        else:
            expected = {k: profile.get(k.lower()) for k in rtmrs}
        if isinstance(expected, dict) and all(
            str(expected.get(k) or "").lower() == v for k, v in rtmrs.items()
        ):
            return str(profile.get("name") or "unnamed")
    return None


def chutes_fetch_measurements(api_base: str, timeout: int) -> tuple[str, str, bytes]:
    url = f"{api_base}/servers/tdx/measurements"
    response = requests.get(url, timeout=timeout, stream=True)
    if response.status_code in (404, 405, 410, 501):
        response.close()
        url = f"{api_base}/servers/tee/measurements"
        response = requests.get(url, timeout=timeout, stream=True)
    with response:
        response.raise_for_status()
        body = read_bounded_body(response, MAX_MEASUREMENTS_BYTES, "Chutes measurements")
        return url, response_content_type(response), body


def chutes_fetch_nras_keys() -> dict[str, jwt.PyJWK]:
    jwks = jwt.PyJWKClient(NRAS_JWKS_URL, timeout=NRAS_JWKS_TIMEOUT_SECONDS)
    return {key.key_id: key for key in jwks.get_signing_keys() if key.key_id is not None}


def chutes_gpu_metadata(gpu_evidence: Any) -> dict[str, Any]:
    first = gpu_evidence[0] if isinstance(gpu_evidence, list) and gpu_evidence else None
    return {
        "gpu_evidence_present": bool(gpu_evidence),
        "gpu_verified": False,
        "gpu_evidence_nonce_matched": None,
        "gpu_arch": first.get("arch") if isinstance(first, dict) else None,
    }


def chutes_verify_gpu(
    gpu_evidence: list[Any],
    expected_report_data: str,
    signing_keys: dict[str, jwt.PyJWK],
) -> dict[str, Any]:
    """Record NVIDIA's signed, nonce-bound platform verdict as supplemental evidence."""
    result = chutes_gpu_metadata(gpu_evidence)
    arch = result["gpu_arch"]
    if not arch:
        return result

    try:
        response = requests.post(
            NRAS_ATTEST_URL,
            json={
                "evidence_list": gpu_evidence,
                "nonce": expected_report_data,
                "arch": arch,
            },
            headers={"accept": "application/json", "content-type": "application/json"},
            timeout=NRAS_POST_TIMEOUT_SECONDS,
        )
        if response.status_code != 200:
            return result
        tokens = response.json()
        if not tokens or not isinstance(tokens, list):
            return result
        platform = tokens[0]
        if not isinstance(platform, list) or len(platform) < 2 or platform[0] != "JWT":
            return result
        token = platform[1]
        signing_key = signing_keys.get(jwt.get_unverified_header(token).get("kid"))
        if signing_key is None:
            return result
        claims = jwt.decode(
            token,
            signing_key,
            algorithms=["ES384"],
            issuer=NRAS_ORIGIN,
            subject="NVIDIA-PLATFORM-ATTESTATION",
            options={"require": ["exp", "sub"]},
        )
        nonce_matched = claims.get("eat_nonce") == expected_report_data
        result["gpu_evidence_nonce_matched"] = nonce_matched
        result["gpu_verified"] = (
            claims.get("x-nvidia-overall-att-result") is True and nonce_matched
        )
    except Exception:  # noqa: BLE001 - GPU failures must remain supplemental
        return result
    return result


async def _run_gpu_io[T](operation: Callable[[], T]) -> T:
    # The default executor joins its threads on asyncio.run() shutdown. A stalled
    # supplemental request must allow the verifier process to exit on time.
    future: Future[T] = Future()

    def run() -> None:
        if future.set_running_or_notify_cancel():
            try:
                future.set_result(operation())
            except Exception as exc:
                future.set_exception(exc)

    Thread(target=run, daemon=True).start()
    return await asyncio.wrap_future(future)


async def chutes_apply_gpu_checks(
    jobs: list[tuple[dict[str, Any], list[Any], str]], verifier_deadline: float
) -> None:
    now = asyncio.get_running_loop().time()
    deadline = min(now + NRAS_GPU_BUDGET_SECONDS, verifier_deadline)
    if not jobs or deadline <= now:
        return

    try:
        async with asyncio.timeout_at(deadline):
            async with asyncio.timeout(NRAS_JWKS_TIMEOUT_SECONDS):
                signing_keys = await _run_gpu_io(chutes_fetch_nras_keys)

            slots = asyncio.Semaphore(NRAS_GPU_CONCURRENCY)

            async def check(instance: dict[str, Any], evidence: list[Any], nonce: str) -> None:
                async with slots:
                    instance["gpu"] = await _run_gpu_io(
                        lambda: chutes_verify_gpu(evidence, nonce, signing_keys)
                    )

            await asyncio.gather(*(check(*job) for job in jobs), return_exceptions=True)
    except Exception:  # noqa: BLE001 - keep the CPU bindings and completed GPU results
        pass


async def chutes_verify_instance(
    evidence: dict[str, Any],
    nonce: str,
    e2e_pubkey: str,
    measurements: list[dict[str, Any]],
) -> tuple[dict[str, Any], str]:
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

    instance = {
        **tdx_session_material(quote_bytes),
        "advisory_ids": sorted(set(verified_report.advisory_ids)),
        "instance_id": instance_id,
        "measurement": measurement,
        "public_key_sha256": sha256_base64_key(e2e_pubkey),
        "tcb_status": tcb_status,
        "gpu": chutes_gpu_metadata(evidence.get("gpu_evidence") or []),
    }
    return instance, expected_report_data


async def verify_chutes(request: dict[str, Any]) -> None:
    try:
        await _verify_chutes(request)
    except ResponseTooLarge as exc:
        failed("chutes", str(exc), failure="evidence_invalid")


async def _verify_chutes(request: dict[str, Any]) -> None:
    started_at = asyncio.get_running_loop().time()
    provider = "chutes"
    options = provider_options(request)
    api_key = (options.get("chutes_api_key") or "").strip()
    basic_auth = options.get("chutes_basic_auth", "false").lower() == "true"
    api_base = chutes_api_base(options)
    if not api_key:
        evidence = None
        try:
            measurements_url, content_type, body = chutes_fetch_measurements(api_base, 15)
            json.loads(body)
            evidence = evidence_bundle(body, measurements_url, content_type)
        except ResponseTooLarge:
            raise
        except (requests.RequestException, ValueError):
            pass
        failed(
            provider,
            "Chutes bearer_token is required to fetch per-instance E2EE public keys",
            failure="verifier_misconfigured",
            evidence=evidence,
        )
        return

    timeout = request_timeout_seconds(request, 60)
    gpu_deadline = started_at + timeout - VERIFIER_EXIT_MARGIN_SECONDS
    headers = chutes_headers(api_key, basic_auth)
    chute_id = chutes_resolve_id(
        request["model_id"],
        headers,
        timeout,
        api_base,
        options,
    )
    attestation_url = f"{api_base}/chutes/{chute_id}/evidence"

    measurements_url, measurements_type, measurements_body = chutes_fetch_measurements(api_base, timeout)
    measurements = json.loads(measurements_body.decode("utf-8"))
    raw_items = [
        raw_http_item(
            "chutes.measurements",
            measurements_url,
            measurements_type,
            measurements_body,
        )
    ]

    nonce = secrets.token_hex(32)
    with requests.get(
        attestation_url,
        params={"nonce": nonce},
        headers=headers,
        timeout=timeout,
        stream=True,
    ) as response:
        response.raise_for_status()
        evidence_body = read_bounded_body(response, MAX_EVIDENCE_BYTES, "Chutes instance evidence")
        evidence_type = response_content_type(response)
    evidence_data = json.loads(evidence_body.decode("utf-8"))
    evidence_items = evidence_data.get("evidence") or []
    raw_items.append(
        raw_http_item(
            "chutes.attestation_evidence",
            f"{attestation_url}?nonce={nonce}",
            evidence_type,
            evidence_body,
        )
    )

    discovery_rounds = chutes_discovery_rounds(options)
    discovery_interval = chutes_discovery_interval_seconds(options)
    pubkeys_url = f"{api_base}/e2e/instances/{chute_id}"
    pubkey_items: dict[str, dict[str, Any]] = {}
    nonce_expires_in = None
    for round_index in range(discovery_rounds):
        if round_index > 0 and discovery_interval > 0:
            time.sleep(discovery_interval)
        with requests.get(
            pubkeys_url,
            headers=headers,
            timeout=timeout,
            stream=True,
        ) as response:
            response.raise_for_status()
            pubkeys_body = read_bounded_body(response, MAX_PUBLIC_KEYS_BYTES, "Chutes public keys")
            pubkeys_type = response_content_type(response)
        pubkeys_data = json.loads(pubkeys_body.decode("utf-8"))
        raw_items.append(
            raw_http_item(
                f"chutes.e2ee_instances.{round_index}",
                pubkeys_url,
                pubkeys_type,
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
            failure="evidence_invalid",
            evidence=raw_http_bundle_evidence(
                raw_items,
                source_url=pubkeys_url,
            ),
        )
        return

    eligible = []
    skipped_without_key = []
    for evidence in evidence_items:
        # Older Chutes deployments return TDX evidence without a type tag.
        tee_type = evidence.get("tee_type")
        if tee_type is not None and (not isinstance(tee_type, str) or tee_type.lower() != "tdx"):
            continue
        instance_id = evidence.get("instance_id")
        e2e_pubkey = pubkeys.get(instance_id)
        if not e2e_pubkey:
            if instance_id:
                skipped_without_key.append(instance_id)
            continue
        eligible.append((evidence, e2e_pubkey))

    cpu_slots = asyncio.Semaphore(CPU_VERIFICATION_CONCURRENCY)

    async def verify_instance(evidence: dict[str, Any], e2e_pubkey: str) -> tuple[dict[str, Any], str]:
        async with cpu_slots:
            return await chutes_verify_instance(evidence, nonce, e2e_pubkey, measurements)

    results = await asyncio.gather(
        *(verify_instance(evidence, e2e_pubkey) for evidence, e2e_pubkey in eligible),
        return_exceptions=True,
    )
    verified = []
    errors = []
    gpu_jobs = []
    for (evidence, _), result in zip(eligible, results, strict=True):
        if isinstance(result, tuple):
            instance, expected_report_data = result
            verified.append(instance)
            if instance["gpu"]["gpu_arch"]:
                gpu_jobs.append((instance, evidence["gpu_evidence"], expected_report_data))
        elif isinstance(result, Exception):
            errors.append(str(result))
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
            failure="no_channel_binding",
            evidence=raw_http_bundle_evidence(
                raw_items,
                source_url=attestation_url,
            ),
        )
        return
    await chutes_apply_gpu_checks(gpu_jobs, gpu_deadline)
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
    # session layer can report each instance's own outcome.
    instance_gpu = {item["instance_id"]: item.get("gpu") for item in verified}
    gpus = [g for g in instance_gpu.values() if isinstance(g, dict)]
    # Per-instance measurement profile (stable hardware identity, no nonce) so a
    # single instance's session can bind its own CPU evidence.
    instance_measurements = {item["instance_id"]: item["measurement"] for item in verified}
    provider_claims = {
        "session_material_version": "chutes/1",
        "instance_tdx_reports": {item["instance_id"]: item.get("tdx_report") for item in verified},
        "instance_tdx_report_sha256": {item["instance_id"]: item.get("tdx_report_sha256") for item in verified},
        "instance_advisory_ids": {item["instance_id"]: item["advisory_ids"] for item in verified},
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

