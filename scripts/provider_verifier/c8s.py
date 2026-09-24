"""Confidential AI (c8s) provider verification.

Confidential AI serves inference from c8s (Confidential Kubernetes) nodes that
run as Intel TDX CVMs. Its ``c8s-tls-lb`` front door terminates public TLS
inside the CVM and returns an ``attest-lb`` receipt: a TDX quote whose
report_data commits to the caller nonce, the serving TLS leaf, and the RA-TLS
mesh leaf and CA. The mesh leaf key also signs that transcript.

Request content is processed on other nodes than the front door. The same
``/attestation`` response carries one nonce-bound ``attest-pq`` receipt per
workload; the gateway, the SGLang router, and every inference worker must each
present one from a node that matches the same reviewed release.

This module implements both transcripts from the protocol description in
``docs/providers/c8s/verification.md``. Measurements and required workloads are
matched against the reviewed registry in ``provider_refs/c8s.json``; nothing
fetched at verification time is trusted as a measurement reference.
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
_ATTEST_PQ_VERSION = "c8s/attest-pq/v1"
# Workload receipt protocols. Both carry receipt version `c8s/attest-pq/v1` and
# the transcript domain tag `c8s-verify/v1`; they differ in the session fields.
_ATTEST_PQ_LEGACY = "c8s/attest-pq/v1"
_ATTEST_PQ_XWING = "c8s/attest-pq/v1+xwing"
_ATTEST_PQ_DOMAIN = b"c8s-verify/v1"
_FRONT_DOOR_SOURCE = "c8s-tls-lb"
_IDENTITY_PROOF_ALGORITHM = "ecdsa-sha384"
# TLS modes in which the public serving key is generated and held inside the TEE.
# `webpki` (an operator-held certificate key) is rejected.
_TEE_HELD_TLS_MODES = frozenset({"acme", "tee-webpki", "cds"})
_MESH_CURVES = (ec.SECP256R1, ec.SECP384R1)
_NONCE_BYTES = 32
_MAX_ATTESTATION_BYTES = 8 * 1024 * 1024
# (field, exact byte length) per workload protocol, in transcript order.
_SESSION_FIELDS: dict[str, tuple[tuple[tuple[str, ...], int], ...]] = {
    _ATTEST_PQ_LEGACY: (
        (("session_pubkey", "x25519"), 32),
        (("session_pubkey", "mlkem768"), 1184),
    ),
    _ATTEST_PQ_XWING: ((("xwing_ek",), 1216), (("xwing_ct",), 1120), (("session_id",), 16)),
}
# Plaintext-path targets that every reviewed release must require.
_REQUIRED_TARGETS = frozenset({"gateway", "sglang-router"})
_INFERENCE_WORKER_RE = re.compile(r"^inference-worker-[0-9]+$")
# Matched-workload stamp on a CDS-issued mesh leaf (c8s-verify-js PROTOCOL.md).
_MATCHED_WORKLOAD_OID = x509.ObjectIdentifier("1.3.6.1.4.1.66378.1.5")
_WORKLOAD_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$")
_ALLOWLIST_VERSION_RE = re.compile(r"^(0|[1-9][0-9]{0,19})$")
# Admission allowlist (`c8s.allowlist/v1`). Its digest is SHA-256 over Go's
# json.Marshal of the allowlist struct: compact, struct fields in declaration
# order, map keys sorted. Field order per object kind; the union covers every
# field the schema has used (mounts `destinations`/`rules`, env `names`/`values`).
_ALLOWLIST_SCHEMA = "c8s.allowlist/v1"
_ALLOWLIST_FIELDS: dict[str, tuple[tuple[str, str], ...]] = {
    "allowlist": (("schema", "str"), ("digests", "map:str"), ("workloads", "map:workload")),
    "workload": (
        ("label", "str"),
        ("initContainers", "list:container"),
        ("containers", "list:container"),
        ("secrets", "secrets"),
    ),
    "container": (
        ("digest", "str"),
        ("image", "str"),
        ("command", "argv"),
        ("args", "argv"),
        ("mounts", "mounts"),
        ("env", "env"),
    ),
    "argv": (("policy", "str"), ("argv", "list:str")),
    "mounts": (("policy", "str"), ("destinations", "list:str"), ("rules", "list:mount_rule")),
    "mount_rule": (
        ("destination", "str"),
        ("kind", "str"),
        ("source", "str"),
        ("readOnly", "bool"),
    ),
    "env": (("policy", "str"), ("names", "list:str"), ("values", "map:str")),
    "secrets": (("policy", "str"), ("read", "list:str"), ("write", "list:str")),
}
# Go's encoding/json string escapes beyond `"` and `\` (HTML-safe, Go >= 1.22).
_GO_JSON_ESCAPES = {
    "\b": "\\b",
    "\f": "\\f",
    "\n": "\\n",
    "\r": "\\r",
    "\t": "\\t",
    "<": "\\u003c",
    ">": "\\u003e",
    "&": "\\u0026",
    "\u2028": "\\u2028",
    "\u2029": "\\u2029",
}
# Env and mount policies that fix what the container gets; `any` does not.
_PINNED_ADMISSION_POLICIES = frozenset({"exact", "deny"})
# Quotes verified at once; DCAP collateral is fetched once per (FMSPC, CA).
_DCAP_CONCURRENCY = 4
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
class _WorkloadBinding:
    target: str
    workload: str
    identity: str


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
    workload_attestation_protocol: str
    required_workloads: tuple[_WorkloadBinding, ...]
    require_pinned_env_mounts: bool
    additional_admission_entries: tuple[str, ...]


@dataclass(frozen=True)
class _PendingQuote:
    """A quote whose non-DCAP checks passed, awaiting DCAP verification."""

    subject: str
    protocol: str
    quote: bytes
    transcript_digest: bytes


@dataclass(frozen=True)
class _Workload:
    binding: _WorkloadBinding
    front_door_mode: str
    allowlist_sha256: str
    pending: _PendingQuote


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


def _load_required_workloads(value: Any, release_id: str) -> tuple[_WorkloadBinding, ...]:
    malformed = ValueError(
        f"{_LABEL} release registry entry {release_id!r} has malformed required_workloads"
    )
    if not isinstance(value, list):
        raise malformed
    bindings: list[_WorkloadBinding] = []
    for item in value:
        if not isinstance(item, dict):
            raise malformed
        fields = [item.get(name) for name in ("target", "workload", "identity")]
        if any(not isinstance(field, str) or not field for field in fields):
            raise malformed
        bindings.append(_WorkloadBinding(*fields))
    targets = [binding.target for binding in bindings]
    if len(set(targets)) != len(targets):
        raise malformed
    if not _REQUIRED_TARGETS <= set(targets) or not any(
        _INFERENCE_WORKER_RE.fullmatch(target) for target in targets
    ):
        raise ValueError(
            f"{_LABEL} release registry entry {release_id!r} must require the gateway, "
            "sglang-router, and inference-worker targets"
        )
    return tuple(bindings)


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
            or entry.get("workload_attestation_protocol") not in _SESSION_FIELDS
            or not isinstance(entry.get("require_pinned_env_mounts"), bool)
            or not isinstance(entry.get("additional_admission_entries", []), list)
            or any(
                not isinstance(name, str) or not name
                for name in entry.get("additional_admission_entries", [])
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
            workload_attestation_protocol=entry["workload_attestation_protocol"],
            required_workloads=_load_required_workloads(
                entry.get("required_workloads"), release_id
            ),
            require_pinned_env_mounts=entry["require_pinned_env_mounts"],
            additional_admission_entries=tuple(entry.get("additional_admission_entries", [])),
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


async def _fetch_collateral(quote: bytes) -> Any:
    """Fetch the Intel DCAP collateral for a quote's FMSPC and PCK CA."""
    import dcap_qvl

    return await dcap_qvl.get_collateral(dcap_qvl.PHALA_PCCS_URL, quote)


