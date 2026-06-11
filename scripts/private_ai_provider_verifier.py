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
import urllib.parse
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


def sha256_bytes_prefixed(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def json_bytes(value: Any) -> bytes:
    return json.dumps(value, separators=(",", ":"), default=str).encode("utf-8")


def data_uri(data: bytes, content_type: str) -> str:
    return f"data:{content_type};base64,{base64.b64encode(data).decode('ascii')}"


def evidence_bundle(
    data: bytes,
    source_url: str | None = None,
    content_type: str = "application/octet-stream",
) -> dict[str, Any]:
    bundle = {
        "digest": sha256_bytes_prefixed(data),
        "data": data_uri(data, content_type),
    }
    if source_url:
        bundle["source_url"] = source_url
    return bundle


def json_evidence_bundle(value: Any, source_url: str | None = None) -> dict[str, Any]:
    return evidence_bundle(json_bytes(value), source_url, "application/json")


def raw_http_item(name: str, source_url: str, content_type: str, body: bytes) -> dict[str, Any]:
    return {
        "name": name,
        "source_url": source_url,
        "sha256": sha256_bytes_prefixed(body),
        "content_type": content_type,
        "body": body,
    }


def response_content_type(response: Any) -> str:
    return str(response.headers.get("content-type") or "application/octet-stream")


def raw_http_bundle_evidence(
    items: list[dict[str, Any]],
    *,
    source_url: str | None = None,
) -> dict[str, Any]:
    boundary = "aci-evidence-" + hashlib.sha256(
        b"".join(item["body"] for item in items)
    ).hexdigest()[:24]
    chunks: list[bytes] = []
    for item in items:
        headers = [
            f"--{boundary}",
            f"Content-Type: {item['content_type']}",
            f"Content-Location: {item['source_url']}",
            f"Content-ID: <{item['name']}>",
            f"Digest: sha-256={base64.b64encode(hashlib.sha256(item['body']).digest()).decode('ascii')}",
            "",
            "",
        ]
        chunks.append("\r\n".join(headers).encode("utf-8"))
        chunks.append(item["body"])
        chunks.append(b"\r\n")
    chunks.append(f"--{boundary}--\r\n".encode("utf-8"))
    return evidence_bundle(
        b"".join(chunks),
        source_url,
        f"multipart/mixed;boundary={boundary}",
    )


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


def tdx_debug_enabled(quote_bytes: bytes) -> bool:
    """True if a TDX v4 quote's TD runs in debug/untrusted mode.

    TD_ATTRIBUTES is 8 bytes at quote offset 168 (header 48 + body offset 120).
    Byte 0 is the little-endian TUD (TD Under Debug) group; DEBUG is bit 0. Per
    dcap-qvl `validate_td10`, any TUD bit set means the TD is untrusted (CPU state
    and private memory are accessible to the host), so we reject a non-zero TUD
    byte. (The previous big-endian `int(hex) & 1` read byte 7 and missed it.)
    """
    td_attributes = quote_bytes[48 + 120 : 48 + 128]
    if len(td_attributes) != 8:
        raise ValueError(f"invalid TDX td_attributes length: {len(td_attributes)}")
    return td_attributes[0] != 0


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
    # Verify with Tinfoil's official Python verifier. It performs the full reference
    # chain that our previous hand-rolled SEV-SNP path skipped: the AMD report
    # signature + VCEK->ASK->ARK certificate chain and policy/TCB (or DCAP for TDX),
    # Sigstore-verified code-measurement provenance bound to the GitHub repo and
    # workflow identity, and the TLS public-key binding. The verified TLS key
    # fingerprint (report_data[0:32]) is returned as the enforceable channel binding.
    from urllib.parse import urlparse

    from tinfoil import SecureClient

    provider = "tinfoil"
    url_origin = request.get("url_origin") or "https://inference.tinfoil.sh"
    parsed = urlparse(url_origin if "://" in url_origin else f"https://{url_origin}")
    enclave_host = parsed.netloc or parsed.path
    attestation_url = f"{url_origin.rstrip('/')}/.well-known/tinfoil-attestation"
    options = request.get("provider_options") or {}
    repo = options.get("tinfoil_repo") or "tinfoilsh/confidential-model-router"

    def _verify():
        client = SecureClient(enclave=enclave_host, repo=repo)
        client.verify()
        return client.get_verification_document()

    try:
        with contextlib.redirect_stdout(sys.stderr):
            doc = await asyncio.to_thread(_verify)
    except Exception as exc:
        failed(provider, f"Tinfoil verification failed: {exc}")
        return

    steps = {
        name: {
            "status": getattr(state, "status", None),
            "error": getattr(state, "error", None),
        }
        for name, state in (doc.steps or {}).items()
    }
    evidence_doc = {
        "config_repo": doc.config_repo,
        "enclave_host": doc.enclave_host,
        "release_digest": doc.release_digest,
        "code_fingerprint": doc.code_fingerprint,
        "enclave_fingerprint": doc.enclave_fingerprint,
        "tls_public_key_fp": doc.tls_public_key,
        "hpke_public_key": doc.hpke_public_key,
        "security_verified": doc.security_verified,
        "steps": steps,
    }
    evidence = json_evidence_bundle(evidence_doc, attestation_url)

    if not doc.security_verified:
        failed(provider, "Tinfoil attestation not verified", evidence=evidence)
        return
    spki = doc.tls_public_key
    if not spki:
        failed(
            provider,
            "Tinfoil verification returned no TLS public key fingerprint",
            evidence=evidence,
        )
        return

    used_router = bool(getattr(doc, "selected_router_endpoint", "")) or repo.endswith(
        "confidential-model-router"
    )
    emit(
        {
            "result": "verified",
            "verifier_id": "tinfoil-verifier/v1",
            "evidence": evidence,
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": request.get("url_origin"),
                    "spki_sha256": spki,
                }
            ],
            "provider_claims": {
                "trust_boundary": "router" if used_router else "model",
                "evidence_scope": "router" if used_router else "model",
                "canonical_model_id": request["model_id"],
                "used_router": used_router,
                "config_repo": doc.config_repo,
                "release_digest": doc.release_digest,
                "code_fingerprint": doc.code_fingerprint,
                "tls_spki_from_report_data": True,
                "verification_steps": {k: v["status"] for k, v in steps.items()},
            },
        }
    )


