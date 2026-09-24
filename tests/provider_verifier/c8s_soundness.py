#!/usr/bin/env python3
"""Hermetic soundness checks for the Confidential AI (c8s) verifier bridge.

The fixtures are live `/attestation` responses from `api.confidential.ai` and
`candidate.api.confidential.ai`, each captured together with the TLS leaf of the
same connection and the Intel DCAP collateral (one per FMSPC) valid at capture
time. Only the network is replaced: every TDX quote (the front door and each
workload receipt) is verified with the real `dcap_qvl` against that collateral
at the capture time, and every gateway policy check runs as in production. Each
tamper case must fail closed.

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
FRONT_DOOR = "frontDoor"
ALL_QUOTES = "*"
REQUIRED = ("gateway", "inference-worker-0", "inference-worker-1", "sglang-router")
MOUNTER = "gateway-state-mounter"


def _fixture(name: str) -> dict[str, Any]:
    return json.loads((FIXTURES / f"{name}.json").read_text(encoding="utf-8"))


PRODUCTION = _fixture("production")
CANDIDATE = _fixture("candidate")


def _leaf(fixture: dict[str, Any]) -> bytes:
    return base64.b64decode(fixture["serving_leaf_der_base64"])


def _receipt(body: dict[str, Any]) -> dict[str, Any]:
    return body["frontDoor"]["receipt"]


def _entry(body: dict[str, Any], target: str) -> dict[str, Any]:
    return next(entry for entry in body["receipts"] if entry["target"] == target)


def _quote_targets(body: dict[str, Any]) -> dict[bytes, str]:
    """Map each quote in a (possibly tampered) body to the target it belongs to."""
    receipts = [(FRONT_DOOR, (body.get("frontDoor") or {}).get("receipt"))]
    receipts += [(entry["target"], entry["receipt"]) for entry in body.get("receipts") or []]
    quotes: dict[bytes, str] = {}
    for target, receipt in receipts:
        if receipt:
            quotes.setdefault(base64.b64decode(receipt["evidence"]["quote"]), target)
    return quotes


@dataclasses.dataclass
class _Result:
    output: dict[str, Any]
    collateral_fetches: int


def _run_full(
    fixture: dict[str, Any] = PRODUCTION,
    *,
    origin: str | None = None,
    client_nonce: str | None = None,
    serving_leaf: bytes | None = None,
    body: Callable[[dict[str, Any]], None] | None = None,
    verified: Callable[[dict[str, Any]], None] | None = None,
    quote_bytes: Callable[[bytes], bytes] | None = None,
    tamper_target: str = FRONT_DOOR,
    registry: Callable[[dict[str, c8s._ReleasePin]], None] | None = None,
    clock_offset: timedelta = timedelta(0),
) -> _Result:
    """Run the bridge on a fixture; `verified`/`quote_bytes` hit `tamper_target`'s quote."""
    attestation = copy.deepcopy(fixture["attestation"])
    if body is not None:
        body(attestation)
    targets = _quote_targets(attestation)
    captured_at = fixture["verified_at_unix"]
    collateral = {
        fmspc: dcap_qvl.QuoteCollateralV3.from_json(json.dumps(value))
        for fmspc, value in fixture["dcap_collateral"].items()
    }
    fetches: list[str] = []

    async def fetch_collateral(quote: bytes) -> Any:
        fmspc = dcap_qvl.Quote.parse(quote).fmspc()
        fetches.append(fmspc)
        return collateral[fmspc]

    real_dcap_verify = c8s._dcap_verify

    def dcap_verify(quote: bytes, quote_collateral: Any) -> dict[str, Any]:
        hit = tamper_target in (ALL_QUOTES, targets.get(quote))
        if hit and quote_bytes is not None:
            quote = quote_bytes(quote)
        result = real_dcap_verify(quote, quote_collateral)
        if hit and verified is not None:
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
        for name in (
            "_new_nonce",
            "_fetch_attestation",
            "_fetch_collateral",
            "_dcap_verify",
            "_now",
            "_load_registry",
        )
    }
    c8s._new_nonce = lambda: client_nonce or fixture["nonce"]
    c8s._fetch_attestation = fetch
    c8s._fetch_collateral = fetch_collateral
    c8s._dcap_verify = dcap_verify
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
    return _Result(json.loads(output.getvalue()), len(fetches))


