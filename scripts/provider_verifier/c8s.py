"""Confidential AI (c8s) provider verification.

Confidential AI serves inference from c8s (Confidential Kubernetes) nodes that
run as Intel TDX CVMs. Its ``c8s-tls-lb`` front door terminates public TLS
inside the CVM and returns an ``attest-lb`` receipt: a TDX quote whose
report_data commits to the caller nonce, the serving TLS leaf, and the RA-TLS
mesh leaf and CA. The mesh leaf key also signs that transcript.

This module implements the transcript from the protocol description in
``docs/providers/c8s/verification.md``. Measurements are matched against the
reviewed registry in ``provider_refs/c8s.json``; nothing fetched at
verification time is trusted as a measurement reference.
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
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from .common import (
    RootOrigin,
    emit,
    failed,
    parse_root_https_origin,
    raw_http_bundle_evidence,
    raw_http_item,
    request_timeout_seconds,
    verifier_id_for,
)

_PROVIDER = "c8s"
_LABEL = "Confidential AI"
_REGISTRY_PATH = Path(__file__).resolve().parent / "provider_refs" / "c8s.json"
_ATTEST_LB_VERSION = "c8s/attest-lb/v1"
_FRONT_DOOR_SOURCE = "c8s-tls-lb"
_IDENTITY_PROOF_ALGORITHM = "ecdsa-sha384"
# TLS modes in which the public serving key is generated and held inside the TEE.
# `webpki` (an operator-held certificate key) is rejected.
_TEE_HELD_TLS_MODES = frozenset({"acme", "tee-webpki", "cds"})
_MESH_CURVES = (ec.SECP256R1, ec.SECP384R1)
_NONCE_BYTES = 32
_MAX_ATTESTATION_BYTES = 8 * 1024 * 1024
# TDX v4 quote: 48-byte header followed by the TD report body.
_TDX_QUOTE_VERSION = 4
_TDX_TEE_TYPE = 0x81
_TDX_MIN_QUOTE_BYTES = 48 + 584
_HEX_RE = re.compile(r"^[0-9a-fA-F]+$")
_HEX_48_RE = re.compile(r"^[0-9a-f]{96}$")
_SHA256_TAG_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_B64URL_RE = re.compile(r"^[A-Za-z0-9_-]+$")


@dataclass(frozen=True)
class _Capture:
    body: bytes
    content_type: str
    serving_leaf_der: bytes


@dataclass(frozen=True)
class _ReleasePin:
    release_id: str
    bundle_sha256: str
    bundle_source: dict[str, Any]
    policy_mode: str
    mrtd: str
    rtmr1: str
    rtmr2: str
    accepted_rtmr3: dict[str, dict[str, Any]]
    accepted_allowlist_sha256: frozenset[str]
    accepted_mesh_ca_sha256: frozenset[str]


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
        raise ValueError(f"{_LABEL} attestation is missing {what}")
    return value


def _require_dict(value: Any, what: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{_LABEL} attestation is missing {what}")
    return value


def _load_registry(path: Path | None = None) -> dict[str, _ReleasePin]:
    """Load the reviewed release registry checked into the gateway release."""
    document = json.loads((path or _REGISTRY_PATH).read_text(encoding="utf-8"))
    if not isinstance(document, dict) or document.get("provider") != _PROVIDER:
        raise ValueError(f"{_LABEL} release registry is malformed")
    releases = document.get("releases")
    if not isinstance(releases, dict):
        raise ValueError(f"{_LABEL} release registry is malformed")
    pins: dict[str, _ReleasePin] = {}
    for release_id, entry in releases.items():
        if not isinstance(entry, dict):
            raise ValueError(f"{_LABEL} release registry entry {release_id!r} is malformed")
        registers = {name: entry.get(name) for name in ("mrtd", "rtmr1", "rtmr2")}
        rtmr3 = {
            item.get("value"): item
            for item in entry.get("accepted_rtmr3") or []
            if isinstance(item, dict)
        }
        allowlists = frozenset(entry.get("accepted_allowlist_sha256") or [])
        mesh_cas = frozenset(entry.get("accepted_mesh_ca_sha256") or [])
        if (
            any(
                not isinstance(value, str) or not _HEX_48_RE.fullmatch(value)
                for value in (*registers.values(), *rtmr3)
            )
            or not rtmr3
            or not allowlists
            or any(
                not isinstance(value, str) or not _SHA256_TAG_RE.fullmatch(value)
                for value in (entry.get("bundle_sha256"), *allowlists, *mesh_cas)
            )
        ):
            raise ValueError(f"{_LABEL} release registry entry {release_id!r} is malformed")
        pins[release_id] = _ReleasePin(
            release_id=release_id,
            bundle_sha256=entry["bundle_sha256"],
            bundle_source=dict(entry.get("bundle_source") or {}),
            policy_mode=str(entry.get("policy_mode") or ""),
            mrtd=registers["mrtd"],
            rtmr1=registers["rtmr1"],
            rtmr2=registers["rtmr2"],
            accepted_rtmr3=rtmr3,
            accepted_allowlist_sha256=allowlists,
            accepted_mesh_ca_sha256=mesh_cas,
        )
    return pins


def _fetch_attestation(endpoint: RootOrigin, nonce: str, timeout: int) -> _Capture:
    """Fetch /attestation over one TLS connection and keep that connection's leaf."""
    context = ssl.create_default_context()
    connection = http.client.HTTPSConnection(
        endpoint.host, endpoint.port, timeout=timeout, context=context
    )
    try:
        connection.connect()
        leaf_der = connection.sock.getpeercert(binary_form=True)
        if not leaf_der:
            raise ValueError(f"{_LABEL} TLS endpoint returned no certificate")
        connection.request(
            "GET",
            f"/attestation?{urlencode({'nonce': nonce})}",
            headers={"Accept": "application/json"},
        )
        response = connection.getresponse()
        if response.status != 200:
            raise ValueError(
                f"{_LABEL} /attestation returned HTTP {response.status}, expected 200"
            )
        content_type = str(response.getheader("content-type") or "")
        if content_type.split(";", 1)[0].strip().lower() != "application/json":
            raise ValueError(
                f"{_LABEL} /attestation returned {content_type!r}, expected application/json"
            )
        body = response.read(_MAX_ATTESTATION_BYTES + 1)
        if len(body) > _MAX_ATTESTATION_BYTES:
            raise ValueError(
                f"{_LABEL} /attestation response exceeds {_MAX_ATTESTATION_BYTES} bytes"
            )
        return _Capture(body=body, content_type=content_type, serving_leaf_der=leaf_der)
    finally:
        connection.close()