async def verify_nearai(request: dict[str, Any]) -> None:
    from confidential_verifier.providers.nearai import NearaiProvider
    from confidential_verifier.verifiers.nearai import NearAICloudVerifier

    provider = "near-ai"
    verifier_id = verifier_id_for(provider)
    # Fail loudly on bridge/verifier contract drift instead of letting a missing
    # method surface as a cryptic AttributeError mid-verification.
    if not hasattr(NearAICloudVerifier, "verify_gateway_component"):
        failed(
            provider,
            "verifier contract drift: NearAICloudVerifier is missing "
            "verify_gateway_component; the confidential_verifier package is out of sync "
            "with this bridge (see scripts/confidential_verifier/VENDOR.md)",
            verifier_id=verifier_id,
        )
        return
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
    attestation_url = "https://cloud-api.near.ai/v1/attestation/report"
    if not gateway:
        failed(
            provider,
            "NEAR AI report did not include gateway_attestation",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return
    spki = gateway.get("tls_cert_fingerprint")
    if not spki:
        failed(
            provider,
            "NEAR AI report did not include TLS SPKI binding",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return
    if not gateway_result.get("is_valid"):
        failed(
            provider,
            "; ".join(gateway_result.get("errors") or [])
            or "NEAR AI gateway verification failed",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return

    raw_report = report.raw or {}
    model_attestations = raw_report.get("model_attestations") or []
    if not isinstance(model_attestations, list) or not model_attestations:
        failed(
            provider,
            "NEAR AI model-scoped report did not include model_attestations",
            evidence=json_evidence_bundle(report_obj, attestation_url),
            verifier_id=verifier_id,
        )
        return

    for index, item in enumerate(model_attestations):
        if not isinstance(item, dict) or not item.get("intel_quote"):
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] did not include intel_quote",
                evidence=json_evidence_bundle(report_obj, attestation_url),
                verifier_id=verifier_id,
            )
            return
        item_nonce = item.get("request_nonce")
        if item_nonce is not None and str(item_nonce).lower() != str(report.request_nonce).lower():
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] nonce did not match request nonce",
                evidence=json_evidence_bundle(report_obj, attestation_url),
                verifier_id=verifier_id,
            )
            return
        # The gateway does not re-verify these nested model quotes through the
        # dstack verifier, so check the TD debug bit here: a debug-mode model TD
        # would otherwise be accepted on the gateway's word alone.
        try:
            if tdx_debug_enabled(bytes.fromhex(item["intel_quote"])):
                failed(
                    provider,
                    f"NEAR AI model_attestations[{index}] TDX quote is in debug mode",
                    evidence=json_evidence_bundle(report_obj, attestation_url),
                    verifier_id=verifier_id,
                )
                return
        except ValueError as exc:
            failed(
                provider,
                f"NEAR AI model_attestations[{index}] intel_quote is not valid TDX bytes: {exc}",
                evidence=json_evidence_bundle(report_obj, attestation_url),
                verifier_id=verifier_id,
            )
            return

    model_attestations_sha256 = sha256_json_prefixed(model_attestations)
    provider_claims = {
        "trust_boundary": "near-ai-gateway",
        "gateway_verified": True,
        "gateway_tls_spki_sha256": spki,
        "model_evidence_present": True,
        "model_attestation_count": len(model_attestations),
        "model_attestations_sha256": model_attestations_sha256,
        "nested_model_attestations_checked_by_gateway": False,
        "canonical_model_id": report.model_id,
    }
    if all(isinstance(item, dict) and item.get("request_nonce") for item in model_attestations):
        provider_claims["model_attestations_nonce_matched"] = True

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "evidence": json_evidence_bundle(report_obj, attestation_url),
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


