#!/usr/bin/env python3
"""Regenerate the deterministic values in spec/test-vectors.md.

Every value in the test-vectors doc is reproduced here from first principles,
so the doc and the reference implementation can be cross-checked against an
independent construction. Run with no arguments to self-verify the
naming-independent sections (1-4, 7) against the doc's published constants and
print the full set of values (including the content-addressed session id and
receipt signature that change with field-name choices).

Requires: python3 stdlib + `cryptography` (Ed25519).

JCS: the vectors are ASCII + integers only, so RFC 8785 canonical JSON is
exactly `json.dumps(v, sort_keys=True, separators=(",", ":"))`.
"""

import hashlib
import json

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization


def jcs(value) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("ascii")


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_prefixed(data: bytes) -> str:
    return "sha256:" + sha256_hex(data)


def ed25519_from_seed(seed: bytes):
    key = Ed25519PrivateKey.from_private_bytes(seed)
    pub = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return key, pub.hex()


# ---- Fixed keys (spec §"Fixed keys") -------------------------------------
IDENTITY_KEY, IDENTITY_PUB = ed25519_from_seed(bytes([0x01]) * 32)
RECEIPT_KEY, RECEIPT_PUB = ed25519_from_seed(bytes([0x02]) * 32)

# Placeholder material (not valid curve points / SPKIs; digests don't need it).
E2EE_PUB = "ab" * 32
TLS_SPKI = "c0" * 32
CHANNEL_SPKI = "d1" * 32

ALGO_ED25519 = "ed25519"
E2EE_ALGO = "x25519-aes-256-gcm-hkdf-sha256"


# ---- §1 workload_id -------------------------------------------------------
identity_pubkey_obj = {"algo": ALGO_ED25519, "public_key": IDENTITY_PUB}
workload_id = sha256_prefixed(jcs(identity_pubkey_obj))

# ---- §2 workload keyset digest -------------------------------------------
keyset = {
    "workload_identity": {
        "public_key": {"algo": ALGO_ED25519, "public_key": IDENTITY_PUB},
        "subject": None,
    },
    "keyset_epoch": {"version": 1, "not_after": 1800000000},
    "receipt_signing_keys": [
        {"key_id": "receipt-1", "algo": ALGO_ED25519, "public_key": RECEIPT_PUB}
    ],
    "e2ee_public_keys": [
        {"key_id": "e2ee-1", "algo": E2EE_ALGO, "public_key": E2EE_PUB}
    ],
    "tls_public_keys": [{"spki_sha256": TLS_SPKI, "domain": "api.example.com"}],
}
workload_keyset_digest = sha256_prefixed(jcs(keyset))

# ---- §3 attestation report_data ------------------------------------------
def report_data(nonce):
    statement = {
        "purpose": "aci.report_data.v1",
        "workload_id": workload_id,
        "workload_keyset_digest": workload_keyset_digest,
        "nonce": nonce,
    }
    return jcs(statement), sha256_hex(jcs(statement))


report_data_bytes, report_data_nonce = report_data("test-nonce")
_, report_data_null = report_data(None)

# ---- §4 keyset endorsement and revocation --------------------------------
endorsement_payload = jcs(
    {
        "purpose": "aci.keyset.endorsement.v1",
        "workload_keyset_digest": workload_keyset_digest,
    }
)
endorsement_sig = IDENTITY_KEY.sign(endorsement_payload).hex()

revocation_payload = jcs(
    {
        "purpose": "aci.keyset.revocation.v1",
        "workload_keyset_digest": workload_keyset_digest,
    }
)
revocation_sig = IDENTITY_KEY.sign(revocation_payload).hex()

# ---- §5 attested session --------------------------------------------------
EVIDENCE_BYTES = b"example-evidence"
evidence_digest = sha256_prefixed(EVIDENCE_BYTES)

CLAIMS = {
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
}
CHANNEL_BINDING = [
    {
        "type": "tls_spki_sha256",
        "origin": "https://upstream.example.com",
        "spki_sha256": CHANNEL_SPKI,
    }
]


