#!/usr/bin/env python3
"""Hermetic soundness checks for the armet-ai provider verifier bridge.

The HTTP fetch, dcap-qvl quote verification and NVIDIA NRAS check are replaced
by deterministic fixtures that react to the bridge's real, freshly generated
nonce. Everything the gateway owns stays real: pin enforcement over the TD
measurements and launch configuration, the report_data commitment to nonce +
TLS SPKI + GPU evidence + model, the mandatory signed GPU gate, debug-TD and
revoked-TCB rejection, reading report_data from the verified report,
exact-bytes evidence, and model scope.

Run: uv run python tests/provider_verifier/armet_ai_soundness.py
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import io
import json
import sys
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))

import provider_verifier.armet_ai as tdx  # noqa: E402

ORIGIN = "https://model.example"
MODEL = "armet/llama-3.3-70b-instruct"
SPKI = "ab" * 32
PINNED = {
    "mrtd": "11" * 48,
    "rtmr0": "22" * 48,
    "rtmr1": "33" * 48,
    "rtmr2": "44" * 48,
    "rtmr3": "55" * 48,
    "td_attributes": "0000001000000000",
    "xfam": "e702060000000000",
    "mr_config_id": "00" * 48,
    "mr_owner": "00" * 48,
    "mr_owner_config": "00" * 48,
}
DCAP_KEYS = {name: key for name, key, _ in tdx.PINNED_FIELDS}
SUBJECT = tdx.measurement_subject(PINNED)


def _quote(debug: bool = False) -> bytes:
    """A TDX-shaped quote: only TD_ATTRIBUTES (offset 168) matters to the bridge;
    everything cryptographic is the (stubbed) dcap-qvl verifier's job."""
    quote = bytearray(1024)
    if debug:
        quote[168] = 0x01
    return bytes(quote)


def _gpu_text(nonce: str, evidence_list: Any = None) -> str:
    # Deliberately non-canonical spacing: the commitment is over these bytes.
    return json.dumps(
        {
            "arch": "HOPPER",
            "nonce": nonce,
            "evidence_list": [{"certificate": "x", "evidence": "y"}]
            if evidence_list is None
            else evidence_list,
        },
        indent=1,
    )


class Fixture:
    """Provider + dcap-qvl + NRAS stand-in. Mutators simulate a lying provider."""

    def __init__(self, **overrides: Any) -> None:
        self.o = overrides
        self.nonce: str | None = None
        self.body: bytes = b""
        self.gpu_text: str | None = None
        self.gpu_checked = False

    def fetch(self, url: str, params: dict[str, str], headers: dict[str, str], timeout: int):
        assert url == ORIGIN + tdx.ATTESTATION_PATH, url
        assert params["model"] == MODEL and params["version"] == "2"
        self.nonce = params["nonce"]
        self.headers = headers
        if "fetch_error" in self.o:
            raise self.o["fetch_error"]
        # The GPU evidence the TD collected and committed to in its quote.
        self.gpu_text = _gpu_text(
            self.o.get("gpu_nonce", self.nonce), self.o.get("gpu_evidence_list")
        )
        report = {
            "version": self.o.get("version", 2),
            "model_id": self.o.get("report_model", MODEL),
            "nonce": self.o.get("echo_nonce", self.nonce),
            "tls_spki_sha256": self.o.get("report_spki", SPKI),
            "quote": _quote(self.o.get("debug", False)).hex(),
            # What the report carries; differs from gpu_text when swapped.
            "nvidia_payload": self.o.get("report_gpu", self.gpu_text),
        }
        if self.o.get("swap_gpu"):
            # Fresh evidence (same nonce) other than what the TD committed to.
            report["nvidia_payload"] = _gpu_text(self.nonce, [{"certificate": "other"}])
        if report["nvidia_payload"] is None:
            del report["nvidia_payload"]
        if "raw_body" in self.o:
            self.body = self.o["raw_body"]
        else:
            # Deliberately non-canonical spacing: evidence must be these exact bytes.
            self.body = json.dumps(report, indent=1).encode()
        return self.body, "application/json"

    async def verify_quote(self, quote_bytes: bytes) -> dict[str, Any]:
        if "dcap_error" in self.o:
            raise self.o["dcap_error"]
        assert self.nonce is not None and self.gpu_text is not None
        report_data = tdx.expected_report_data(
            self.o.get("bound_nonce", self.nonce),
            self.o.get("bound_spki", SPKI),
            hashlib.sha256(self.gpu_text.encode()).digest(),
            self.o.get("bound_model", MODEL),
        )
        if self.o.get("raw_digest_encoding"):
            # The same digest as raw bytes + zero padding: a different encoding.
            report_data = bytes.fromhex(report_data.decode()) + b"\x00" * 32
        if self.o.get("uppercase_hex"):
            report_data = report_data.upper()
        pinned = {**PINNED, **self.o.get("registers", {})}
        body = {DCAP_KEYS[name]: value for name, value in pinned.items()}
        body.update({"mr_seam": "66" * 48, "tee_tcb_svn": "07" * 16})
        body["report_data"] = report_data.hex()
        report = {"TD15": {"base": body}} if self.o.get("td15") else {"TD10": body}
        return {
            "status": self.o.get("tcb_status", "UpToDate"),
            "advisory_ids": self.o.get("advisory_ids", []),
            "report": report,
        }

    def check_gpu(self, gpu_text: str) -> Any:
        self.gpu_checked = True
        if "gpu_check_error" in self.o:
            raise self.o["gpu_check_error"]
        assert gpu_text == self.gpu_text
        return SimpleNamespace(
            valid=self.o.get("gpu_valid", True),
            errors=["nras says no"],
            report={
                "overall_result": self.o.get("nras_overall", True),
                "nonce": self.o.get("nras_nonce", self.nonce),
                "gpus": self.o.get(
                    "nras_gpus",
                    {
                        "GPU-0": {"model": "GH100", "attestation_report_nonce_match": True},
                        "GPU-1": {
                            "model": "GH100",
                            "attestation_report_nonce_match": self.o.get(
                                "nras_gpu1_match", True
                            ),
                        },
                    },
                ),
            },
        )


