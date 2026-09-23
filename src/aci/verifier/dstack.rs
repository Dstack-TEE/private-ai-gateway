//! Gateway policy for dstack KMS receipt-key custody.

use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use serde_json::Value;
use sha3::{Digest, Keccak256};

use super::aci_service::{AciServiceVerificationError, AciServiceVerifierPolicy};
use super::decode_hex;
use crate::aci::types::WorkloadKeyset;

/// Residual gap (gaps item 17): the chain signs the evidence's self-asserted
/// `kms_public_key`, and the link from that k256 scalar to the published
/// Ed25519 receipt key rests on the measured workload code — which is why
/// the policy anchor must itself be measured.
pub(super) fn verify_dstack_kms_receipt_custody(
    evidence: &Value,
    keyset: &WorkloadKeyset,
    app_id: &[u8],
    policy: &AciServiceVerifierPolicy,
) -> Result<(), AciServiceVerificationError> {
    let key_custody = evidence
        .get("key_custody")
        .ok_or(AciServiceVerificationError::MissingKeyCustody)?;
    let provider = key_custody
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody("missing provider".to_string())
        })?;
    if provider != "dstack-kms" {
        return Err(AciServiceVerificationError::UnsupportedKeyCustodyProvider(
            provider.to_string(),
        ));
    }
    let keys = key_custody
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody("missing keys".to_string())
        })?;
    let receipt = keys
        .iter()
        .find(|key| key.get("role").and_then(Value::as_str) == Some("receipt"))
        .ok_or(AciServiceVerificationError::MissingReceiptKeyCustody)?;
    let public_key = receipt
        .get("public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody(
                "receipt key custody missing public_key".to_string(),
            )
        })?;
    if !keyset
        .receipt_signing_keys
        .iter()
        .any(|key| key.public_key_hex == public_key)
    {
        return Err(AciServiceVerificationError::ReceiptKeyCustodyMismatch);
    }
    let kms_public_key = receipt
        .get("kms_public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody(
                "receipt key custody missing kms_public_key".to_string(),
            )
        })?;
    let purpose = receipt
        .get("purpose")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody(
                "receipt key custody missing purpose".to_string(),
            )
        })?;
    // The chain must be for the receipt key-derivation path, not any KMS key
    // the app happens to hold.
    if purpose != "aci.receipt.ed25519.v1" {
        return Err(AciServiceVerificationError::InvalidKeyCustody(format!(
            "receipt key custody purpose is {purpose:?}, expected \"aci.receipt.ed25519.v1\""
        )));
    }
    let signature_chain = receipt
        .get("signature_chain")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody(
                "receipt key custody missing signature_chain".to_string(),
            )
        })?;
    if signature_chain.len() != 2 {
        return Err(AciServiceVerificationError::InvalidKeyCustody(format!(
            "receipt key custody signature_chain must contain 2 signatures, got {}",
            signature_chain.len()
        )));
    }
    let purpose_signature = signature_chain[0]
        .as_str()
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody(
                "receipt key custody signature_chain[0] is not a string".to_string(),
            )
        })
        .and_then(|s| decode_hex(s).map_err(AciServiceVerificationError::InvalidKeyCustody))?;
    let app_signature = signature_chain[1]
        .as_str()
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidKeyCustody(
                "receipt key custody signature_chain[1] is not a string".to_string(),
            )
        })
        .and_then(|s| decode_hex(s).map_err(AciServiceVerificationError::InvalidKeyCustody))?;

    let kms_public_key_compressed = compressed_k256_public_key_hex(kms_public_key)
        .map_err(AciServiceVerificationError::KmsSignatureChain)?;
    let purpose_message = format!("{purpose}:{kms_public_key_compressed}");
    let app_public_key = recover_k256_public_key(purpose_message.as_bytes(), &purpose_signature)
        .map_err(AciServiceVerificationError::KmsSignatureChain)?;
    let app_public_key_compressed = app_public_key.to_sec1_bytes();
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id,
        &app_public_key_compressed,
    ]
    .concat();
    let root_public_key = recover_k256_public_key(&root_message, &app_signature)
        .map_err(AciServiceVerificationError::KmsSignatureChain)?;
    let root_public_key_compressed = hex::encode(root_public_key.to_sec1_bytes());
    if !policy
        .accepted_kms_root_public_keys
        .contains(&root_public_key_compressed)
    {
        return Err(AciServiceVerificationError::KmsRootRejected);
    }
    Ok(())
}

fn recover_k256_public_key(message: &[u8], signature: &[u8]) -> Result<K256VerifyingKey, String> {
    if signature.len() != 65 {
        return Err(format!(
            "recoverable secp256k1 signature must be 65 bytes, got {}",
            signature.len()
        ));
    }
    let mut recovery_byte = signature[64];
    if (27..=30).contains(&recovery_byte) {
        recovery_byte -= 27;
    }
    let recid = RecoveryId::from_byte(recovery_byte)
        .ok_or_else(|| format!("invalid recovery id: {}", signature[64]))?;
    let sig = K256Signature::from_slice(&signature[..64])
        .map_err(|e| format!("invalid secp256k1 signature: {e}"))?;
    let digest = Keccak256::new_with_prefix(message);
    K256VerifyingKey::recover_from_digest(digest, &sig, recid)
        .map_err(|e| format!("secp256k1 public key recovery failed: {e}"))
}

pub(super) fn compressed_k256_public_key_hex(public_key_hex: &str) -> Result<String, String> {
    let public_key = decode_hex(public_key_hex)?;
    let point = EncodedPoint::from_bytes(public_key)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    let key = K256VerifyingKey::from_encoded_point(&point)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    Ok(hex::encode(key.to_sec1_bytes()))
}
