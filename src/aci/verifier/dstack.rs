//! dstack-specific verification helpers: event-log/app-id checks, RTMR replay,
//! KMS receipt-key custody, and secp256k1 key recovery.

use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256, Sha384};
use sha3::Keccak256;

use super::aci_service::{dcap_rtmr3, AciServiceVerificationError, AciServiceVerifierPolicy};
use super::decode_hex;
use crate::aci::types::WorkloadKeyset;

const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x08000001;

/// The provider-neutral wire shape of a dstack event-log entry.
///
/// Verification only consumes the attested JSON evidence. Keeping this type
/// local avoids coupling the cross-platform `aci` verifier to the Unix-socket
/// client used by the gateway service process.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DstackEventLog {
    pub imr: u32,
    pub event_type: u32,
    pub digest: String,
    pub event: String,
    pub event_payload: String,
}

/// Replay the dstack event log to RTMR3 and require it to match the quote,
/// returning the verified events. The `aci` CLI reuses this for its own
/// §9.1(4) compose check, so failures are plain strings rather than this
/// module's provider-verifier error type.
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
    let rtmr3 = replay_dstack_rtmr(&events, 3).map_err(|e| e.to_string())?;
    let quote_rtmr3 =
        dcap_rtmr3(report).ok_or("dstack event log verification requires a TDX quote")?;
    if rtmr3.as_slice() != quote_rtmr3 {
        return Err("dstack event_log RTMR3 does not match verified quote".to_string());
    }
    Ok(events)
}

/// The single pre-`system-ready` dstack runtime event named `event_name`
/// (`None` when the verified log carries none). A log carrying more than one
/// is the tampering shape this lookup exists to catch, so it is an error,
/// never a silent "absent".
pub fn dstack_rtmr3_event<'a>(
    events: &'a [DstackEventLog],
    event_name: &str,
) -> Result<Option<&'a DstackEventLog>, String> {
    runtime_event_before_system_ready(events, event_name).map_err(|e| e.to_string())
}

fn runtime_event_before_system_ready<'a>(
    events: &'a [DstackEventLog],
    event_name: &str,
) -> Result<Option<&'a DstackEventLog>, AciServiceVerificationError> {
    let mut matches = events
        .iter()
        .take_while(|event| {
            !(event.imr == 3
                && event.event_type == DSTACK_RUNTIME_EVENT_TYPE
                && event.event == "system-ready")
        })
        .filter(|event| {
            event.imr == 3
                && event.event_type == DSTACK_RUNTIME_EVENT_TYPE
                && event.event == event_name
        });
    let event = matches.next();
    if matches.next().is_some() {
        return Err(AciServiceVerificationError::InvalidEventLog(format!(
            "multiple pre-system-ready {event_name} events"
        )));
    }
    Ok(event)
}

/// The RTMR3-measured compose hash that `app_compose` reproduces (§9.1(4)).
///
/// Returns the measured hash as lowercase hex, so a caller can report or pin
/// it. Shared so both verifiers compare the same preimage the same way.
pub(super) fn verify_dstack_compose_measurement(
    evidence: &Value,
    events: &[DstackEventLog],
) -> Result<String, String> {
    let measured = dstack_rtmr3_event(events, "compose-hash")
        .map_err(|e| format!("dstack event log rejected: {e}"))?
        .ok_or_else(|| "verified event log carries no compose-hash event".to_string())?;
    let measured_hash: [u8; 32] = decode_hex(&measured.event_payload)?
        .as_slice()
        .try_into()
        .map_err(|_| "compose-hash event must contain 32 bytes".to_string())?;
    verify_dstack_app_compose(evidence, &measured_hash).map_err(|e| e.to_string())?;
    Ok(hex::encode(measured_hash))
}

/// The RTMR3-measured app-id, which the custody chain and the policy anchor
/// on (§9.1(5)).
pub(super) fn dstack_app_id(events: &[DstackEventLog]) -> Result<Vec<u8>, String> {
    let event = dstack_rtmr3_event(events, "app-id")
        .map_err(|e| format!("dstack event log rejected: {e}"))?
        .ok_or_else(|| "verified event log carries no app-id event".to_string())?;
    decode_hex(&event.event_payload)
}

