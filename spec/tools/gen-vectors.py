#!/usr/bin/env python3
"""Regenerate the deterministic values in spec/test-vectors.md.

Every value in the test-vectors doc is reproduced here from first principles —
an implementation independent of the Rust reference — so the doc, the
reference implementation (`tests/spec_vectors.rs`), and this script can be
cross-checked against each other. Run with no arguments to verify every
published constant and print the full set of values with intermediates.

Requires: python3 stdlib + `cryptography` (Ed25519, X25519, HKDF, AES-GCM).

The artifact bytes below are the exact bytes the reference implementation
serves for this fixture content. Consumers hash and verify these bytes as-is
(Appendix A); JCS is just a producer-side way to emit deterministic output. JSON
here is compact (`separators=(",", ":")`), insertion-ordered, ASCII, matching
the reference implementation's wire order.
"""

import hashlib
import json

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.asymmetric.x25519 import (
    X25519PrivateKey,
    X25519PublicKey,
)
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.hashes import SHA256
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

import base64


def dump(value) -> bytes:
    """Compact, insertion-ordered JSON bytes (the keyset wire form)."""
    return json.dumps(value, separators=(",", ":")).encode("ascii")


def jcs(value) -> bytes:
    """JCS bytes under the ACI constraints (ASCII names, integer numbers):
    compact serialization with sorted member names (spec §7.2, §8)."""
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("ascii")


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_prefixed(data: bytes) -> str:
    return "sha256:" + sha256_hex(data)


def b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def ed25519_from_seed(seed: bytes):
    key = Ed25519PrivateKey.from_private_bytes(seed)
    pub = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return key, pub.hex()


def x25519_from_seed(seed: bytes):
    key = X25519PrivateKey.from_private_bytes(seed)
    pub = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return key, pub.hex()


# ---- Fixed keys ------------------------------------------------------------
RECEIPT_KEY, RECEIPT_PUB = ed25519_from_seed(bytes([0x02]) * 32)
E2EE_KEY, E2EE_PUB = x25519_from_seed(bytes([0x03]) * 32)
CLIENT_KEY, CLIENT_PUB = x25519_from_seed(bytes([0x04]) * 32)
EPH_REQUEST_KEY, EPH_REQUEST_PUB = x25519_from_seed(bytes([0x05]) * 32)
EPH_RESPONSE_KEY, EPH_RESPONSE_PUB = x25519_from_seed(bytes([0x06]) * 32)
EPH_SSE_KEY, EPH_SSE_PUB = x25519_from_seed(bytes([0x07]) * 32)

TLS_SPKI = "c0" * 32
CHANNEL_SPKI = "d1" * 32

ALGO_ED25519 = "ed25519"
E2EE_ALGO = "x25519-aes-256-gcm-hkdf-sha256"

# ---- §1 workload keyset (spec §3.1) ----------------------------------------
KEYSET = {
    "subject": "dstack-app://example-app",
    "not_after": 1800000000,
    "receipt_signing_keys": [
        {"key_id": "receipt-1", "algo": ALGO_ED25519, "public_key": RECEIPT_PUB}
    ],
    "e2ee_public_keys": [
        {"key_id": "e2ee-1", "algo": E2EE_ALGO, "public_key": E2EE_PUB}
    ],
    "tls_public_keys": [{"spki_sha256": TLS_SPKI, "domain": "api.example.com"}],
}
KEYSET_BYTES = jcs(KEYSET)
KEYSET_DIGEST = sha256_prefixed(KEYSET_BYTES)


# ---- §2 attestation statement and report_data (spec §3.2) -------------------
def statement(nonce) -> bytes:
    nonce_member = "null" if nonce is None else f'"{nonce}"'
    return (
        '{"keyset_digest":"%s","nonce":%s,"purpose":"aci.report_data.v1"}'
        % (KEYSET_DIGEST, nonce_member)
    ).encode("ascii")


TEST_NONCE = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"  # bytes 0x00..0x1f
STATEMENT_NONCE = statement(TEST_NONCE)
STATEMENT_NULL = statement(None)
REPORT_DATA_NONCE = sha256_hex(STATEMENT_NONCE)
REPORT_DATA_NULL = sha256_hex(STATEMENT_NULL)
REPORT_DATA_SLOT = REPORT_DATA_NONCE + "00" * 32

# ---- §3 attested session (spec §8) ------------------------------------------
EVIDENCE_BYTES = b"example-evidence"
EVIDENCE_DIGEST = sha256_prefixed(EVIDENCE_BYTES)
EVIDENCE_DATA_URI = "data:text/plain;base64," + b64(EVIDENCE_BYTES)

SESSION = {
    "api_version": "aci/1",
    "upstream_name": "demo-upstream",
    "endpoint": "https://upstream.example.com",
    "verifier_id": "example/1",
    "established_at": 1750000000,
    "expires_at": 1750003600,
    "channel_binding": [
        {
            "type": "tls_spki_sha256",
            "origin": "https://upstream.example.com",
            "spki_sha256": CHANNEL_SPKI,
        }
    ],
    "claims": {
        "tee_attested": {
            "status": "asserted",
            "source": "hardware_proven",
            "reason": "example quote verified",
        },
        "gpu_attested": {"status": "unknown"},
        "tcb_up_to_date": {"status": "unknown"},
        "os_known_good": {"status": "unknown"},
        "serving_software_known_good": {"status": "unknown"},
        "model_weights_provenance": {"status": "unknown"},
        # `extra` keys in ascending order (the reference stores them sorted).
        "extra": {"gpu_arch": "HOPPER", "tcb_status": "UpToDate"},
    },
    "evidence": {"digest": EVIDENCE_DIGEST, "data": EVIDENCE_DATA_URI},
}
SESSION_BYTES = jcs(SESSION)
SESSION_ID = sha256_hex(SESSION_BYTES)  # bare hex: ids are not digest fields