def session_material(upstream_label_key):
    """The content-addressing material (§9.2). `upstream_label_key` selects the
    field name for the operator's upstream label ("upstream_name" now,
    "provider" before the pre-launch rename)."""
    return {
        upstream_label_key: "demo-upstream",
        "endpoint": "https://upstream.example.com",
        "verifier_id": "example/1",
        "identity": None,
        "channel_binding": CHANNEL_BINDING,
        "claims": CLAIMS,
        "evidence_digest": evidence_digest,
    }


def session_id(upstream_label_key):
    return "as_" + sha256_hex(jcs(session_material(upstream_label_key)))


SESSION_MATERIAL = jcs(session_material("upstream_name"))
SESSION_ID = session_id("upstream_name")

# ---- §6 receipt signing ---------------------------------------------------
REQUEST_BODY = b'{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}'
RESPONSE_BODY = b'{"choices":[],"id":"chatcmpl-123"}'
request_body_hash = sha256_prefixed(REQUEST_BODY)
response_body_hash = sha256_prefixed(RESPONSE_BODY)


def upstream_verified_event(provider_type_key, sid):
    """The `upstream.verified` event. `provider_type_key` selects the field name
    for the verifier adapter type ("provider_type" now, "provider" before the
    rename); the operator label is always `upstream_name`."""
    return {
        "seq": 2,
        "type": "upstream.verified",
        "upstream_name": "demo-upstream",
        provider_type_key: None,
        "model_id": "demo-model",
        "url_origin": "https://upstream.example.com",
        "verifier_id": "example/1",
        "result": "verified",
        "required": True,
        "reason": None,
        "channel_bindings": CHANNEL_BINDING,
        "provider_claims": None,
        "session_id": sid,
        "claims": CLAIMS,
    }


def receipt(provider_type_key, sid):
    return {
        "api_version": "aci/1",
        "receipt_id": "rcpt-0001",
        "chat_id": "chatcmpl-123",
        "model": "demo-model",
        "workload_id": workload_id,
        "workload_keyset_digest": workload_keyset_digest,
        "endpoint": "/v1/chat/completions",
        "method": "POST",
        "served_at": 1750000000,
        "event_log": [
            {"seq": 0, "type": "request.received", "body_hash": request_body_hash},
            {"seq": 1, "type": "request.forwarded", "body_hash": request_body_hash},
            upstream_verified_event(provider_type_key, sid),
            {
                "seq": 3,
                "type": "response.returned",
                "cleartext_hash": response_body_hash,
                "wire_hash": response_body_hash,
            },
        ],
        "signature": {"algo": ALGO_ED25519, "key_id": "receipt-1"},
    }


RECEIPT_CANONICAL = jcs(receipt("provider_type", SESSION_ID))
RECEIPT_SHA256 = sha256_hex(RECEIPT_CANONICAL)
RECEIPT_SIG = RECEIPT_KEY.sign(RECEIPT_CANONICAL).hex()

# ---- §7 E2EE AAD ----------------------------------------------------------
request_aad = jcs(
    {
        "purpose": "aci.e2ee.request.v2",
        "algo": E2EE_ALGO,
        "field": "messages.0.content",
        "model": "demo-model",
        "nonce": "6e6f6e63652d31323334",
        "ts": 1750000000,
    }
)
response_aad = jcs(
    {
        "purpose": "aci.e2ee.response.v2",
        "algo": E2EE_ALGO,
        "field": "choices.0.message.content",
        "id": "chatcmpl-123",
        "model": "demo-model",
        "nonce": "6e6f6e63652d31323334",
        "ts": 1750000000,
    }
)


def _check(label, got, want):
    status = "ok" if got == want else "MISMATCH"
    if got != want:
        print(f"[{status}] {label}\n    got:  {got}\n    want: {want}")
    return got == want