def _phala_direct_compose_hash_ok(info: dict[str, Any]) -> tuple[bool, str]:
    """Verify SHA256(app_compose) == reported compose_hash, mirroring the dstack
    verifiers. Returns (ok, reason)."""
    tcb_info = info.get("tcb_info") or {}
    if isinstance(tcb_info, str):
        try:
            tcb_info = json.loads(tcb_info)
        except json.JSONDecodeError:
            tcb_info = {}
    app_compose = tcb_info.get("app_compose")
    reported = info.get("compose_hash")
    if not app_compose or not reported:
        return False, "PhalaDirect report is missing app_compose or compose_hash"
    calculated = hashlib.sha256(app_compose.encode("utf-8")).hexdigest()
    if calculated.lower() != str(reported).lower():
        return False, "PhalaDirect compose hash mismatch"
    return True, ""


async def verify_phala_direct(request: dict[str, Any]) -> None:
    """Verify a Phala dstack-vllm-proxy attestation endpoint reached directly.

    Mirrors the NEAR AI path: fetch the per-model attestation report (version 2),
    verify the dstack TDX quote, the report_data binding (signing address +
    request nonce + custom-domain TLS SPKI), the compose hash, and the GPU
    evidence; then return the attested TLS SPKI as the enforceable channel
    binding so the forwarding backend pins the connection.
    """
    from confidential_verifier.verifiers.dstack import DstackVerifier, verify_report_data
    from confidential_verifier.verifiers.nearai import _tdx_report_data_hex
    from confidential_verifier.verifiers.nvidia import NvidiaGpuVerifier
    from dstack_os_image import resolve_os_image

    provider = "phala-direct"
    verifier_id = verifier_id_for(provider)
    options = provider_options(request)

    raw_origin = request.get("url_origin")
    if not raw_origin:
        failed(provider, "PhalaDirect upstream is missing url_origin", verifier_id=verifier_id)
        return
    url_origin = raw_origin.rstrip("/")
    bearer = (options.get("phala_direct_bearer_token") or "").strip()
    try:
        timeout = int(request.get("timeout_seconds") or 30)
    except (TypeError, ValueError):
        timeout = 30

    nonce = secrets.token_hex(32)
    attestation_url = f"{url_origin}/v1/attestation/report"
    query = urllib.parse.urlencode(
        {"signing_algo": "ecdsa", "nonce": nonce, "version": "2"}
    )
    http_request = urllib.request.Request(f"{attestation_url}?{query}")
    if bearer:
        http_request.add_header("Authorization", f"Bearer {bearer}")
    try:
        with urllib.request.urlopen(http_request, timeout=timeout) as response:
            body = response.read()
        report = json.loads(body.decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        failed(
            provider,
            f"failed to fetch PhalaDirect attestation report: {exc}",
            verifier_id=verifier_id,
        )
        return

    evidence = json_evidence_bundle(report, attestation_url)

    # Per-model endpoints return a single attestation; prefer all_attestations[0].
    attestation = report
    all_attestations = report.get("all_attestations")
    if isinstance(all_attestations, list) and all_attestations and isinstance(
        all_attestations[0], dict
    ):
        attestation = all_attestations[0]

    intel_quote = attestation.get("intel_quote") or attestation.get("quote")
    signing_address = attestation.get("signing_address")
    tls_cert_fingerprint = attestation.get("tls_cert_fingerprint")
    event_log = attestation.get("event_log") or ""
    vm_config = attestation.get("vm_config") or ""
    info = attestation.get("info") or {}
    report_nonce = attestation.get("request_nonce")
    nvidia_payload = attestation.get("nvidia_payload")

    if not intel_quote:
        failed(provider, "PhalaDirect report missing intel_quote", evidence=evidence, verifier_id=verifier_id)
        return
    if not signing_address:
        failed(provider, "PhalaDirect report missing signing_address", evidence=evidence, verifier_id=verifier_id)
        return
    if not tls_cert_fingerprint:
        failed(
            provider,
            "PhalaDirect report did not include tls_cert_fingerprint; the proxy "
            "must serve attestation version 2 (custom-domain TLS SPKI binding)",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return
    if report_nonce is not None and str(report_nonce).lower() != nonce.lower():
        failed(provider, "PhalaDirect report nonce did not match request nonce", evidence=evidence, verifier_id=verifier_id)
        return

    # Reject a TD running in debug/untrusted mode: its CPU state and private
    # memory are accessible to the host, so the TEE guarantee does not hold.
    try:
        if tdx_debug_enabled(bytes.fromhex(intel_quote)):
            failed(provider, "PhalaDirect TDX quote is in debug mode (TD_ATTRIBUTES TUD set)", evidence=evidence, verifier_id=verifier_id)
            return
    except ValueError as exc:
        failed(provider, f"PhalaDirect intel_quote is not valid TDX quote bytes: {exc}", evidence=evidence, verifier_id=verifier_id)
        return

    if isinstance(vm_config, (dict, list)):
        vm_config = json.dumps(vm_config)

    dstack_verifier_url = os.getenv("DSTACK_VERIFIER_URL", "http://localhost:8080")

    # 1. dstack TDX quote / TCB / OS-image verification. Wrap only the noisy
    # verifier call in the stdout redirect; emit()/failed() must reach real stdout
    # (the Rust caller reads the result JSON from stdout).
    with contextlib.redirect_stdout(sys.stderr):
        dstack_result = await asyncio.to_thread(
            DstackVerifier(dstack_verifier_url).verify, intel_quote, event_log, vm_config
        )
    if not dstack_result.get("is_valid"):
        failed(
            provider,
            f"dstack quote verification failed: {dstack_result.get('reason', 'unknown')}",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # 2. Compose hash: SHA256(app_compose) == reported compose_hash.
    compose_ok, compose_reason = _phala_direct_compose_hash_ok(info)
    if not compose_ok:
        failed(provider, compose_reason, evidence=evidence, verifier_id=verifier_id)
        return

    # 3. report_data binding: nonce + signing address + TLS SPKI. Parse
    # report_data from the quote bytes the dstack verifier just proved authentic
    # (it does not surface report_data itself).
    report_data_hex = dstack_result.get("report_data") or _tdx_report_data_hex(intel_quote)
    if not report_data_hex:
        failed(
            provider,
            "could not obtain report_data from PhalaDirect quote; cannot verify nonce/address/TLS binding",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return
    binding = verify_report_data(
        report_data_hex,
        signing_address,
        nonce,
        tls_cert_fingerprint=tls_cert_fingerprint,
    )
    if not binding.get("valid"):
        failed(
            provider,
            f"PhalaDirect report_data binding failed: {binding.get('error') or 'mismatch'}",
            evidence=evidence,
            verifier_id=verifier_id,
        )
        return

    # 4. GPU (NVIDIA confidential computing) evidence — SUPPLEMENTAL, never a gate.
    # The CPU TEE quote + report_data binding + compose integrity above are the
    # trust gate. A standalone gateway-side NRAS check only proves a CC-capable
    # GPU exists for a nonce; it does not prove that GPU is bound to this CPU TEE
    # or serving this request (that is the measured serving software's job, inside
    # the quote). So we record the GPU outcome as metadata and do not fail on it.
    gpu_evidence_present = False
    gpu_evidence_nonce_matched: bool | None = None
    gpu_attested = False
    gpu_arch = None
    payload = nvidia_payload
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except json.JSONDecodeError:
            payload = None
    if isinstance(payload, dict) and payload.get("evidence_list"):
        gpu_evidence_present = True
        gpu_arch = payload.get("arch")
        gpu_nonce = payload.get("nonce")
        gpu_evidence_nonce_matched = bool(gpu_nonce) and str(gpu_nonce).lower() == nonce.lower()
        try:
            with contextlib.redirect_stdout(sys.stderr):
                gpu_result = await NvidiaGpuVerifier().verify(payload)
            gpu_attested = bool(gpu_result.model_verified) and bool(gpu_evidence_nonce_matched)
        except Exception:  # noqa: BLE001 - supplemental; a GPU error is never fatal
            gpu_attested = False

    # Surface the granular TDX TCB status (e.g. "UpToDate", "OutOfDate") so the
    # session layer can populate a tri-state `tcb_up_to_date` claim instead of
    # only seeing the dstack verifier's overall is_valid.
    dstack_details = dstack_result.get("details") if isinstance(dstack_result, dict) else None
    tcb_status = dstack_details.get("tcb_status") if isinstance(dstack_details, dict) else None

    # OS-image provenance (production-vs-dev decision).
    #
    # The attested os_image_hash (proved by the dstack verifier — is_valid implies
    # os_image_hash_verified) is SHA256(sha256sum.txt) of dstack's published OS
    # image, and that manifest pins SHA256(metadata.json) — so the image's `is_dev`
    # flag is cryptographically bound to the attested hash. resolve_os_image()
    # re-downloads the published image, re-verifies that binding, and reads is_dev
    # (known fleet images are seeded, so the common case is offline). A dev image
    # (dstack-nvidia-dev-*, SSH/serial-console enabled) is NOT a production OS.
    #
    # This is recorded metadata, not a gate (mirrors how GPU evidence is handled):
    # the deployed fleet currently runs dev images, and gating here would reject
    # them. The session layer decides policy. None means "could not resolve"
    # (unknown hash + offline/unreachable) — never silently treated as production.
    app_info = dstack_details.get("app_info") if isinstance(dstack_details, dict) else None
    os_image_hash = app_info.get("os_image_hash") if isinstance(app_info, dict) else None
    if not os_image_hash and isinstance(dstack_details, dict):
        os_image_hash = dstack_details.get("os_image_hash")

    os_image = resolve_os_image(os_image_hash) if os_image_hash else None
    if os_image is not None:
        os_image_is_dev = bool(os_image.get("is_dev"))
        os_image_version = os_image.get("version")
        production_os_image = not os_image_is_dev
    else:
        os_image_is_dev = None
        os_image_version = None
        production_os_image = None

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "evidence": evidence,
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": raw_origin,
                    "spki_sha256": tls_cert_fingerprint,
                }
            ],
            "provider_claims": {
                "trust_boundary": "phala-dstack-cvm",
                "evidence_scope": "model_instance",
                "canonical_model_id": request["model_id"],
                "attestation_version": 2,
                "tls_spki_from_report_data": True,
                "signing_address": signing_address,
                "report_data_nonce_matched": True,
                "compose_hash_verified": True,
                "tdx_debug_mode": False,
                # OS-image provenance, resolved from dstack's published image and
                # bound to the attested os_image_hash. production_os_image is the
                # prod-vs-dev decision (None only if the hash could not be resolved).
                "os_image_hash": os_image_hash,
                "os_image_version": os_image_version,
                "os_image_is_dev": os_image_is_dev,
                "production_os_image": production_os_image,
                # GPU is supplemental metadata, not part of the trust gate.
                "gpu_verified": gpu_attested,
                "gpu_evidence_present": gpu_evidence_present,
                "gpu_evidence_nonce_matched": gpu_evidence_nonce_matched,
                "gpu_arch": gpu_arch,
                "tcb_status": tcb_status,
            },
        }
    )


