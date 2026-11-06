"""Confidential AI front-door verification using c8s attest-lb.

Pins are supplied through accepted_subjects. Evidence describes the front door
at verification time; forwarding uses the standard TLS SPKI verifier cache.
Protocol references: MIT TEErminator and c8s-verify-js; see verification.md.
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import hmac
import http.client
import json
import re
import secrets
import ssl
import struct
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Any
from urllib.parse import urlencode, urlsplit

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.serialization import (
    Encoding,
    PublicFormat,
)

from .common import (
    emit,
    failed,
    raw_http_bundle_evidence,
    raw_http_item,
    provider_options,
    request_timeout_seconds,
    verifier_id_for,
)

_PROVIDER = "confidential-ai"
_LABEL = "Confidential AI"
_ATTEST_LB_PATH = "/.well-known/c8s/attest-lb"
_ATTEST_LB_VERSION = "c8s/attest-lb/v1"
_IDENTITY_PROOF_ALGORITHM = "ecdsa-sha384"
# TEErminator's single clock-skew allowance, on NotBefore only.
_NOT_BEFORE_SKEW = timedelta(minutes=5)
_NONCE_BYTES = 32
# (name, path, response cap) in request order, all on one TLS connection.
_ENDPOINTS = (
    ("attest-lb", _ATTEST_LB_PATH, 1 << 20),
    ("allowlist", "/allowlist", 4 << 20),
    ("routes", "/.well-known/c8s/routes", 4 << 20),
)
_ALLOWLIST_SCHEMA = "c8s.allowlist/v1"
# Matched-workload stamp on a CDS-issued mesh leaf (TEErminator workloadext.go).
_MATCHED_WORKLOAD_OID = x509.ObjectIdentifier("1.3.6.1.4.1.66378.1.5")
_WORKLOAD_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$")
_ALLOWLIST_VERSION_RE = re.compile(r"^[1-9][0-9]{0,19}$")
# TDX v4 quote: 48-byte header followed by the TD report body.
_TDX_QUOTE_VERSION = 4
_TDX_TEE_TYPE = 0x81
_TDX_MIN_QUOTE_BYTES = 48 + 584
_HEX_48_RE = re.compile(r"^[0-9a-f]{96}$")
_SHA256_TAG_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_B64URL_RE = re.compile(r"^[A-Za-z0-9_-]+$")
_PEM_BEGIN_RE = re.compile(r"-----BEGIN ([^-]+)-----")


@dataclass(frozen=True)
class _Response:
    url: str
    content_type: str
    body: bytes


@dataclass(frozen=True)
class _Capture:
    serving_leaf_der: bytes
    responses: dict[str, _Response]


@dataclass(frozen=True)
class _Pins:
    mrtd: str
    rtmr1: str
    rtmr2: str
    rtmr3: str
    workload: str
    allowlist_sha256: tuple[str, ...]


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _new_nonce() -> str:
    return _b64url(secrets.token_bytes(_NONCE_BYTES))


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def _b64url_decode(value: Any, what: str) -> bytes:
    if not isinstance(value, str) or not _B64URL_RE.fullmatch(value):
        raise ValueError(f"{_LABEL} {what} is not unpadded base64url")
    raw = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
    if _b64url(raw) != value:
        raise ValueError(f"{_LABEL} {what} is not canonical base64url")
    return raw


def _sha256_tag(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def _require_str(value: Any, what: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{_LABEL} {what} is missing")
    return value


def _require_dict(value: Any, what: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{_LABEL} {what} is missing")
    return value


@dataclass(frozen=True)
class _Origin:
    origin: str
    host: str
    port: int


def _parse_origin(value: Any) -> _Origin:
    origin = _require_str(value, "url_origin")
    parsed = urlsplit(origin)
    if (
        parsed.scheme != "https" or not parsed.hostname
        or parsed.username is not None or parsed.password is not None
        or parsed.path not in ("", "/") or parsed.query or parsed.fragment
    ):
        raise ValueError(f"{_LABEL} url_origin must be a root HTTPS origin without userinfo, query or fragment")
    return _Origin(origin.rstrip("/"), parsed.hostname, parsed.port or 443)


def _load_pin(request: dict[str, Any]) -> _Pins:
    fields: dict[str, str] = {}
    allowlists: set[str] = set()
    prefix = "confidential_ai_accepted_subject:"
    for option, enabled in provider_options(request).items():
        if not option.startswith(prefix) or enabled != "true":
            continue
        kind, _, value = option[len(prefix):].partition(":")
        if kind == "c8s-allowlist" and _SHA256_TAG_RE.fullmatch(value):
            allowlists.add(value)
            continue
        if kind in ("tdx-mrtd", "tdx-rtmr1", "tdx-rtmr2", "tdx-rtmr3") and _HEX_48_RE.fullmatch(value):
            name = kind[4:]
        elif kind == "c8s-workload" and _WORKLOAD_NAME_RE.fullmatch(value):
            name = "workload"
        else:
            raise ValueError(f"{_LABEL} accepted_subjects contains an invalid pin")
        if name in fields:
            raise ValueError(f"{_LABEL} accepted_subjects has duplicate {name} pins")
        fields[name] = value
    if set(fields) != {"mrtd", "rtmr1", "rtmr2", "rtmr3", "workload"} or not allowlists:
        raise ValueError(f"{_LABEL} accepted_subjects requires four measurement pins, workload and allowlist")
    return _Pins(**fields, allowlist_sha256=tuple(sorted(allowlists)))


def _verify_routes(raw: bytes, route_digest: bytes, host: str) -> int:
    """Match the published manifest to the digest committed by the attested router.

    The pinned node image's router verifies manifests with the RTMR3-bound
    launch operator key; see c8s operator.md#route-manifest. No client key or
    duplicate signature verification is needed for this baked-node profile.
    """
    envelope = _require_dict(json.loads(raw), "routes envelope")
    manifest_raw = base64.b64decode(
        _require_str(envelope.get("manifest"), "routes manifest"), validate=True
    )
    if not hmac.compare_digest(hashlib.sha256(manifest_raw).digest(), route_digest):
        raise ValueError(f"{_LABEL} route digest does not match the attested route_digest")
    manifest = _require_dict(json.loads(manifest_raw), "route manifest")
    if manifest.get("schema") != "c8s.routes/v1":
        raise ValueError(f"{_LABEL} unsupported route manifest schema")
    revision = manifest.get("revision")
    if type(revision) is not int or revision <= 0:
        raise ValueError(f"{_LABEL} route manifest has invalid revision")
    hosts = _require_dict(manifest.get("hosts"), "route hosts")
    routes = _require_dict(hosts.get(host), "routed host").get("routes")
    # Require an explicit route covering the OpenAI chat path. The digest ties
    # these bytes to the manifest already parsed and applied by the attested LB.
    target = "/v1/chat/completions"
    if not isinstance(routes, list) or not any(
        isinstance(r, dict) and isinstance(r.get("path"), str) and r["path"].startswith("/")
        and ((r.get("match") == "exact" and r["path"] == target)
             or (r.get("match") == "prefix" and target.startswith(r["path"])))
        and isinstance(r.get("backend"), dict) and r["backend"].get("workload")
        and r["backend"].get("address") for r in routes
    ):
        raise ValueError(f"{_LABEL} host is not routed for {target}")
    return revision


def _fetch(endpoint: _Origin, nonce: str, timeout: int) -> _Capture:
    """GET attest-lb, /allowlist, and routes over one WebPKI-validated TLS connection.

    The default context verifies the chain and the hostname. ``auto_open = 0``
    makes a server-closed connection fail instead of silently reconnecting, so
    every response rides the connection whose serving leaf is recorded.
    """
    context = ssl.create_default_context()
    connection = http.client.HTTPSConnection(
        endpoint.host, endpoint.port, timeout=timeout, context=context
    )
    try:
        connection.connect()
        connection.auto_open = 0
        leaf_der = connection.sock.getpeercert(binary_form=True)
        if not leaf_der:
            raise ValueError(f"{_LABEL} TLS endpoint returned no certificate")
        responses: dict[str, _Response] = {}
        for name, path, limit in _ENDPOINTS:
            target = f"{path}?{urlencode({'nonce': nonce})}" if name == "attest-lb" else path
            connection.request("GET", target, headers={"Accept": "application/json"})
            response = connection.getresponse()
            body = response.read(limit + 1)
            if response.status != 200:
                raise ValueError(f"{_LABEL} {path} returned HTTP {response.status}, expected 200")
            content_type = str(response.getheader("content-type") or "")
            if content_type.split(";", 1)[0].strip().lower() != "application/json":
                raise ValueError(
                    f"{_LABEL} {path} returned {content_type!r}, expected application/json"
                )
            if len(body) > limit:
                raise ValueError(f"{_LABEL} {path} response exceeds {limit} bytes")
            responses[name] = _Response(f"{endpoint.origin}{target}", content_type, body)
        return _Capture(serving_leaf_der=leaf_der, responses=responses)
    except http.client.NotConnected as exc:
        raise ValueError(f"{_LABEL} closed the attestation TLS connection") from exc
    finally:
        connection.close()


async def _fetch_collateral(quote: bytes) -> Any:
    """Fetch the Intel DCAP collateral for a quote's FMSPC and PCK CA."""
    import dcap_qvl

    return await dcap_qvl.get_collateral(dcap_qvl.PHALA_PCCS_URL, quote)