async def _verify_quote(quote: bytes) -> dict[str, Any]:
    """Verify a TDX quote with Intel DCAP collateral; returns the verified report."""
    import dcap_qvl

    verified = await dcap_qvl.get_collateral_and_verify(quote)
    return json.loads(verified.to_json())


def _decode_quote(value: Any) -> bytes:
    text = _require_str(value, "frontDoor.receipt.evidence.quote").strip()
    if _HEX_RE.fullmatch(text) and len(text) % 2 == 0:
        raw = bytes.fromhex(text)
    else:
        standard = "+" in text or "/" in text
        url_safe = "-" in text or "_" in text
        if (standard and url_safe) or not re.fullmatch(r"[A-Za-z0-9+/_-]+={0,2}", text):
            raise ValueError(f"{_LABEL} front-door quote is neither hex nor base64")
        text = text.rstrip("=").replace("-", "+").replace("_", "/")
        raw = base64.b64decode(text + "=" * (-len(text) % 4), validate=True)
    if len(raw) < _TDX_MIN_QUOTE_BYTES:
        raise ValueError(f"{_LABEL} front-door quote is truncated")
    version, _, tee_type = struct.unpack_from("<HHI", raw)
    if version != _TDX_QUOTE_VERSION or tee_type != _TDX_TEE_TYPE:
        raise ValueError(f"{_LABEL} front-door quote is not a TDX v4 quote")
    return raw