def _dcap_verify(quote: bytes, collateral: Any) -> dict[str, Any]:
    """Verify a TDX quote against collateral at the current time; returns the report."""
    import dcap_qvl

    verified = dcap_qvl.verify(quote, collateral, int(_now().timestamp()))
    return json.loads(verified.to_json())


class _QuoteVerifier:
    """Verifies quotes with bounded concurrency and shares collateral per (FMSPC, CA).

    Nodes on the same platform model share one FMSPC, so a response whose
    front door and workers span one or two node types costs one or two
    collateral fetches instead of one per quote.
    """

    def __init__(self, timeout: int) -> None:
        self._timeout = timeout
        self._slots = asyncio.Semaphore(_DCAP_CONCURRENCY)
        self._collateral: dict[tuple[str, str, bool], asyncio.Task[Any]] = {}

    def _collateral_for(self, quote: bytes) -> asyncio.Task[Any]:
        import dcap_qvl

        parsed = dcap_qvl.Quote.parse(quote)
        key = (parsed.fmspc(), parsed.ca(), parsed.is_sgx())
        task = self._collateral.get(key)
        if task is None:
            task = asyncio.ensure_future(
                asyncio.wait_for(_fetch_collateral(quote), self._timeout)
            )
            self._collateral[key] = task
        return task

    async def _verify(self, pending: _PendingQuote) -> dict[str, Any]:
        async with self._slots:
            try:
                collateral = await asyncio.shield(self._collateral_for(pending.quote))
                return await asyncio.to_thread(_dcap_verify, pending.quote, collateral)
            except Exception as exc:  # noqa: BLE001 - attribute the failing quote
                raise ValueError(
                    f"{_LABEL} {pending.subject} quote failed DCAP verification: {exc}"
                ) from exc

    async def verify_all(self, quotes: list[_PendingQuote]) -> list[dict[str, Any]]:
        """Verify every quote; raise the first failure in input order."""
        results = await asyncio.gather(
            *(self._verify(pending) for pending in quotes), return_exceptions=True
        )
        for result in results:
            if isinstance(result, BaseException):
                raise result
        return results