/// Verify that `app_compose` is the preimage of the compose measurement
/// bound into RTMR3 by the verified event log.
pub(super) fn verify_dstack_app_compose(
    evidence: &Value,
    measured_compose_hash: &[u8; 32],
) -> Result<(), AciServiceVerificationError> {
    let app_compose = evidence
        .get("app_compose")
        .and_then(Value::as_str)
        .ok_or(AciServiceVerificationError::MissingAppCompose)?;
    let actual_compose_hash: [u8; 32] = Sha256::digest(app_compose.as_bytes()).into();
    if &actual_compose_hash != measured_compose_hash {
        return Err(AciServiceVerificationError::AppComposeHashMismatch);
    }
    Ok(())
}

fn replay_dstack_rtmr(
    events: &[DstackEventLog],
    imr: u32,
) -> Result<[u8; 48], AciServiceVerificationError> {
    let mut mr = vec![0u8; 48];
    for event in events.iter().filter(|event| event.imr == imr) {
        let mut digest = dstack_event_digest(event)?;
        if digest.len() < 48 {
            digest.resize(48, 0);
        }
        mr.extend_from_slice(&digest);
        mr = Sha384::digest(&mr).to_vec();
    }
    mr.as_slice().try_into().map_err(|_| {
        AciServiceVerificationError::InvalidEventLog("replayed RTMR is not 48 bytes".to_string())
    })
}

fn dstack_event_digest(event: &DstackEventLog) -> Result<Vec<u8>, AciServiceVerificationError> {
    if event.event_type != DSTACK_RUNTIME_EVENT_TYPE {
        return decode_hex(&event.digest).map_err(AciServiceVerificationError::InvalidEventLog);
    }

    let payload =
        decode_hex(&event.event_payload).map_err(AciServiceVerificationError::InvalidEventLog)?;
    let mut hasher = Sha384::new();
    hasher.update(event.event_type.to_ne_bytes());
    hasher.update(b":");
    hasher.update(event.event.as_bytes());
    hasher.update(b":");
    hasher.update(payload);
    Ok(hasher.finalize().to_vec())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_event(event: &str, payload: &[u8]) -> DstackEventLog {
        let mut event = DstackEventLog {
            imr: 3,
            event_type: DSTACK_RUNTIME_EVENT_TYPE,
            digest: String::new(),
            event: event.to_string(),
            event_payload: hex::encode(payload),
        };
        event.digest = hex::encode(dstack_event_digest(&event).unwrap());
        event
    }

    #[test]
    fn replay_recomputes_runtime_event_digest_from_semantic_fields() {
        let measured = runtime_event("app-id", &[0x11; 20]);
        let expected_rtmr = replay_dstack_rtmr(std::slice::from_ref(&measured), 3).unwrap();

        let mut tampered = runtime_event("compose-hash", &[0x22; 32]);
        tampered.digest = measured.digest;
        let tampered_rtmr = replay_dstack_rtmr(&[tampered], 3).unwrap();

        assert_ne!(tampered_rtmr, expected_rtmr);
    }

    #[test]
    fn semantic_events_must_be_dstack_runtime_events() {
        let disguised_firmware_event = DstackEventLog {
            imr: 3,
            event_type: 0,
            digest: hex::encode([0x33; 48]),
            event: "compose-hash".to_string(),
            event_payload: hex::encode([0x44; 32]),
        };

        assert!(
            runtime_event_before_system_ready(&[disguised_firmware_event], "compose-hash")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_duplicate_semantic_events() {
        let compose_hash = runtime_event("compose-hash", &[0x44; 32]);
        let err = runtime_event_before_system_ready(
            &[compose_hash.clone(), compose_hash],
            "compose-hash",
        )
        .unwrap_err()
        .to_string();

        assert_eq!(
            err,
            "invalid dstack event_log evidence: multiple pre-system-ready compose-hash events"
        );
    }
}
