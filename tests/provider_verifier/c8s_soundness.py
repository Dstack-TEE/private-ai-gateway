#!/usr/bin/env python3
"""Hermetic soundness checks for the Confidential AI (c8s) verifier bridge.

The fixtures are live `/attestation` responses from `api.confidential.ai` and
`candidate.api.confidential.ai`, each captured together with the TLS leaf of the
same connection and the Intel DCAP collateral valid at capture time. Only the
network is replaced: the TDX quote is verified with the real `dcap_qvl` against
that collateral at the capture time, and every gateway policy check runs as in
production. Each tamper case must fail closed.

Run: uv run python tests/provider_verifier/c8s_soundness.py
"""

from __future__ import annotations

import asyncio
import base64
import copy
import dataclasses
import hashlib
import io
import json
import sys
from contextlib import redirect_stdout
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable

import dcap_qvl

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import provider_verifier.c8s as c8s  # noqa: E402

FIXTURES = ROOT / "tests" / "fixtures" / "c8s"
ZERO_48 = "00" * 48


def _fixture(name: str) -> dict[str, Any]:
    return json.loads((FIXTURES / f"{name}.json").read_text(encoding="utf-8"))


PRODUCTION = _fixture("production")
CANDIDATE = _fixture("candidate")


def _leaf(fixture: dict[str, Any]) -> bytes:
    return base64.b64decode(fixture["serving_leaf_der_base64"])


def _receipt(body: dict[str, Any]) -> dict[str, Any]:
    return body["frontDoor"]["receipt"]


def _run(
    fixture: dict[str, Any] = PRODUCTION,
    *,
    origin: str | None = None,
    client_nonce: str | None = None,
    serving_leaf: bytes | None = None,
    body: Callable[[dict[str, Any]], None] | None = None,
    verified: Callable[[dict[str, Any]], None] | None = None,
    registry: Callable[[dict[str, c8s._ReleasePin]], None] | None = None,
    quote_bytes: Callable[[bytes], bytes] | None = None,
    clock_offset: timedelta = timedelta(0),
) -> dict[str, Any]:
    attestation = copy.deepcopy(fixture["attestation"])
    if body is not None:
        body(attestation)
    captured_at = fixture["verified_at_unix"]
    collateral = dcap_qvl.QuoteCollateralV3.from_json(json.dumps(fixture["dcap_collateral"]))

    async def verify_quote(quote: bytes) -> dict[str, Any]:
        if quote_bytes is not None:
            quote = quote_bytes(quote)
        result = json.loads(dcap_qvl.verify(quote, collateral, captured_at).to_json())
        if verified is not None:
            verified(result)
        return result

    def fetch(endpoint, nonce, _timeout):
        if endpoint.origin != fixture["origin"]:
            raise AssertionError(f"unexpected fetch origin {endpoint.origin!r}")
        if nonce != (client_nonce or fixture["nonce"]):
            raise AssertionError("fetch did not receive the generated nonce")
        return c8s._Capture(
            body=json.dumps(attestation).encode(),
            content_type="application/json",
            serving_leaf_der=serving_leaf or _leaf(fixture),
        )

    pins = c8s._load_registry()
    if registry is not None:
        registry(pins)

    originals = {
        name: getattr(c8s, name)
        for name in ("_new_nonce", "_fetch_attestation", "_verify_quote", "_now", "_load_registry")
    }
    c8s._new_nonce = lambda: client_nonce or fixture["nonce"]
    c8s._fetch_attestation = fetch
    c8s._verify_quote = verify_quote
    c8s._now = lambda: datetime.fromtimestamp(captured_at, timezone.utc) + clock_offset
    c8s._load_registry = lambda: pins
    request = {
        "provider": "c8s",
        "upstream_name": "confidential-ai",
        "url_origin": origin or fixture["origin"],
        "model_id": "fixture-model",
        "timeout_seconds": 5,
    }
    output = io.StringIO()
    try:
        with redirect_stdout(output):
            asyncio.run(c8s.verify_c8s(request))
    finally:
        for name, value in originals.items():
            setattr(c8s, name, value)
    return json.loads(output.getvalue())


def _expect_failure(
    failures: list[str], name: str, expected_reason: str, **kwargs: Any
) -> None:
    output = _run(**kwargs)
    reason = output.get("reason") or ""
    if output.get("result") != "failed" or expected_reason not in reason:
        failures.append(f"{name}: expected failure containing {expected_reason!r}, got {reason!r}")


