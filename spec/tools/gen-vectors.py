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
(spec §3); JCS is just a producer-side way to emit deterministic output. JSON
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
    """Compact, insertion-ordered JSON bytes (the reference wire form)."""
    return json.dumps(value, separators=(",", ":")).encode("ascii")


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

# ---- §1 workload keyset (spec §4.1) ----------------------------------------
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
KEYSET_BYTES = dump(KEYSET)
KEYSET_DIGEST = sha256_prefixed(KEYSET_BYTES)
KEYSET_B64 = b64(KEYSET_BYTES)


# ---- §2 attestation statement and report_data (spec §4.2) -------------------
def statement(nonce) -> bytes:
    nonce_member = "null" if nonce is None else f'"{nonce}"'
    return (
        '{"keyset_digest":"%s","nonce":%s,"purpose":"aci.report_data.v1"}'
        % (KEYSET_DIGEST, nonce_member)
    ).encode("ascii")


STATEMENT_NONCE = statement("test-nonce")
STATEMENT_NULL = statement(None)
REPORT_DATA_NONCE = sha256_hex(STATEMENT_NONCE)
REPORT_DATA_NULL = sha256_hex(STATEMENT_NULL)
REPORT_DATA_SLOT = REPORT_DATA_NONCE + "00" * 32

# ---- §3 attested session (spec §9) ------------------------------------------
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
SESSION_BYTES = dump(SESSION)
SESSION_ID = sha256_prefixed(SESSION_BYTES)

# ---- §4 receipt (spec §8) ----------------------------------------------------
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
PAYLOAD_BYTES = dump(RECEIPT_PAYLOAD)
PAYLOAD_SHA256 = sha256_hex(PAYLOAD_BYTES)
PAYLOAD_B64 = b64(PAYLOAD_BYTES)
RECEIPT_SIG = RECEIPT_KEY.sign(PAYLOAD_BYTES).hex()

ENVELOPE = {
    "payload_b64": PAYLOAD_B64,
    "key_id": "receipt-1",
    "algo": ALGO_ED25519,
    "signature": RECEIPT_SIG,
}

# ---- §5 E2EE v3 (spec §7.1) ---------------------------------------------------
CONTEXT_REQUEST = "aci.e2ee.v3.request"
CONTEXT_RESPONSE = "aci.e2ee.v3.response"
ENVELOPE_MODEL = "demo-model"


def aad(context: str, model: str, client_public_key_hex: str | None = None) -> bytes:
    """§7.1 AAD; the request appends the 0x00-delimited client key hex (§7.2)."""
    tail = b"" if client_public_key_hex is None else b"\x00" + client_public_key_hex.encode()
    return context.encode() + b"\x00" + model.encode() + tail


def seal(ephemeral: X25519PrivateKey, recipient_pub_hex: str, context: str,
         model: str, gcm_nonce: bytes, plaintext: bytes,
         client_public_key_hex: str | None = None):
    """The §7.1 sealing with pinned ephemeral key and GCM nonce. Returns
    (shared_secret, aes_key, sealed_bytes)."""
    recipient = X25519PublicKey.from_public_bytes(bytes.fromhex(recipient_pub_hex))
    shared = ephemeral.exchange(recipient)
    key = HKDF(algorithm=SHA256(), length=32, salt=None,
               info=context.encode()).derive(shared)
    ct = AESGCM(key).encrypt(gcm_nonce, plaintext,
                             aad(context, model, client_public_key_hex))
    eph_pub = ephemeral.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return shared, key, eph_pub + gcm_nonce + ct


REQUEST_GCM_NONCE = bytes.fromhex("000102030405060708090a0b")
RESPONSE_GCM_NONCE = bytes.fromhex("101112131415161718191a1b")
SSE_GCM_NONCE = bytes.fromhex("202122232425262728292a2b")

REQUEST_SHARED, REQUEST_AES_KEY, REQUEST_SEALED = seal(
    EPH_REQUEST_KEY, E2EE_PUB, CONTEXT_REQUEST, ENVELOPE_MODEL,
    REQUEST_GCM_NONCE, REQUEST_BODY, CLIENT_PUB,
)
REQUEST_ENVELOPE = {"model": ENVELOPE_MODEL, "sealed_b64": b64(REQUEST_SEALED)}

RESPONSE_SHARED, RESPONSE_AES_KEY, RESPONSE_SEALED = seal(
    EPH_RESPONSE_KEY, CLIENT_PUB, CONTEXT_RESPONSE, ENVELOPE_MODEL,
    RESPONSE_GCM_NONCE, RESPONSE_BODY,
)
RESPONSE_ENVELOPE = {"sealed_b64": b64(RESPONSE_SEALED)}