def _run(fixture: dict[str, Any] = PRODUCTION, **kwargs: Any) -> dict[str, Any]:
    return _run_full(fixture, **kwargs).output


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


def _set_entry(target: str, path: str, value: Any) -> Callable[[dict[str, Any]], None]:
    def mutate(document: dict[str, Any]) -> None:
        _set(path, value)(_entry(document, target))

    return mutate


def _td(field: str, value: str) -> Callable[[dict[str, Any]], None]:
    def mutate(result: dict[str, Any]) -> None:
        result["report"]["TD10"][field] = value

    return mutate


def _pin(release_id: str, **changes: Any) -> Callable[[dict[str, c8s._ReleasePin]], None]:
    def mutate(pins: dict[str, c8s._ReleasePin]) -> None:
        pins[release_id] = dataclasses.replace(pins[release_id], **changes)

    return mutate


def _flip_b64url(value: str, index: int = -1) -> str:
    raw = bytearray(c8s._b64url_decode(value, "field"))
    raw[index] ^= 1
    return c8s._b64url(bytes(raw))


def _check_verified(
    failures: list[str], name: str, fixture: dict[str, Any], collateral_fetches: int
) -> None:
    result = _run_full(fixture)
    output = result.output
    if output.get("result") != "verified":
        failures.append(f"{name}: expected verified, got {output.get('reason')!r}")
        return
    # Quotes on the same FMSPC share one collateral fetch.
    if result.collateral_fetches != collateral_fetches:
        failures.append(
            f"{name}: fetched collateral {result.collateral_fetches} times, "
            f"expected once per FMSPC ({collateral_fetches})"
        )
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
    pin = c8s._load_registry()[release["id"]]
    for key, value in {
        "tcb_status": "UpToDate",
        "gpu_verified": False,
        "release_id": release["id"],
        "bundle_sha256": release["bundleSha256"],
        "mesh_ca_sha256": fixture["attestation"]["c8s"]["meshCaSha256"],
        "front_door_mode": "acme",
        "operator_key_armed": True,
        "rtmr3_source": "observed-live",
        "workload_attestation_protocol": pin.workload_attestation_protocol,
    }.items():
        if claims.get(key) != value:
            failures.append(f"{name}: provider claim {key}={claims.get(key)!r}, expected {value!r}")
    if claims.get("registry_entry", {}).get("release_id") != release["id"]:
        failures.append(f"{name}: provider claims do not name the matched registry entry")
    # Both live allowlists admit the plaintext path with env and mounts `any`.
    admission = claims.get("admission_env_mounts") or {}
    if (
        claims.get("admission_env_mounts_pinned") is not False
        or claims.get("require_pinned_env_mounts") is not False
        or sorted(admission) != sorted((*REQUIRED, MOUNTER))
        or any(item.get("pinned") is not False for item in admission.values())
        or admission.get("gateway", {}).get("allowlist_entry")
        != next(b.identity for b in pin.required_workloads if b.target == "gateway")
    ):
        failures.append(f"{name}: unexpected admission env/mounts claims {admission!r}")
    workloads = claims.get("verified_workloads") or {}
    if sorted(workloads) != sorted(REQUIRED):
        failures.append(f"{name}: verified_workloads covers {sorted(workloads)!r}")
    for binding in pin.required_workloads:
        workload = workloads.get(binding.target) or {}
        expected = {
            "identity": binding.identity,
            "workload": binding.workload,
            "release_id": pin.release_id,
            "tcb_status": "UpToDate",
            "mrtd": pin.mrtd,
            "rtmr1": pin.rtmr1,
            "rtmr2": pin.rtmr2,
            "operator_key_armed": True,
        }
        mismatched = {key for key, value in expected.items() if workload.get(key) != value}
        if mismatched or workload.get("rtmr3") not in pin.accepted_rtmr3:
            failures.append(f"{name}: verified_workloads[{binding.target!r}] = {workload!r}")
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