def _set(path: str, value: Any) -> Callable[[dict[str, Any]], None]:
    def mutate(document: dict[str, Any]) -> None:
        *parents, leaf = path.split(".")
        for key in parents:
            document = document[key]
        document[leaf] = value

    return mutate


def _td(field: str, value: str) -> Callable[[dict[str, Any]], None]:
    def mutate(result: dict[str, Any]) -> None:
        result["report"]["TD10"][field] = value

    return mutate


def _pin(release_id: str, **changes: Any) -> Callable[[dict[str, c8s._ReleasePin]], None]:
    def mutate(pins: dict[str, c8s._ReleasePin]) -> None:
        pins[release_id] = dataclasses.replace(pins[release_id], **changes)

    return mutate


def _check_verified(failures: list[str], name: str, fixture: dict[str, Any]) -> None:
    output = _run(fixture)
    if output.get("result") != "verified":
        failures.append(f"{name}: expected verified, got {output.get('reason')!r}")
        return
    from cryptography import x509
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

    spki = x509.load_der_x509_certificate(_leaf(fixture)).public_key().public_bytes(
        Encoding.DER, PublicFormat.SubjectPublicKeyInfo
    )
    expected_binding = {
        "type": "tls_spki_sha256",
        "origin": fixture["origin"],
        "spki_sha256": hashlib.sha256(spki).hexdigest(),
    }
    if output.get("channel_bindings") != [expected_binding]:
        failures.append(f"{name}: unexpected channel bindings {output.get('channel_bindings')!r}")
    if output.get("attested_scope") != "router":
        failures.append(f"{name}: c8s must declare router scope")
    claims = output.get("provider_claims") or {}
    release = fixture["attestation"]["release"]
    for key, value in {
        "tcb_status": "UpToDate",
        "gpu_verified": False,
        "release_id": release["id"],
        "bundle_sha256": release["bundleSha256"],
        "mesh_ca_sha256": fixture["attestation"]["c8s"]["meshCaSha256"],
        "front_door_mode": "acme",
        "operator_key_armed": True,
        "rtmr3_source": "observed-live",
    }.items():
        if claims.get(key) != value:
            failures.append(f"{name}: provider claim {key}={claims.get(key)!r}, expected {value!r}")
    if claims.get("registry_entry", {}).get("release_id") != release["id"]:
        failures.append(f"{name}: provider claims do not name the matched registry entry")
    evidence = output.get("evidence") or {}
    data = base64.b64decode(evidence.get("data", "").split(",", 1)[-1])
    if evidence.get("digest") != f"sha256:{hashlib.sha256(data).hexdigest()}":
        failures.append(f"{name}: evidence digest does not cover the evidence bytes")
    if _leaf(fixture) not in data:
        failures.append(f"{name}: evidence omits the serving leaf the transcript was checked against")


def _check_quote_decoding(failures: list[str]) -> None:
    quote = base64.b64decode(_receipt(PRODUCTION["attestation"])["evidence"]["quote"])
    encodings = {
        "hex": quote.hex(),
        "base64": base64.b64encode(quote).decode(),
        "base64url": base64.urlsafe_b64encode(quote).rstrip(b"=").decode(),
    }
    for name, text in encodings.items():
        try:
            if c8s._decode_quote(text) != quote:
                failures.append(f"quote-{name}: decoded bytes differ")
        except ValueError as exc:
            failures.append(f"quote-{name}: rejected a valid encoding: {exc}")
    for name, text in {
        "mixed-alphabet": encodings["base64"][:-4] + "-_AA",
        "not-tdx": base64.b64encode(b"\x03\x00" + quote[2:]).decode(),
        "truncated": quote[:200].hex(),
    }.items():
        try:
            c8s._decode_quote(text)
            failures.append(f"quote-{name}: accepted an invalid quote encoding")
        except ValueError:
            pass


def _check_registry_fails_closed(failures: list[str], tmp: Path) -> None:
    document = json.loads(c8s._REGISTRY_PATH.read_text(encoding="utf-8"))
    for release in document["releases"].values():
        release["accepted_rtmr3"] = []
    path = tmp / "c8s.json"
    path.write_text(json.dumps(document), encoding="utf-8")
    try:
        c8s._load_registry(path)
        failures.append("registry: accepted a release without an explicit RTMR3 set")
    except ValueError:
        pass