def _dcap_verify(quote: bytes, collateral: Any) -> dict[str, Any]:
    """Verify a TDX quote against collateral at the current time; returns the report."""
    import dcap_qvl

    verified = dcap_qvl.verify(quote, collateral, int(_now().timestamp()))
    return json.loads(verified.to_json())


def _decode_quote(value: Any) -> bytes:
    text = _require_str(value, "attest-lb evidence.quote")
    try:
        raw = base64.b64decode(text, validate=True)
    except ValueError as exc:
        raise ValueError(f"{_LABEL} attest-lb quote is not base64") from exc
    if len(raw) < _TDX_MIN_QUOTE_BYTES:
        raise ValueError(f"{_LABEL} attest-lb quote is truncated")
    version, _, tee_type = struct.unpack_from("<HHI", raw)
    if version != _TDX_QUOTE_VERSION or tee_type != _TDX_TEE_TYPE:
        raise ValueError(f"{_LABEL} attest-lb quote is not a TDX v4 quote")
    return raw


def _length_prefixed(data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + data


def attest_lb_transcript_digest(
    front_door_mode: str,
    nonce: bytes,
    serving_leaf_der: bytes,
    mesh_leaf_der: bytes,
    mesh_ca_der: bytes,
    route_digest: bytes,
) -> bytes:
    """SHA-384 over the length-prefixed c8s/attest-lb/v1 transcript."""
    fields = (
        _ATTEST_LB_VERSION.encode("ascii"),
        front_door_mode.encode("utf-8"),
        nonce,
        hashlib.sha256(serving_leaf_der).digest(),
        hashlib.sha256(mesh_leaf_der).digest(),
        hashlib.sha256(mesh_ca_der).digest(),
        route_digest,
    )
    return hashlib.sha384(b"".join(_length_prefixed(field) for field in fields)).digest()


def _mesh_chain(receipt: dict[str, Any]) -> tuple[x509.Certificate, list[x509.Certificate]]:
    """Split cds_cert_pem into the mesh leaf (first block) and the CA candidates."""
    pem = _require_str(receipt.get("cds_cert_pem"), "attest-lb cds_cert_pem")
    blocks = _PEM_BEGIN_RE.findall(pem)
    try:
        certificates = x509.load_pem_x509_certificates(pem.encode("ascii"))
    except ValueError as exc:
        raise ValueError(f"{_LABEL} cds_cert_pem is not a PEM certificate chain") from exc
    if any(block != "CERTIFICATE" for block in blocks) or len(blocks) != len(certificates):
        raise ValueError(f"{_LABEL} cds_cert_pem holds a block that is not a certificate")
    if len(certificates) < 2:
        raise ValueError(f"{_LABEL} cds_cert_pem has no issuing CA after the mesh leaf")
    return certificates[0], certificates[1:]


def _select_committed_ca(
    candidates: list[x509.Certificate], mesh_ca_sha256: Any
) -> x509.Certificate:
    """The proof's commitment, not chain position, selects the mesh CA."""
    want = _b64url_decode(mesh_ca_sha256, "identity_proof.mesh_ca_sha256")
    if len(want) != 32:
        raise ValueError(f"{_LABEL} identity_proof.mesh_ca_sha256 is not 32 bytes")
    for ca in candidates:
        if hmac.compare_digest(hashlib.sha256(ca.public_bytes(Encoding.DER)).digest(), want):
            return ca
    raise ValueError(f"{_LABEL} no served CA matches identity_proof.mesh_ca_sha256")


def _check_validity(certificate: x509.Certificate, name: str, now: datetime) -> None:
    if now + _NOT_BEFORE_SKEW < certificate.not_valid_before_utc:
        raise ValueError(f"{_LABEL} {name} certificate is not yet valid")
    if now > certificate.not_valid_after_utc:
        raise ValueError(f"{_LABEL} {name} certificate has expired")


def _verify_direct_chain(
    certificate: x509.Certificate, ca: x509.Certificate, name: str, now: datetime
) -> None:
    """Require ``certificate`` to be issued directly by the committed CA, both valid now."""
    try:
        constraints = ca.extensions.get_extension_for_class(x509.BasicConstraints).value
    except x509.ExtensionNotFound:
        constraints = None
    try:
        usage = ca.extensions.get_extension_for_class(x509.KeyUsage).value
    except x509.ExtensionNotFound:
        usage = None
    if constraints is None or not constraints.ca or (usage is not None and not usage.key_cert_sign):
        raise ValueError(f"{_LABEL} committed mesh CA is not a certificate-signing CA")
    try:
        certificate.verify_directly_issued_by(ca)
    except (InvalidSignature, ValueError, TypeError) as exc:
        raise ValueError(f"{_LABEL} {name} is not issued by the committed mesh CA") from exc
    _check_validity(certificate, name, now)
    _check_validity(ca, "committed mesh CA", now)


def _verify_identity_proof(
    proof: dict[str, Any], transcript_digest: bytes, mesh_leaf: x509.Certificate
) -> None:
    """The mesh leaf key's ECDSA signature over SHA-384(transcript digest)."""
    public_key = mesh_leaf.public_key()
    if not isinstance(public_key, ec.EllipticCurvePublicKey):
        raise ValueError(f"{_LABEL} mesh leaf key is not an ECDSA key")
    signature = _b64url_decode(proof.get("signature"), "identity_proof.signature")
    try:
        public_key.verify(signature, transcript_digest, ec.ECDSA(hashes.SHA384()))
    except InvalidSignature as exc:
        raise ValueError(
            f"{_LABEL} identity proof signature does not verify under the mesh leaf key"
        ) from exc


def _der_read(data: bytes, offset: int, tag: int) -> tuple[bytes, int]:
    """Read one short-form DER TLV with the expected tag; returns (value, next offset)."""
    if offset + 2 > len(data) or data[offset] != tag or data[offset + 1] >= 0x80:
        raise ValueError("unexpected DER encoding")
    end = offset + 2 + data[offset + 1]
    if end > len(data):
        raise ValueError("truncated DER value")
    return data[offset + 2 : end], end


def _matched_workload(leaf: x509.Certificate) -> tuple[str, str, str]:
    """Parse the CA-stamped matched-workload extension; returns (name, version, digest tag).

    MatchedWorkload ::= SEQUENCE { formatVersion INTEGER (1), name IA5String,
    allowlistVersion IA5String, allowlistDigest OCTET STRING (32) }. The longest
    valid value is 124 bytes, so every length is short-form; a long-form length
    is non-minimal DER and rejected, like TEErminator's re-encoding check.
    """
    try:
        stamps = [ext for ext in leaf.extensions if ext.oid == _MATCHED_WORKLOAD_OID]
    except x509.DuplicateExtension as exc:
        raise ValueError(f"{_LABEL} mesh leaf has duplicate matched-workload stamps") from exc
    if len(stamps) != 1 or not isinstance(stamps[0].value, x509.UnrecognizedExtension):
        raise ValueError(f"{_LABEL} mesh leaf has no single matched-workload stamp")
    der = stamps[0].value.value
    try:
        body, end = _der_read(der, 0, 0x30)
        if end != len(der):
            raise ValueError("trailing bytes")
        version, offset = _der_read(body, 0, 0x02)
        name, offset = _der_read(body, offset, 0x16)
        allowlist_version, offset = _der_read(body, offset, 0x16)
        digest, offset = _der_read(body, offset, 0x04)
        if offset != len(body) or version != b"\x01" or len(digest) != 32:
            raise ValueError("unexpected fields")
        name_text = name.decode("ascii")
        version_text = allowlist_version.decode("ascii")
        if not _WORKLOAD_NAME_RE.fullmatch(name_text) or not _ALLOWLIST_VERSION_RE.fullmatch(
            version_text
        ):
            raise ValueError("invalid field grammar")
    except (ValueError, UnicodeDecodeError) as exc:
        raise ValueError(f"{_LABEL} matched-workload stamp is malformed") from exc
    return name_text, version_text, f"sha256:{digest.hex()}"


def _check_allowlist(raw: bytes, workload: str) -> None:
    """The stamped workload must resolve in the digest-checked allowlist document."""
    try:
        document = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise ValueError(f"{_LABEL} /allowlist is not JSON") from exc
    if not isinstance(document, dict) or document.get("schema") != _ALLOWLIST_SCHEMA:
        raise ValueError(f"{_LABEL} /allowlist is not a {_ALLOWLIST_SCHEMA} document")
    workloads = document.get("workloads")
    if not isinstance(workloads, dict) or workload not in workloads:
        raise ValueError(f"{_LABEL} stamped workload {workload!r} is not in /allowlist")


def _td_report(verified: dict[str, Any]) -> dict[str, str]:
    td10 = (verified.get("report") or {}).get("TD10")
    if not isinstance(td10, dict):
        raise ValueError(f"{_LABEL} DCAP result does not contain a TDX TD report")
    fields = {
        name: str(td10.get(name) or "").lower()
        for name in (
            "td_attributes",
            "mr_td",
            "rt_mr0",
            "rt_mr1",
            "rt_mr2",
            "rt_mr3",
            "report_data",
        )
    }
    if len(fields["td_attributes"]) != 16 or len(fields["report_data"]) != 128:
        raise ValueError(f"{_LABEL} DCAP TD report is malformed")
    for name in ("mr_td", "rt_mr0", "rt_mr1", "rt_mr2", "rt_mr3"):
        if not _HEX_48_RE.fullmatch(fields[name]):
            raise ValueError(f"{_LABEL} DCAP TD report is missing {name}")
    return fields


def _check_report(
    verified: dict[str, Any], transcript_digest: bytes, pin: _Pins
) -> tuple[str, dict[str, str]]:
    """Check TCB, debug mode, report_data and configured measurements in the verified quote."""
    tcb_status = verified.get("status")
    if tcb_status != "UpToDate":
        raise ValueError(f"{_LABEL} TDX TCB status is {tcb_status!r}, expected 'UpToDate'")
    report = _td_report(verified)
    if bytes.fromhex(report["td_attributes"])[0] != 0:
        raise ValueError(f"{_LABEL} TD runs in debug mode (TD_ATTRIBUTES TUD set)")
    report_data = bytes.fromhex(report["report_data"])
    if not hmac.compare_digest(report_data[:48], transcript_digest):
        raise ValueError(f"{_LABEL} report_data does not match the {_ATTEST_LB_VERSION} transcript")
    if report_data[48:] != bytes(16):
        raise ValueError(f"{_LABEL} report_data padding is not zero")
    for name, field in (
        ("MRTD", "mr_td"), ("RTMR1", "rt_mr1"), ("RTMR2", "rt_mr2"), ("RTMR3", "rt_mr3")
    ):
        if not hmac.compare_digest(report[field], getattr(pin, name.lower())):
            raise ValueError(f"{_LABEL} {name} does not match configured pin")
    return tcb_status, report


async def verify_c8s(request: dict[str, Any]) -> None:
    """Verify a Confidential AI c8s front door and pin its serving TLS key."""

    verifier_id = verifier_id_for(_PROVIDER)
    evidence = None
    try:
        endpoint = _parse_origin(request.get("url_origin"))
        timeout = request_timeout_seconds(request, 60)
        pin = _load_pin(request)

        nonce = _new_nonce()
        nonce_raw = _b64url_decode(nonce, "client nonce")
        capture = await asyncio.to_thread(_fetch, endpoint, nonce, timeout)
        evidence = raw_http_bundle_evidence(
            [
                raw_http_item(name, response.url, response.content_type, response.body)
                for name, response in capture.responses.items()
            ]
            + [
                raw_http_item(
                    "serving-leaf",
                    endpoint.origin,
                    "application/pkix-cert",
                    capture.serving_leaf_der,
                )
            ],
            source_url=endpoint.origin,
        )
        # (b) Receipt shape: binding identifier, exact nonce echo, TEE-held mode.
        receipt = _require_dict(
            json.loads(capture.responses["attest-lb"].body.decode("utf-8")), "attest-lb receipt"
        )
        if receipt.get("version") != _ATTEST_LB_VERSION:
            raise ValueError(f"{_LABEL} attest-lb receipt is not {_ATTEST_LB_VERSION}")
        if receipt.get("platform") != "tdx":
            raise ValueError(f"{_LABEL} attest-lb receipt platform is not tdx")
        if not hmac.compare_digest(_require_str(receipt.get("nonce"), "attest-lb nonce"), nonce):
            raise ValueError(f"{_LABEL} attest-lb nonce does not match the request nonce")
        front_door_mode = _require_str(receipt.get("front_door_mode"), "attest-lb front_door_mode")
        if front_door_mode != "acme":
            raise ValueError(f"{_LABEL} front_door_mode must be acme, got {front_door_mode!r}")
        route_digest_hex = _require_str(receipt.get("route_digest"), "route_digest")
        if not re.fullmatch(r"[0-9a-f]{64}", route_digest_hex):
            raise ValueError(f"{_LABEL} route_digest must be 64 lowercase hex")
        route_digest = bytes.fromhex(route_digest_hex)
        # Not in TEErminator; the transcript binds the leaf either way.
        serving_leaf_sha256 = _b64url(hashlib.sha256(capture.serving_leaf_der).digest())
        if receipt.get("serving_leaf_sha256") != serving_leaf_sha256:
            raise ValueError(
                f"{_LABEL} attest-lb receipt does not name the TLS leaf served on this connection"
            )

        # (c) Mesh leaf and the CA the proof commits to; (d) proof fields.
        mesh_leaf, ca_candidates = _mesh_chain(receipt)
        proof = _require_dict(receipt.get("identity_proof"), "attest-lb identity_proof")
        mesh_ca = _select_committed_ca(ca_candidates, proof.get("mesh_ca_sha256"))
        if proof.get("algorithm") != _IDENTITY_PROOF_ALGORITHM:
            raise ValueError(
                f"{_LABEL} identity proof algorithm is {proof.get('algorithm')!r}, "
                f"expected {_IDENTITY_PROOF_ALGORITHM!r}"
            )
        mesh_leaf_der = mesh_leaf.public_bytes(Encoding.DER)
        mesh_ca_der = mesh_ca.public_bytes(Encoding.DER)
        if proof.get("leaf_sha256") != _b64url(hashlib.sha256(mesh_leaf_der).digest()):
            raise ValueError(f"{_LABEL} identity_proof.leaf_sha256 does not match the mesh leaf")

        # (e) Transcript over the connection's serving leaf; (f) proof of possession.
        transcript_digest = attest_lb_transcript_digest(
            front_door_mode, nonce_raw, capture.serving_leaf_der,
            mesh_leaf_der, mesh_ca_der, route_digest,
        )
        _verify_identity_proof(proof, transcript_digest, mesh_leaf)

        # (g) Mesh leaf directly issued by the committed CA; (h) serving-leaf trust.
        # In acme mode the TLS handshake already ran WebPKI chain and hostname checks.
        now = _now()
        _verify_direct_chain(mesh_leaf, mesh_ca, "mesh leaf", now)
        serving_leaf = x509.load_der_x509_certificate(capture.serving_leaf_der)
        _check_validity(serving_leaf, "serving leaf", now)

        # (j) CA-vouched stamp: front-door workload and the allowlist it matched.
        workload, allowlist_version, stamped_allowlist = _matched_workload(mesh_leaf)
        if workload != pin.workload:
            raise ValueError(
                f"{_LABEL} mesh leaf is stamped for workload {workload!r}, expected "
                f"{pin.workload!r}"
            )
        allowlist_raw = capture.responses["allowlist"].body
        allowlist_sha256 = _sha256_tag(allowlist_raw)
        if stamped_allowlist != allowlist_sha256:
            raise ValueError(
                f"{_LABEL} stamped allowlist {stamped_allowlist} does not match the /allowlist "
                f"bytes {allowlist_sha256}"
            )
        if allowlist_sha256 not in pin.allowlist_sha256:
            raise ValueError(
                f"{_LABEL} allowlist {allowlist_sha256} is not in accepted_subjects"
            )
        _check_allowlist(allowlist_raw, workload)

        # (e') Hardware evidence: DCAP with online collateral, then the configured pins.
        evidence_fields = _require_dict(receipt.get("evidence"), "attest-lb evidence")
        quote = _decode_quote(evidence_fields.get("quote"))
        try:
            collateral = await asyncio.wait_for(_fetch_collateral(quote), timeout)
            verified = await asyncio.to_thread(_dcap_verify, quote, collateral)
        except Exception as exc:  # noqa: BLE001 - report DCAP failures as such
            raise ValueError(f"{_LABEL} quote failed DCAP verification: {exc}") from exc
        tcb_status, report = _check_report(verified, transcript_digest, pin)
        route_revision = _verify_routes(
            capture.responses["routes"].body, route_digest, endpoint.host
        )

        tls_spki = hashlib.sha256(
            serving_leaf.public_key().public_bytes(Encoding.DER, PublicFormat.SubjectPublicKeyInfo)
        ).hexdigest()
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
                "provider_claims": {
                    "trust_boundary": "c8s-front-door",
                    "evidence_scope": "router",
                    "attestation_protocol": _ATTEST_LB_VERSION,
                    "tcb_status": tcb_status,
                    "tdx_debug_mode": False,
                    "tls_spki_sha256": tls_spki,
                    "serving_leaf_sha256": hashlib.sha256(capture.serving_leaf_der).hexdigest(),
                    "front_door_mode": front_door_mode,
                    "mesh_ca_sha256": _sha256_tag(mesh_ca_der),
                    "front_door_workload": workload,
                    "allowlist_sha256": allowlist_sha256,
                    # CA-vouched store counter; the digest is what is checked.
                    "allowlist_version": allowlist_version,
                    "node_measurements_pinned": True,
                    "mrtd": report["mr_td"],
                    "rtmr1": report["rt_mr1"],
                    "rtmr2": report["rt_mr2"],
                    "rtmr3": report["rt_mr3"],
                    "route_digest": route_digest_hex,
                    "route_revision": route_revision,
                    "verification_scope": "front door at verification time",
                    "continuous_policy_enforcement": False,
                    "worker_identity_verified": False,
                    # RTMR0 is per-host firmware configuration; recorded, never pinned.
                    "rtmr0": report["rt_mr0"],
                    # The provider exposes no verifiable GPU evidence.
                    "gpu_verified": False,
                },
            }
        )
    except Exception as exc:  # noqa: BLE001 - bridge returns failures as JSON
        failed(_PROVIDER, str(exc), evidence=evidence, verifier_id=verifier_id)
