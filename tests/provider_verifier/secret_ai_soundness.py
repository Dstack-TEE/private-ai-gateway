#!/usr/bin/env python3
"""Hermetic soundness checks for the SecretAI provider verifier bridge.

The HTTP transport and official SecretVM cryptographic checks are replaced with
deterministic fixtures. Gateway-owned policy remains real: origin validation,
SPKI and GPU report_data bindings, exact compose bytes, production workload
selection, optional operator pinning, evidence emission, and router scope.

Run: uv run python tests/provider_verifier/secret_ai_soundness.py
"""

from __future__ import annotations

import asyncio
import hashlib
import io
import json
import sys
import types
from contextlib import redirect_stdout
from dataclasses import replace
from pathlib import Path
from typing import Any

import requests
import secretvm.verify.workload as workload

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))

import provider_verifier.secret_ai as secret_ai  # noqa: E402

ORIGIN = "https://secret.example:21434"
SPKI = "ab" * 32
GPU_NONCE = "cd" * 32
COMPOSE = b"services:\n  inference:\n    image: example.invalid/model@sha256:" + b"11" * 32 + b"\n"
COMPOSE_SHA256 = hashlib.sha256(COMPOSE).hexdigest()
WORKLOAD = secret_ai._PinnedWorkload(
    cpu_type="tdx",
    environment="prod",
    template_name="4xlarge_256gb_gpu",
    artifacts_version="v0.0.33",
    compose_sha256=COMPOSE_SHA256,
)


def _result(
    *, valid: Any, attestation_type: str = "TDX", report: dict[str, Any] | None = None
):
    return types.SimpleNamespace(
        valid=valid,
        errors=[] if valid else ["fixture verification failure"],
        attestation_type=attestation_type,
        report=report or {},
    )


def _run(
    *,
    origin: str = ORIGIN,
    accepted_workloads: tuple[str, ...] = (),
    tls_spki: str = SPKI,
    report_data: str = SPKI + GPU_NONCE,
    gpu_nonce: str = GPU_NONCE,
    cpu_valid: Any = True,
    cpu_type: str = "TDX",
    cpu_policy: str = "0x0000000000020000",
    tdx_tcb_status: str = "UpToDate",
    sev_reported_tcb: tuple[int, int, int, int] = (10, 0, 23, 88),
    gpu_valid: Any = True,
    verified_gpu_overall_result: Any = True,
    verified_gpu_nonce: str = GPU_NONCE,
    verified_gpu_nonce_match: bool = True,
    verified_gpu_model: str = "GH100",
    workload: secret_ai._PinnedWorkload = WORKLOAD,
) -> dict[str, Any]:
    gpu_body = json.dumps(
        {"nonce": gpu_nonce, "arch": "HOPPER", "evidence_list": [{"arch": "HOPPER"}]}
    ).encode()
    bodies = {
        "cpu": (b"fixture-cpu-evidence", "text/plain; charset=utf-8"),
        "gpu": (gpu_body, "text/plain; charset=utf-8"),
        "docker-compose": (COMPOSE, "text/plain; charset=utf-8"),
    }
    cpu_report = {
        "report_data": report_data,
        "mr_td": "22" * 48,
        "policy": cpu_policy,
        "reported_tcb": dict(zip(secret_ai._SEV_TCB_FIELDS, sev_reported_tcb, strict=True)),
    }
    if cpu_type == "TDX":
        cpu_report["tcb_status"] = tdx_tcb_status

    def fetch(_endpoint, name, _timeout):
        return bodies[name]

    def resolve(resolved_cpu_type, report, compose):
        if resolved_cpu_type != cpu_type or report is not cpu_report:
            raise AssertionError("resolver did not receive the verified CPU result")
        if compose != COMPOSE:
            raise AssertionError("resolver did not receive the exact compose bytes")
        return workload

    originals = {
        "_fetch_evidence": secret_ai._fetch_evidence,
        "_tls_spki_sha256": secret_ai._tls_spki_sha256,
        "_check_cpu_attestation": secret_ai._check_cpu_attestation,
        "_check_gpu_attestation": secret_ai._check_gpu_attestation,
        "_resolve_pinned_workload": secret_ai._resolve_pinned_workload,
    }
    secret_ai._fetch_evidence = fetch
    secret_ai._tls_spki_sha256 = lambda _endpoint: tls_spki
    secret_ai._check_cpu_attestation = lambda _text: _result(
        valid=cpu_valid, attestation_type=cpu_type, report=cpu_report
    )
    secret_ai._check_gpu_attestation = lambda _text: _result(
        valid=gpu_valid,
        report={
            "overall_result": verified_gpu_overall_result,
            "nonce": verified_gpu_nonce,
            "gpus": {
                "GPU-0": {
                    "model": verified_gpu_model,
                    "attestation_report_nonce_match": verified_gpu_nonce_match,
                }
            },
        },
    )
    secret_ai._resolve_pinned_workload = resolve
    options = {
        f"secret_ai_accepted_workload_id:{workload_id}": "true"
        for workload_id in accepted_workloads
    }
    request = {
        "provider": "secret-ai",
        "upstream_name": "secret-ai",
        "url_origin": origin,
        "model_id": "fixture-model",
        "provider_options": options,
        "timeout_seconds": 5,
    }
    output = io.StringIO()
    try:
        with redirect_stdout(output):
            asyncio.run(secret_ai.verify_secret_ai(request))
    finally:
        for name, value in originals.items():
            setattr(secret_ai, name, value)
    return json.loads(output.getvalue())