def _decode_quote(value: Any, subject: str = "front door") -> bytes:
    text = _require_str(value, f"{subject} receipt evidence.quote").strip()
    if _HEX_RE.fullmatch(text) and len(text) % 2 == 0:
        raw = bytes.fromhex(text)
    else:
        standard = "+" in text or "/" in text
        url_safe = "-" in text or "_" in text
        if (standard and url_safe) or not re.fullmatch(r"[A-Za-z0-9+/_-]+={0,2}", text):
            raise ValueError(f"{_LABEL} {subject} quote is neither hex nor base64")
        text = text.rstrip("=").replace("-", "+").replace("_", "/")
        raw = base64.b64decode(text + "=" * (-len(text) % 4), validate=True)
    if len(raw) < _TDX_MIN_QUOTE_BYTES:
        raise ValueError(f"{_LABEL} {subject} quote is truncated")
    version, _, tee_type = struct.unpack_from("<HHI", raw)
    if version != _TDX_QUOTE_VERSION or tee_type != _TDX_TEE_TYPE:
        raise ValueError(f"{_LABEL} {subject} quote is not a TDX v4 quote")
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


def attest_pq_transcript_digest(
    front_door_mode: str,
    mesh_ca_der: bytes,
    mesh_leaf_der: bytes,
    session_fields: tuple[bytes, ...],
    nonce: bytes,
) -> bytes:
    """SHA-384 over the length-prefixed attest-pq identity transcript.

    ``session_fields`` is ``(x25519_pub, mlkem768_ek)`` for c8s/attest-pq/v1 and
    ``(xwing_ek, xwing_ct, session_id)`` for c8s/attest-pq/v1+xwing.
    """
    fields = (
        _ATTEST_PQ_DOMAIN,
        front_door_mode.encode("utf-8"),
        hashlib.sha256(mesh_ca_der).digest(),
        hashlib.sha256(mesh_leaf_der).digest(),
        *session_fields,
        nonce,
    )
    return hashlib.sha384(b"".join(_length_prefixed(field) for field in fields)).digest()


def _session_fields(receipt: dict[str, Any], protocol: str, subject: str) -> tuple[bytes, ...]:
    values: list[bytes] = []
    for path, size in _SESSION_FIELDS[protocol]:
        value: Any = receipt
        for key in path:
            value = value.get(key) if isinstance(value, dict) else None
        raw = _b64url_decode(value, f"{subject} receipt {'.'.join(path)}")
        if len(raw) != size:
            raise ValueError(
                f"{_LABEL} {subject} receipt {'.'.join(path)} is {len(raw)} bytes, expected {size}"
            )
        values.append(raw)
    return tuple(values)


