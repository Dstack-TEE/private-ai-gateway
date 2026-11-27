#!/usr/bin/env python3
"""Regenerate the deterministic values in the ACI, E2EE v2 and v2.1, and
proxy-image binding vector docs.

Every value in the test-vectors doc is reproduced here from first principles —
an implementation independent of the Rust reference — so the doc, the
reference implementation (`tests/spec_vectors.rs`), and this script can be
cross-checked against each other. Run with no arguments to verify every
published constant and print the full set of values with intermediates.

Requires: python3 stdlib + `cryptography` (Ed25519 and X25519).

The artifact bytes below are the exact bytes the reference implementation
serves for this fixture content. Consumers hash and verify these bytes as-is
(Appendix A); JCS is just a producer-side way to emit deterministic output. JSON
here is compact (`separators=(",", ":")`), insertion-ordered, ASCII, matching
the reference implementation's wire order.
"""

import hashlib
import hmac
import json

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.asymmetric.x25519 import (
    X25519PrivateKey,
    X25519PublicKey,
)

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

# ---- proxy-image binding extension §6: the §3 session with that binding -----
PROXY_BINDING = {
    "type": "proxy_image_sha256",
    "provider": "privatemode",
    "proxy_image_digest": "sha256:" + "e1" * 32,
    "credential_sha256": "f2" * 32,
}
PROXY_SESSION_ID = sha256_hex(jcs({**SESSION, "channel_binding": [PROXY_BINDING]}))

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

# ---- E2EE v2 AAD (E2EE v2 spec §6) ----------------------------------------
REQUEST_AAD = jcs({
    "purpose": "aci.e2ee.request.v2",
    "algo": E2EE_ALGO,
    "model": "demo-model",
    "field": "messages.0.content",
    "nonce": TEST_NONCE,
    "ts": 1750000000,
})
RESPONSE_AAD = jcs({
    "purpose": "aci.e2ee.response.v2",
    "algo": E2EE_ALGO,
    "model": "demo-model",
    "id": "chatcmpl-123",
    "field": "choices.0.message.content",
    "nonce": TEST_NONCE,
    "ts": 1750000000,
})

# ---- E2EE v2.1 (E2EE v2 spec §10) ------------------------------------------
# Every random input is fixed so the ciphertexts are reproducible. Real clients
# and services draw them fresh for every request and every field.
V21_ALGO = "x25519-aes-256-gcm-hkdf-sha256-v2.1"
V21_REQUEST_SECRET = bytes([0x0A]) * 32
V21_CLIENT_KEY, V21_CLIENT_PUB = x25519_from_seed(bytes([0x04]) * 32)
V21_FIELD_EPH, _ = x25519_from_seed(bytes([0x05]) * 32)
V21_FIELD_NONCE = bytes([0x06]) * 12
V21_REQUEST_KEY_EPH, _ = x25519_from_seed(bytes([0x07]) * 32)
V21_REQUEST_KEY_NONCE = bytes([0x08]) * 12
V21_RESPONSE_EPH, _ = x25519_from_seed(bytes([0x09]) * 32)
V21_RESPONSE_NONCE = bytes([0x0B]) * 12


def x25519_seal(eph, recipient_pub_hex, plaintext, aad, salt, info, nonce):
    """E2EE v2 §4 X25519 field: ephemeral pub || nonce || ciphertext || tag."""
    recipient = X25519PublicKey.from_public_bytes(bytes.fromhex(recipient_pub_hex))
    shared = eph.exchange(recipient)
    key = HKDF(algorithm=hashes.SHA256(), length=32, salt=salt, info=info).derive(shared)
    eph_pub = eph.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    return (eph_pub + nonce + AESGCM(key).encrypt(nonce, plaintext, aad)).hex()