def _check_matched_workload_parser(failures: list[str]) -> None:
    """The stamp parser accepts the c8s-verify-js golden vector and nothing looser."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec

    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(x509.NameOID.COMMON_NAME, "stamp-test")])
    now = datetime.now(timezone.utc)

    def leaf(*stamps: bytes) -> x509.Certificate:
        builder = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(key.public_key())
            .serial_number(1)
            .not_valid_before(now)
            .not_valid_after(now + timedelta(days=1))
        )
        for stamp in stamps:
            builder = builder.add_extension(
                x509.UnrecognizedExtension(c8s._MATCHED_WORKLOAD_OID, stamp), critical=False
            )
        return builder.sign(key, hashes.SHA256())

    def tlv(tag: int, value: bytes) -> bytes:
        return bytes([tag, len(value)]) + value

    def stamp(
        version: bytes = b"\x01",
        workload: bytes = b"api",
        allowlist_version: bytes = b"7",
        digest: bytes = b"\x11" * 32,
    ) -> bytes:
        return tlv(
            0x30,
            tlv(0x02, version)
            + tlv(0x16, workload)
            + tlv(0x16, allowlist_version)
            + tlv(0x04, digest),
        )

    # PROTOCOL.md golden vector: {v1, name "api", allowlistVersion "7", digest 0x11 x 32}.
    golden = bytes.fromhex("302d0201011603617069160137" + "0420" + "11" * 32)
    if stamp() != golden:
        failures.append("stamp-golden: test encoder does not reproduce the golden vector")
    if c8s._matched_workload(leaf(golden), "test") != ("api", "sha256:" + "11" * 32):
        failures.append("stamp-golden: parser did not accept the golden vector")
    for label, certificate in {
        "absent": leaf(),
        "trailing-byte": leaf(tlv(0x30, golden[2:] + b"\x00")),
        "version-2": leaf(stamp(version=b"\x02")),
        "long-form-length": leaf(bytes([0x30, 0x81, 0x2D]) + golden[2:]),
        "short-digest": leaf(stamp(digest=b"\x11" * 31)),
        "leading-zero-allowlist-version": leaf(stamp(allowlist_version=b"07")),
        "bad-name": leaf(stamp(workload=b"-api")),
    }.items():
        try:
            c8s._matched_workload(certificate, "test")
            failures.append(f"stamp-{label}: parser accepted a malformed stamp")
        except ValueError:
            pass


def _check_registry_fails_closed(failures: list[str], tmp: Path) -> None:
    def mutate_all(change: Callable[[dict[str, Any]], None]) -> dict[str, Any]:
        document = json.loads(c8s._REGISTRY_PATH.read_text(encoding="utf-8"))
        for release in document["releases"].values():
            change(release)
        return document

    def drop_router(release: dict[str, Any]) -> None:
        release["required_workloads"] = [
            item for item in release["required_workloads"] if item["target"] != "sglang-router"
        ]

    def drop_workers(release: dict[str, Any]) -> None:
        release["required_workloads"] = [
            item
            for item in release["required_workloads"]
            if not item["target"].startswith("inference-worker-")
        ]

    def duplicate_target(release: dict[str, Any]) -> None:
        release["required_workloads"].append(release["required_workloads"][0])

    for name, change in {
        "no-rtmr3": lambda release: release.update(accepted_rtmr3=[]),
        "no-workload-protocol": lambda release: release.pop("workload_attestation_protocol"),
        "unknown-workload-protocol": lambda release: release.update(
            workload_attestation_protocol="c8s/attest-pq/v2"
        ),
        "no-required-workloads": lambda release: release.pop("required_workloads"),
        "no-env-mounts-policy": lambda release: release.pop("require_pinned_env_mounts"),
        "env-mounts-policy-not-bool": lambda release: release.update(
            require_pinned_env_mounts="false"
        ),
        "no-router": drop_router,
        "no-workers": drop_workers,
        "duplicate-target": duplicate_target,
    }.items():
        path = tmp / f"c8s-{name}.json"
        path.write_text(json.dumps(mutate_all(change)), encoding="utf-8")
        try:
            c8s._load_registry(path)
            failures.append(f"registry-{name}: accepted an under-specified release entry")
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


def _allowlist(fixture: dict[str, Any]) -> dict[str, Any]:
    return copy.deepcopy(fixture["attestation"]["c8s"]["activeAllowlist"]["document"])


def _pin_all_env_mounts(document: dict[str, Any]) -> None:
    for entry in document["workloads"].values():
        for container in (entry.get("initContainers") or []) + (entry.get("containers") or []):
            container["env"] = {"policy": "exact", "values": {"MODE": "prod"}}
            container["mounts"] = {"policy": "deny"}


def _check_allowlist_canonicalization(failures: list[str]) -> None:
    """The allowlist digest is Go's json.Marshal of the struct, re-derived from JSON."""
    for name, fixture in (("production", PRODUCTION), ("candidate", CANDIDATE)):
        active = fixture["attestation"]["c8s"]["activeAllowlist"]
        if c8s.allowlist_canonical_sha256(active["document"]) != active["sha256"]:
            failures.append(f"allowlist-canonical-{name}: canonical digest does not reproduce")
    expected = '"a\\u003cb\\u003e\\u0026\\n\\u0001\\b\\u2028\\"é"'
    if c8s._go_json_string('a<b>&\n\x01\b\u2028"é') != expected:
        failures.append("allowlist-go-escapes: string escaping differs from Go encoding/json")
    for label, mutate in {
        "unknown-field": lambda doc: doc["workloads"][MOUNTER].update(extra=True),
        "wrong-schema": lambda doc: doc.update(schema="c8s.allowlist/v2"),
        "number": lambda doc: doc["workloads"][MOUNTER].update(label=1),
    }.items():
        document = _allowlist(PRODUCTION)
        mutate(document)
        try:
            c8s.allowlist_canonical_sha256(document)
            failures.append(f"allowlist-{label}: canonicalized an out-of-schema document")
        except ValueError:
            pass
    # Env/mounts analysis: `any` anywhere in an entry, init containers included, unpins it.
    document = _allowlist(PRODUCTION)
    _pin_all_env_mounts(document)
    if not c8s._admission_env_mounts(document, MOUNTER)["pinned"]:
        failures.append("admission-pinned: exact env and denied mounts must count as pinned")
    entry = next(e for e in document["workloads"].values() if e.get("initContainers"))
    entry["initContainers"][0]["mounts"] = {"policy": "any"}
    name = next(n for n, e in document["workloads"].items() if e is entry)
    if c8s._admission_env_mounts(document, name)["pinned"]:
        failures.append("admission-init-any: an init container with mounts any must unpin")


