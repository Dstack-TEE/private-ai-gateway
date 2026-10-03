"""ArmetAI provider verification: a per-model Intel TDX endpoint (not dstack).

The provider serves one model per attested TD, with NVIDIA confidential-
computing GPUs, and publishes a fresh, nonce-bound TDX quote at
``GET {origin}/v1/attestation/report``. This adapter:

1. fetches the report with a fresh 32-byte nonce and the requested model id;
2. verifies the quote to Intel's root with dcap-qvl (signature + collateral);
3. rejects a debug TD and a revoked platform TCB;
4. checks that the *verified* report_data commits to the nonce, the served TLS
   SPKI, the exact GPU evidence bytes and the model id (see
   ``expected_report_data``);
5. requires the verified TD measurements and launch configuration to match an
   operator pin (``accepted_subjects``);
6. requires the GPU evidence to verify with NVIDIA NRAS, through the signed
   ``secretvm.verify`` result, for the same nonce; and
7. returns the attested TLS SPKI as the ``tls_spki_sha256`` channel binding
   the gateway pins on every forwarded request.

Every step gates. Wire contract the provider implements is documented in
``docs/providers/armet-ai/verification.md``.
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import hmac
import json
import re
import secrets
import sys
from typing import Any

from .common import (
    emit,
    evidence_bundle,
    failed,
    provider_options,
    request_timeout_seconds,
    tdx_debug_enabled,
    verified_nras_gpu_claims,
    verifier_id_for,
)

PROVIDER = "armet-ai"
ATTESTATION_PATH = "/v1/attestation/report"
REPORT_VERSION = 2
# Domain separator for the report_data commitment. Changing it is a wire break.
REPORT_DATA_DOMAIN = b"private-ai-gateway/armet-ai/v2\x00"
SUBJECT_PREFIX = "tdx-measurement:sha256:"
ACCEPTED_SUBJECT_OPTION = "armet_ai_accepted_subject:"
BEARER_OPTION = "armet_ai_bearer_token"
# TCB statuses that fail closed. Other non-UpToDate statuses are recorded and
# surface as a refuted `tcb_up_to_date` claim, the same bar the other TDX
# adapters apply (see audit-criteria.md criterion 14).
REJECTED_TCB_STATUSES = frozenset({"revoked"})
# NVIDIA evidence for several GPUs is large; anything past this is refused.
MAX_GPU_PAYLOAD_BYTES = 2 * 1024 * 1024

# Pinned TD report fields, in subject-hash order: (claim name, dcap-qvl key,
# byte length). The five measurement registers identify the software; the rest
# pin how the host launched it (attributes, XSAVE features, config/owner IDs),
# so the same image launched with weaker settings does not match.
PINNED_FIELDS: tuple[tuple[str, str, int], ...] = (
    ("mrtd", "mr_td", 48),
    ("rtmr0", "rt_mr0", 48),
    ("rtmr1", "rt_mr1", 48),
    ("rtmr2", "rt_mr2", 48),
    ("rtmr3", "rt_mr3", 48),
    ("td_attributes", "td_attributes", 8),
    ("xfam", "xfam", 8),
    ("mr_config_id", "mr_config_id", 48),
    ("mr_owner", "mr_owner", 48),
    ("mr_owner_config", "mr_owner_config", 48),
)
# TDX module identity: recorded for platform provenance, not pinned, because a
# TDX module update is covered by the TCB status rather than by a new release.
MODULE_FIELDS: tuple[tuple[str, str, int], ...] = (
    ("mr_seam", "mr_seam", 48),
    ("tee_tcb_svn", "tee_tcb_svn", 16),
)

_HEX64 = re.compile(r"^[0-9a-f]{64}$")


# --------------------------------------------------------------------------
# Pure helpers (unit-tested directly)
# --------------------------------------------------------------------------


def expected_report_data(
    nonce_hex: str, spki_sha256_hex: str, gpu_payload_sha256: bytes, model_id: str
) -> bytes:
    """The 64-byte report_data the provider's TD must place in its quote.

    report_data[0:64] = ASCII(lowercase hex(SHA-256(DOMAIN || nonce(32B) || spki_sha256(32B)
                                                     || gpu_payload_sha256(32B)
                                                     || UTF8(model_id))))

    The 64 hex characters fill the whole field and carry the full 256-bit
    digest, so the commitment is as strong as the raw-digest encoding.
    """
    nonce = bytes.fromhex(nonce_hex)
    spki = bytes.fromhex(spki_sha256_hex)
    if len(nonce) != 32 or len(spki) != 32 or len(gpu_payload_sha256) != 32:
        raise ValueError("nonce, spki_sha256 and gpu_payload_sha256 must each be 32 bytes")
    digest = hashlib.sha256(
        REPORT_DATA_DOMAIN + nonce + spki + gpu_payload_sha256 + model_id.encode("utf-8")
    ).hexdigest()
    return digest.encode("ascii")


def td_report_fields(dcap_result: dict[str, Any]) -> dict[str, Any]:
    """The TD report body from a dcap-qvl VerifiedReport JSON (TDX 1.0 or 1.5)."""
    report = dcap_result.get("report") or {}
    if "TD10" in report:
        return report["TD10"] or {}
    if "TD15" in report:
        td15 = report["TD15"] or {}
        return td15.get("base") or td15
    raise ValueError("dcap-qvl report is not a TDX TD report")


def _read_fields(
    fields: dict[str, Any], spec: tuple[tuple[str, str, int], ...]
) -> dict[str, str]:
    out: dict[str, str] = {}
    for name, key, size in spec:
        value = fields.get(key)
        if not isinstance(value, str):
            raise ValueError(f"verified TD report is missing {key}")
        raw = bytes.fromhex(value)
        if len(raw) != size:
            raise ValueError(f"verified TD report {key} is {len(raw)} bytes, expected {size}")
        out[name] = raw.hex()
    return out


def pinned_fields(fields: dict[str, Any]) -> dict[str, str]:
    return _read_fields(fields, PINNED_FIELDS)


def module_fields(fields: dict[str, Any]) -> dict[str, str]:
    return _read_fields(fields, MODULE_FIELDS)


def measurement_subject(pinned: dict[str, str]) -> str:
    """Pin identifier: SHA-256 over the PINNED_FIELDS, raw bytes, in order."""
    material = b"".join(bytes.fromhex(pinned[name]) for name, _, _ in PINNED_FIELDS)
    return SUBJECT_PREFIX + hashlib.sha256(material).hexdigest()


def accepted_subjects(options: dict[str, str]) -> set[str]:
    return {
        key[len(ACCEPTED_SUBJECT_OPTION):].lower()
        for key, value in options.items()
        if key.startswith(ACCEPTED_SUBJECT_OPTION) and value == "true"
    }


# --------------------------------------------------------------------------
# I/O seams (replaced by the hermetic soundness tests)
# --------------------------------------------------------------------------


def fetch_report(
    url: str, params: dict[str, str], headers: dict[str, str], timeout: int
) -> tuple[bytes, str]:
    """GET the attestation report; returns (exact body bytes, content type)."""
    import requests

    response = requests.get(url, params=params, headers=headers, timeout=timeout)
    response.raise_for_status()
    return response.content, str(response.headers.get("content-type") or "application/json")


async def verify_quote(quote_bytes: bytes) -> dict[str, Any]:
    """Verify a TDX quote to Intel's root with dcap-qvl; returns the report JSON."""
    import dcap_qvl

    verified = await dcap_qvl.get_collateral_and_verify(quote_bytes)
    return json.loads(verified.to_json())


