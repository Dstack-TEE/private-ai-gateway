//! dstack mechanisms: event-log replay to RTMR3, the measurements it binds
//! (§9.1(4)), and the KMS signature chain behind the receipt key (§9.1(5)).
//!
//! Which KMS roots and app-ids are acceptable is the caller's policy: the
//! chain check returns the root it recovers for the caller to appraise.

use aci_protocol::types::WorkloadKeyset;
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sha3::Keccak256;

use crate::decode_hex;

// Keep the public name while using upstream's wire parser and versioned event type.
// Digest and payload fields are decoded bytes; serde still uses hex on the wire.
pub use cc_eventlog::TdxEvent as DstackEventLog;

/// An event log whose replay matched a quote's RTMR3. Only
/// [`verify_dstack_event_log`] builds one, so a measurement read from it is
/// always one the quote binds.
#[derive(Debug)]
pub struct VerifiedEventLog(Vec<DstackEventLog>);

/// Replay the dstack event log to RTMR3 and require it to match the quote's
/// `rt_mr3` (`None` for a quote that is not TDX), returning the verified
/// log.
pub fn verify_dstack_event_log(
    evidence: &Value,
    quote_rtmr3: Option<&[u8; 48]>,
) -> Result<VerifiedEventLog, String> {
    let event_log = evidence
        .get("event_log")
        .and_then(Value::as_str)
        .ok_or("missing dstack event_log evidence")?;
    let events = serde_json::from_str::<Vec<DstackEventLog>>(event_log)
        .map_err(|e| format!("invalid dstack event_log evidence: {e}"))?;
    cc_eventlog::tdx::validate_v2_preimages(&events).map_err(|e| e.to_string())?;
    let mut runtime_events = Vec::new();
    for (index, event) in events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.imr == 3)
    {
        // RTMR3 is dstack's runtime register. Other event types have no
        // reconstructable preimage in TdxEvent, so cannot establish this binding.
        let runtime = event
            .to_runtime_event()
            .ok_or_else(|| format!("dstack RTMR3 event {index} is not a runtime event"))?;
        if event.digest != runtime.sha384_digest() {
            return Err(format!(
                "dstack runtime event {index} digest does not match its fields"
            ));
        }
        runtime_events.push(runtime);
    }
    let rtmr3 = cc_eventlog::replay_events::<ez_hash::Sha384>(&runtime_events, None);
    let quote_rtmr3 = quote_rtmr3.ok_or("dstack event log verification requires a TDX quote")?;
    if rtmr3.as_slice() != quote_rtmr3 {
        return Err("dstack event_log RTMR3 does not match verified quote".to_string());
    }
    Ok(VerifiedEventLog(events))
}

/// The first boot-time runtime event named `event_name`, following dstack's
/// `find_event` semantics: stop at `system-ready`, ignoring application events.
pub fn dstack_rtmr3_event<'a>(
    events: &'a [DstackEventLog],
    event_name: &str,
) -> Result<Option<&'a DstackEventLog>, String> {
    Ok(events
        .iter()
        .filter(|event| event.imr == 3 && event.is_runtime_event())
        .take_while(|event| event.event != "system-ready")
        .find(|event| event.event == event_name))
}

/// The RTMR3-measured compose hash that `app_compose` reproduces (§9.1(4)).
///
/// Returns the measured hash as lowercase hex, so a caller can report or pin
/// it.
pub fn verify_dstack_compose_measurement(
    evidence: &Value,
    events: &VerifiedEventLog,
) -> Result<String, String> {
    let measured = dstack_rtmr3_event(&events.0, "compose-hash")
        .map_err(|e| format!("dstack event log rejected: {e}"))?
        .ok_or_else(|| "verified event log carries no compose-hash event".to_string())?;
    let measured_hash: [u8; 32] = measured
        .event_payload
        .as_slice()
        .try_into()
        .map_err(|_| "compose-hash event must contain 32 bytes".to_string())?;
    verify_dstack_app_compose(evidence, &measured_hash)?;
    Ok(hex::encode(measured_hash))
}

/// The RTMR3-measured app-id, which the custody chain and the policy anchor
/// on (§9.1(5)).
pub fn dstack_app_id(events: &VerifiedEventLog) -> Result<Vec<u8>, String> {
    let event = dstack_rtmr3_event(&events.0, "app-id")
        .map_err(|e| format!("dstack event log rejected: {e}"))?
        .ok_or_else(|| "verified event log carries no app-id event".to_string())?;
    Ok(event.event_payload.clone())
}

