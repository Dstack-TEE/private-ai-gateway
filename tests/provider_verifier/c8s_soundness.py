#!/usr/bin/env python3
"""Soundness checks: live v0.16.5 quote with captured Intel collateral, plus
synthetic CA chains for stamp/chain edge cases. All production policy checks
run; only the network, clock and (for synthetic cases) verified report are
substituted. Run with uv run --frozen python tests/provider_verifier/c8s_soundness.py.
"""
from __future__ import annotations

import asyncio
import base64
import copy
import hashlib
import io
import json
import sys
from contextlib import redirect_stdout
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable
from unittest.mock import patch

import dcap_qvl
from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import provider_verifier.c8s as c8s

FIXTURE = json.loads((ROOT / "tests/fixtures/c8s/v0.16.5.json").read_text())
SUBJECTS = FIXTURE["accepted_subjects"]
WORKLOAD = next(s.removeprefix("c8s-workload:") for s in SUBJECTS if s.startswith("c8s-workload:"))
ORIGIN = FIXTURE["origin"]
LEAF = base64.b64decode(FIXTURE["serving_leaf_der_base64"])
BODIES = {name: base64.b64decode(body) for name, body in FIXTURE["responses"].items()}
ATTEST_LB = json.loads(BODIES["attest-lb"])
ALLOWLIST = BODIES["allowlist"]
ALLOWLIST_SHA256 = "sha256:" + hashlib.sha256(ALLOWLIST).hexdigest()
CAPTURED_AT = datetime.fromtimestamp(FIXTURE["verified_at_unix"], timezone.utc)
NONCE_RAW = c8s._b64url_decode(FIXTURE["nonce"], "nonce")
Mutation = Callable[[dict[str, Any]], None]


def _run(*, origin=ORIGIN, client_nonce=None, serving_leaf=LEAF,
         receipt=None, allowlist=ALLOWLIST, routes=BODIES["routes"],
         verified=None, quote_bytes=None, subjects=None,
         clock_offset=timedelta(0)):
    attest_lb = copy.deepcopy(ATTEST_LB)
    if receipt:
        receipt(attest_lb)
    pins = SUBJECTS.copy()
    if subjects:
        subjects(pins)
    collateral = dcap_qvl.QuoteCollateralV3.from_json(json.dumps(FIXTURE["dcap_collateral"]))

    async def fetch_collateral(quote):
        return collateral

    real_verify = c8s._dcap_verify
    def dcap_verify(quote, collateral):
        result = real_verify(quote_bytes(quote) if quote_bytes else quote, collateral)
        if verified:
            verified(result)
        return result

    def fetch(endpoint, nonce, timeout):
        return c8s._Capture(serving_leaf, {
            name: c8s._Response(ORIGIN + "/" + name, "application/json", body)
            for name, body in {"attest-lb": json.dumps(attest_lb).encode(),
                               "allowlist": allowlist, "routes": routes}.items()
        })
    output = io.StringIO()
    with patch.multiple(c8s, _new_nonce=lambda: client_nonce or FIXTURE["nonce"],
                        _fetch=fetch, _fetch_collateral=fetch_collateral,
                        _dcap_verify=dcap_verify, _now=lambda: CAPTURED_AT + clock_offset):
        with redirect_stdout(output):
            asyncio.run(c8s.verify_c8s({"provider": "confidential-ai", "url_origin": origin,
                "model_id": "provider-model", "provider_options": {
                    "confidential_ai_accepted_subject:" + pin: "true" for pin in pins}}))
    return json.loads(output.getvalue())


def _set(path: str, value: Any) -> Mutation:
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


def _flip_b64url(value: str) -> str:
    raw = bytearray(c8s._b64url_decode(value, "field"))
    raw[-1] ^= 1
    return c8s._b64url(bytes(raw))


def _tlv(tag: int, value: bytes) -> bytes:
    return bytes([tag, len(value)]) + value


def _stamp(
    name: bytes = WORKLOAD.encode(),
    version: bytes = b"5",
    digest: bytes = bytes.fromhex(ALLOWLIST_SHA256[7:]),
    format_version: bytes = b"\x01",
) -> bytes:
    return _tlv(
        0x30,
        _tlv(0x02, format_version) + _tlv(0x16, name) + _tlv(0x16, version) + _tlv(0x04, digest),
    )


