//! dstack-specific verification helpers: event-log/app-id checks, RTMR replay,
//! KMS identity custody, and secp256k1 key recovery.

use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha384};
use sha3::Keccak256;

use super::dcap::{dcap_rtmr3, AciDcapVerificationError, AciDcapVerifierPolicy};
use super::decode_hex;
use crate::aci::types::AttestationReport;

#[derive(Debug, Deserialize)]
struct DstackEventLog {
    imr: u32,
    digest: String,
    event: String,
    event_payload: String,
}

pub(super) fn verify_dstack_event_log_and_app_id(
    evidence: &Value,
    report: &dcap_qvl::quote::Report,
) -> Result<Vec<u8>, AciDcapVerificationError> {
    let event_log = evidence
        .get("event_log")
        .and_then(Value::as_str)
        .ok_or(AciDcapVerificationError::MissingEventLog)?;
    let events = serde_json::from_str::<Vec<DstackEventLog>>(event_log)
        .map_err(|e| AciDcapVerificationError::InvalidEventLog(e.to_string()))?;
    let rtmr3 = replay_dstack_rtmr(&events, 3)?;
    let quote_rtmr3 = dcap_rtmr3(report).ok_or_else(|| {
        AciDcapVerificationError::InvalidEventLog(
            "dstack event log verification requires a TDX quote".to_string(),
        )
    })?;
    if rtmr3.as_slice() != quote_rtmr3 {
        return Err(AciDcapVerificationError::EventLogRtmrMismatch);
    }
    let app_id = events
        .iter()
        .take_while(|event| !(event.imr == 3 && event.event == "system-ready"))
        .find(|event| event.imr == 3 && event.event == "app-id")
        .ok_or(AciDcapVerificationError::MissingAppId)?;
    decode_hex(&app_id.event_payload).map_err(AciDcapVerificationError::InvalidEventLog)
}

fn replay_dstack_rtmr(
    events: &[DstackEventLog],
    imr: u32,
) -> Result<[u8; 48], AciDcapVerificationError> {
    let mut mr = vec![0u8; 48];
    for event in events.iter().filter(|event| event.imr == imr) {
        let mut digest =
            decode_hex(&event.digest).map_err(AciDcapVerificationError::InvalidEventLog)?;
        if digest.len() < 48 {
            digest.resize(48, 0);
        }
        mr.extend_from_slice(&digest);
        mr = Sha384::digest(&mr).to_vec();
    }
    mr.as_slice().try_into().map_err(|_| {
        AciDcapVerificationError::InvalidEventLog("replayed RTMR is not 48 bytes".to_string())
    })
}

pub(super) fn verify_dstack_kms_identity_custody(
    report: &AttestationReport,
    app_id: &[u8],
    policy: &AciDcapVerifierPolicy,
) -> Result<(), AciDcapVerificationError> {
    let key_custody = report
        .attestation
        .evidence
        .get("key_custody")
        .ok_or(AciDcapVerificationError::MissingKeyCustody)?;
    let provider = key_custody
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody("missing provider".to_string())
        })?;
    if provider != "dstack-kms" {
        return Err(AciDcapVerificationError::UnsupportedKeyCustodyProvider(
            provider.to_string(),
        ));
    }
    let keys = key_custody
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| AciDcapVerificationError::InvalidKeyCustody("missing keys".to_string()))?;
    let identity = keys
        .iter()
        .find(|key| key.get("role").and_then(Value::as_str) == Some("identity"))
        .ok_or(AciDcapVerificationError::MissingIdentityKeyCustody)?;
    let public_key = identity
        .get("public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody missing public_key".to_string(),
            )
        })?;
    if public_key
        != report
            .attestation
            .workload_keyset
            .workload_identity
            .public_key
            .public_key_hex
    {
        return Err(AciDcapVerificationError::IdentityKeyCustodyMismatch);
    }
    let purpose = identity
        .get("purpose")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody missing purpose".to_string(),
            )
        })?;
    let signature_chain = identity
        .get("signature_chain")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody missing signature_chain".to_string(),
            )
        })?;
    if signature_chain.len() != 2 {
        return Err(AciDcapVerificationError::InvalidKeyCustody(format!(
            "identity key custody signature_chain must contain 2 signatures, got {}",
            signature_chain.len()
        )));
    }
    let purpose_signature = signature_chain[0]
        .as_str()
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody signature_chain[0] is not a string".to_string(),
            )
        })
        .and_then(|s| decode_hex(s).map_err(AciDcapVerificationError::InvalidKeyCustody))?;
    let app_signature = signature_chain[1]
        .as_str()
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody signature_chain[1] is not a string".to_string(),
            )
        })
        .and_then(|s| decode_hex(s).map_err(AciDcapVerificationError::InvalidKeyCustody))?;

    let identity_public_key_compressed = compressed_k256_public_key_hex(public_key)
        .map_err(AciDcapVerificationError::KmsSignatureChain)?;
    let purpose_message = format!("{purpose}:{identity_public_key_compressed}");
    let app_public_key = recover_k256_public_key(purpose_message.as_bytes(), &purpose_signature)
        .map_err(AciDcapVerificationError::KmsSignatureChain)?;
    let app_public_key_compressed = app_public_key.to_sec1_bytes();
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id,
        &app_public_key_compressed,
    ]
    .concat();
    let root_public_key = recover_k256_public_key(&root_message, &app_signature)
        .map_err(AciDcapVerificationError::KmsSignatureChain)?;
    let root_public_key_compressed = hex::encode(root_public_key.to_sec1_bytes());
    if !policy
        .accepted_kms_root_public_keys
        .contains(&root_public_key_compressed)
    {
        return Err(AciDcapVerificationError::KmsRootRejected);
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