/// Verify that `app_compose` is the preimage of the compose measurement
/// bound into RTMR3 by the verified event log.
pub fn verify_dstack_app_compose(
    evidence: &Value,
    measured_compose_hash: &[u8; 32],
) -> Result<(), String> {
    let app_compose = evidence
        .get("app_compose")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing dstack app_compose evidence".to_string())?;
    let actual_compose_hash: [u8; 32] = Sha256::digest(app_compose.as_bytes()).into();
    if &actual_compose_hash != measured_compose_hash {
        return Err(
            "dstack app_compose preimage does not match the RTMR3-bound compose hash".into(),
        );
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum KeyCustodyError {
    #[error("missing dstack KMS key custody evidence")]
    MissingKeyCustody,
    #[error("unsupported key custody provider: {0}")]
    UnsupportedKeyCustodyProvider(String),
    #[error("invalid dstack KMS key custody evidence: {0}")]
    InvalidKeyCustody(String),
    #[error("missing dstack KMS receipt key custody evidence")]
    MissingReceiptKeyCustody,
    #[error("dstack KMS receipt key custody public key does not match the attested keyset")]
    ReceiptKeyCustodyMismatch,
    #[error("dstack KMS signature chain verification failed: {0}")]
    KmsSignatureChain(String),
}

/// Public identities established by the receipt custody-chain check. This does
/// not prove the Ed25519 derivation independently of measured workload code.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VerifiedReceiptCustody {
    pub provider: String,
    pub receipt_public_key: String,
    pub purpose: String,
    pub kms_public_key: String,
    pub app_public_key: String,
    pub root_public_key: String,
}

/// The dstack KMS chain behind the receipt key (§9.1(5)): the custody entry
/// must name a receipt key the keyset attests, and its signature chain must
/// lead from the measured `app_id` to a KMS root. Returns normalized public
/// identities, with secp256k1 keys in compressed lowercase hex; whether the
/// recovered root is accepted is the caller's policy.
///
/// Residual gap (conformance gaps item 11): the chain signs the evidence's
/// self-asserted `kms_public_key`, and the link from that k256 scalar to the
/// published Ed25519 receipt key rests on the measured workload code — which
/// is why the policy anchor must itself be measured.
#[must_use = "the recovered KMS root must be checked against the custody policy"]
pub fn verify_dstack_kms_receipt_custody(
    evidence: &Value,
    keyset: &WorkloadKeyset,
    app_id: &[u8],
) -> Result<VerifiedReceiptCustody, KeyCustodyError> {
    let key_custody = evidence
        .get("key_custody")
        .ok_or(KeyCustodyError::MissingKeyCustody)?;
    let provider = key_custody
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| KeyCustodyError::InvalidKeyCustody("missing provider".to_string()))?;
    if provider != "dstack-kms" {
        return Err(KeyCustodyError::UnsupportedKeyCustodyProvider(
            provider.to_string(),
        ));
    }
    let keys = key_custody
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| KeyCustodyError::InvalidKeyCustody("missing keys".to_string()))?;
    let receipt = keys
        .iter()
        .find(|key| key.get("role").and_then(Value::as_str) == Some("receipt"))
        .ok_or(KeyCustodyError::MissingReceiptKeyCustody)?;
    let field = |name: &str| {
        receipt.get(name).and_then(Value::as_str).ok_or_else(|| {
            KeyCustodyError::InvalidKeyCustody(format!("receipt key custody missing {name}"))
        })
    };
    let public_key = field("public_key")?;
    if !keyset
        .receipt_signing_keys
        .iter()
        .any(|key| key.public_key_hex == public_key)
    {
        return Err(KeyCustodyError::ReceiptKeyCustodyMismatch);
    }
    let kms_public_key = field("kms_public_key")?;
    let purpose = field("purpose")?;
    // The chain must be for the receipt key-derivation path, not any KMS key
    // the app happens to hold.
    if purpose != "aci.receipt.ed25519.v1" {
        return Err(KeyCustodyError::InvalidKeyCustody(format!(
            "receipt key custody purpose is {purpose:?}, expected \"aci.receipt.ed25519.v1\""
        )));
    }
    let signature_chain = receipt
        .get("signature_chain")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KeyCustodyError::InvalidKeyCustody(
                "receipt key custody missing signature_chain".to_string(),
            )
        })?;
    if signature_chain.len() != 2 {
        return Err(KeyCustodyError::InvalidKeyCustody(format!(
            "receipt key custody signature_chain must contain 2 signatures, got {}",
            signature_chain.len()
        )));
    }
    let signature = |index: usize| {
        signature_chain[index]
            .as_str()
            .ok_or_else(|| {
                KeyCustodyError::InvalidKeyCustody(format!(
                    "receipt key custody signature_chain[{index}] is not a string"
                ))
            })
            .and_then(|s| decode_hex(s).map_err(KeyCustodyError::InvalidKeyCustody))
    };
    let purpose_signature = signature(0)?;
    let app_signature = signature(1)?;

    let kms_public_key_compressed = compressed_k256_public_key_hex(kms_public_key)
        .map_err(KeyCustodyError::KmsSignatureChain)?;
    let purpose_message = format!("{purpose}:{kms_public_key_compressed}");
    let app_public_key = recover_k256_public_key(purpose_message.as_bytes(), &purpose_signature)
        .map_err(KeyCustodyError::KmsSignatureChain)?;
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id,
        &app_public_key.to_sec1_bytes(),
    ]
    .concat();
    let root_public_key = recover_k256_public_key(&root_message, &app_signature)
        .map_err(KeyCustodyError::KmsSignatureChain)?;
    Ok(VerifiedReceiptCustody {
        provider: provider.to_string(),
        receipt_public_key: hex::encode(
            decode_hex(public_key).map_err(KeyCustodyError::InvalidKeyCustody)?,
        ),
        purpose: purpose.to_string(),
        kms_public_key: kms_public_key_compressed,
        app_public_key: hex::encode(app_public_key.to_sec1_bytes()),
        root_public_key: hex::encode(root_public_key.to_sec1_bytes()),
    })
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