V21_FIELD_AAD = jcs({
    "purpose": "aci.e2ee.request.v2.1",
    "algo": V21_ALGO,
    "model": "demo-model",
    "field": "messages.0.content",
    "nonce": TEST_NONCE,
    "ts": 1750000000,
})
# Every v2.1 plaintext is the JSON encoding of the value (§10.5).
V21_FIELD_CIPHERTEXT = x25519_seal(
    V21_FIELD_EPH, E2EE_PUB, json.dumps("hello").encode(), V21_FIELD_AAD,
    V21_REQUEST_SECRET, b"aci.e2ee.v2.1.x25519", V21_FIELD_NONCE)
V21_BODY = {
    "model": "demo-model",
    "temperature": 0.7,
    "messages": [{"role": "user", "content": V21_FIELD_CIPHERTEXT}],
}
# RFC 8785 JCS. Python's float repr matches the ECMAScript form JCS requires
# for this body's numbers; other bodies may need a full JCS library.
V21_BODY_JCS = jcs(V21_BODY)
V21_BODY_DIGEST = hashlib.sha256(V21_BODY_JCS).digest()
V21_REQUEST_KEY_AAD = jcs({
    "purpose": "aci.e2ee.request-key.v2.1",
    "algo": V21_ALGO,
    "model": "demo-model",
    "client_pub_key": V21_CLIENT_PUB,
    "nonce": TEST_NONCE,
    "ts": 1750000000,
})
V21_REQUEST_KEY = x25519_seal(
    V21_REQUEST_KEY_EPH, E2EE_PUB, V21_REQUEST_SECRET + V21_BODY_DIGEST,
    V21_REQUEST_KEY_AAD, None, b"aci.e2ee.v2.1.x25519.request-key",
    V21_REQUEST_KEY_NONCE)
V21_RESPONSE_AAD = jcs({
    "purpose": "aci.e2ee.response.v2.1",
    "algo": V21_ALGO,
    "model": "demo-model",
    "id": "chatcmpl-123",
    "field": "choices.0.message.content",
    "nonce": TEST_NONCE,
    "ts": 1750000000,
})
# Every v2.1 plaintext is the JSON encoding of the value (§10.5).
V21_RESPONSE_CIPHERTEXT = x25519_seal(
    V21_RESPONSE_EPH, V21_CLIENT_PUB, json.dumps("plain-answer").encode(), V21_RESPONSE_AAD,
    V21_REQUEST_SECRET, b"aci.e2ee.v2.1.x25519", V21_RESPONSE_NONCE)

V21_RESPONSE_OBJECT = {
    "id": "chatcmpl-123",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": V21_RESPONSE_CIPHERTEXT}}],
}
V21_MAC_KEY = HKDF(algorithm=hashes.SHA256(), length=32, salt=None,
                   info=b"aci.e2ee.v2.1.response-mac").derive(V21_REQUEST_SECRET)
V21_RESPONSE_MAC = hmac.new(V21_MAC_KEY, jcs(V21_RESPONSE_OBJECT), hashlib.sha256).hexdigest()

# A two-event stream: one encrypted delta, one readable-only finish event.
V21_STREAM_AAD = jcs({
    "purpose": "aci.e2ee.response.v2.1",
    "algo": V21_ALGO,
    "model": "demo-model",
    "id": "chatcmpl-123",
    "field": "choices.0.delta.content",
    "nonce": TEST_NONCE,
    "ts": 1750000000,
})
V21_STREAM_CIPHERTEXT = x25519_seal(
    x25519_from_seed(bytes([0x0C]) * 32)[0], V21_CLIENT_PUB, json.dumps("Hello").encode(), V21_STREAM_AAD,
    V21_REQUEST_SECRET, b"aci.e2ee.v2.1.x25519", bytes([0x0D]) * 12)


def v21_mac(obj):
    return hmac.new(V21_MAC_KEY, jcs(obj), hashlib.sha256).hexdigest()


def v21_event(obj):
    """One SSE `data:` line carrying `obj` plus its MAC, as JCS."""
    return "data: " + jcs({**obj, "e2ee_mac": v21_mac(obj)}).decode() + "\n\n"


V21_EVENT_1 = {"id": "chatcmpl-123", "e2ee_seq": 0,
               "choices": [{"index": 0, "delta": {"content": V21_STREAM_CIPHERTEXT}}]}