def _mesh_chain(
    receipt: dict[str, Any],
    subject: str = "front door",
    expected_ca_der: bytes | None = None,
) -> tuple[x509.Certificate, x509.Certificate]:
    pem = _require_str(receipt.get("cds_cert_pem"), f"{subject} receipt cds_cert_pem")
    try:
        certificates = x509.load_pem_x509_certificates(pem.encode("ascii"))
    except ValueError as exc:
        raise ValueError(f"{_LABEL} {subject} mesh certificate chain is not valid PEM") from exc
    if len(certificates) != 2:
        raise ValueError(
            f"{_LABEL} {subject} mesh chain must contain exactly the mesh leaf and mesh CA"
        )
    leaf, ca = certificates
    if expected_ca_der is not None and not hmac.compare_digest(
        ca.public_bytes(Encoding.DER), expected_ca_der
    ):
        raise ValueError(f"{_LABEL} {subject} mesh CA is not the front door's mesh CA")
    try:
        constraints = ca.extensions.get_extension_for_class(x509.BasicConstraints).value
    except x509.ExtensionNotFound as exc:
        raise ValueError(f"{_LABEL} {subject} mesh CA is not a CA certificate") from exc
    if not constraints.ca:
        raise ValueError(f"{_LABEL} {subject} mesh CA is not a CA certificate")
    now = _now()
    for name, certificate in (("mesh leaf", leaf), ("mesh CA", ca)):
        if not certificate.not_valid_before_utc <= now <= certificate.not_valid_after_utc:
            raise ValueError(
                f"{_LABEL} {subject} {name} certificate is outside its validity period"
            )
    try:
        ca.verify_directly_issued_by(ca)
        leaf.verify_directly_issued_by(ca)
    except (InvalidSignature, ValueError, TypeError) as exc:
        raise ValueError(f"{_LABEL} {subject} mesh leaf does not chain to the mesh CA") from exc
    return leaf, ca


def _verify_identity_proof(
    receipt: dict[str, Any],
    transcript_digest: bytes,
    mesh_leaf: x509.Certificate,
    mesh_leaf_der: bytes,
    mesh_ca_der: bytes,
    subject: str = "front door",
) -> None:
    proof = _require_dict(receipt.get("identity_proof"), f"{subject} receipt identity_proof")
    if proof.get("algorithm") != _IDENTITY_PROOF_ALGORITHM:
        raise ValueError(
            f"{_LABEL} {subject} identity proof algorithm is {proof.get('algorithm')!r}, "
            f"expected {_IDENTITY_PROOF_ALGORITHM!r}"
        )
    if proof.get("leaf_sha256") != _b64url(hashlib.sha256(mesh_leaf_der).digest()):
        raise ValueError(f"{_LABEL} {subject} identity proof does not name the returned mesh leaf")
    if proof.get("mesh_ca_sha256") != _b64url(hashlib.sha256(mesh_ca_der).digest()):
        raise ValueError(f"{_LABEL} {subject} identity proof does not name the returned mesh CA")
    public_key = mesh_leaf.public_key()
    if not isinstance(public_key, ec.EllipticCurvePublicKey) or not isinstance(
        public_key.curve, _MESH_CURVES
    ):
        raise ValueError(f"{_LABEL} {subject} mesh leaf key is not a NIST P-256 or P-384 key")
    signature = _b64url_decode(proof.get("signature"), f"{subject} identity proof signature")
    try:
        public_key.verify(signature, transcript_digest, ec.ECDSA(hashes.SHA384()))
    except InvalidSignature as exc:
        raise ValueError(
            f"{_LABEL} {subject} identity proof signature does not verify under the mesh leaf key"
        ) from exc


def _der_read(data: bytes, offset: int, tag: int) -> tuple[bytes, int]:
    """Read one short-form DER TLV with the expected tag; returns (value, next offset)."""
    if offset + 2 > len(data) or data[offset] != tag or data[offset + 1] >= 0x80:
        raise ValueError("unexpected DER encoding")
    end = offset + 2 + data[offset + 1]
    if end > len(data):
        raise ValueError("truncated DER value")
    return data[offset + 2 : end], end


