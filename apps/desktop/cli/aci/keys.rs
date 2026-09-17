//! ACI receipt signature verification.

use ed25519_dalek::VerifyingKey;

use super::types::KeyedPublicKey;

/// The baseline signature algorithm every verifier implements (§7.2).
const ALGO_ED25519: &str = "ed25519";

/// Verify a raw RFC 8032 Ed25519 receipt signature over canonical payload
/// bytes. The attested keyset entry selects the algorithm; unsupported or
/// malformed entries fail closed.
pub fn verify_receipt_signature(
    receipt_key: &KeyedPublicKey,
    payload: &[u8],
    signature: &[u8],
) -> bool {
    if receipt_key.algo != ALGO_ED25519 {
        return false;
    }
    let Ok(pub_bytes) = hex::decode(&receipt_key.public_key_hex) else {
        return false;
    };
    let Ok(arr) = <[u8; 32]>::try_from(pub_bytes.as_slice()) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&arr) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    vk.verify_strict(payload, &ed25519_dalek::Signature::from_bytes(&sig_arr))
        .is_ok()
}
