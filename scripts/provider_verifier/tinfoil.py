"""Tinfoil provider verification."""

from __future__ import annotations

import asyncio
import base64
import contextlib
import gzip
import sys
from typing import Any

from .common import emit, failed, json_evidence_bundle, provider_options


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
    options = provider_options(request)
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
    # Tinfoil is a router upstream: we request the router attestation (the
    # confidential-model-router enclave) explicitly rather than inferring the
    # scope from the response. The verified channel is that router enclave,
    # shared by every model, so the attested session is per router (like NEAR
    # AI) and the served model is recorded on the receipt, not in the session.
    # Fail closed if the verification was not router-scoped, so a non-router
    # deployment can never be treated as one channel for many models.
    if not used_router:
        failed(
            provider,
            "Tinfoil verification was not router-scoped; expected the "
            f"confidential-model-router enclave (repo={repo!r})",
            evidence=evidence,
        )
        return
    emit(
        {
            "result": "verified",
            "verifier_id": "tinfoil-verifier/v1",
            "attested_scope": "router",
            "evidence": evidence,
            "channel_bindings": [
                {
                    "type": "tls_spki_sha256",
                    "origin": request.get("url_origin"),
                    "spki_sha256": spki,
                }
            ],
            "provider_claims": {
                # Router channel scope — model-independent. The served model is
                # deliberately NOT folded in: it would split one verified channel
                # into a session per model, and it is recorded on the receipt.
                "trust_boundary": "router",
                "evidence_scope": "router",
                "used_router": True,
                "config_repo": doc.config_repo,
                "release_digest": doc.release_digest,
                "code_fingerprint": doc.code_fingerprint,
                "tls_spki_from_report_data": True,
                "verification_steps": {k: v["status"] for k, v in steps.items()},
                # NOTE: we deliberately do NOT emit a `tcb_status` here. Tinfoil's
                # official verifier (SecureClient.verify) owns the TCB gate as part
                # of security_verified, but exposes no separable TcbStatus, so there
                # is no raw collateral value to surface. The session layer records a
                # verifier-derived tcb_up_to_date claim for Tinfoil instead of
                # fabricating a hardware-proven "UpToDate" status.
            },
        }
    )