def _expect_failure(
    failures: list[str], name: str, expected_reason: str, **kwargs: Any
) -> None:
    output = _run(**kwargs)
    reason = output.get("reason") or ""
    if output.get("result") != "failed" or expected_reason not in reason:
        failures.append(
            f"{name}: expected failure containing {expected_reason!r}, got {output!r}"
        )


def _check_real_workload_resolvers(failures: list[str]) -> None:
    compose = b"services:\n  fixture:\n    image: fixture.invalid/model@sha256:" + b"33" * 32 + b"\n"
    compose_sha256 = hashlib.sha256(compose).hexdigest()

    tdx_entry = workload._load_tdx_registry()[0]
    tdx_report = {
        "mr_td": tdx_entry["mrtd"],
        "rt_mr0": tdx_entry["rtmr0"],
        "rt_mr1": tdx_entry["rtmr1"],
        "rt_mr2": tdx_entry["rtmr2"],
        "rt_mr3": workload._calculate_rtmr3(compose, tdx_entry["rootfs_data"]),
    }
    tdx_matches = secret_ai._resolve_tdx_workload(tdx_report, compose)
    if len(tdx_matches) != 1:
        failures.append(f"tdx-resolver: expected one real registry match, got {tdx_matches!r}")
    elif next(iter(tdx_matches)).compose_sha256 != compose_sha256:
        failures.append("tdx-resolver: matched identity did not bind exact compose bytes")
    if secret_ai._resolve_tdx_workload(tdx_report, compose + b"# tampered\n"):
        failures.append("tdx-resolver: accepted tampered compose bytes")

    sev_entry = workload._load_sev_registry()[0]
    prefix = "console=ttyS0 loglevel=7"
    if sev_entry.get("cmdline_extra"):
        prefix += f" {sev_entry['cmdline_extra']}"
    cmdline = (
        f"{prefix} docker_compose_hash={compose_sha256} "
        f"rootfs_hash={sev_entry['rootfs_hash']}"
    )
    sev_report = {"measurement": workload._sev_calc_measurement(sev_entry, 1, cmdline)}
    sev_matches = secret_ai._resolve_sev_workload(sev_report, compose)
    expected = secret_ai._PinnedWorkload(
        cpu_type="sev-snp",
        environment=sev_entry["vm_type"],
        template_name="small",
        artifacts_version=sev_entry["artifacts_ver"],
        compose_sha256=compose_sha256,
    )
    if sev_matches != {expected}:
        failures.append(f"sev-resolver: expected one real registry match, got {sev_matches!r}")
    if secret_ai._resolve_sev_workload(sev_report, compose + b"# tampered\n"):
        failures.append("sev-resolver: accepted tampered compose bytes")