/// A secp256k1 public key (compressed or uncompressed hex) in compressed hex.
pub fn compressed_k256_public_key_hex(public_key_hex: &str) -> Result<String, String> {
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

    fn signed_custody() -> (WorkloadKeyset, Value, Vec<u8>, String, String, String) {
        use aci_protocol::types::KeyedPublicKey;
        use k256::ecdsa::SigningKey;
        let key = |byte| SigningKey::from_slice(&[byte; 32]).unwrap();
        let root = key(1);
        let app = key(2);
        let kms = key(3);
        let app_id = vec![4; 20];
        let public = |key: &SigningKey| hex::encode(key.verifying_key().to_sec1_bytes());
        let sign = |key: &SigningKey, message: &[u8]| {
            let (signature, recovery) = key
                .sign_digest_recoverable(Keccak256::new_with_prefix(message))
                .unwrap();
            let mut bytes = signature.to_vec();
            bytes.push(recovery.to_byte());
            hex::encode(bytes)
        };
        let purpose = "aci.receipt.ed25519.v1";
        let app_message = [
            b"dstack-kms-issued:".as_slice(),
            &app_id,
            app.verifying_key().to_sec1_bytes().as_ref(),
        ]
        .concat();
        let keyset = WorkloadKeyset {
            subject: None,
            not_after: 1000,
            receipt_signing_keys: vec![KeyedPublicKey {
                key_id: "receipt".into(),
                algo: "ed25519".into(),
                public_key_hex: "11".repeat(32),
            }],
            e2ee_public_keys: vec![],
            tls_public_keys: vec![],
        };
        let evidence = serde_json::json!({"key_custody": {
            "provider": "dstack-kms", "keys": [{
                "role": "receipt", "public_key": "11".repeat(32), "purpose": purpose,
                // Normalization must agree with the compressed signed value.
                "kms_public_key": hex::encode(kms.verifying_key().to_encoded_point(false).as_bytes()),
                "signature_chain": [sign(&app, format!("{purpose}:{}", public(&kms)).as_bytes()),
                    sign(&root, &app_message)],
            }],
        }});
        (
            keyset,
            evidence,
            app_id,
            public(&kms),
            public(&app),
            public(&root),
        )
    }

    #[test]
    fn structured_receipt_custody_recovers_signed_identities() {
        let (keyset, evidence, app_id, kms, app, root) = signed_custody();
        let custody = verify_dstack_kms_receipt_custody(&evidence, &keyset, &app_id).unwrap();
        assert_eq!(
            custody,
            VerifiedReceiptCustody {
                provider: "dstack-kms".into(),
                receipt_public_key: "11".repeat(32),
                purpose: "aci.receipt.ed25519.v1".into(),
                kms_public_key: kms,
                app_public_key: app,
                root_public_key: root,
            }
        );
        assert!(serde_json::to_value(custody)
            .unwrap()
            .get("signature_chain")
            .is_none());
    }

    #[test]
    fn receipt_custody_rejects_invalid_chains() {
        let (keyset, evidence, app_id, ..) = signed_custody();
        for (field, value) in [
            ("signature_chain", serde_json::json!(["00", "00"])),
            ("purpose", serde_json::json!("other-purpose")),
            ("public_key", serde_json::json!("22".repeat(32))),
            ("kms_public_key", serde_json::json!("invalid")),
        ] {
            let mut bad = evidence.clone();
            bad["key_custody"]["keys"][0][field] = value;
            assert!(
                verify_dstack_kms_receipt_custody(&bad, &keyset, &app_id).is_err(),
                "{field}"
            );
        }
    }
}