V21_EVENT_2 = {"id": "chatcmpl-123", "e2ee_seq": 1,
               "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}
V21_STREAM = v21_event(V21_EVENT_1) + v21_event(V21_EVENT_2) + "data: [DONE]\n\n"
# Rejected: a validly MAC'd event repeated, the events reordered, or event 1 dropped.
V21_REJECT_REPEATED = (v21_event(V21_EVENT_1) + v21_event(V21_EVENT_1)
                       + v21_event(V21_EVENT_2) + "data: [DONE]\n\n")
V21_REJECT_REORDERED = v21_event(V21_EVENT_2) + v21_event(V21_EVENT_1) + "data: [DONE]\n\n"
V21_REJECT_DROPPED = v21_event(V21_EVENT_2) + "data: [DONE]\n\n"
# Rejected: a readable member edited after the MAC.
V21_REJECT_EDITED = v21_event(V21_EVENT_1) + v21_event(V21_EVENT_2).replace(
    '"finish_reason":"stop"', '"finish_reason":"length"') + "data: [DONE]\n\n"
# Rejected: a validly MAC'd event from another response under the same secret.
V21_REJECT_SPLICED = v21_event(V21_EVENT_1) + v21_event(
    {**V21_EVENT_2, "id": "chatcmpl-124"}) + "data: [DONE]\n\n"
# Rejected: an event without a MAC.
V21_REJECT_NO_MAC = ("data: " + jcs(V21_EVENT_1).decode() + "\n\n"
                     + v21_event(V21_EVENT_2) + "data: [DONE]\n\n")
# Rejected: an SSE line other than `data:` or a comment.
V21_REJECT_FOREIGN_LINE = "id: 1\n" + V21_STREAM

# ---- Published constants (must match the spec vector documents) -------------
PINNED = {
    "receipt-1 public key": (RECEIPT_PUB,
        "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394"),
    "e2ee-1 public key": (E2EE_PUB,
        "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22"),
    "§1 workload_keyset_digest": (KEYSET_DIGEST,
        "sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371"),
    "§2 report_data (test nonce)": (REPORT_DATA_NONCE,
        "df2174d28130852b413646a3786927b93e94c11d770268b65def8bdba45cb49e"),
    "§2 report_data (null)": (REPORT_DATA_NULL,
        "0633919ca3f00e97bafaa3304278eb22420cc3ff0d19f87dfca2d3f7508150bc"),
    "§3 session_id": (SESSION_ID,
        "95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f"),
    "proxy-image binding session_id": (PROXY_SESSION_ID,
        "c9e76e6247b2bf28ce034b7c3b4866b9fe9ce9177e9858ad999503e63ed4f0df"),
    "§4 sha256(signing input)": (SIGNING_INPUT_SHA256,
        "1bd328e6880a5a12b3915af95ea32111310e04ab9e21ac3d71ce268e33b965c9"),
    "§4 signature": (RECEIPT_SIG,
        "d5b005e093bde3b577faf270b7184b09e169cacb0ecb206b103bd2581f997db0"
        "3da616175454b063323a23ac1dc68f1ce506c2a6eba8aa0561d5e724f0b80c03"),
    "§5 request AAD": (REQUEST_AAD.decode(),
        '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content",'
        '"model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",'
        '"purpose":"aci.e2ee.request.v2","ts":1750000000}'),
    "§5 response AAD": (RESPONSE_AAD.decode(),
        '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content",'
        '"id":"chatcmpl-123","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",'
        '"purpose":"aci.e2ee.response.v2","ts":1750000000}'),
    "v2.1 body digest": (V21_BODY_DIGEST.hex(),
        "dc75693896a5b40b45592667704f41ff057a5063c92e3117d930b98049032da5"),
    "v2.1 request field ciphertext": (V21_FIELD_CIPHERTEXT,
        "50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d"
        "0606060606060606060606067c3f905a1e58cb237be461eb622de6935599cbae"
        "71e1e1"),
    "v2.1 X-E2EE-Request-Key": (V21_REQUEST_KEY,
        "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d"
        "080808080808080808080808d4c43624763b8e0079b38271adfa9fae2e78243b"
        "b0fa918cd95421605c3b82b5bd7ac017cda0e8ac1c9836d409013e2369dd90e2"
        "54dbbe44006ff09d098c5f212ff666088c3724c5c51b43e51576c633"),
    "v2.1 response MAC": (V21_RESPONSE_MAC,
        "85874235a2eb56bdcf4bf6dc7f52f0fab9b3edfd410274bb9aaf56b4dd620c28"),
    "v2.1 stream delta ciphertext": (V21_STREAM_CIPHERTEXT,
        "97c3b10b4d6c133a78ea5dcc1cf6421d3f81ae37b1f628ce14ca6fce7730f333"
        "0d0d0d0d0d0d0d0d0d0d0d0d9ec4f191936e4e179f68a3bbe739537e639286fe"
        "5c6a6b"),
    "v2.1 stream event 1 MAC": (v21_mac(V21_EVENT_1),
        "5ebf698533dd276e7017daf135bde41e3bf351b5e49d08223260b5e03e57ec21"),
    "v2.1 stream event 2 MAC": (v21_mac(V21_EVENT_2),
        "7d11e2d261802a4a90f15a70727e541c1972168c6e053dd7ef5d4e6fe3b5cd7c"),
    "v2.1 spliced event MAC": (v21_mac({**V21_EVENT_2, "id": "chatcmpl-124"}),
        "bc1bf0a5179c96234d1533533f31f7d2fc5471004c2b7f7b10d37e4d94bb24fe"),
    "v2.1 response field ciphertext": (V21_RESPONSE_CIPHERTEXT,
        "57db4b359f23ae5e146e4e2512056704722506348c150c14753d0c933d04d421"
        "0b0b0b0b0b0b0b0b0b0b0b0b2db692c9ddf499146c55c93bff5841a9a068773a"
        "ca36860864a0c2f3f37f"),
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
    print("== proxy-image binding §6 ==")
    print("binding =", jcs(PROXY_BINDING).decode())
    print("session_id =", PROXY_SESSION_ID)
    print()
    print("== §4 receipt ==")
    print("request body_hash =", REQUEST_BODY_HASH)
    print("response body_hash =", RESPONSE_BODY_HASH)
    print("signing input (JCS, no signature) =", SIGNING_INPUT.decode())
    print("sha256(signing input) =", SIGNING_INPUT_SHA256)
    print("signature =", RECEIPT_SIG)
    print("document bytes =", DOCUMENT_BYTES.decode())
    print()
    print("== E2EE v2 §6 AAD ==")
    print("request AAD =", REQUEST_AAD.decode())
    print("response AAD =", RESPONSE_AAD.decode())
    print()
    print("== E2EE v2.1 §10 ==")
    print("client X25519 (seed 04*32) pub =", V21_CLIENT_PUB)
    print("request field AAD =", V21_FIELD_AAD.decode())
    print("request field ciphertext =", V21_FIELD_CIPHERTEXT)
    print("body JCS =", V21_BODY_JCS.decode())
    print("body digest =", V21_BODY_DIGEST.hex())
    print("request-key AAD =", V21_REQUEST_KEY_AAD.decode())
    print("X-E2EE-Request-Key =", V21_REQUEST_KEY)
    print("response field AAD =", V21_RESPONSE_AAD.decode())
    print("response field ciphertext =", V21_RESPONSE_CIPHERTEXT)
    print("response object JCS =", jcs(V21_RESPONSE_OBJECT).decode())
    print("response MAC =", V21_RESPONSE_MAC)
    print("stream delta ciphertext =", V21_STREAM_CIPHERTEXT)
    print("stream event 1 MAC =", v21_mac(V21_EVENT_1))
    print("stream event 2 MAC =", v21_mac(V21_EVENT_2))
    print("spliced event MAC =", v21_mac({**V21_EVENT_2, "id": "chatcmpl-124"}))
    print("stream =", repr(V21_STREAM))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