def _der(tag, body):
    n = len(body)
    size = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([tag]) + (bytes([n]) if n < 128 else bytes([0x80 | len(size)]) + size) + body


def _parts(body):
    parts = []
    while body:
        n = body[1]
        offset = 2
        if n & 0x80:
            offset += n & 0x7f
            n = int.from_bytes(body[2:offset], "big")
        parts.append((body[0], body[offset:offset + n]))
        body = body[offset + n:]
    return parts


def _duplicate_stamp(cert, issuer_key):
    # Construct a correctly signed certificate containing duplicate stamp OIDs;
    # cryptography's CertificateBuilder intentionally refuses to create one.
    cert_parts = _parts(_parts(cert.public_bytes(Encoding.DER))[0][1])
    tbs = _parts(cert_parts[0][1])
    extension_wrapper = _parts(tbs[-1][1])[0]
    extensions = extension_wrapper[1]
    last_extension = _parts(extensions)[-1]
    tbs[-1] = (0xa3, _der(0x30, extensions + _der(*last_extension)))
    encoded_tbs = _der(0x30, b"".join(_der(*p) for p in tbs))
    signature = issuer_key.sign(encoded_tbs, ec.ECDSA(hashes.SHA384()))
    return x509.load_der_x509_certificate(_der(0x30, encoded_tbs + _der(*cert_parts[1])
        + _der(0x03, b"\x00" + signature)))


def _certificate(
    subject: str,
    key: ec.EllipticCurvePrivateKey,
    issuer: str,
    issuer_key: ec.EllipticCurvePrivateKey,
    *,
    ca: bool,
    stamp: bytes | None = None,
    not_after: datetime | None = None,
) -> x509.Certificate:
    builder = (
        x509.CertificateBuilder()
        .subject_name(x509.Name([x509.NameAttribute(x509.NameOID.COMMON_NAME, subject)]))
        .issuer_name(x509.Name([x509.NameAttribute(x509.NameOID.COMMON_NAME, issuer)]))
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(CAPTURED_AT - timedelta(hours=1))
        .not_valid_after(not_after or CAPTURED_AT + timedelta(hours=6))
        .add_extension(x509.BasicConstraints(ca=ca, path_length=None), critical=True)
    )
    if stamp is not None:
        builder = builder.add_extension(
            x509.UnrecognizedExtension(c8s._MATCHED_WORKLOAD_OID, stamp), critical=False
        )
    return builder.sign(issuer_key, hashes.SHA384())


def _pem(*certificates: x509.Certificate) -> str:
    return "".join(cert.public_bytes(Encoding.PEM).decode() for cert in certificates)


def _synthetic(
    *,
    stamp: bytes | None = None,
    leaf_issuer: str = "committed",
    committed_is_ca: bool = True,
    mode: str = "acme",
    serving_leaf: bytes = LEAF,
    include_stamp: bool = True,
    duplicate_stamp: bool = False,
) -> dict[str, Any]:
    """`_run` kwargs for a test mesh chain whose proof and report_data are consistent."""
    ca_key = ec.generate_private_key(ec.SECP384R1())
    other_key = ec.generate_private_key(ec.SECP384R1())
    leaf_key = ec.generate_private_key(ec.SECP256R1())
    committed = _certificate("test mesh CA", ca_key, "test mesh CA", ca_key, ca=committed_is_ca)
    signer = ca_key if leaf_issuer == "committed" else other_key
    leaf = _certificate(
        "test mesh leaf", leaf_key, "test mesh CA", signer, ca=False, stamp=(stamp if stamp is not None else _stamp()) if include_stamp else None
    )
    if duplicate_stamp:
        leaf = _duplicate_stamp(leaf, signer)
    leaf_der, ca_der = leaf.public_bytes(Encoding.DER), committed.public_bytes(Encoding.DER)
    transcript = c8s.attest_lb_transcript_digest(mode, NONCE_RAW, serving_leaf, leaf_der, ca_der, bytes.fromhex(ATTEST_LB["route_digest"]))

    def receipt(document: dict[str, Any]) -> None:
        document["front_door_mode"] = mode
        document["serving_leaf_sha256"] = c8s._b64url(hashlib.sha256(serving_leaf).digest())
        document["cds_cert_pem"] = _pem(leaf, committed)
        document["identity_proof"] = {
            "algorithm": "ecdsa-sha384",
            "leaf_sha256": c8s._b64url(hashlib.sha256(leaf_der).digest()),
            "mesh_ca_sha256": c8s._b64url(hashlib.sha256(ca_der).digest()),
            "signature": c8s._b64url(leaf_key.sign(transcript, ec.ECDSA(hashes.SHA384()))),
        }

    # Stands in for a genuine quote over this chain: report_data is the transcript.
    return {
        "receipt": receipt,
        "serving_leaf": serving_leaf,
        "verified": _td("report_data", (transcript + bytes(16)).hex()),
    }