def main():
    # Naming-independent published constants (unchanged by the pre-launch
    # rename): reproducing these proves the crypto/JCS constructions are right.
    ok = True
    ok &= _check("§ identity public key", IDENTITY_PUB,
                 "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c")
    ok &= _check("§ receipt public key", RECEIPT_PUB,
                 "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394")
    ok &= _check("§1 workload_id", workload_id,
                 "sha256:57c2c8fa98bcf11441f1eff9ef087db67a5560a026082e96903e15365677b8c0")
    ok &= _check("§2 workload_keyset_digest", workload_keyset_digest,
                 "sha256:f2fba7e1b1451e0c0231df624f293407692ef939d3e0e55bca723131bea3f1ff")
    ok &= _check("§3 report_data (nonce)", report_data_nonce,
                 "0b8cc28d7e989a88b1e969af20aa2b224afdc2c99f24c97c31a4af330c964ecf")
    ok &= _check("§3 report_data (null)", report_data_null,
                 "e1818eadad3c28375c625e2fa2d2ffd983d2760c84ce17f8527ddcac884c21b9")
    ok &= _check("§4 endorsement sig", endorsement_sig,
                 "64e0a4f5d7af28dfdacc102d14c13470b4ddbd90708e190fc0e787f07b36f20e"
                 "da0ef1f42ea96b8a7f290eb64a918574dc914ce06b6ea023d2153275f06fd201")
    ok &= _check("§4 revocation sig", revocation_sig,
                 "5f30e02aa53bb628c7f6410636e9f5e33402d2b0b416a6ed278ea3e6e40b48a9"
                 "af6ba6e5e55abb89a7ad4627eca444a73cad9d25e22bf239c9c6b362d48ed50f")
    ok &= _check("§5 evidence digest", evidence_digest,
                 "sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d")
    ok &= _check("§7 request AAD", request_aad.decode(),
                 '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content",'
                 '"model":"demo-model","nonce":"6e6f6e63652d31323334",'
                 '"purpose":"aci.e2ee.request.v2","ts":1750000000}')
    ok &= _check("§7 response AAD", response_aad.decode(),
                 '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content",'
                 '"id":"chatcmpl-123","model":"demo-model","nonce":"6e6f6e63652d31323334",'
                 '"purpose":"aci.e2ee.response.v2","ts":1750000000}')

    # Cross-check that the ONLY thing the rename changes is the field name: the
    # pre-rename ("provider") material/receipt must still reproduce the prior
    # published id/hash/signature.
    ok &= _check("§5 session_id (pre-rename provider)", session_id("provider"),
                 "as_d850af230a623da93279f86922f2d15aeb0a05c3c448feac9198c5b2b0a3b9c3")
    pre_canonical = jcs(receipt("provider", session_id("provider")))
    ok &= _check("§6 receipt sha256 (pre-rename provider)", sha256_hex(pre_canonical),
                 "167f07a49364efbc74649e7b05555248f46f944262a96934a4b4e5f359f95cfa")
    ok &= _check("§6 receipt sig (pre-rename provider)", RECEIPT_KEY.sign(pre_canonical).hex(),
                 "84568f9eed0981d275407e3ea9b03d7f6c9a428923ac99c45b9ef94aa8bada6a"
                 "39b5a63410fb48eb8057a6d4f8d2fb5a2ba28bc8f94d3c707e7fb7f360f39d00")

    print("SELF-CHECK:", "all published constants reproduced" if ok else "FAILURES ABOVE")
    print()
    print("== Regenerated values (post-rename: upstream_name / provider_type) ==")
    print("§1 workload_id =", workload_id)
    print("§2 workload_keyset_digest =", workload_keyset_digest)
    print("§5 session material =", SESSION_MATERIAL.decode())
    print("§5 session_id =", SESSION_ID)
    print("§6 request body_hash =", request_body_hash)
    print("§6 response body_hash =", response_body_hash)
    print("§6 receipt canonical =", RECEIPT_CANONICAL.decode())
    print("§6 sha256(canonical) =", RECEIPT_SHA256)
    print("§6 signature.value =", RECEIPT_SIG)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
