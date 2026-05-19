//! ECDSA-secp256k1 receipt signature verification per ACI §9.4.

use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use private_ai_gateway::aci::keys::{verify_receipt_signature, ALGO_ECDSA_SECP256K1};
use private_ai_gateway::aci::types::KeyedPublicKey;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

fn fresh_keypair() -> (SigningKey, KeyedPublicKey) {
    let sk = SigningKey::random(&mut OsRng);
    let pk_uncompressed = sk
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let pk = KeyedPublicKey {
        key_id: "rk-ecdsa-1".to_string(),
        algo: ALGO_ECDSA_SECP256K1.to_string(),
        public_key_hex: hex::encode(pk_uncompressed),
    };
    (sk, pk)
}

/// Sign `sha256(canonical_bytes)` and pad as r || s || v.
///
/// Uses k256's `sign_prehash_recoverable` to produce a true recovery
/// id; the verifier ignores `v` (public key is known) but
/// range-checks it.
fn sign_recoverable(sk: &SigningKey, canonical_bytes: &[u8]) -> Vec<u8> {
    let prehash: [u8; 32] = Sha256::digest(canonical_bytes).into();
    let (sig, recid): (Signature, RecoveryId) =
        sk.sign_prehash_recoverable(&prehash).expect("sign");
    let mut out = Vec::with_capacity(65);
    out.extend_from_slice(&sig.to_bytes());
    out.push(recid.to_byte());
    out
}

#[test]
fn recoverable_65_byte_signature_verifies() {
    let (sk, pk) = fresh_keypair();
    let canonical_bytes = b"the canonical bytes of an unsigned ACI receipt";
    let sig = sign_recoverable(&sk, canonical_bytes);
    assert_eq!(sig.len(), 65);
    assert!(verify_receipt_signature(&pk, canonical_bytes, &sig));
}

#[test]
fn ethereum_v_27_accepted() {
    let (sk, pk) = fresh_keypair();
    let canonical_bytes = b"another receipt";
    let mut sig = sign_recoverable(&sk, canonical_bytes);
    // Ethereum encodings shift the recovery id by 27.
    sig[64] += 27;
    assert!(verify_receipt_signature(&pk, canonical_bytes, &sig));
}

#[test]
fn signature_without_recovery_byte_rejected() {
    let (sk, pk) = fresh_keypair();
    let canonical_bytes = b"a receipt";
    let sig65 = sign_recoverable(&sk, canonical_bytes);
    let sig64 = &sig65[..64];
    assert!(!verify_receipt_signature(&pk, canonical_bytes, sig64));
}

#[test]
fn signature_with_out_of_range_recovery_byte_rejected() {
    let (sk, pk) = fresh_keypair();
    let canonical_bytes = b"a receipt";
    let mut sig = sign_recoverable(&sk, canonical_bytes);
    sig[64] = 200;
    assert!(!verify_receipt_signature(&pk, canonical_bytes, &sig));
}

#[test]
fn signature_under_wrong_canonical_bytes_does_not_verify() {
    let (sk, pk) = fresh_keypair();
    let sig = sign_recoverable(&sk, b"the bytes signed");
    assert!(!verify_receipt_signature(
        &pk,
        b"the bytes that were not signed",
        &sig
    ));
}

#[test]
fn double_hashed_signature_does_not_verify() {
    // Pin the bug class: a signer that signs sha256(sha256(canonical))
    // (i.e. a double-hash) produces a signature the spec-compliant
    // verifier rejects. The verifier hashes canonical_bytes once via
    // k256's `Verifier::verify`, so the only signature shape that
    // verifies is one over sha256(canonical_bytes).
    let (sk, pk) = fresh_keypair();
    let canonical_bytes = b"verifier prehashes once";
    let prehash: [u8; 32] = Sha256::digest(canonical_bytes).into();
    let double: [u8; 32] = Sha256::digest(prehash).into();
    let (sig, recid): (Signature, RecoveryId) = sk.sign_prehash_recoverable(&double).expect("sign");
    let mut buf = Vec::with_capacity(65);
    buf.extend_from_slice(&sig.to_bytes());
    buf.push(recid.to_byte());
    assert!(!verify_receipt_signature(&pk, canonical_bytes, &buf));
}

#[test]
fn regular_signature_padded_with_fake_recovery_byte_is_rejected() {
    // A bare ECDSA signature plus an arbitrary v is not an ACI §9.4
    // recoverable signature. Verification must recover the listed
    // receipt public key from r || s || v, not merely range-check v.
    let (sk, pk) = fresh_keypair();
    let canonical_bytes = b"another receipt body";
    let prehash: [u8; 32] = Sha256::digest(canonical_bytes).into();
    let (sig, recid): (Signature, RecoveryId) =
        sk.sign_prehash_recoverable(&prehash).expect("sign");
    let mut buf = Vec::with_capacity(65);
    buf.extend_from_slice(&sig.to_bytes());
    buf.push(recid.to_byte() ^ 1);
    assert!(!verify_receipt_signature(&pk, canonical_bytes, &buf));
}