def check_gpu_attestation(gpu_text: str) -> Any:
    """NVIDIA NRAS verification with signed-result checking (secretvm-verify)."""
    from secretvm.verify import check_nvidia_gpu_attestation

    with contextlib.redirect_stdout(sys.stderr):
        return check_nvidia_gpu_attestation(gpu_text)


# --------------------------------------------------------------------------
# Bridge entry point
# --------------------------------------------------------------------------


async def verify_armet_ai(request: dict[str, Any]) -> None:
    verifier_id = verifier_id_for(PROVIDER)

    def reject(reason: str, **extra: Any) -> None:
        failed(PROVIDER, reason, verifier_id=verifier_id, **extra)

    options = provider_options(request)
    subjects = accepted_subjects(options)
    if not subjects:
        reject("armet-ai upstream has no accepted_subjects; refusing an unpinned TD")
        return

    raw_origin = request.get("url_origin")
    model_id = request.get("model_id")
    if not raw_origin or not model_id:
        reject("armet-ai request is missing url_origin or model_id")
        return
    origin = str(raw_origin).rstrip("/")
    if not origin.startswith("https://"):
        reject("armet-ai upstream origin must be https")
        return
    timeout = request_timeout_seconds(request, 30)
    bearer = (options.get(BEARER_OPTION) or "").strip()

    nonce = secrets.token_hex(32)
    url = origin + ATTESTATION_PATH
    params = {"nonce": nonce, "model": str(model_id), "version": str(REPORT_VERSION)}
    headers = {"Authorization": f"Bearer {bearer}"} if bearer else {}

    try:
        body, content_type = await asyncio.to_thread(fetch_report, url, params, headers, timeout)
    except Exception as exc:  # noqa: BLE001
        reject(f"failed to fetch armet-ai attestation report: {exc}")
        return

    # Evidence is the exact response bytes the checks below consumed.
    evidence = evidence_bundle(body, url, content_type)

    try:
        report = json.loads(body)
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        reject(f"armet-ai attestation report is not JSON: {exc}", evidence=evidence)
        return
    if not isinstance(report, dict):
        reject("armet-ai attestation report must be a JSON object", evidence=evidence)
        return

    if report.get("version") != REPORT_VERSION:
        reject(f"armet-ai report version must be {REPORT_VERSION}", evidence=evidence)
        return
    if str(report.get("nonce") or "").lower() != nonce:
        reject("armet-ai report nonce did not match the request nonce", evidence=evidence)
        return
    if report.get("model_id") != model_id:
        reject(
            f"armet-ai report model_id {report.get('model_id')!r} does not match "
            f"requested model {model_id!r}",
            evidence=evidence,
        )
        return
    spki = str(report.get("tls_spki_sha256") or "").lower()
    if not _HEX64.match(spki):
        reject("armet-ai report tls_spki_sha256 must be 64 lowercase hex", evidence=evidence)
        return
    quote_hex = report.get("quote")
    try:
        quote_bytes = bytes.fromhex(str(quote_hex or ""))
    except ValueError:
        reject("armet-ai report quote is not hex", evidence=evidence)
        return
    if not quote_bytes:
        reject("armet-ai report is missing quote", evidence=evidence)
        return

    # GPU evidence is mandatory. It travels as a JSON *string* so its hash is
    # over unambiguous bytes; the TD commits to that hash in report_data.
    gpu_text = report.get("nvidia_payload")
    if not isinstance(gpu_text, str) or not gpu_text:
        reject("armet-ai report is missing nvidia_payload GPU evidence", evidence=evidence)
        return
    gpu_bytes = gpu_text.encode("utf-8")
    if len(gpu_bytes) > MAX_GPU_PAYLOAD_BYTES:
        reject("armet-ai nvidia_payload exceeds the size limit", evidence=evidence)
        return
    try:
        gpu_payload = json.loads(gpu_text)
    except json.JSONDecodeError as exc:
        reject(f"armet-ai nvidia_payload is not JSON: {exc}", evidence=evidence)
        return
    if not isinstance(gpu_payload, dict) or not gpu_payload.get("evidence_list"):
        reject("armet-ai nvidia_payload carries no GPU evidence_list", evidence=evidence)
        return
    gpu_nonce = str(gpu_payload.get("nonce") or "").lower()
    if not hmac.compare_digest(gpu_nonce, nonce):
        reject("armet-ai GPU evidence nonce did not match the request nonce", evidence=evidence)
        return

    try:
        if tdx_debug_enabled(quote_bytes):
            reject(
                "armet-ai TDX quote is in debug mode (TD_ATTRIBUTES TUD set)",
                evidence=evidence,
            )
            return
    except ValueError as exc:
        reject(f"armet-ai quote is not a TDX quote: {exc}", evidence=evidence)
        return

    # 1. Quote signature and collateral, to the Intel root.
    try:
        with contextlib.redirect_stdout(sys.stderr):
            dcap_result = await verify_quote(quote_bytes)
    except Exception as exc:  # noqa: BLE001
        reject(f"armet-ai quote verification failed: {exc}", evidence=evidence)
        return
    tcb_status = dcap_result.get("status")
    if not tcb_status:
        reject("dcap-qvl returned no TCB status", evidence=evidence)
        return
    if str(tcb_status).lower() in REJECTED_TCB_STATUSES:
        reject(f"armet-ai platform TCB status is {tcb_status}", evidence=evidence)
        return

    try:
        fields = td_report_fields(dcap_result)
        pinned = pinned_fields(fields)
        module = module_fields(fields)
        verified_report_data = bytes.fromhex(str(fields.get("report_data") or ""))
    except ValueError as exc:
        reject(f"armet-ai verified report is malformed: {exc}", evidence=evidence)
        return

    # 2. report_data binding, read from the VERIFIED report (not raw bytes).
    expected = expected_report_data(
        nonce, spki, hashlib.sha256(gpu_bytes).digest(), str(model_id)
    )
    if not hmac.compare_digest(verified_report_data, expected):
        reject(
            "armet-ai report_data does not commit to this nonce, TLS SPKI, GPU evidence "
            "and model",
            evidence=evidence,
        )
        return

    # 3. Measurement pin. The failure reason names the observed subject so an
    #    operator can review a new release and then pin it deliberately.
    subject = measurement_subject(pinned)
    if subject not in subjects:
        reject(
            f"armet-ai measurement {subject} is not in accepted_subjects",
            evidence=evidence,
            observed_measurements=pinned,
        )
        return

    # 4. GPU: mandatory, NRAS-signed, same nonce, every GPU.
    try:
        gpu_result = await asyncio.to_thread(check_gpu_attestation, gpu_text)
    except Exception as exc:  # noqa: BLE001
        reject(f"armet-ai GPU attestation failed: {exc}", evidence=evidence)
        return
    if gpu_result.valid is not True:
        reason = "; ".join(str(error) for error in gpu_result.errors) or "unknown failure"
        reject(f"armet-ai GPU attestation failed: {reason}", evidence=evidence)
        return
    try:
        gpu_models, gpu_count = verified_nras_gpu_claims(gpu_result, nonce, "armet-ai")
    except ValueError as exc:
        reject(str(exc), evidence=evidence)
        return

    emit(
        {
            "result": "verified",
            "verifier_id": verifier_id,
            "attested_scope": "model",
            "evidence": evidence,
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": raw_origin,
                    "spki_sha256": spki,
                }
            ],
            "provider_claims": {
                "trust_boundary": "armet-ai-td",
                "evidence_scope": "model_instance",
                "canonical_model_id": model_id,
                "report_version": REPORT_VERSION,
                "report_data_nonce_matched": True,
                "tls_spki_from_report_data": True,
                "model_id_from_report_data": True,
                "gpu_evidence_from_report_data": True,
                "tdx_debug_mode": False,
                "accepted_subject": subject,
                "measurements": pinned,
                "tdx_module": module,
                "tcb_status": tcb_status,
                "advisory_ids": dcap_result.get("advisory_ids") or [],
                "gpu_verified": True,
                "gpu_models": gpu_models,
                "gpu_count": gpu_count,
            },
        }
    )