def _run(
    subjects: tuple[str, ...] = (SUBJECT,),
    origin: str = ORIGIN,
    extra_options: dict[str, str] | None = None,
    **overrides: Any,
) -> tuple[dict[str, Any], Fixture]:
    fixture = Fixture(**overrides)
    tdx.fetch_report = fixture.fetch
    tdx.verify_quote = fixture.verify_quote
    tdx.check_gpu_attestation = fixture.check_gpu
    options = {f"{tdx.ACCEPTED_SUBJECT_OPTION}{s}": "true" for s in subjects}
    options.update(extra_options or {})
    request = {
        "provider": "armet-ai",
        "url_origin": origin,
        "model_id": MODEL,
        "timeout_seconds": 5,
        "provider_options": options,
    }
    buf = io.StringIO()
    with redirect_stdout(buf):
        asyncio.run(tdx.verify_armet_ai(request))
    return json.loads(buf.getvalue()), fixture


def check() -> list[str]:
    failures: list[str] = []

    def expect(name: str, cond: bool, detail: Any = "") -> None:
        if not cond:
            failures.append(f"{name}: {detail}")

    def rejects(name: str, needle: str, **kwargs: Any) -> Fixture:
        out, fixture = _run(**kwargs)
        expect(name, out.get("result") == "failed", out)
        expect(f"{name} reason", needle in str(out.get("reason")), out.get("reason"))
        expect(f"{name} no binding", not out.get("channel_bindings"), out)
        return fixture

    # ---- pure helpers ------------------------------------------------------
    gpu_hash = b"\x77" * 32
    rd = tdx.expected_report_data("00" * 32, "ff" * 32, gpu_hash, "m")
    expect("report_data length", len(rd) == 64, len(rd))
    expect("report_data is lowercase hex ascii", rd == rd.lower() and bytes.fromhex(rd.decode()))
    expect(
        "report_data vector",
        rd
        == hashlib.sha256(
            b"private-ai-gateway/armet-ai/v2\x00" + b"\x00" * 32 + b"\xff" * 32 + gpu_hash + b"m"
        )
        .hexdigest()
        .encode(),
        rd,
    )
    expect(
        "report_data binds model",
        rd != tdx.expected_report_data("00" * 32, "ff" * 32, gpu_hash, "m2"),
    )
    expect(
        "report_data binds GPU evidence",
        rd != tdx.expected_report_data("00" * 32, "ff" * 32, b"\x78" * 32, "m"),
    )
    expect(
        "subject vector",
        SUBJECT
        == "tdx-measurement:sha256:"
        + hashlib.sha256(
            b"".join(bytes.fromhex(PINNED[name]) for name, _, _ in tdx.PINNED_FIELDS)
        ).hexdigest(),
        SUBJECT,
    )

    # ---- positive path -----------------------------------------------------
    out, fixture = _run(extra_options={tdx.BEARER_OPTION: "tok"})
    expect("verified", out.get("result") == "verified", out)
    expect("scope is model", out.get("attested_scope") == "model", out)
    expect("verifier id", out.get("verifier_id") == "private-ai-verifier/armet-ai/v1", out)
    expect(
        "binding",
        out.get("channel_bindings")
        == [{"type": "tls_spki_sha256", "origin": ORIGIN, "spki_sha256": SPKI}],
        out.get("channel_bindings"),
    )
    expect("bearer sent", fixture.headers == {"Authorization": "Bearer tok"}, fixture.headers)
    evidence = out.get("evidence") or {}
    data = str(evidence.get("data", ""))
    decoded = base64.b64decode(data.split(",", 1)[1]) if "," in data else b""
    expect("evidence exact bytes", decoded == fixture.body)
    expect(
        "evidence digest",
        evidence.get("digest") == "sha256:" + hashlib.sha256(fixture.body).hexdigest(),
        evidence.get("digest"),
    )
    claims = out.get("provider_claims") or {}
    expect("claim subject", claims.get("accepted_subject") == SUBJECT, claims)
    expect("claim measurements", claims.get("measurements") == PINNED, claims)
    expect(
        "claim tdx module",
        claims.get("tdx_module") == {"mr_seam": "66" * 48, "tee_tcb_svn": "07" * 16},
        claims,
    )
    expect("claim tcb", claims.get("tcb_status") == "UpToDate", claims)
    expect("claim model", claims.get("canonical_model_id") == MODEL, claims)
    expect("claim gpu verified", claims.get("gpu_verified") is True, claims)
    expect("claim gpu models", claims.get("gpu_models") == ["GH100"], claims)
    expect("claim gpu count", claims.get("gpu_count") == 2, claims)
    expect("no unsigned gpu arch", "gpu_arch" not in claims, claims)

    out, _ = _run(td15=True)
    expect("TDX 1.5 report accepted", out.get("result") == "verified", out)

    # A stale TCB is recorded (refuted claim downstream), not gated.
    out, _ = _run(tcb_status="OutOfDate", advisory_ids=["INTEL-SA-00000"])
    expect("OutOfDate recorded", out.get("result") == "verified", out)
    expect(
        "OutOfDate surfaced",
        (out.get("provider_claims") or {}).get("tcb_status") == "OutOfDate",
        out,
    )

    # ---- fail-closed cases -------------------------------------------------
    rejects("no pins", "no accepted_subjects", subjects=())
    rejects("http origin", "must be https", origin="http://model.example")
    rejects("fetch error", "failed to fetch", fetch_error=RuntimeError("boom"))
    rejects("not json", "not JSON", raw_body=b"<html>")
    rejects("version 1 report", "version must be 2", version=1)
    rejects("stale nonce echo", "nonce did not match", echo_nonce="00" * 32)
    rejects("model alias", "does not match requested model", report_model="armet/other")
    rejects("bad spki", "tls_spki_sha256 must be", report_spki="xyz")
    rejects("debug TD", "debug mode", debug=True)
    rejects("dcap failure", "quote verification failed", dcap_error=ValueError("bad sig"))
    rejects("revoked TCB", "Revoked", tcb_status="Revoked")
    # report_data is the heart of the binding: every committed field must matter.
    rejects("replayed quote (old nonce)", "report_data does not commit", bound_nonce="00" * 32)
    rejects("swapped TLS key", "report_data does not commit", report_spki="cd" * 32)
    rejects("quote for another key", "report_data does not commit", bound_spki="cd" * 32)
    rejects("quote for another model", "report_data does not commit", bound_model="armet/other")
    # Exactly one encoding is accepted: lowercase hex ASCII over all 64 bytes.
    rejects("raw digest encoding", "report_data does not commit", raw_digest_encoding=True)
    rejects("uppercase hex encoding", "report_data does not commit", uppercase_hex=True)
    for name in PINNED:
        size = dict((n, s) for n, _, s in tdx.PINNED_FIELDS)[name]
        rejects(f"unpinned {name}", "not in accepted_subjects", registers={name: "99" * size})
    rejects("malformed register", "malformed", registers={"rtmr3": "99" * 10})

    # The unpinned-measurement failure names the observed values for review.
    out, _ = _run(registers={"rtmr3": "99" * 48})
    expect(
        "observed measurements reported",
        (out.get("observed_measurements") or {}).get("rtmr3") == "99" * 48,
        out,
    )

    # ---- mandatory GPU gate ------------------------------------------------
    rejects("gpu payload missing", "missing nvidia_payload", report_gpu=None)
    rejects("gpu payload as object", "missing nvidia_payload", report_gpu={"nonce": "x"})
    rejects("gpu payload not json", "nvidia_payload is not JSON", report_gpu="{nope")
    rejects("gpu evidence empty", "no GPU evidence_list", gpu_evidence_list=[])
    rejects("gpu stale nonce", "GPU evidence nonce did not match", gpu_nonce="00" * 32)
    # Fresh evidence from some other GPU, swapped in after the TD quoted: the
    # quote commits to the original bytes, so the substitution cannot pass.
    swapped = rejects("gpu evidence swapped", "report_data does not commit", swap_gpu=True)
    expect("swapped gpu never reaches NRAS", swapped.gpu_checked is False)
    fx = rejects("nras invalid", "GPU attestation failed: nras says no", gpu_valid=False)
    expect("nras invalid was checked", fx.gpu_checked is True)
    rejects("nras truthy string", "GPU attestation failed", gpu_valid="true")
    rejects("nras error", "GPU attestation failed", gpu_check_error=RuntimeError("down"))
    rejects("nras overall false", "overall attestation result is not true", nras_overall=False)
    rejects("nras signed nonce", "NRAS nonce does not match", nras_nonce="ef" * 32)
    rejects("nras one gpu nonce", "does not verify the nonce", nras_gpu1_match=False)
    rejects("nras no gpus", "no signed per-GPU reports", nras_gpus={})

    return failures


def main() -> int:
    original = (tdx.fetch_report, tdx.verify_quote, tdx.check_gpu_attestation)
    try:
        failures = check()
    finally:
        tdx.fetch_report, tdx.verify_quote, tdx.check_gpu_attestation = original
    if failures:
        print("ARMETAI BRIDGE SOUNDNESS FAILURES:")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("armet-ai bridge soundness: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