def _check_rtmr3_zero_readiness(failures: list[str]) -> None:
    """A future release that removes the operator key pins RTMR3 to zero.

    No such release exists, so this uses a hypothetical registry entry. Today's
    quotes (operator key armed, non-zero RTMR3) must fail against it; quotes
    that report a zero RTMR3 on every node must pass and report the key as not
    armed, which the Rust claim mapper turns into an asserted `os_known_good`.
    """
    zero_only = {ZERO_48: {"value": ZERO_48, "source": "hypothetical", "operator_key_armed": False}}
    for name, fixture in (("production", PRODUCTION), ("candidate", CANDIDATE)):
        release_id = fixture["attestation"]["release"]["id"]
        hypothetical = _pin(release_id, accepted_rtmr3=zero_only)
        _expect_failure(
            failures,
            f"rtmr3-zero-release-{name}",
            "is not in the accepted set",
            fixture=fixture,
            registry=hypothetical,
        )
        output = _run(
            fixture,
            registry=hypothetical,
            verified=_td("rt_mr3", ZERO_48),
            tamper_target=ALL_QUOTES,
        )
        claims = output.get("provider_claims") or {}
        workloads = claims.get("verified_workloads") or {}
        if (
            output.get("result") != "verified"
            or claims.get("operator_key_armed") is not False
            or claims.get("rtmr3") != ZERO_48
            or any(
                workload.get("operator_key_armed") is not False for workload in workloads.values()
            )
        ):
            failures.append(
                f"rtmr3-zero-release-{name}: a zero-RTMR3 release must verify with the operator "
                f"key disarmed, got {output.get('result')!r} {output.get('reason')!r}"
            )
        # One node still armed keeps the whole upstream armed.
        output = _run(
            fixture,
            registry=_pin(
                release_id,
                accepted_rtmr3={
                    **zero_only,
                    **c8s._load_registry()[release_id].accepted_rtmr3,
                },
            ),
            verified=_td("rt_mr3", ZERO_48),
            tamper_target=FRONT_DOOR,
        )
        if (output.get("provider_claims") or {}).get("operator_key_armed") is not True:
            failures.append(f"rtmr3-mixed-{name}: an armed worker node must keep the key armed")


