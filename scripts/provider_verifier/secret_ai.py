"""SecretAI support embedded in the private AI provider verifier.

The pinned ``secretvm-verify`` package performs TDX, strict AMD SEV-SNP, and
NVIDIA NRAS verification; this module adds gateway origin, optional workload
pinning, and TLS binding policy.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import hashlib
import hmac
import json
import re
import socket
import ssl
import struct
import sys
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit

from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from cryptography.x509 import load_der_x509_certificate
import requests
import secretvm.verify.workload as workload
from secretvm.verify import (
    check_nvidia_gpu_attestation,
    check_sev_cpu_attestation,
    check_tdx_cpu_attestation,
)

from .common import (
    emit,
    failed,
    provider_options,
    raw_http_bundle_evidence,
    raw_http_item,
    request_timeout_seconds,
    verifier_id_for,
)

_MAX_EVIDENCE_BYTES = {
    "cpu": 64 * 1024,
    "gpu": 2 * 1024 * 1024,
    "docker-compose": 2 * 1024 * 1024,
}
# Only registry entries built for SecretVM production environments are eligible.
_PRODUCTION_ENVIRONMENTS = {"prod", "gpu_prod"}
_SEV_VCPU_PROFILES = {
    "small": 1,
    "medium": 2,
    "large": 4,
    "2xlarge": 8,
    "4xlarge": 16,
}
_HEX_32_RE = re.compile(r"^[0-9a-f]{64}$")
_HEX_48_RE = re.compile(r"^[0-9a-f]{96}$")
_HEX_64_RE = re.compile(r"^[0-9a-f]{128}$")
_ACCEPTED_WORKLOAD_PREFIX = "secret_ai_accepted_workload_id:"
_SEV_TCB_FIELDS = ("boot_loader", "tee", "snp", "microcode")
# Reviewed against the live JEDI SEV-SNP deployment on 2026-07-21.
_MINIMUM_SEV_TCB = {"boot_loader": 10, "tee": 0, "snp": 23, "microcode": 88}
_SEV_MIGRATE_MA = 1 << 18


@dataclass(frozen=True)
class _Endpoint:
    origin: str
    host: str
    port: int


@dataclass(frozen=True)
class _PinnedWorkload:
    cpu_type: str
    environment: str
    template_name: str
    artifacts_version: str
    compose_sha256: str

    @property
    def workload_id(self) -> str:
        cpu_type = self.cpu_type.lower()
        return (
            f"secretvm:{cpu_type}:{self.environment}:{self.template_name}:"
            f"{self.artifacts_version}:sha256:{self.compose_sha256}"
        )


def _parse_origin(value: Any) -> _Endpoint:
    if not isinstance(value, str) or not value:
        raise ValueError("SecretAI upstream is missing url_origin")
    parsed = urlsplit(value)
    if parsed.scheme != "https":
        raise ValueError("SecretAI url_origin must use https://")
    if parsed.username is not None or parsed.password is not None:
        raise ValueError("SecretAI url_origin must not contain userinfo")
    if parsed.hostname is None:
        raise ValueError("SecretAI url_origin must include a host")
    if parsed.path not in ("", "/"):
        raise ValueError("SecretAI url_origin must not include a path")
    if parsed.query or parsed.fragment:
        raise ValueError("SecretAI url_origin must not include a query or fragment")
    try:
        port = parsed.port or 443
    except ValueError as exc:
        raise ValueError("SecretAI url_origin has an invalid port") from exc
    origin = value[:-1] if parsed.path == "/" else value
    return _Endpoint(origin=origin, host=parsed.hostname, port=port)


def _fetch_evidence(endpoint: _Endpoint, name: str, timeout: int) -> tuple[bytes, str]:
    url = f"{endpoint.origin}/{name}"
    limit = _MAX_EVIDENCE_BYTES[name]
    response = requests.get(
        url,
        headers={"Accept": "text/plain"},
        timeout=timeout,
        verify=True,
        allow_redirects=False,
        stream=True,
    )
    with response:
        if response.status_code != 200:
            raise ValueError(
                f"SecretAI /{name} returned HTTP {response.status_code}, expected 200"
            )
        content_type = str(response.headers.get("content-type") or "")
        media_type = content_type.split(";", 1)[0].strip().lower()
        if media_type != "text/plain":
            raise ValueError(
                f"SecretAI /{name} returned {content_type!r}, expected text/plain"
            )
        content_length = response.headers.get("content-length")
        if content_length is not None:
            try:
                declared_length = int(content_length)
            except ValueError as exc:
                raise ValueError(
                    f"SecretAI /{name} returned an invalid Content-Length"
                ) from exc
            if declared_length < 0 or declared_length > limit:
                raise ValueError(f"SecretAI /{name} response exceeds {limit} bytes")

        chunks: list[bytes] = []
        length = 0
        for chunk in response.iter_content(chunk_size=64 * 1024):
            if not chunk:
                continue
            length += len(chunk)
            if length > limit:
                raise ValueError(f"SecretAI /{name} response exceeds {limit} bytes")
            chunks.append(chunk)
        return b"".join(chunks), content_type


def _tls_spki_sha256(endpoint: _Endpoint) -> str:
    context = ssl.create_default_context()
    with socket.create_connection((endpoint.host, endpoint.port), timeout=10) as sock:
        with context.wrap_socket(sock, server_hostname=endpoint.host) as tls_sock:
            cert_der = tls_sock.getpeercert(binary_form=True)
    if not cert_der:
        raise ssl.SSLError("SecretAI TLS endpoint returned no certificate")
    cert = load_der_x509_certificate(cert_der)
    spki = cert.public_key().public_bytes(Encoding.DER, PublicFormat.SubjectPublicKeyInfo)
    return hashlib.sha256(spki).hexdigest()


def _check_cpu_attestation(cpu_text: str) -> Any:
    text = cpu_text.strip()
    quote_type = None
    if re.fullmatch(r"[0-9a-fA-F]+", text) and len(text) % 2 == 0:
        raw = bytes.fromhex(text)
        if len(raw) >= 8:
            version, _, tee_type = struct.unpack_from("<HHI", raw)
            if version == 4 and tee_type == 0x81:
                quote_type = "TDX"
    if quote_type is None:
        try:
            raw = base64.b64decode(text, validate=True)
        except (ValueError, base64.binascii.Error):
            raw = b""
        if len(raw) >= 0x038:
            version = struct.unpack_from("<I", raw)[0]
            signature_algorithm = struct.unpack_from("<I", raw, 0x034)[0]
            if version in (2, 3, 4) and signature_algorithm == 1:
                quote_type = "SEV-SNP"
    if quote_type is None:
        raise ValueError(
            "SecretAI /cpu must contain a raw hex TDX quote or base64 SEV-SNP report"
        )

    with contextlib.redirect_stdout(sys.stderr):
        if quote_type == "TDX":
            return check_tdx_cpu_attestation(text)
        # strict=True forbids stale AMD KDS collateral fallback. The package
        # remains cache-efficient while valid collateral is within its lifetime.
        return check_sev_cpu_attestation(text, strict=True)


def _check_gpu_attestation(gpu_text: str) -> Any:
    with contextlib.redirect_stdout(sys.stderr):
        return check_nvidia_gpu_attestation(gpu_text)


def _require_safe_sev_policy(cpu_report: dict[str, Any]) -> None:
    policy_text = cpu_report.get("policy")
    if not isinstance(policy_text, str) or not re.fullmatch(
        r"0x[0-9a-f]{16}", policy_text
    ):
        raise ValueError("SecretAI SEV-SNP report is missing its signed guest policy")
    if int(policy_text, 16) & _SEV_MIGRATE_MA:
        raise ValueError("SecretAI SEV-SNP guest policy permits an unverified migration agent")


def _require_current_cpu_tcb(
    cpu_type: str, cpu_report: dict[str, Any]
) -> dict[str, int] | None:
    if cpu_type == "TDX":
        status = cpu_report.get("tcb_status")
        if status != "UpToDate":
            raise ValueError(f"SecretAI TDX TCB status is {status!r}, expected 'UpToDate'")
        return None

    reported = cpu_report.get("reported_tcb")
    if not isinstance(reported, dict) or any(
        type(reported.get(field)) is not int for field in _SEV_TCB_FIELDS
    ):
        raise ValueError("SecretAI SEV-SNP report is missing its signed reported_tcb")
    if any(reported[field] < _MINIMUM_SEV_TCB[field] for field in _SEV_TCB_FIELDS):
        raise ValueError(
            f"SecretAI SEV-SNP reported_tcb {reported!r} is below minimum "
            f"{_MINIMUM_SEV_TCB!r}"
        )
    return _MINIMUM_SEV_TCB


def _verified_gpu_claims(gpu_result: Any, gpu_nonce: str) -> tuple[list[str], int]:
    report = getattr(gpu_result, "report", None)
    if not isinstance(report, dict):
        raise ValueError("SecretAI NRAS result is missing its signed report")
    if report.get("overall_result") is not True:
        raise ValueError("SecretAI NRAS signed overall attestation result is not true")
    verified_nonce = str(report.get("nonce") or "").lower()
    if not _HEX_32_RE.fullmatch(verified_nonce) or not hmac.compare_digest(
        verified_nonce, gpu_nonce
    ):
        raise ValueError("SecretAI NRAS nonce does not match the CPU-bound GPU nonce")

    gpu_reports = report.get("gpus")
    if not isinstance(gpu_reports, dict) or not gpu_reports:
        raise ValueError("SecretAI NRAS result contains no signed per-GPU reports")
    models: set[str] = set()
    for gpu_id, gpu_report in gpu_reports.items():
        if not isinstance(gpu_report, dict):
            raise ValueError(f"SecretAI NRAS report for GPU {gpu_id!r} is malformed")
        if gpu_report.get("attestation_report_nonce_match") is not True:
            raise ValueError(
                f"SecretAI NRAS report for GPU {gpu_id!r} does not verify the nonce"
            )
        model = gpu_report.get("model")
        if isinstance(model, str) and model:
            models.add(model)
    return sorted(models), len(gpu_reports)


def _resolve_tdx_workload(cpu_report: dict[str, Any], compose: bytes) -> set[_PinnedWorkload]:
    fields = {
        "mrtd": str(cpu_report.get("mr_td") or "").lower(),
        "rtmr0": str(cpu_report.get("rt_mr0") or "").lower(),
        "rtmr1": str(cpu_report.get("rt_mr1") or "").lower(),
        "rtmr2": str(cpu_report.get("rt_mr2") or "").lower(),
        "rtmr3": str(cpu_report.get("rt_mr3") or "").lower(),
    }
    if any(not _HEX_48_RE.fullmatch(value) for value in fields.values()):
        raise ValueError("SecretAI TDX report is missing a 48-byte measurement register")

    matches: set[_PinnedWorkload] = set()
    compose_sha256 = hashlib.sha256(compose).hexdigest()
    for entry in workload._load_tdx_registry():
        if any(
            entry[name] != fields[name]
            for name in ("mrtd", "rtmr0", "rtmr1", "rtmr2")
        ):
            continue
        expected_rtmr3 = workload._calculate_rtmr3(compose, entry["rootfs_data"])
        if hmac.compare_digest(expected_rtmr3, fields["rtmr3"]):
            matches.add(
                _PinnedWorkload(
                    cpu_type="tdx",
                    environment=entry["vm_type"],
                    template_name=entry["template_name"],
                    artifacts_version=entry["artifacts_ver"],
                    compose_sha256=compose_sha256,
                )
            )
    return matches


def _resolve_sev_workload(cpu_report: dict[str, Any], compose: bytes) -> set[_PinnedWorkload]:
    measurement = str(cpu_report.get("measurement") or "").lower()
    if not _HEX_48_RE.fullmatch(measurement):
        raise ValueError("SecretAI SEV-SNP report is missing a 48-byte launch measurement")

    compose_sha256 = hashlib.sha256(compose).hexdigest()
    matches: set[_PinnedWorkload] = set()
    for entry in workload._load_sev_registry():
        prefix = "console=ttyS0 loglevel=7"
        if entry.get("cmdline_extra"):
            prefix += f" {entry['cmdline_extra']}"
        cmdline = (
            f"{prefix} docker_compose_hash={compose_sha256} "
            f"rootfs_hash={entry['rootfs_hash']}"
        )
        for template_name, vcpus in _SEV_VCPU_PROFILES.items():
            expected = workload._sev_calc_measurement(entry, vcpus, cmdline)
            if hmac.compare_digest(expected, measurement):
                # Zeroed SEV family_id cannot identify the vCPU profile, so the
                # template label is derived from the uniquely matching launch digest.
                matches.add(
                    _PinnedWorkload(
                        cpu_type="sev-snp",
                        environment=entry["vm_type"],
                        template_name=template_name,
                        artifacts_version=entry["artifacts_ver"],
                        compose_sha256=compose_sha256,
                    )
                )
    return matches


def _resolve_pinned_workload(
    cpu_type: str,
    cpu_report: dict[str, Any],
    compose: bytes,
) -> _PinnedWorkload:
    if cpu_type == "TDX":
        matches = _resolve_tdx_workload(cpu_report, compose)
    elif cpu_type == "SEV-SNP":
        matches = _resolve_sev_workload(cpu_report, compose)
    else:
        raise ValueError(f"SecretAI returned unsupported CPU attestation type {cpu_type!r}")
    if not matches:
        raise ValueError(
            "SecretAI launch measurement does not match the pinned SecretVM registry and compose"
        )
    if len(matches) != 1:
        identities = ", ".join(sorted(match.workload_id for match in matches))
        raise ValueError(f"SecretAI launch measurement resolves to ambiguous workloads: {identities}")
    return next(iter(matches))


def _accepted_workload_ids(options: dict[str, str]) -> set[str]:
    return {
        key[len(_ACCEPTED_WORKLOAD_PREFIX) :]
        for key, value in options.items()
        if key.startswith(_ACCEPTED_WORKLOAD_PREFIX) and value == "true"
    }


async def verify_secret_ai(request: dict[str, Any]) -> None:
    """Verify one SecretAI inference origin and emit its enforceable SPKI binding."""

    provider = "secret-ai"
    verifier_id = verifier_id_for(provider)
    evidence = None
    try:
        endpoint = _parse_origin(request.get("url_origin"))
        options = provider_options(request)
        accepted_workloads = _accepted_workload_ids(options)
        timeout = request_timeout_seconds(request, 60)

        fetched = await asyncio.gather(
            *(
                asyncio.to_thread(_fetch_evidence, endpoint, name, timeout)
                for name in ("cpu", "gpu", "docker-compose")
            )
        )
        bodies = dict(zip(("cpu", "gpu", "docker-compose"), fetched, strict=True))
        evidence_items = [
            raw_http_item(
                name,
                f"{endpoint.origin}/{name}",
                bodies[name][1],
                bodies[name][0],
            )
            for name in ("cpu", "gpu", "docker-compose")
        ]
        evidence = raw_http_bundle_evidence(evidence_items, source_url=endpoint.origin)

        cpu_text = bodies["cpu"][0].decode("utf-8", errors="strict")
        gpu_text = bodies["gpu"][0].decode("utf-8", errors="strict")
        compose = bodies["docker-compose"][0]

        tls_spki = await asyncio.to_thread(_tls_spki_sha256, endpoint)

        cpu_result = await asyncio.to_thread(_check_cpu_attestation, cpu_text)
        if cpu_result.valid is not True:
            reason = "; ".join(str(error) for error in cpu_result.errors) or "unknown failure"
            raise ValueError(f"SecretAI CPU attestation failed: {reason}")
        cpu_type = str(cpu_result.attestation_type)
        if cpu_type not in ("TDX", "SEV-SNP"):
            raise ValueError(f"SecretAI returned unsupported CPU attestation type {cpu_type!r}")
        cpu_report = cpu_result.report
        if cpu_type == "SEV-SNP":
            _require_safe_sev_policy(cpu_report)
        minimum_sev_tcb = _require_current_cpu_tcb(cpu_type, cpu_report)
        report_data = str(cpu_report.get("report_data") or "").lower()
        if not _HEX_64_RE.fullmatch(report_data):
            raise ValueError("SecretAI CPU report_data is not exactly 64 bytes")
        if not hmac.compare_digest(report_data[:64], tls_spki):
            raise ValueError("SecretAI CPU report_data does not bind the inference TLS SPKI")

        gpu_payload = json.loads(gpu_text)
        if not isinstance(gpu_payload, dict):
            raise ValueError("SecretAI /gpu response must be a JSON object")
        gpu_nonce = str(gpu_payload.get("nonce") or "").lower()
        if not _HEX_32_RE.fullmatch(gpu_nonce):
            raise ValueError("SecretAI GPU evidence nonce is not exactly 32 bytes")
        if not hmac.compare_digest(report_data[64:], gpu_nonce):
            raise ValueError("SecretAI GPU nonce is not bound into CPU report_data")
        gpu_result = await asyncio.to_thread(_check_gpu_attestation, gpu_text)
        if gpu_result.valid is not True:
            reason = "; ".join(str(error) for error in gpu_result.errors) or "unknown failure"
            raise ValueError(f"SecretAI GPU attestation failed: {reason}")
        gpu_models, gpu_count = _verified_gpu_claims(gpu_result, gpu_nonce)

        workload = await asyncio.to_thread(
            _resolve_pinned_workload, cpu_type, cpu_report, compose
        )
        if workload.environment not in _PRODUCTION_ENVIRONMENTS:
            raise ValueError(
                f"SecretAI workload uses non-production environment {workload.environment!r}"
            )
        if accepted_workloads and workload.workload_id not in accepted_workloads:
            raise ValueError(
                f"SecretAI workload {workload.workload_id!r} is not in accepted_workload_ids"
            )

        tcb_status = cpu_report.get("tcb_status")
        provider_claims: dict[str, Any] = {
            "trust_boundary": "single-secretvm-inference-origin",
            "cpu_attestation_type": cpu_type,
            "tls_spki_sha256": tls_spki,
            "gpu_verified": True,
            "gpu_models": gpu_models,
            "gpu_count": gpu_count,
            "production_os_image": True,
            "workload_id": workload.workload_id,
            "template_name": workload.template_name,
            "artifacts_version": workload.artifacts_version,
            "environment": workload.environment,
            "compose_sha256": f"sha256:{workload.compose_sha256}",
        }
        if accepted_workloads:
            provider_claims["accepted_workload_id"] = workload.workload_id
        if isinstance(tcb_status, str) and tcb_status:
            provider_claims["tcb_status"] = tcb_status
        if minimum_sev_tcb is not None:
            provider_claims["minimum_sev_tcb"] = minimum_sev_tcb
            provider_claims["reported_sev_tcb"] = cpu_report["reported_tcb"]

        emit(
            {
                "result": "verified",
                "verifier_id": verifier_id,
                "evidence": evidence,
                "attested_scope": "router",
                "channel_bindings": [
                    {
                        "type": "tls_spki_sha256",
                        "origin": endpoint.origin,
                        "spki_sha256": tls_spki,
                    }
                ],
                "provider_claims": provider_claims,
            }
        )
    except Exception as exc:  # noqa: BLE001 - bridge returns failures as JSON
        failed(provider, str(exc), evidence=evidence, verifier_id=verifier_id)