def _check_stamp_parser(failures: list[str]) -> None:
    """The stamp parser accepts the c8s-verify-js golden vector and nothing looser."""
    key = ec.generate_private_key(ec.SECP256R1())

    def leaf(*stamps: bytes) -> x509.Certificate:
        name = x509.Name([x509.NameAttribute(x509.NameOID.COMMON_NAME, "stamp-test")])
        builder = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(key.public_key())
            .serial_number(1)
            .not_valid_before(CAPTURED_AT)
            .not_valid_after(CAPTURED_AT + timedelta(days=1))
        )
        for stamp in stamps:
            builder = builder.add_extension(
                x509.UnrecognizedExtension(c8s._MATCHED_WORKLOAD_OID, stamp), critical=False
            )
        return builder.sign(key, hashes.SHA256())

    golden = bytes.fromhex("302d0201011603617069160137" + "0420" + "11" * 32)
    if _stamp(b"api", b"7", b"\x11" * 32) != golden:
        failures.append("stamp-golden: test encoder does not reproduce the golden vector")
    if c8s._matched_workload(leaf(golden)) != ("api", "7", "sha256:" + "11" * 32):
        failures.append("stamp-golden: parser did not accept the golden vector")
    for label, certificate in {
        "absent": leaf(),
        "trailing-byte": leaf(_tlv(0x30, golden[2:] + b"\x00")),
        "version-2": leaf(_stamp(format_version=b"\x02")),
        "long-form-length": leaf(bytes([0x30, 0x81, 0x2D]) + golden[2:]),
        "short-digest": leaf(_stamp(digest=b"\x11" * 31)),
        "allowlist-version-zero": leaf(_stamp(version=b"0")),
        "allowlist-version-leading-zero": leaf(_stamp(version=b"07")),
        "bad-name": leaf(_stamp(name=b"-api")),
        "long-name": leaf(_stamp(name=b"a" * 64)),
    }.items():
        try:
            c8s._matched_workload(certificate)
            failures.append(f"stamp-{label}: parser accepted a malformed stamp")
        except ValueError:
            pass


def _check_fetch_policy(failures: list[str]) -> None:
    """All three GETs ride one certificate-verified connection that never reconnects."""

    class Response:
        def __init__(self, status: int, content_type: str, body: bytes):
            self.status = status
            self._content_type = content_type
            self._body = body

        def getheader(self, name: str):
            return self._content_type if name == "content-type" else None

        def read(self, amount: int) -> bytes:
            return self._body[:amount]

    def connection_class(responses: dict[str, Response], log: list[str]):
        class Socket:
            def getpeercert(self, binary_form: bool):
                return LEAF

        class Connection:
            def __init__(self, host, port, timeout, context):
                if context.verify_mode.name != "CERT_REQUIRED" or not context.check_hostname:
                    raise AssertionError("the fetch must verify the certificate and hostname")
                self.sock = Socket()
                self.auto_open = 1

            def connect(self):
                log.append("connect")

            def request(self, method, path, headers):
                if self.auto_open:
                    raise AssertionError("the fetch must not reconnect on a closed connection")
                log.append(f"{method} {path}")
                self._path = path.split("?", 1)[0]

            def getresponse(self):
                return responses.get(self._path, Response(200, "application/json", b"{}"))

            def close(self):
                pass

        return Connection

    endpoint = c8s._parse_origin(ORIGIN)
    original = c8s.http.client.HTTPSConnection
    try:
        log: list[str] = []
        c8s.http.client.HTTPSConnection = connection_class({}, log)
        capture = c8s._fetch(endpoint, "n0nce", 5)
        expected = [
            "connect",
            "GET /.well-known/c8s/attest-lb?nonce=n0nce",
            "GET /allowlist",
            "GET /.well-known/c8s/routes",
        ]
        if log != expected or capture.serving_leaf_der != LEAF:
            failures.append(f"fetch: unexpected requests or leaf capture {log!r}")
        for name, path, response, reason in (
            ("status", "/allowlist", Response(503, "application/json", b"{}"), "HTTP 503"),
            (
                "content-type",
                "/.well-known/c8s/attest-lb",
                Response(200, "text/html", b"{}"),
                "expected application/json",
            ),
            (
                "size",
                "/allowlist",
                Response(200, "application/json", b"x" * ((4 << 20) + 1)),
                "exceeds",
            ),
        ):
            c8s.http.client.HTTPSConnection = connection_class({path: response}, [])
            try:
                c8s._fetch(endpoint, "n0nce", 5)
                failures.append(f"fetch-{name}: accepted an invalid response")
            except ValueError as exc:
                if reason not in str(exc):
                    failures.append(f"fetch-{name}: unexpected error {exc}")
    finally:
        c8s.http.client.HTTPSConnection = original