def check(tmp: Path) -> list[str]:
    failures: list[str] = []
    _check_verified(failures, "production", PRODUCTION, collateral_fetches=1)
    _check_verified(failures, "candidate", CANDIDATE, collateral_fetches=2)
    _check_quote_decoding(failures)
    _check_matched_workload_parser(failures)
    _check_registry_fails_closed(failures, tmp)
    _check_fetch_policy(failures)
    _check_rtmr3_zero_readiness(failures)
    _check_allowlist_canonicalization(failures)

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
    worker = "inference-worker-0"

    def replay_with_fresh_nonce(document: dict[str, Any]) -> None:
        document["nonce"] = fresh_nonce
        _receipt(document)["nonce"] = fresh_nonce

    def bad_signature(document: dict[str, Any]) -> None:
        proof = _receipt(document)["identity_proof"]
        proof["signature"] = _flip_b64url(proof["signature"])

    def flip_report_data_byte(quote: bytes) -> bytes:
        tampered = bytearray(quote)
        tampered[48 + 520] ^= 1
        return bytes(tampered)

    def drop_worker(document: dict[str, Any]) -> None:
        document["receipts"] = [
            entry for entry in document["receipts"] if entry["target"] != "inference-worker-1"
        ]

    def duplicate_gateway(document: dict[str, Any]) -> None:
        document["receipts"].append(copy.deepcopy(_entry(document, "gateway")))

    def unreviewed_worker(document: dict[str, Any]) -> None:
        extra = copy.deepcopy(_entry(document, worker))
        extra["target"] = "inference-worker-2"
        document["receipts"].append(extra)

    def no_receipts(document: dict[str, Any]) -> None:
        document.pop("receipts")

    def worker_session_key(document: dict[str, Any]) -> None:
        keys = _entry(document, worker)["receipt"]["session_pubkey"]
        keys["x25519"] = _flip_b64url(keys["x25519"])

    def worker_xwing_ct(document: dict[str, Any]) -> None:
        receipt = _entry(document, worker)["receipt"]
        receipt["xwing_ct"] = _flip_b64url(receipt["xwing_ct"])

    def worker_short_session_id(document: dict[str, Any]) -> None:
        receipt = _entry(document, worker)["receipt"]
        receipt["session_id"] = c8s._b64url(b"\x01" * 8)

    def worker_bad_signature(document: dict[str, Any]) -> None:
        proof = _entry(document, worker)["receipt"]["identity_proof"]
        proof["signature"] = _flip_b64url(proof["signature"])

    def worker_other_cluster_ca(document: dict[str, Any]) -> None:
        # A genuine chain from the candidate cluster: valid on its own, different CA.
        other = _entry(CANDIDATE["attestation"], worker)["receipt"]["cds_cert_pem"]
        _entry(document, worker)["receipt"]["cds_cert_pem"] = other

    def worker_foreign_ca(document: dict[str, Any]) -> None:
        receipt = _entry(document, worker)["receipt"]
        chain = receipt["cds_cert_pem"]
        leaf = chain[: chain.index("-----END CERTIFICATE-----") + 26]
        other_ca = cand_chain[cand_chain.index("-----END CERTIFICATE-----") + 26 :]
        receipt["cds_cert_pem"] = leaf + other_ca

    def swap_worker_receipts(document: dict[str, Any]) -> None:
        # Relabel two genuine worker receipts; only the CA-stamped leaf reveals the swap.
        first, second = _entry(document, "inference-worker-0"), _entry(
            document, "inference-worker-1"
        )
        first["receipt"], second["receipt"] = second["receipt"], first["receipt"]

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
            "report_data does not match the c8s/attest-lb/v1 transcript",
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
        # Required workloads: every plaintext-path target, exactly once.
        ("no-receipts", "missing receipts", {"body": no_receipts}),
        (
            "worker-missing",
            "missing the receipt for workload 'inference-worker-1'",
            {"body": drop_worker},
        ),
        ("gateway-duplicated", "'gateway' has more than one receipt", {"body": duplicate_gateway}),
        (
            "worker-unreviewed",
            "'inference-worker-2' is not a reviewed target",
            {"body": unreviewed_worker},
        ),
        (
            "declared-protocol",
            "c8s.attestationProtocol",
            {"body": _set("c8s.attestationProtocol", "c8s/attest-pq/v1+xwing")},
        ),
        # Workload identity.
        (
            "worker-identity-label",
            "identity 'gateway-db0b6ee' does not match",
            {"body": _set_entry(worker, "identity", "gateway-db0b6ee")},
        ),
        (
            "worker-workload-label",
            "workload 'sglang-router-684393d' does not match",
            {"body": _set_entry(worker, "workload", "sglang-router-684393d")},
        ),
        (
            "worker-receipts-swapped",
            "matched-workload stamp 'inference-worker-1-tool-calls-v1' does not match",
            {"body": swap_worker_receipts},
        ),
        # Workload receipt freshness and transcript.
        (
            "worker-nonce",
            "workload 'inference-worker-0' receipt nonce does not match",
            {"body": _set_entry(worker, "receipt.nonce", fresh_nonce)},
        ),
        (
            "worker-version",
            "receipt is not c8s/attest-pq/v1",
            {"body": _set_entry(worker, "receipt.version", "c8s/attest-lb/v1")},
        ),
        (
            "worker-session-key",
            "workload 'inference-worker-0' identity proof signature does not verify",
            {"body": worker_session_key},
        ),
        (
            "worker-front-door-mode",
            "workload 'inference-worker-0' identity proof signature does not verify",
            {"body": _set_entry(worker, "receipt.front_door_mode", "cds")},
        ),
        (
            "worker-signature",
            "workload 'inference-worker-0' identity proof signature does not verify",
            {"body": worker_bad_signature},
        ),
        (
            "worker-report-data",
            "workload 'inference-worker-0' report_data does not match the "
            "c8s/attest-pq/v1 transcript",
            {"verified": _td("report_data", "ab" * 48 + "00" * 16), "tamper_target": worker},
        ),
        (
            "worker-quote-bytes",
            "workload 'inference-worker-0' quote failed DCAP verification",
            {"quote_bytes": flip_report_data_byte, "tamper_target": worker},
        ),
        # Workload mesh CA.
        (
            "worker-other-cluster-ca",
            "workload 'inference-worker-0' mesh CA is not the front door's mesh CA",
            {"body": worker_other_cluster_ca},
        ),
        (
            "worker-foreign-ca",
            "workload 'inference-worker-0' mesh CA is not the front door's mesh CA",
            {"body": worker_foreign_ca},
        ),
        # Workload node policy.
        (
            "worker-debug",
            "workload 'inference-worker-0' TD runs in debug mode",
            {"verified": _td("td_attributes", "0100001000000000"), "tamper_target": worker},
        ),
        (
            "worker-tcb-out-of-date",
            "workload 'inference-worker-0' TDX TCB status is 'OutOfDate'",
            {"verified": _set("status", "OutOfDate"), "tamper_target": worker},
        ),
        (
            "router-tcb-sw-hardening",
            "workload 'sglang-router' TDX TCB status is 'SWHardeningNeeded'",
            {"verified": _set("status", "SWHardeningNeeded"), "tamper_target": "sglang-router"},
        ),
        (
            "worker-mrtd",
            "workload 'inference-worker-0' MRTD does not match",
            {"verified": _td("mr_td", "11" * 48), "tamper_target": worker},
        ),
        (
            "worker-rtmr1",
            "workload 'inference-worker-0' RTMR1 does not match",
            {"verified": _td("rt_mr1", "11" * 48), "tamper_target": worker},
        ),
        (
            "gateway-rtmr2",
            "workload 'gateway' RTMR2 does not match",
            {"verified": _td("rt_mr2", "11" * 48), "tamper_target": "gateway"},
        ),
        (
            "worker-rtmr3-zero",
            "workload 'inference-worker-0' RTMR3 " + ZERO_48,
            {"verified": _td("rt_mr3", ZERO_48), "tamper_target": worker},
        ),
        (
            "worker-rtmr3-other-release",
            "workload 'inference-worker-0' RTMR3",
            {"verified": _td("rt_mr3", candidate_rtmr3), "tamper_target": worker},
        ),
    ]
    # Admission allowlist: the inspected document must be the one the digests name.
    def tampered_allowlist(document: dict[str, Any]) -> None:
        allowlist = document["c8s"]["activeAllowlist"]["document"]
        _pin_all_env_mounts(allowlist)

    def rehashed_allowlist(document: dict[str, Any]) -> None:
        active = document["c8s"]["activeAllowlist"]
        _pin_all_env_mounts(active["document"])
        active["sha256"] = c8s.allowlist_canonical_sha256(active["document"])

    prod_pinned_doc = _allowlist(PRODUCTION)
    _pin_all_env_mounts(prod_pinned_doc)
    prod_pinned_sha = c8s.allowlist_canonical_sha256(prod_pinned_doc)
    prod_allowlists = c8s._load_registry()[prod_release].accepted_allowlist_sha256
    cases += [
        (
            "allowlist-document-tampered",
            "c8s.activeAllowlist.document does not hash to c8s.activeAllowlist.sha256",
            {"body": tampered_allowlist},
        ),
        (
            "allowlist-document-missing",
            "missing c8s.activeAllowlist.document",
            {"body": _set("c8s.activeAllowlist.document", None)},
        ),
        # A self-consistent substitute (even one the registry accepted) is not
        # the snapshot the workloads' CA-signed stamps name.
        (
            "allowlist-not-stamped",
            "stamped allowlist sha256:05235c60",
            {
                "body": rehashed_allowlist,
                "registry": _pin(
                    prod_release,
                    accepted_allowlist_sha256=prod_allowlists | {prod_pinned_sha},
                ),
            },
        ),
        (
            "allowlist-entry-missing",
            "allowlist has no entry 'gateway-state-mounter-x'",
            {"registry": _pin(prod_release, additional_admission_entries=(MOUNTER + "-x",))},
        ),
    ]
    for name, reason, kwargs in cases:
        _expect_failure(failures, name, reason, **kwargs)

    # Launch policy: a release that requires pinned env/mounts rejects today's allowlists.
    for name, fixture in (("production", PRODUCTION), ("candidate", CANDIDATE)):
        release_id = fixture["attestation"]["release"]["id"]
        _expect_failure(
            failures,
            f"require-pinned-env-mounts-{name}",
            "requires pinned env and mounts, but the allowlist admits gateway, "
            "gateway-state-mounter, inference-worker-0, inference-worker-1, sglang-router",
            fixture=fixture,
            registry=_pin(release_id, require_pinned_env_mounts=True),
        )

    # Candidate uses the X-Wing transcript; its session fields are bound too.
    for name, reason, kwargs in [
        (
            "candidate-mesh-ca-not-pinned",
            "is not accepted for release",
            {
                "registry": _pin(
                    cand_release, accepted_mesh_ca_sha256=frozenset({"sha256:" + "00" * 32})
                )
            },
        ),
        (
            "candidate-worker-xwing-ct",
            "workload 'inference-worker-0' identity proof signature does not verify",
            {"body": worker_xwing_ct},
        ),
        (
            "candidate-worker-session-id-size",
            "session_id is 8 bytes, expected 16",
            {"body": worker_short_session_id},
        ),
        (
            "candidate-worker-report-data",
            "report_data does not match the c8s/attest-pq/v1+xwing transcript",
            {"verified": _td("report_data", "ab" * 48 + "00" * 16), "tamper_target": worker},
        ),
        (
            "candidate-legacy-protocol",
            "receipt session_pubkey.x25519 is not unpadded base64url",
            {
                "registry": _pin(cand_release, workload_attestation_protocol="c8s/attest-pq/v1"),
                "body": _set("c8s.attestationProtocol", None),
            },
        ),
        (
            "candidate-worker-rtmr1",
            "workload 'inference-worker-1' RTMR1 does not match",
            {"verified": _td("rt_mr1", "11" * 48), "tamper_target": "inference-worker-1"},
        ),
    ]:
        _expect_failure(failures, name, reason, fixture=CANDIDATE, **kwargs)
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