def _check_fetch_policy(failures: list[str]) -> None:
    class Response:
        def __init__(self, status: int, chunks: list[bytes], headers: dict[str, str]):
            self.status_code = status
            self._chunks = chunks
            self.headers = headers

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def iter_content(self, chunk_size: int):
            if chunk_size != 64 * 1024:
                raise AssertionError("unexpected stream chunk size")
            yield from self._chunks

    endpoint = secret_ai._parse_origin(ORIGIN)
    original_get = requests.get
    max_cpu_bytes = secret_ai._MAX_EVIDENCE_BYTES["cpu"]

    def mocked_get(response):
        def get(*_args, **kwargs):
            if kwargs.get("allow_redirects") is not False:
                raise AssertionError("SecretAI evidence requests must disable redirects")
            if kwargs.get("stream") is not True:
                raise AssertionError("SecretAI evidence requests must stream response bodies")
            return response

        return get

    try:
        requests.get = mocked_get(
            Response(
                302,
                [],
                {
                    "content-type": "text/plain",
                    "location": "https://other.example/cpu",
                },
            )
        )
        try:
            secret_ai._fetch_evidence(endpoint, "cpu", 5)
            failures.append("fetch-policy: followed or accepted an HTTP redirect")
        except ValueError as exc:
            if "HTTP 302" not in str(exc):
                failures.append(f"fetch-policy: wrong redirect failure: {exc}")

        requests.get = mocked_get(
            Response(
                200,
                [b"x" * max_cpu_bytes, b"x"],
                {"content-type": "text/plain"},
            )
        )
        try:
            secret_ai._fetch_evidence(endpoint, "cpu", 5)
            failures.append("fetch-policy: accepted a streamed body over the size limit")
        except ValueError as exc:
            if str(max_cpu_bytes) not in str(exc):
                failures.append(f"fetch-policy: wrong size-limit failure: {exc}")

        requests.get = mocked_get(
            Response(
                200,
                [],
                {
                    "content-type": "text/plain",
                    "content-length": str(max_cpu_bytes + 1),
                },
            )
        )
        try:
            secret_ai._fetch_evidence(endpoint, "cpu", 5)
            failures.append("fetch-policy: accepted an oversized declared Content-Length")
        except ValueError:
            pass
    finally:
        requests.get = original_get


def _check_cpu_input_policy(failures: list[str]) -> None:
    original_get = requests.get
    network_calls = 0

    def forbidden_get(*_args, **_kwargs):
        nonlocal network_calls
        network_calls += 1
        raise AssertionError("URL-looking CPU evidence must not trigger a fetch")

    requests.get = forbidden_get
    try:
        try:
            secret_ai._check_cpu_attestation("https://relay.example/base")
            failures.append("cpu-input-policy: accepted URL-looking CPU evidence")
        except ValueError as exc:
            if "raw hex TDX quote or base64 SEV-SNP report" not in str(exc):
                failures.append(f"cpu-input-policy: wrong rejection: {exc}")
        except AssertionError as exc:
            failures.append(f"cpu-input-policy: performed an off-origin fetch: {exc}")
    finally:
        requests.get = original_get
    if network_calls:
        failures.append(
            f"cpu-input-policy: URL-looking evidence made {network_calls} network request(s)"
        )