def _matched_workload(leaf: x509.Certificate, subject: str) -> tuple[str, str]:
    """Parse the CA-stamped matched-workload extension; returns (name, allowlist digest tag).

    MatchedWorkload ::= SEQUENCE { formatVersion INTEGER (1), name IA5String,
    allowlistVersion IA5String, allowlistDigest OCTET STRING (32) }. Every
    field fits in a short-form length, so a long-form length is non-minimal.
    """
    stamps = [ext for ext in leaf.extensions if ext.oid == _MATCHED_WORKLOAD_OID]
    if len(stamps) != 1 or not isinstance(stamps[0].value, x509.UnrecognizedExtension):
        raise ValueError(f"{_LABEL} {subject} mesh leaf has no single matched-workload stamp")
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
        if not _WORKLOAD_NAME_RE.fullmatch(name_text) or not _ALLOWLIST_VERSION_RE.fullmatch(
            allowlist_version.decode("ascii")
        ):
            raise ValueError("invalid field grammar")
    except (ValueError, UnicodeDecodeError) as exc:
        raise ValueError(f"{_LABEL} {subject} matched-workload stamp is malformed") from exc
    return name_text, f"sha256:{digest.hex()}"


def _go_json_string(value: str) -> str:
    out = ['"']
    for char in value:
        if char in ('"', "\\"):
            out.append("\\" + char)
        elif char in _GO_JSON_ESCAPES:
            out.append(_GO_JSON_ESCAPES[char])
        elif ord(char) < 0x20:
            out.append(f"\\u{ord(char):04x}")
        elif 0xD800 <= ord(char) <= 0xDFFF:
            # Go replaces invalid UTF-8; a lone surrogate has no canonical form.
            raise ValueError("lone surrogate")
        else:
            out.append(char)
    out.append('"')
    return "".join(out)


def _canonical_allowlist_json(value: Any, kind: str) -> str:
    """Re-encode a parsed allowlist value exactly as Go's json.Marshal would."""
    if kind == "str":
        if not isinstance(value, str):
            raise ValueError("expected a string")
        return _go_json_string(value)
    if kind == "bool":
        if not isinstance(value, bool):
            raise ValueError("expected a boolean")
        return "true" if value else "false"
    if kind.startswith(("list:", "map:")) and value is None:
        return "null"
    if kind.startswith("list:"):
        if not isinstance(value, list):
            raise ValueError("expected a list")
        items = (_canonical_allowlist_json(item, kind[5:]) for item in value)
        return "[" + ",".join(items) + "]"
    if kind.startswith("map:"):
        if not isinstance(value, dict):
            raise ValueError("expected an object")
        items = (
            _go_json_string(key) + ":" + _canonical_allowlist_json(value[key], kind[4:])
            for key in sorted(value)
        )
        return "{" + ",".join(items) + "}"
    fields = _ALLOWLIST_FIELDS[kind]
    if not isinstance(value, dict):
        raise ValueError(f"expected a {kind} object")
    unknown = set(value) - {name for name, _ in fields}
    if unknown:
        raise ValueError(f"unknown {kind} fields {sorted(unknown)}")
    items = (
        _go_json_string(name) + ":" + _canonical_allowlist_json(value[name], field_kind)
        for name, field_kind in fields
        if name in value
    )
    return "{" + ",".join(items) + "}"


def allowlist_canonical_sha256(document: Any) -> str:
    """SHA-256 tag of the canonical bytes of a parsed `c8s.allowlist/v1` document."""
    if not isinstance(document, dict) or document.get("schema") != _ALLOWLIST_SCHEMA:
        raise ValueError(f"{_LABEL} allowlist document is not {_ALLOWLIST_SCHEMA}")
    try:
        canonical = _canonical_allowlist_json(document, "allowlist")
    except ValueError as exc:
        raise ValueError(f"{_LABEL} allowlist document has no canonical form: {exc}") from exc
    return _sha256_tag(canonical.encode("utf-8"))


