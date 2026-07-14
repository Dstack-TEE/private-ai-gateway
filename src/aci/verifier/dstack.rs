//! dstack-specific verification helpers: event-log/app-id checks, RTMR replay,
//! KMS key custody, and secp256k1 key recovery.

use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha384};
use sha3::Keccak256;

use super::aci_service::{dcap_rtmr3, AciServiceVerificationError, AciServiceVerifierPolicy};
use super::decode_hex;
use crate::aci::types::WorkloadKeyset;

#[derive(Debug, Deserialize)]
pub struct DstackEventLog {
    imr: u32,
    digest: String,
    event: String,
    pub event_payload: String,
}

/// Replay the dstack event log to RTMR3, require it to match the quote, and
/// return the verified events — the app-id (§4.3) and compose-hash (§10.1(4))
/// bindings each read one boot-time event from the result.
pub fn verify_dstack_event_log(
    evidence: &Value,
    report: &dcap_qvl::quote::Report,
) -> Result<Vec<DstackEventLog>, String> {
    let event_log = evidence
        .get("event_log")
        .and_then(Value::as_str)
        .ok_or("missing dstack event_log evidence")?;
    let events = serde_json::from_str::<Vec<DstackEventLog>>(event_log)
        .map_err(|e| format!("invalid dstack event_log evidence: {e}"))?;
    let rtmr3 = replay_dstack_rtmr(&events, 3)?;
    let quote_rtmr3 =
        dcap_rtmr3(report).ok_or("dstack event log verification requires a TDX quote")?;
    if rtmr3.as_slice() != quote_rtmr3 {
        return Err("dstack event_log RTMR3 does not match verified quote".to_string());
    }
    Ok(events)
}

/// The first RTMR3 event named `event`, measured before `system-ready`.
pub fn dstack_rtmr3_event<'a>(
    events: &'a [DstackEventLog],
    event: &str,
) -> Option<&'a DstackEventLog> {
    events
        .iter()
        .take_while(|e| !(e.imr == 3 && e.event == "system-ready"))
        .find(|e| e.imr == 3 && e.event == event)
}

fn replay_dstack_rtmr(events: &[DstackEventLog], imr: u32) -> Result<[u8; 48], String> {
    let mut mr = vec![0u8; 48];
    for event in events.iter().filter(|event| event.imr == imr) {
        let mut digest = decode_hex(&event.digest)?;
        if digest.len() < 48 {
            digest.resize(48, 0);
        }
        mr.extend_from_slice(&digest);
        mr = Sha384::digest(&mr).to_vec();
    }
    mr.as_slice()
        .try_into()
        .map_err(|_| "replayed RTMR is not 48 bytes".to_string())
}

/// Verify the KMS custody chain for the attested receipt signing key (§4.3).
///
/// The evidence entry publishes the released key's k256 counterpart
/// (`kms_public_key`) because the dstack KMS chain covers
/// `"{purpose}:{compressed-k256-pubkey}"` regardless of the role's own
/// algorithm. The chain proves the KMS released that path/purpose key to the
/// measured app (`app_id`, taken from the verified event log) under an
/// accepted root; the link from the k256 counterpart to the published
/// Ed25519 key rests on the measured workload code, like the rest of the
/// evidence.
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