def check() -> list[str]:
    failures: list[str] = []
    _check_real_workload_resolvers(failures)
    _check_fetch_policy(failures)
    _check_cpu_input_policy(failures)

    output = _run()
    if output.get("result") != "verified":
        failures.append(f"genuine: expected verified, got {output!r}")
    else:
        if output.get("attested_scope") != "router":
            failures.append("genuine: SecretAI must declare router scope")
        if output.get("channel_bindings") != [
            {"type": "tls_spki_sha256", "origin": ORIGIN, "spki_sha256": SPKI}
        ]:
            failures.append(f"genuine: wrong channel binding {output.get('channel_bindings')!r}")
        claims = output.get("provider_claims") or {}
        if claims.get("workload_id") != WORKLOAD.workload_id:
            failures.append(f"genuine: wrong workload identity {claims.get('workload_id')!r}")
        if "accepted_workload_id" in claims:
            failures.append("genuine: unpinned workload was reported as operator-approved")
        if "measurement" in claims:
            failures.append("genuine: surfaced a CPU-specific register as a generic measurement")
        if claims.get("compose_sha256") != f"sha256:{COMPOSE_SHA256}":
            failures.append("genuine: exact compose digest was not surfaced")
        if claims.get("gpu_verified") is not True:
            failures.append("genuine: mandatory NRAS result was not surfaced")
        if claims.get("gpu_models") != ["GH100"] or claims.get("gpu_count") != 1:
            failures.append("genuine: signed NRAS GPU identity was not surfaced")
        if "gpu_arch" in claims:
            failures.append("genuine: unverified top-level GPU arch was surfaced")
        if output.get("verifier_id") != "private-ai-verifier/secret-ai/v1":
            failures.append(f"genuine: wrong verifier id {output.get('verifier_id')!r}")
        evidence = output.get("evidence") or {}
        if not str(evidence.get("data") or "").startswith("data:multipart/mixed;"):
            failures.append("genuine: raw CPU/GPU/compose evidence bundle is missing")

    pinned = _run(accepted_workloads=(WORKLOAD.workload_id,))
    pinned_claims = pinned.get("provider_claims") or {}
    if (
        pinned.get("result") != "verified"
        or pinned_claims.get("accepted_workload_id") != WORKLOAD.workload_id
    ):
        failures.append(f"pinned-workload: expected matching pin, got {pinned!r}")
    _expect_failure(failures, "non-https", "must use https", origin="http://secret.example")
    _expect_failure(failures, "path", "must not include a path", origin=ORIGIN + "//")
    _expect_failure(failures, "spki", "does not bind", tls_spki="ef" * 32)
    _expect_failure(failures, "gpu-nonce", "not bound", gpu_nonce="ef" * 32)
    _expect_failure(failures, "cpu", "CPU attestation failed", cpu_valid=False)
    _expect_failure(
        failures,
        "cpu-valid-type",
        "CPU attestation failed",
        cpu_valid="true",
    )
    _expect_failure(
        failures,
        "tdx-tcb",
        "expected 'UpToDate'",
        tdx_tcb_status="OutOfDate",
    )
    _expect_failure(
        failures,
        "sev-migration-agent",
        "permits an unverified migration agent",
        cpu_type="SEV-SNP",
        cpu_policy="0x0000000000060000",
    )
    _expect_failure(
        failures,
        "sev-below-minimum-tcb",
        "is below minimum",
        cpu_type="SEV-SNP",
        sev_reported_tcb=(10, 0, 23, 87),
    )
    sev_workload = replace(WORKLOAD, cpu_type="sev-snp", template_name="4xlarge")
    sev = _run(cpu_type="SEV-SNP", workload=sev_workload)
    if sev.get("result") != "verified":
        failures.append(f"sev-minimum-tcb: expected verified, got {sev!r}")
    _expect_failure(failures, "gpu", "GPU attestation failed", gpu_valid=False)
    _expect_failure(
        failures,
        "gpu-valid-type",
        "GPU attestation failed",
        gpu_valid="true",
    )
    _expect_failure(
        failures,
        "nras-overall-result-type",
        "overall attestation result is not true",
        verified_gpu_overall_result="false",
    )
    _expect_failure(
        failures,
        "nras-nonce",
        "NRAS nonce does not match",
        verified_gpu_nonce="ef" * 32,
    )
    _expect_failure(
        failures,
        "nras-per-gpu-nonce",
        "does not verify the nonce",
        verified_gpu_nonce_match=False,
    )
    _expect_failure(
        failures,
        "dev-environment",
        "non-production environment",
        workload=replace(WORKLOAD, environment="dev"),
    )
    _expect_failure(
        failures,
        "unapproved-workload",
        "not in accepted_workload_ids",
        accepted_workloads=("secretvm:tdx:prod:other:v0:sha256:" + "00" * 32,),
    )
    return failures


def main() -> int:
    failures = check()
    if failures:
        print("SECRETAI BRIDGE SOUNDNESS FAILURES:")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("secret-ai bridge soundness checks OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