SSE_EVENT_BODY = (
    b'{"id":"chatcmpl-123","object":"chat.completion.chunk",'
    b'"choices":[{"index":0,"delta":{"content":"hi"}}]}'
)
SSE_SHARED, SSE_AES_KEY, SSE_SEALED = seal(
    EPH_SSE_KEY, CLIENT_PUB, CONTEXT_RESPONSE, ENVELOPE_MODEL,
    SSE_GCM_NONCE, SSE_EVENT_BODY,
)
SSE_WIRE = b"data: " + dump({"sealed_b64": b64(SSE_SEALED)}) + b"\n\ndata: [DONE]\n\n"

# ---- Published constants (must match spec/test-vectors.md) -------------------
PINNED = {
    "receipt-1 public key": (RECEIPT_PUB,
        "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394"),
    "e2ee-1 public key": (E2EE_PUB,
        "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22"),
    "client public key": (CLIENT_PUB,
        "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b"),
    "§1 workload_keyset_digest": (KEYSET_DIGEST,
        "sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4"),
    "§2 report_data (test-nonce)": (REPORT_DATA_NONCE,
        "8b899aae55437dec4d1d0d435920e112aca2a74d17595eeb601a7764d901ea07"),
    "§2 report_data (null)": (REPORT_DATA_NULL,
        "a98b0e34ef2ce05cf7d3fd64d86889deaf6836b8aa4e5d8baa9dd437fea07987"),
    "§3 session_id": (SESSION_ID,
        "sha256:a595d269728e15fe8236af46586fe84f220696c0d7d4e647eed36922b7b20cb6"),
    "§4 sha256(payload)": (PAYLOAD_SHA256,
        "5a04d7ce350a09a9faa4f32e5a21790cd1080a46239039538bac98c798dc2dab"),
    "§4 signature": (RECEIPT_SIG,
        "b0b2c830be73d6b6ad9a90b75b9c347a930e6a918e6e4f70ad1c3ce0d3dbfe67"
        "89504be5f7d317d24ba9eb84cd8bf634d58e898de89baa7fc939abd12e1b7400"),
    "§5 request sealed sha256": (sha256_hex(REQUEST_SEALED),
        "f659d87733296175e98b40662bacc965179bfa43d036b73d9790ea643c817afa"),
    "§5 response sealed sha256": (sha256_hex(RESPONSE_SEALED),
        "24fea6abf43c6db9675a6de63e2f3c9412a03afc7114a7c184ebe418347fbc6a"),
    "§5 sse sealed sha256": (sha256_hex(SSE_SEALED),
        "49947db6b84eef6b1ee1ceb1d63623f991de81c73dc9a9b26f7dc4224a9a1ecc"),
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
    print("keyset bytes =", KEYSET_BYTES.decode())
    print("workload_keyset_b64 =", KEYSET_B64)
    print("workload_keyset_digest =", KEYSET_DIGEST)
    print()
    print("== §2 attestation statement / report_data ==")
    print("statement (test-nonce) =", STATEMENT_NONCE.decode())
    print("report_data (test-nonce) =", REPORT_DATA_NONCE)
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
    print("payload bytes =", PAYLOAD_BYTES.decode())
    print("sha256(payload) =", PAYLOAD_SHA256)
    print("payload_b64 =", PAYLOAD_B64)
    print("signature =", RECEIPT_SIG)
    print("envelope =", dump(ENVELOPE).decode())
    print()
    print("== §5 E2EE v3 ==")
    print("request AAD hex =", aad(CONTEXT_REQUEST, ENVELOPE_MODEL, CLIENT_PUB).hex())
    print("request shared_secret =", REQUEST_SHARED.hex())
    print("request AES key =", REQUEST_AES_KEY.hex())
    print("request sealed hex =", REQUEST_SEALED.hex())
    print("request envelope =", dump(REQUEST_ENVELOPE).decode())
    print()
    print("response AAD hex =", aad(CONTEXT_RESPONSE, ENVELOPE_MODEL).hex())
    print("response shared_secret =", RESPONSE_SHARED.hex())
    print("response AES key =", RESPONSE_AES_KEY.hex())
    print("response sealed hex =", RESPONSE_SEALED.hex())
    print("response body =", dump(RESPONSE_ENVELOPE).decode())
    print()
    print("sse event body =", SSE_EVENT_BODY.decode())
    print("sse shared_secret =", SSE_SHARED.hex())
    print("sse AES key =", SSE_AES_KEY.hex())
    print("sse sealed hex =", SSE_SEALED.hex())
    print("sse wire bytes =", SSE_WIRE.decode(), end="")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