def _admission_env_mounts(document: dict[str, Any], entry_name: str) -> dict[str, Any]:
    """Report whether every container of an allowlist entry has pinned env and mounts."""
    entry = (document.get("workloads") or {}).get(entry_name)
    if not isinstance(entry, dict):
        raise ValueError(f"{_LABEL} allowlist has no entry {entry_name!r}")
    containers = []
    for field in ("initContainers", "containers"):
        for container in entry.get(field) or []:
            containers.append(
                {
                    "kind": "init" if field == "initContainers" else "main",
                    "digest": container.get("digest"),
                    "image": container.get("image"),
                    "env": (container.get("env") or {}).get("policy"),
                    "mounts": (container.get("mounts") or {}).get("policy"),
                }
            )
    if not containers:
        raise ValueError(f"{_LABEL} allowlist entry {entry_name!r} has no containers")
    return {
        "allowlist_entry": entry_name,
        "pinned": all(
            container["env"] in _PINNED_ADMISSION_POLICIES
            and container["mounts"] in _PINNED_ADMISSION_POLICIES
            for container in containers
        ),
        "containers": containers,
    }


def _td_report(verified: dict[str, Any], subject: str) -> dict[str, str]:
    td10 = (verified.get("report") or {}).get("TD10")
    if not isinstance(td10, dict):
        raise ValueError(f"{_LABEL} {subject} DCAP result does not contain a TDX TD report")
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
        raise ValueError(f"{_LABEL} {subject} DCAP TD report is malformed")
    for name in ("mr_td", "rt_mr0", "rt_mr1", "rt_mr2", "rt_mr3"):
        if not _HEX_48_RE.fullmatch(fields[name]):
            raise ValueError(f"{_LABEL} {subject} DCAP TD report is missing {name}")
    return fields


def _check_measurements(
    report: dict[str, str], pin: _ReleasePin, subject: str
) -> dict[str, Any]:
    for name, field in (("MRTD", "mr_td"), ("RTMR1", "rt_mr1"), ("RTMR2", "rt_mr2")):
        expected = getattr(pin, name.lower())
        if not hmac.compare_digest(report[field], expected):
            raise ValueError(
                f"{_LABEL} {subject} {name} does not match reviewed release {pin.release_id!r}"
            )
    rtmr3 = pin.accepted_rtmr3.get(report["rt_mr3"])
    if rtmr3 is None:
        raise ValueError(
            f"{_LABEL} {subject} RTMR3 {report['rt_mr3']} is not in the accepted set for "
            f"release {pin.release_id!r}"
        )
    return rtmr3


def _check_report(
    verified: dict[str, Any], pending: _PendingQuote, pin: _ReleasePin
) -> tuple[str, dict[str, str], dict[str, Any]]:
    """Apply the TCB, debug, report_data, and release-pin policy to one verified quote."""
    subject = pending.subject
    tcb_status = verified.get("status")
    if tcb_status != "UpToDate":
        raise ValueError(
            f"{_LABEL} {subject} TDX TCB status is {tcb_status!r}, expected 'UpToDate'"
        )
    report = _td_report(verified, subject)
    if bytes.fromhex(report["td_attributes"])[0] != 0:
        raise ValueError(f"{_LABEL} {subject} TD runs in debug mode (TD_ATTRIBUTES TUD set)")
    report_data = bytes.fromhex(report["report_data"])
    if not hmac.compare_digest(report_data[:48], pending.transcript_digest):
        raise ValueError(
            f"{_LABEL} {subject} report_data does not match the {pending.protocol} transcript"
        )
    if report_data[48:] != bytes(16):
        raise ValueError(f"{_LABEL} {subject} report_data padding is not zero")
    return tcb_status, report, _check_measurements(report, pin, subject)


def _collect_receipts(
    document: dict[str, Any], pin: _ReleasePin
) -> dict[str, dict[str, Any]]:
    """Map each required target to its single receipt entry; fail on gaps or extras."""
    entries = document.get("receipts")
    if not isinstance(entries, list):
        raise ValueError(f"{_LABEL} attestation is missing receipts")
    required = {binding.target for binding in pin.required_workloads}
    found: dict[str, dict[str, Any]] = {}
    for entry in entries:
        target = entry.get("target") if isinstance(entry, dict) else None
        if not isinstance(target, str) or not target:
            raise ValueError(f"{_LABEL} attestation has a receipt without a target")
        if target in required:
            if target in found:
                raise ValueError(f"{_LABEL} workload {target!r} has more than one receipt")
            found[target] = entry
        elif _INFERENCE_WORKER_RE.fullmatch(target):
            raise ValueError(
                f"{_LABEL} inference worker {target!r} is not a reviewed target of release "
                f"{pin.release_id!r}"
            )
    for binding in pin.required_workloads:
        if binding.target not in found:
            raise ValueError(
                f"{_LABEL} attestation is missing the receipt for workload {binding.target!r}"
            )
    return found