async def verify_chutes(request: dict[str, Any]) -> None:
    provider = "chutes"
    options = provider_options(request)
    api_key = (options.get("chutes_api_key") or "").strip()
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
    headers = chutes_headers(api_key)
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
    provider_claims = {
        "trust_boundary": "model_instance",
        "evidence_scope": "model_instance",
        "chute_id": chute_id,
        "canonical_model_id": request["model_id"],
        "verified_instance_count": len(verified),
        "verified_instance_ids": [item["instance_id"] for item in verified],
        "verified_public_key_sha256": [item["public_key_sha256"] for item in verified],
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


async def main() -> None:
    request = json.loads(sys.stdin.read())
    # Default to the vendored `confidential_verifier` package next to this script
    # (see scripts/confidential_verifier/VENDOR.md). An external private-ai-verifier
    # checkout can override via PRIVATE_AI_VERIFIER_DIR, which is inserted ahead of
    # the vendored copy on sys.path.
    script_dir = os.path.dirname(os.path.abspath(__file__))
    if script_dir not in sys.path:
        sys.path.append(script_dir)
    private_ai_dir = os.environ.get("PRIVATE_AI_VERIFIER_DIR")
    if private_ai_dir:
        sys.path.insert(0, private_ai_dir)
    provider = request.get("provider")
    try:
        if provider == "tinfoil":
            await verify_tinfoil(request)
        elif provider == "near-ai":
            await verify_nearai(request)
        elif provider == "phala-direct":
            await verify_phala_direct(request)
        elif provider == "chutes":
            await verify_chutes(request)
        else:
            failed(str(provider), f"unsupported provider: {provider!r}")
    except Exception as exc:
        failed(str(provider), str(exc))


if __name__ == "__main__":
    asyncio.run(main())