def _check_routes(failures):
    manifest = json.loads(base64.b64decode(json.loads(BODIES["routes"])["manifest"]))
    def publication(manifest):
        raw = json.dumps(manifest).encode()
        return json.dumps({"manifest": base64.b64encode(raw).decode()}).encode(), hashlib.sha256(raw).digest()
    body, digest = publication(manifest)
    assert c8s._verify_routes(body, digest, "api.confidential.ai") == manifest["revision"]
    for name, mutation in [
        ("unrouted host", _set("hosts", {})),
        ("unrouted path", lambda m: m["hosts"]["api.confidential.ai"].update(routes=[])),
        ("wrong schema", _set("schema", "c8s.routes/v2")),
    ]:
        changed = copy.deepcopy(manifest)
        mutation(changed)
        body, digest = publication(changed)
        try:
            c8s._verify_routes(body, digest, "api.confidential.ai")
        except ValueError:
            continue
        failures.append(f"routes {name}: accepted invalid publication")


def _subject(kind, value):
    def replace(subjects):
        subjects[:] = [kind + ":" + value if s.startswith(kind + ":") else s for s in subjects]
    return replace


def check():
    failures = []
    _check_stamp_parser(failures)
    _check_fetch_policy(failures)
    _check_routes(failures)
    for name, kwargs in [("live fixture", {}), ("synthetic", _synthetic())]:
        out = _run(**kwargs)
        if out.get("result") != "verified":
            failures.append(f"{name}: {out.get('reason')}")
            continue
        claims = out["provider_claims"]
        assert out["attested_scope"] == "router"
        assert out["verifier_id"] == "private-ai-verifier/confidential-ai/v1"
        assert claims["continuous_policy_enforcement"] is False
        assert claims["worker_identity_verified"] is False
        assert claims["gpu_verified"] is False
        spki = x509.load_der_x509_certificate(LEAF).public_key().public_bytes(
            Encoding.DER, PublicFormat.SubjectPublicKeyInfo)
        assert out["channel_bindings"] == [{"type": "tls_spki_sha256", "origin": ORIGIN,
            "spki_sha256": hashlib.sha256(spki).hexdigest()}]
        evidence = out["evidence"]
        raw = base64.b64decode(evidence["data"].split(",", 1)[1])
        assert evidence["digest"] == "sha256:" + hashlib.sha256(raw).hexdigest()
        assert all(body in raw for body in [ALLOWLIST, LEAF, BODIES["routes"]])

    def flip_quote(quote):
        data = bytearray(quote)
        data[48 + 520] ^= 1
        return bytes(data)

    def bad_signature(receipt):
        receipt["identity_proof"]["signature"] = _flip_b64url(receipt["identity_proof"]["signature"])

    # Bind a six-field (retired) transcript with a valid identity signature.
    six = _synthetic()
    original_receipt = six["receipt"]
    def six_field(receipt):
        original_receipt(receipt)
        leaf, cas = c8s._mesh_chain(receipt)
        fields = [b"c8s/attest-lb/v1", b"acme", NONCE_RAW,
                  hashlib.sha256(LEAF).digest(), hashlib.sha256(leaf.public_bytes(Encoding.DER)).digest(),
                  hashlib.sha256(cas[0].public_bytes(Encoding.DER)).digest()]
        # A mismatching VERIFIED report_data must fail even if identity proof
        # remains valid for the seven-field protocol.
        six["digest"] = hashlib.sha384(b"".join(c8s._length_prefixed(v) for v in fields)).digest()
    six["receipt"] = six_field
    six["verified"] = lambda result: result["report"]["TD10"].update(
        report_data=(six["digest"] + bytes(16)).hex())

    cases = [
        ("nonce", "nonce does not match", {"receipt": _set("nonce", "wrong")} ),
        ("serving leaf", "does not name the TLS leaf", {"serving_leaf": b"wrong"}),
        ("six fields", "report_data does not match", six),
        ("wrong RTMR3", "RTMR3 does not match", {"verified": _td("rt_mr3", "00" * 48)}),
        ("allowlist pin", "is not in accepted_subjects", {"subjects": _subject("c8s-allowlist", "sha256:" + "00" * 32)}),
        ("allowlist bytes", "does not match the /allowlist bytes", {"allowlist": ALLOWLIST + b" "}),
        ("stamp missing", "no single matched-workload stamp", _synthetic(include_stamp=False)),
        ("stamp duplicate", "duplicate matched-workload stamps", _synthetic(duplicate_stamp=True)),
        ("wrong workload", "stamped for workload", _synthetic(stamp=_stamp(name=b"gateway"))),
        ("debug TD", "debug mode", {"verified": _td("td_attributes", "0100001000000000")}),
        ("OutOfDate", "expected 'UpToDate'", {"verified": _set("status", "OutOfDate")}),
        ("SWHardeningNeeded", "expected 'UpToDate'", {"verified": _set("status", "SWHardeningNeeded")}),
        ("webpki", "must be acme", {"receipt": _set("front_door_mode", "webpki")}),
        ("cds", "must be acme", {"receipt": _set("front_door_mode", "cds")}),
        ("identity signature", "signature does not verify", {"receipt": bad_signature}),
        ("route digest", "route digest does not match", {"routes": json.dumps({
            **json.loads(BODIES["routes"]), "manifest": base64.b64encode(b"{}").decode()}).encode()}),
        ("quote tamper", "DCAP verification", {"quote_bytes": flip_quote}),
        ("mesh chain", "not issued by", _synthetic(leaf_issuer="other")),
        ("CA", "not a certificate-signing CA", _synthetic(committed_is_ca=False)),
        ("missing pins", "accepted_subjects requires", {"subjects": lambda p: p.clear()}),
        ("partial pins", "accepted_subjects requires", {"subjects": lambda p: p.pop(0)}),
        ("duplicate pins", "duplicate mrtd pins", {"subjects": lambda p: p.append("tdx-mrtd:" + "00" * 48)}),
        ("unknown pin prefix", "accepted_subjects contains an invalid pin", {"subjects": lambda p: p.append("unknown:pin")}),
    ]
    for register, field in [("mrtd", "mr_td"), ("rtmr1", "rt_mr1"), ("rtmr2", "rt_mr2")]:
        cases.append((register, register.upper() + " does not match", {"verified": _td(field, "11" * 48)}))
    nibble_reasons = {
        "tdx-mrtd": "MRTD does not match configured pin",
        "tdx-rtmr1": "RTMR1 does not match configured pin",
        "tdx-rtmr2": "RTMR2 does not match configured pin",
        "tdx-rtmr3": "RTMR3 does not match configured pin",
        "c8s-workload": "mesh leaf is stamped for workload",
        "c8s-allowlist": f"allowlist {ALLOWLIST_SHA256} is not in accepted_subjects",
    }
    for index, subject in enumerate(SUBJECTS):
        changed = subject[:-1] + ("0" if subject[-1] != "0" else "1")
        kind = subject.split(":", 1)[0]
        cases.append(("one-nibble " + kind, nibble_reasons[kind], {
            "subjects": lambda pins, i=index, value=changed: pins.__setitem__(i, value),
        }))
    for name, reason, kwargs in cases:
        out = _run(**kwargs)
        if out.get("result") != "failed" or reason not in out.get("reason", ""):
            failures.append(f"{name}: expected {reason!r}, got {out.get('reason')!r}")
    return failures


if __name__ == "__main__":
    failures = check()
    for failure in failures:
        print("FAIL " + failure)
    if failures:
        raise SystemExit(1)
    print("c8s soundness: all checks passed")