def _prepare_workload(
    entry: dict[str, Any],
    binding: _WorkloadBinding,
    pin: _ReleasePin,
    nonce: str,
    nonce_raw: bytes,
    mesh_ca_der: bytes,
    allowlist_sha256: str,
) -> _Workload:
    """Check one workload receipt up to (not including) DCAP verification."""
    subject = f"workload {binding.target!r}"
    for field in ("workload", "identity"):
        if entry.get(field) != getattr(binding, field):
            raise ValueError(
                f"{_LABEL} {subject} {field} {entry.get(field)!r} does not match the reviewed "
                f"target binding {getattr(binding, field)!r}"
            )
    receipt = _require_dict(entry.get("receipt"), f"{subject} receipt")
    if receipt.get("version") != _ATTEST_PQ_VERSION:
        raise ValueError(f"{_LABEL} {subject} receipt is not {_ATTEST_PQ_VERSION}")
    if receipt.get("platform") != "tdx":
        raise ValueError(f"{_LABEL} {subject} receipt platform is not tdx")
    receipt_nonce = _require_str(receipt.get("nonce"), f"{subject} receipt nonce")
    if not hmac.compare_digest(receipt_nonce, nonce):
        raise ValueError(f"{_LABEL} {subject} receipt nonce does not match the request nonce")
    front_door_mode = _require_str(
        receipt.get("front_door_mode"), f"{subject} receipt front_door_mode"
    )

    mesh_leaf, _ = _mesh_chain(receipt, subject, expected_ca_der=mesh_ca_der)
    mesh_leaf_der = mesh_leaf.public_bytes(Encoding.DER)
    stamped_name, stamped_allowlist = _matched_workload(mesh_leaf, subject)
    if stamped_name != binding.identity:
        raise ValueError(
            f"{_LABEL} {subject} mesh leaf matched-workload stamp {stamped_name!r} does not match "
            f"the reviewed identity {binding.identity!r}"
        )
    if stamped_allowlist != allowlist_sha256:
        raise ValueError(
            f"{_LABEL} {subject} stamped allowlist {stamped_allowlist} is not the active "
            f"allowlist {allowlist_sha256}"
        )

    protocol = pin.workload_attestation_protocol
    transcript_digest = attest_pq_transcript_digest(
        front_door_mode,
        mesh_ca_der,
        mesh_leaf_der,
        _session_fields(receipt, protocol, subject),
        nonce_raw,
    )
    _verify_identity_proof(
        receipt, transcript_digest, mesh_leaf, mesh_leaf_der, mesh_ca_der, subject
    )
    quote = _decode_quote(
        _require_dict(receipt.get("evidence"), f"{subject} receipt evidence").get("quote"),
        subject,
    )
    return _Workload(
        binding=binding,
        front_door_mode=front_door_mode,
        allowlist_sha256=allowlist_sha256,
        pending=_PendingQuote(subject, protocol, quote, transcript_digest),
    )