def _length_prefixed(data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + data


def attest_lb_transcript_digest(
    front_door_mode: str,
    nonce: bytes,
    serving_leaf_der: bytes,
    mesh_leaf_der: bytes,
    mesh_ca_der: bytes,
) -> bytes:
    """SHA-384 over the length-prefixed c8s/attest-lb/v1 transcript."""
    fields = (
        _ATTEST_LB_VERSION.encode("ascii"),
        front_door_mode.encode("utf-8"),
        nonce,
        hashlib.sha256(serving_leaf_der).digest(),
        hashlib.sha256(mesh_leaf_der).digest(),
        hashlib.sha256(mesh_ca_der).digest(),
    )
    return hashlib.sha384(b"".join(_length_prefixed(field) for field in fields)).digest()


def _mesh_chain(receipt: dict[str, Any]) -> tuple[x509.Certificate, x509.Certificate]:
    pem = _require_str(receipt.get("cds_cert_pem"), "frontDoor.receipt.cds_cert_pem")
    try:
        certificates = x509.load_pem_x509_certificates(pem.encode("ascii"))
    except ValueError as exc:
        raise ValueError(f"{_LABEL} mesh certificate chain is not valid PEM") from exc
    if len(certificates) != 2:
        raise ValueError(f"{_LABEL} mesh chain must contain exactly the mesh leaf and mesh CA")
    leaf, ca = certificates
    try:
        constraints = ca.extensions.get_extension_for_class(x509.BasicConstraints).value
    except x509.ExtensionNotFound as exc:
        raise ValueError(f"{_LABEL} mesh CA is not a CA certificate") from exc
    if not constraints.ca:
        raise ValueError(f"{_LABEL} mesh CA is not a CA certificate")
    now = _now()
    for name, certificate in (("mesh leaf", leaf), ("mesh CA", ca)):
        if not certificate.not_valid_before_utc <= now <= certificate.not_valid_after_utc:
            raise ValueError(f"{_LABEL} {name} certificate is outside its validity period")
    try:
        ca.verify_directly_issued_by(ca)
        leaf.verify_directly_issued_by(ca)
    except (InvalidSignature, ValueError, TypeError) as exc:
        raise ValueError(f"{_LABEL} mesh leaf does not chain to the mesh CA") from exc
    return leaf, ca


def _verify_identity_proof(
    receipt: dict[str, Any],
    transcript_digest: bytes,
    mesh_leaf: x509.Certificate,
    mesh_leaf_der: bytes,
    mesh_ca_der: bytes,
) -> None:
    proof = _require_dict(receipt.get("identity_proof"), "frontDoor.receipt.identity_proof")
    if proof.get("algorithm") != _IDENTITY_PROOF_ALGORITHM:
        raise ValueError(
            f"{_LABEL} identity proof algorithm is {proof.get('algorithm')!r}, "
            f"expected {_IDENTITY_PROOF_ALGORITHM!r}"
        )
    if proof.get("leaf_sha256") != _b64url(hashlib.sha256(mesh_leaf_der).digest()):
        raise ValueError(f"{_LABEL} identity proof does not name the returned mesh leaf")
    if proof.get("mesh_ca_sha256") != _b64url(hashlib.sha256(mesh_ca_der).digest()):
        raise ValueError(f"{_LABEL} identity proof does not name the returned mesh CA")
    public_key = mesh_leaf.public_key()
    if not isinstance(public_key, ec.EllipticCurvePublicKey) or not isinstance(
        public_key.curve, _MESH_CURVES
    ):
        raise ValueError(f"{_LABEL} mesh leaf key is not a NIST P-256 or P-384 key")
    signature = _b64url_decode(proof.get("signature"), "identity proof signature")
    try:
        public_key.verify(signature, transcript_digest, ec.ECDSA(hashes.SHA384()))
    except InvalidSignature as exc:
        raise ValueError(
            f"{_LABEL} identity proof signature does not verify under the mesh leaf key"
        ) from exc


def _td_report(verified: dict[str, Any]) -> dict[str, str]:
    td10 = (verified.get("report") or {}).get("TD10")
    if not isinstance(td10, dict):
        raise ValueError(f"{_LABEL} DCAP result does not contain a TDX TD report")
    fields = {
        name: str(td10.get(name) or "").lower()
        for name in ("td_attributes", "mr_td", "rt_mr1", "rt_mr2", "rt_mr3", "report_data")
    }
    if len(fields["td_attributes"]) != 16 or len(fields["report_data"]) != 128:
        raise ValueError(f"{_LABEL} DCAP TD report is malformed")
    for name in ("mr_td", "rt_mr1", "rt_mr2", "rt_mr3"):
        if not _HEX_48_RE.fullmatch(fields[name]):
            raise ValueError(f"{_LABEL} DCAP TD report is missing {name}")
    return fields


def _check_measurements(report: dict[str, str], pin: _ReleasePin) -> dict[str, Any]:
    for name, field in (("MRTD", "mr_td"), ("RTMR1", "rt_mr1"), ("RTMR2", "rt_mr2")):
        expected = getattr(pin, name.lower())
        if not hmac.compare_digest(report[field], expected):
            raise ValueError(
                f"{_LABEL} {name} does not match reviewed release {pin.release_id!r}"
            )
    rtmr3 = pin.accepted_rtmr3.get(report["rt_mr3"])
    if rtmr3 is None:
        raise ValueError(
            f"{_LABEL} RTMR3 {report['rt_mr3']} is not in the accepted set for "
            f"release {pin.release_id!r}"
        )
    return rtmr3


async def verify_c8s(request: dict[str, Any]) -> None:
    """Verify a Confidential AI c8s front door and emit its TLS SPKI binding."""

    verifier_id = verifier_id_for(_PROVIDER)
    evidence = None
    try:
        endpoint = parse_root_https_origin(request.get("url_origin"), _LABEL)
        timeout = request_timeout_seconds(request, 60)
        registry = _load_registry()

        nonce = _new_nonce()
        nonce_raw = _b64url_decode(nonce, "client nonce")
        capture = await asyncio.to_thread(_fetch_attestation, endpoint, nonce, timeout)
        attestation_url = f"{endpoint.origin}/attestation?{urlencode({'nonce': nonce})}"
        evidence = raw_http_bundle_evidence(
            [
                raw_http_item("attestation", attestation_url, capture.content_type, capture.body),
                raw_http_item(
                    "serving-leaf",
                    endpoint.origin,
                    "application/pkix-cert",
                    capture.serving_leaf_der,
                ),
            ],
            source_url=endpoint.origin,
        )

        document = json.loads(capture.body.decode("utf-8"))
        document = _require_dict(document, "a JSON object body")
        if document.get("schemaVersion") != 2:
            raise ValueError(
                f"{_LABEL} attestation schemaVersion is {document.get('schemaVersion')!r}, expected 2"
            )
        if not hmac.compare_digest(_require_str(document.get("nonce"), "nonce"), nonce):
            raise ValueError(f"{_LABEL} attestation nonce does not match the request nonce")

        release = _require_dict(document.get("release"), "release")
        release_id = _require_str(release.get("id"), "release.id")
        pin = registry.get(release_id)
        if pin is None:
            raise ValueError(f"{_LABEL} release {release_id!r} is not in the reviewed registry")
        bundle_sha256 = _require_str(release.get("bundleSha256"), "release.bundleSha256")
        if bundle_sha256 != pin.bundle_sha256:
            raise ValueError(
                f"{_LABEL} release {release_id!r} bundleSha256 does not match the reviewed bundle"
            )

        tls_mode = _require_str(_require_dict(document.get("tls"), "tls").get("mode"), "tls.mode")
        if tls_mode not in _TEE_HELD_TLS_MODES:
            raise ValueError(
                f"{_LABEL} tls.mode {tls_mode!r} does not keep the serving key inside the TEE"
            )

        front_door = _require_dict(document.get("frontDoor"), "frontDoor")
        if front_door.get("source") != _FRONT_DOOR_SOURCE:
            raise ValueError(f"{_LABEL} front door is not a {_FRONT_DOOR_SOURCE} receipt")
        receipt = _require_dict(front_door.get("receipt"), "frontDoor.receipt")
        if receipt.get("version") != _ATTEST_LB_VERSION:
            raise ValueError(f"{_LABEL} front-door receipt is not {_ATTEST_LB_VERSION}")
        if receipt.get("platform") != "tdx":
            raise ValueError(f"{_LABEL} front-door receipt platform is not tdx")
        if not hmac.compare_digest(
            _require_str(receipt.get("nonce"), "frontDoor.receipt.nonce"), nonce
        ):
            raise ValueError(f"{_LABEL} front-door receipt nonce does not match the request nonce")
        front_door_mode = _require_str(
            receipt.get("front_door_mode"), "frontDoor.receipt.front_door_mode"
        )
        if front_door_mode not in _TEE_HELD_TLS_MODES or front_door_mode != tls_mode:
            raise ValueError(
                f"{_LABEL} front_door_mode {front_door_mode!r} is not the TEE-held tls.mode"
            )
        serving_leaf_sha256 = _b64url(hashlib.sha256(capture.serving_leaf_der).digest())
        if receipt.get("serving_leaf_sha256") != serving_leaf_sha256:
            raise ValueError(
                f"{_LABEL} front-door receipt does not name the TLS leaf served on this connection"
            )

        c8s = _require_dict(document.get("c8s"), "c8s")
        mesh_leaf, mesh_ca = _mesh_chain(receipt)
        mesh_leaf_der = mesh_leaf.public_bytes(Encoding.DER)
        mesh_ca_der = mesh_ca.public_bytes(Encoding.DER)
        mesh_ca_sha256 = _sha256_tag(mesh_ca_der)
        if c8s.get("meshCaSha256") != mesh_ca_sha256:
            raise ValueError(f"{_LABEL} c8s.meshCaSha256 does not match the returned mesh CA")
        if pin.accepted_mesh_ca_sha256 and mesh_ca_sha256 not in pin.accepted_mesh_ca_sha256:
            raise ValueError(
                f"{_LABEL} mesh CA {mesh_ca_sha256} is not accepted for release {release_id!r}"
            )

        transcript_digest = attest_lb_transcript_digest(
            front_door_mode,
            nonce_raw,
            capture.serving_leaf_der,
            mesh_leaf_der,
            mesh_ca_der,
        )
        _verify_identity_proof(receipt, transcript_digest, mesh_leaf, mesh_leaf_der, mesh_ca_der)

        allowlist_sha256 = _require_str(
            _require_dict(c8s.get("activeAllowlist"), "c8s.activeAllowlist").get("sha256"),
            "c8s.activeAllowlist.sha256",
        )
        if allowlist_sha256 not in pin.accepted_allowlist_sha256:
            raise ValueError(
                f"{_LABEL} allowlist {allowlist_sha256} is not accepted for release {release_id!r}"
            )

        quote = _decode_quote(
            _require_dict(receipt.get("evidence"), "frontDoor.receipt.evidence").get("quote")
        )
        verified = await _verify_quote(quote)
        tcb_status = verified.get("status")
        if tcb_status != "UpToDate":
            raise ValueError(f"{_LABEL} TDX TCB status is {tcb_status!r}, expected 'UpToDate'")
        report = _td_report(verified)
        if bytes.fromhex(report["td_attributes"])[0] != 0:
            raise ValueError(f"{_LABEL} front-door TD runs in debug mode (TD_ATTRIBUTES TUD set)")
        report_data = bytes.fromhex(report["report_data"])
        if not hmac.compare_digest(report_data[:48], transcript_digest):
            raise ValueError(
                f"{_LABEL} TDX report_data does not match the attest-lb transcript"
            )
        if report_data[48:] != bytes(16):
            raise ValueError(f"{_LABEL} TDX report_data padding is not zero")
        rtmr3 = _check_measurements(report, pin)

        serving_leaf = x509.load_der_x509_certificate(capture.serving_leaf_der)
        tls_spki = hashlib.sha256(
            serving_leaf.public_key().public_bytes(
                Encoding.DER, PublicFormat.SubjectPublicKeyInfo
            )
        ).hexdigest()
        gpu_evidence = document.get("gpuEvidence")
        gpu_status = gpu_evidence.get("status") if isinstance(gpu_evidence, dict) else None

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
                    "tls_mode": tls_mode,
                    "front_door_mode": front_door_mode,
                    "release_id": release_id,
                    "bundle_sha256": bundle_sha256,
                    "bundle_signed": pin.bundle_source.get("signed") is True,
                    "policy_mode": pin.policy_mode,
                    "mesh_ca_sha256": mesh_ca_sha256,
                    "allowlist_sha256": allowlist_sha256,
                    "node_measurements_pinned": True,
                    "mrtd": report["mr_td"],
                    "rtmr1": report["rt_mr1"],
                    "rtmr2": report["rt_mr2"],
                    "rtmr3": report["rt_mr3"],
                    "rtmr3_source": rtmr3.get("source"),
                    "operator_key_armed": rtmr3.get("operator_key_armed") is not False,
                    "registry_entry": {
                        "release_id": pin.release_id,
                        "bundle_sha256": pin.bundle_sha256,
                        "bundle_source": pin.bundle_source,
                    },
                    "provider_scope": document.get("scope"),
                    "provider_operational_status": document.get("operationalStatus"),
                    # GPU evidence is not verified here and never gates the lease.
                    "gpu_verified": False,
                    "gpu_evidence_status": gpu_status,
                },
            }
        )
    except Exception as exc:  # noqa: BLE001 - bridge returns failures as JSON
        failed(_PROVIDER, str(exc), evidence=evidence, verifier_id=verifier_id)