# ---- §4 receipt (spec §7) ----------------------------------------------------
REQUEST_BODY = b'{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}'
RESPONSE_BODY = b'{"choices":[],"id":"chatcmpl-123"}'
REQUEST_BODY_HASH = sha256_prefixed(REQUEST_BODY)
RESPONSE_BODY_HASH = sha256_prefixed(RESPONSE_BODY)

RECEIPT_PAYLOAD = {
    "api_version": "aci/1",
    "receipt_id": "rcpt-0001",
    "chat_id": "chatcmpl-123",
    "model": "demo-model",
    "workload_keyset_digest": KEYSET_DIGEST,
    "endpoint": "/v1/chat/completions",
    "method": "POST",
    "served_at": 1750000000,
    "event_log": [
        {"type": "request.received", "body_hash": REQUEST_BODY_HASH},
        {"type": "request.forwarded", "body_hash": REQUEST_BODY_HASH},
        {
            "type": "upstream.verified",
            "result": "verified",
            "required": True,
            "model_id": "demo-model",
            "session_id": SESSION_ID,
        },
        {"type": "response.returned", "body_hash": RESPONSE_BODY_HASH},
    ],
}
UNSIGNED_DOCUMENT = {**RECEIPT_PAYLOAD, "key_id": "receipt-1"}
SIGNING_INPUT = jcs(UNSIGNED_DOCUMENT)
SIGNING_INPUT_SHA256 = sha256_hex(SIGNING_INPUT)
RECEIPT_SIG = RECEIPT_KEY.sign(SIGNING_INPUT).hex()
DOCUMENT_BYTES = jcs({**UNSIGNED_DOCUMENT, "signature": RECEIPT_SIG})

# ---- Published constants (must match spec/test-vectors.md) -------------------
PINNED = {
    "receipt-1 public key": (RECEIPT_PUB,
        "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394"),
    "e2ee-1 public key": (E2EE_PUB,
        "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22"),
    "client public key": (CLIENT_PUB,
        "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b"),
    "§1 workload_keyset_digest": (KEYSET_DIGEST,
        "sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371"),
    "§2 report_data (test nonce)": (REPORT_DATA_NONCE,
        "df2174d28130852b413646a3786927b93e94c11d770268b65def8bdba45cb49e"),
    "§2 report_data (null)": (REPORT_DATA_NULL,
        "0633919ca3f00e97bafaa3304278eb22420cc3ff0d19f87dfca2d3f7508150bc"),
    "§3 session_id": (SESSION_ID,
        "95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f"),
    "§4 sha256(signing input)": (SIGNING_INPUT_SHA256,
        "1bd328e6880a5a12b3915af95ea32111310e04ab9e21ac3d71ce268e33b965c9"),
    "§4 signature": (RECEIPT_SIG,
        "d5b005e093bde3b577faf270b7184b09e169cacb0ecb206b103bd2581f997db0"
        "3da616175454b063323a23ac1dc68f1ce506c2a6eba8aa0561d5e724f0b80c03"),
}


def main():
    ok = True
    for label, (got, want) in PINNED.items():
        if got != want:
            print(f"[MISMATCH] {label}\n    got:  {got}\n    want: {want}")
            ok = False
    print("SELF-CHECK:", "all published constants reproduced" if ok
          else "FAILURES ABOVE")
    print()
    print("== Fixed keys ==")
    print("receipt-1 (ed25519, seed 02*32) pub =", RECEIPT_PUB)
    print("e2ee-1 (x25519, seed 03*32) pub =", E2EE_PUB)
    print("client (x25519, seed 04*32) pub =", CLIENT_PUB)
    print("request ephemeral (seed 05*32) pub =", EPH_REQUEST_PUB)
    print("response ephemeral (seed 06*32) pub =", EPH_RESPONSE_PUB)
    print("sse ephemeral (seed 07*32) pub =", EPH_SSE_PUB)
    print()
    print("== §1 workload keyset ==")
    print("keyset JCS bytes =", KEYSET_BYTES.decode())
    print("workload_keyset_digest =", KEYSET_DIGEST)
    print()
    print("== §2 attestation statement / report_data ==")
    print("test nonce =", TEST_NONCE)
    print("statement (test nonce) =", STATEMENT_NONCE.decode())
    print("report_data (test nonce) =", REPORT_DATA_NONCE)
    print("statement (null) =", STATEMENT_NULL.decode())
    print("report_data (null) =", REPORT_DATA_NULL)
    print("report-data slot (64 bytes) =", REPORT_DATA_SLOT)
    print()
    print("== §3 attested session ==")
    print("evidence digest =", EVIDENCE_DIGEST)
    print("evidence data URI =", EVIDENCE_DATA_URI)
    print("session bytes =", SESSION_BYTES.decode())
    print("session_id =", SESSION_ID)
    print()
    print("== §4 receipt ==")
    print("request body_hash =", REQUEST_BODY_HASH)
    print("response body_hash =", RESPONSE_BODY_HASH)
    print("signing input (JCS, no signature) =", SIGNING_INPUT.decode())
    print("sha256(signing input) =", SIGNING_INPUT_SHA256)
    print("signature =", RECEIPT_SIG)
    print("document bytes =", DOCUMENT_BYTES.decode())
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