def _check_fetch_policy(failures: list[str]) -> None:
    class Response:
        def __init__(self, status: int, content_type: str, body: bytes):
            self.status = status
            self._content_type = content_type
            self._body = body

        def getheader(self, name: str):
            return self._content_type if name == "content-type" else None

        def read(self, amount: int) -> bytes:
            return self._body[:amount]

    def connection_class(response: Response, requested: list[str]):
        class Socket:
            def getpeercert(self, binary_form: bool):
                return _leaf(PRODUCTION)

        class Connection:
            def __init__(self, host, port, timeout, context):
                if context.verify_mode.name != "CERT_REQUIRED":
                    raise AssertionError("attestation fetch must verify the TLS certificate")
                self.sock = Socket()

            def connect(self):
                pass

            def request(self, method, path, headers):
                requested.append(f"{method} {path}")

            def getresponse(self):
                return response

            def close(self):
                pass

        return Connection

    endpoint = c8s.parse_root_https_origin("https://api.confidential.ai", "Confidential AI")
    original = c8s.http.client.HTTPSConnection
    try:
        requested: list[str] = []
        c8s.http.client.HTTPSConnection = connection_class(
            Response(200, "application/json", b"{}"), requested
        )
        capture = c8s._fetch_attestation(endpoint, "n0nce", 5)
        if requested != ["GET /attestation?nonce=n0nce"] or capture.serving_leaf_der != _leaf(
            PRODUCTION
        ):
            failures.append(f"fetch: unexpected request or leaf capture {requested!r}")
        for name, response, reason in (
            ("status", Response(503, "application/json", b"{}"), "HTTP 503"),
            ("content-type", Response(200, "text/html", b"{}"), "expected application/json"),
            (
                "size",
                Response(200, "application/json", b"x" * (c8s._MAX_ATTESTATION_BYTES + 1)),
                "exceeds",
            ),
        ):
            c8s.http.client.HTTPSConnection = connection_class(response, [])
            try:
                c8s._fetch_attestation(endpoint, "n0nce", 5)
                failures.append(f"fetch-{name}: accepted an invalid response")
            except ValueError as exc:
                if reason not in str(exc):
                    failures.append(f"fetch-{name}: unexpected error {exc}")
    finally:
        c8s.http.client.HTTPSConnection = original