async def verify_c8s(request: dict[str, Any]) -> None:
    """Verify a Confidential AI c8s front door and its plaintext-path workloads."""

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

        active_allowlist = _require_dict(c8s.get("activeAllowlist"), "c8s.activeAllowlist")
        allowlist_sha256 = _require_str(
            active_allowlist.get("sha256"), "c8s.activeAllowlist.sha256"
        )
        if allowlist_sha256 not in pin.accepted_allowlist_sha256:
            raise ValueError(
                f"{_LABEL} allowlist {allowlist_sha256} is not accepted for release {release_id!r}"
            )
        # The document is inspected below, so it must be the one the digest names.
        # Each workload's CA-stamped allowlist digest must also be this digest.
        allowlist = _require_dict(active_allowlist.get("document"), "c8s.activeAllowlist.document")
        if allowlist_canonical_sha256(allowlist) != allowlist_sha256:
            raise ValueError(
                f"{_LABEL} c8s.activeAllowlist.document does not hash to "
                "c8s.activeAllowlist.sha256"
            )

        front_door_quote = _PendingQuote(
            subject="front door",
            protocol=_ATTEST_LB_VERSION,
            quote=_decode_quote(
                _require_dict(receipt.get("evidence"), "frontDoor.receipt.evidence").get("quote")
            ),
            transcript_digest=transcript_digest,
        )

        # The response may state the workload protocol; it must agree with the review.
        declared_protocol = c8s.get("attestationProtocol")
        if declared_protocol is not None and declared_protocol != pin.workload_attestation_protocol:
            raise ValueError(
                f"{_LABEL} c8s.attestationProtocol {declared_protocol!r} is not the reviewed "
                f"{pin.workload_attestation_protocol!r} for release {release_id!r}"
            )
        entries = _collect_receipts(document, pin)
        workloads = [
            _prepare_workload(
                entries[binding.target],
                binding,
                pin,
                nonce,
                nonce_raw,
                mesh_ca_der,
                allowlist_sha256,
            )
            for binding in pin.required_workloads
        ]

        # Admission policy of each plaintext-path workload's stamped entry, plus
        # the reviewed entries that can reach them (e.g. gateway-state-mounter).
        admission = {
            binding.target: _admission_env_mounts(allowlist, binding.identity)
            for binding in pin.required_workloads
        }
        for entry_name in pin.additional_admission_entries:
            admission[entry_name] = _admission_env_mounts(allowlist, entry_name)
        env_mounts_pinned = all(item["pinned"] for item in admission.values())
        if pin.require_pinned_env_mounts and not env_mounts_pinned:
            unpinned = sorted(name for name, item in admission.items() if not item["pinned"])
            raise ValueError(
                f"{_LABEL} release {release_id!r} requires pinned env and mounts, but the "
                f"allowlist admits {', '.join(unpinned)} with an unconstrained env or mount policy"
            )

        pending = [front_door_quote, *(workload.pending for workload in workloads)]
        verified = await _QuoteVerifier(timeout).verify_all(pending)
        tcb_status, report, rtmr3 = _check_report(verified[0], front_door_quote, pin)
        rtmr3_entries = [rtmr3]
        verified_workloads: dict[str, dict[str, Any]] = {}
        for workload, result in zip(workloads, verified[1:]):
            workload_tcb, workload_report, workload_rtmr3 = _check_report(
                result, workload.pending, pin
            )
            rtmr3_entries.append(workload_rtmr3)
            verified_workloads[workload.binding.target] = {
                "workload": workload.binding.workload,
                "identity": workload.binding.identity,
                "release_id": pin.release_id,
                "attestation_protocol": workload.pending.protocol,
                "front_door_mode": workload.front_door_mode,
                "allowlist_sha256": workload.allowlist_sha256,
                "tcb_status": workload_tcb,
                "mrtd": workload_report["mr_td"],
                "rtmr1": workload_report["rt_mr1"],
                "rtmr2": workload_report["rt_mr2"],
                "rtmr3": workload_report["rt_mr3"],
                "rtmr3_source": workload_rtmr3.get("source"),
                "operator_key_armed": workload_rtmr3.get("operator_key_armed") is not False,
                # RTMR0 is per-host firmware configuration; recorded, never pinned.
                "rtmr0": workload_report["rt_mr0"],
            }

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
                    "trust_boundary": "c8s-front-door-and-plaintext-workloads",
                    "evidence_scope": "router",
                    "attestation_protocol": _ATTEST_LB_VERSION,
                    "workload_attestation_protocol": pin.workload_attestation_protocol,
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
                    # Armed if any verified node's accepted RTMR3 arms the operator key.
                    "operator_key_armed": any(
                        entry.get("operator_key_armed") is not False for entry in rtmr3_entries
                    ),
                    "verified_workloads": verified_workloads,
                    # Env and mounts are not attested; an `any` policy lets any
                    # Kubernetes API writer change them without changing a quote.
                    "admission_env_mounts_pinned": env_mounts_pinned,
                    "admission_env_mounts": admission,
                    "require_pinned_env_mounts": pin.require_pinned_env_mounts,
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