def check(tmp: Path) -> list[str]:
    failures: list[str] = []
    _check_verified(failures, "production", PRODUCTION)
    _check_verified(failures, "candidate", CANDIDATE)
    _check_quote_decoding(failures)
    _check_registry_fails_closed(failures, tmp)
    _check_fetch_policy(failures)

    prod_release = PRODUCTION["attestation"]["release"]["id"]
    cand_release = CANDIDATE["attestation"]["release"]["id"]
    candidate_rtmr3 = next(iter(c8s._load_registry()[cand_release].accepted_rtmr3))
    other_leaf = _leaf(CANDIDATE)
    other_leaf_hash = c8s._b64url(hashlib.sha256(other_leaf).digest())
    fresh_nonce = c8s._b64url(b"\x07" * 32)
    cand_chain = _receipt(CANDIDATE["attestation"])["cds_cert_pem"]
    prod_chain = _receipt(PRODUCTION["attestation"])["cds_cert_pem"]
    foreign_ca_chain = (
        prod_chain[: prod_chain.index("-----END CERTIFICATE-----") + 26]
        + cand_chain[cand_chain.index("-----END CERTIFICATE-----") + 26 :]
    )

    def replay_with_fresh_nonce(document: dict[str, Any]) -> None:
        document["nonce"] = fresh_nonce
        _receipt(document)["nonce"] = fresh_nonce

    def bad_signature(document: dict[str, Any]) -> None:
        proof = _receipt(document)["identity_proof"]
        signature = bytearray(c8s._b64url_decode(proof["signature"], "signature"))
        signature[-1] ^= 1
        proof["signature"] = c8s._b64url(bytes(signature))

    def flip_report_data_byte(quote: bytes) -> bytes:
        tampered = bytearray(quote)
        tampered[48 + 520] ^= 1
        return bytes(tampered)

    cases: list[tuple[str, str, dict[str, Any]]] = [
        ("non-https", "must use https", {"origin": "http://api.confidential.ai"}),
        ("path", "must not include a path", {"origin": "https://api.confidential.ai/v1"}),
        # Serving leaf binding.
        ("swapped-leaf", "does not name the TLS leaf", {"serving_leaf": other_leaf}),
        (
            "swapped-leaf-rewritten-receipt",
            "identity proof signature does not verify",
            {
                "serving_leaf": other_leaf,
                "body": _set("frontDoor.receipt.serving_leaf_sha256", other_leaf_hash),
            },
        ),
        # Nonce freshness.
        ("wrong-nonce", "attestation nonce does not match", {"client_nonce": fresh_nonce}),
        (
            "wrong-receipt-nonce",
            "receipt nonce does not match",
            {"body": _set("frontDoor.receipt.nonce", fresh_nonce)},
        ),
        (
            "replayed-receipt",
            "identity proof signature does not verify",
            {"client_nonce": fresh_nonce, "body": replay_with_fresh_nonce},
        ),
        # report_data and quote integrity.
        (
            "report-data",
            "report_data does not match the attest-lb transcript",
            {"verified": _td("report_data", "ab" * 48 + "00" * 16)},
        ),
        (
            "report-data-padding",
            "padding is not zero",
            {
                "verified": lambda result: result["report"]["TD10"].update(
                    report_data=result["report"]["TD10"]["report_data"][:96] + "01" * 16
                )
            },
        ),
        ("quote-bytes", "Verification failed", {"quote_bytes": flip_report_data_byte}),
        ("debug", "debug mode", {"verified": _td("td_attributes", "0100001000000000")}),
        ("tcb-out-of-date", "TCB status is 'OutOfDate'", {"verified": _set("status", "OutOfDate")}),
        (
            "tcb-sw-hardening",
            "TCB status is 'SWHardeningNeeded'",
            {"verified": _set("status", "SWHardeningNeeded")},
        ),
        # Reviewed measurements.
        ("mrtd", "MRTD does not match", {"verified": _td("mr_td", "11" * 48)}),
        ("rtmr1", "RTMR1 does not match", {"verified": _td("rt_mr1", "11" * 48)}),
        ("rtmr2", "RTMR2 does not match", {"verified": _td("rt_mr2", "11" * 48)}),
        ("rtmr3-zero", "RTMR3", {"verified": _td("rt_mr3", ZERO_48)}),
        (
            "rtmr3-not-accepted",
            "not in the accepted set",
            {"registry": _pin(prod_release, accepted_rtmr3={ZERO_48: {"source": "test"}})},
        ),
        # An RTMR3 accepted for another release does not carry over.
        ("rtmr3-other-release", "RTMR3", {"verified": _td("rt_mr3", candidate_rtmr3)}),
        # Release registry and policy.
        (
            "unknown-release",
            "not in the reviewed registry",
            {"body": _set("release.id", "v9.9.9")},
        ),
        (
            "bundle-digest",
            "bundleSha256 does not match",
            {"body": _set("release.bundleSha256", "sha256:" + "00" * 32)},
        ),
        (
            "allowlist",
            "is not accepted for release",
            {"body": _set("c8s.activeAllowlist.sha256", "sha256:" + "00" * 32)},
        ),
        ("webpki", "does not keep the serving key inside the TEE", {"body": _set("tls.mode", "webpki")}),
        (
            "webpki-front-door",
            "front_door_mode 'webpki'",
            {"body": _set("frontDoor.receipt.front_door_mode", "webpki")},
        ),
        ("no-front-door", "missing frontDoor", {"body": _set("frontDoor", None)}),
        # Mesh identity.
        ("identity-signature", "identity proof signature does not verify", {"body": bad_signature}),
        (
            "mesh-ca-hash",
            "meshCaSha256 does not match",
            {"body": _set("c8s.meshCaSha256", "sha256:" + "00" * 32)},
        ),
        (
            "mesh-ca-foreign",
            "does not chain to the mesh CA",
            {"body": _set("frontDoor.receipt.cds_cert_pem", foreign_ca_chain)},
        ),
        (
            "mesh-leaf-expired",
            "outside its validity period",
            {"clock_offset": timedelta(days=2)},
        ),
    ]
    for name, reason, kwargs in cases:
        _expect_failure(failures, name, reason, **kwargs)

    _expect_failure(
        failures,
        "candidate-mesh-ca-not-pinned",
        "is not accepted for release",
        fixture=CANDIDATE,
        registry=_pin(cand_release, accepted_mesh_ca_sha256=frozenset({"sha256:" + "00" * 32})),
    )
    return failures


def main() -> int:
    import tempfile

    with tempfile.TemporaryDirectory(prefix="c8s-soundness-") as tmp:
        failures = check(Path(tmp))
    if failures:
        for failure in failures:
            print(f"FAIL {failure}")
        return 1
    print("c8s soundness: all checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
