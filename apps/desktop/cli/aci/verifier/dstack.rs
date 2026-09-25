//! dstack verification: event-log replay for the provenance appraisal, and
//! the KMS signature chain for key custody.

use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use serde_json::Value;
use sha2::{Digest, Sha256, Sha384};
use sha3::Keccak256;

use super::decode_hex;
use super::policy::CustodyPolicy;
use crate::aci::types::WorkloadKeyset;

const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x08000001;

/// Wire representation of the dstack event evidence consumed by verification.
/// Verification is platform-neutral and does not depend on the Unix client SDK.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct DstackEventLog {
    pub imr: u32,
    pub event_type: u32,
    pub digest: String,
    pub event: String,
    pub event_payload: String,
}

/// Replay the dstack event log to RTMR3 and require it to match the quote,
/// returning the verified events. Private AI Proxy reuses this for its own
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
    let rtmr3 = replay_dstack_rtmr(&events, 3)?;
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
) -> Result<Option<&'a DstackEventLog>, String> {
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
        return Err(format!(
            "invalid dstack event_log evidence: multiple pre-system-ready {event_name} events"
        ));
    }
    Ok(event)
}

/// The RTMR3-measured compose hash that `app_compose` reproduces (§9.1(4)).
///
/// Returns the measured hash as lowercase hex, so a caller can report or pin
/// it.
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
    verify_dstack_app_compose(evidence, &measured_hash)?;
    Ok(hex::encode(measured_hash))
}

/// Verify that `app_compose` is the preimage of the compose measurement
/// bound into RTMR3 by the verified event log.
pub(super) fn verify_dstack_app_compose(
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

fn replay_dstack_rtmr(events: &[DstackEventLog], imr: u32) -> Result<[u8; 48], String> {
    let mut mr = vec![0u8; 48];
    for event in events.iter().filter(|event| event.imr == imr) {
        let mut digest = dstack_event_digest(event)?;
        if digest.len() < 48 {
            digest.resize(48, 0);
        }
        mr.extend_from_slice(&digest);
        mr = Sha384::digest(&mr).to_vec();
    }
    mr.as_slice()
        .try_into()
        .map_err(|_| "invalid dstack event_log evidence: replayed RTMR is not 48 bytes".into())
}

fn dstack_event_digest(event: &DstackEventLog) -> Result<Vec<u8>, String> {
    if event.event_type != DSTACK_RUNTIME_EVENT_TYPE {
        return decode_hex(&event.digest)
            .map_err(|error| format!("invalid dstack event_log evidence: {error}"));
    }

    let payload = decode_hex(&event.event_payload)
        .map_err(|error| format!("invalid dstack event_log evidence: {error}"))?;
    let mut hasher = Sha384::new();
    hasher.update(event.event_type.to_ne_bytes());
    hasher.update(b":");
    hasher.update(event.event.as_bytes());
    hasher.update(b":");
    hasher.update(payload);
    Ok(hasher.finalize().to_vec())
}

/// The RTMR3-measured app-id, which the custody chain and the policy anchor
/// on (§9.1(5)).
pub(super) fn dstack_app_id(events: &[DstackEventLog]) -> Result<Vec<u8>, String> {
    let event = dstack_rtmr3_event(events, "app-id")
        .map_err(|e| format!("dstack event log rejected: {e}"))?
        .ok_or_else(|| "verified event log carries no app-id event".to_string())?;
    decode_hex(&event.event_payload)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CustodyError {
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
    #[error("dstack KMS root public key is not accepted by the custody policy")]
    KmsRootRejected,
}

/// §9.1(5) under the dstack KMS policy, following Gateway's
/// `verify_dstack_kms_receipt_custody`: the receipt key's custody entry must
/// name a key the keyset attests, and its signature chain must lead from
/// the measured `app_id` to an accepted KMS root.
///
/// Residual gap (conformance gaps item 16): the chain signs the evidence's
/// self-asserted `kms_public_key`, and the link from that k256 scalar to the
/// published Ed25519 receipt key rests on the measured workload code — which
/// is why the policy anchor must itself be measured.
pub(super) fn verify_dstack_kms_receipt_custody(
    evidence: &Value,
    keyset: &WorkloadKeyset,
    app_id: &[u8],
    policy: &CustodyPolicy,
) -> Result<(), CustodyError> {
    let key_custody = evidence
        .get("key_custody")
        .ok_or(CustodyError::MissingKeyCustody)?;
    let provider = key_custody
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| CustodyError::InvalidKeyCustody("missing provider".to_string()))?;
    if provider != "dstack-kms" {
        return Err(CustodyError::UnsupportedKeyCustodyProvider(
            provider.to_string(),
        ));
    }
    let keys = key_custody
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| CustodyError::InvalidKeyCustody("missing keys".to_string()))?;
    let receipt = keys
        .iter()
        .find(|key| key.get("role").and_then(Value::as_str) == Some("receipt"))
        .ok_or(CustodyError::MissingReceiptKeyCustody)?;
    let field = |name: &str| {
        receipt.get(name).and_then(Value::as_str).ok_or_else(|| {
            CustodyError::InvalidKeyCustody(format!("receipt key custody missing {name}"))
        })
    };
    let public_key = field("public_key")?;
    if !keyset
        .receipt_signing_keys
        .iter()
        .any(|key| key.public_key_hex == public_key)
    {
        return Err(CustodyError::ReceiptKeyCustodyMismatch);
    }
    let kms_public_key = field("kms_public_key")?;
    let purpose = field("purpose")?;
    // The chain must be for the receipt key-derivation path, not any KMS key
    // the app happens to hold.
    if purpose != "aci.receipt.ed25519.v1" {
        return Err(CustodyError::InvalidKeyCustody(format!(
            "receipt key custody purpose is {purpose:?}, expected \"aci.receipt.ed25519.v1\""
        )));
    }
    let signature_chain = receipt
        .get("signature_chain")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CustodyError::InvalidKeyCustody(
                "receipt key custody missing signature_chain".to_string(),
            )
        })?;
    if signature_chain.len() != 2 {
        return Err(CustodyError::InvalidKeyCustody(format!(
            "receipt key custody signature_chain must contain 2 signatures, got {}",
            signature_chain.len()
        )));
    }
    let signature = |index: usize| {
        signature_chain[index]
            .as_str()
            .ok_or_else(|| {
                CustodyError::InvalidKeyCustody(format!(
                    "receipt key custody signature_chain[{index}] is not a string"
                ))
            })
            .and_then(|s| decode_hex(s).map_err(CustodyError::InvalidKeyCustody))
    };
    let purpose_signature = signature(0)?;
    let app_signature = signature(1)?;

    let kms_public_key_compressed =
        compressed_k256_public_key_hex(kms_public_key).map_err(CustodyError::KmsSignatureChain)?;
    let purpose_message = format!("{purpose}:{kms_public_key_compressed}");
    let app_public_key = recover_k256_public_key(purpose_message.as_bytes(), &purpose_signature)
        .map_err(CustodyError::KmsSignatureChain)?;
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id,
        &app_public_key.to_sec1_bytes(),
    ]
    .concat();
    let root_public_key = recover_k256_public_key(&root_message, &app_signature)
        .map_err(CustodyError::KmsSignatureChain)?;
    if !policy.accepts_kms_root(&hex::encode(root_public_key.to_sec1_bytes())) {
        return Err(CustodyError::KmsRootRejected);
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

/// A secp256k1 public key (compressed or uncompressed hex) in compressed hex.
pub(super) fn compressed_k256_public_key_hex(public_key_hex: &str) -> Result<String, String> {
    let public_key = decode_hex(public_key_hex)?;
    let point = EncodedPoint::from_bytes(public_key)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    let key = K256VerifyingKey::from_encoded_point(&point)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    Ok(hex::encode(key.to_sec1_bytes()))
}

fn dcap_rtmr3(report: &dcap_qvl::quote::Report) -> Option<&[u8; 48]> {
    match report {
        dcap_qvl::quote::Report::TD10(report) => Some(&report.rt_mr3),
        dcap_qvl::quote::Report::TD15(report) => Some(&report.base.rt_mr3),
        dcap_qvl::quote::Report::SgxEnclave(_) => None,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use k256::ecdsa::SigningKey;
    use serde_json::json;

    use super::*;
    use crate::aci::types::KeyedPublicKey;

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

    pub(in crate::aci::verifier) fn signing_key(byte: u8) -> SigningKey {
        SigningKey::from_slice(&[byte; 32]).unwrap()
    }

    fn sign_recoverable(key: &SigningKey, message: &[u8]) -> String {
        let (signature, recid) = key
            .sign_digest_recoverable(Keccak256::new_with_prefix(message))
            .unwrap();
        let mut out = signature.to_vec();
        out.push(recid.to_byte());
        hex::encode(out)
    }

    pub(in crate::aci::verifier) const APP_ID: [u8; 20] = [0xab; 20];

    /// A keyset and custody evidence for the receipt key derived from the
    /// 32-byte KMS scalar 3×32: the keyset lists its Ed25519 public key, the
    /// evidence publishes the k256 counterpart, and the chain runs app key 2
    /// -> KMS root `root`.
    pub(in crate::aci::verifier) fn custody_fixture(root: &SigningKey) -> (WorkloadKeyset, Value) {
        let receipt_scalar = [3u8; 32];
        let app = signing_key(2);
        let kms_public = hex::encode(
            SigningKey::from_slice(&receipt_scalar)
                .unwrap()
                .verifying_key()
                .to_sec1_bytes(),
        );
        let purpose_signature = sign_recoverable(
            &app,
            format!("aci.receipt.ed25519.v1:{kms_public}").as_bytes(),
        );
        let root_message = [
            b"dstack-kms-issued".as_slice(),
            b":",
            APP_ID.as_slice(),
            &app.verifying_key().to_sec1_bytes(),
        ]
        .concat();
        let app_signature = sign_recoverable(root, &root_message);
        let ed25519_public = hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&receipt_scalar)
                .verifying_key()
                .as_bytes(),
        );
        let keyset = WorkloadKeyset {
            subject: None,
            not_after: u64::MAX,
            receipt_signing_keys: vec![KeyedPublicKey {
                key_id: "receipt-1".to_string(),
                algo: "ed25519".to_string(),
                public_key_hex: ed25519_public.clone(),
            }],
            e2ee_public_keys: Vec::new(),
            tls_public_keys: Vec::new(),
        };
        let evidence = json!({
            "key_custody": {
                "provider": "dstack-kms",
                "keys": [{
                    "role": "receipt",
                    "purpose": "aci.receipt.ed25519.v1",
                    "public_key": ed25519_public,
                    "kms_public_key": kms_public,
                    "signature_chain": [purpose_signature, app_signature],
                }]
            }
        });
        (keyset, evidence)
    }

    pub(in crate::aci::verifier) fn policy_with_root(root: &SigningKey) -> CustodyPolicy {
        let root_uncompressed =
            hex::encode(root.verifying_key().to_encoded_point(false).as_bytes());
        CustodyPolicy::new(
            [format!("app-id:0x{}", hex::encode(APP_ID))],
            [root_uncompressed],
        )
        .unwrap()
    }

    #[test]
    fn custody_chain_to_an_accepted_root_verifies() {
        let root = signing_key(1);
        let (keyset, evidence) = custody_fixture(&root);

        verify_dstack_kms_receipt_custody(&evidence, &keyset, &APP_ID, &policy_with_root(&root))
            .unwrap();
    }

    #[test]
    fn custody_chain_to_another_root_is_rejected() {
        let (keyset, evidence) = custody_fixture(&signing_key(1));

        let err = verify_dstack_kms_receipt_custody(
            &evidence,
            &keyset,
            &APP_ID,
            &policy_with_root(&signing_key(4)),
        )
        .unwrap_err();

        assert!(matches!(err, CustodyError::KmsRootRejected));
    }

    #[test]
    fn custody_for_a_key_the_keyset_does_not_attest_is_rejected() {
        let root = signing_key(1);
        let (mut keyset, evidence) = custody_fixture(&root);
        keyset.receipt_signing_keys[0].public_key_hex = "ff".repeat(32);

        let err = verify_dstack_kms_receipt_custody(
            &evidence,
            &keyset,
            &APP_ID,
            &policy_with_root(&root),
        )
        .unwrap_err();

        assert!(matches!(err, CustodyError::ReceiptKeyCustodyMismatch));
    }

    #[test]
    fn custody_chain_for_another_app_is_rejected() {
        let root = signing_key(1);
        let (keyset, evidence) = custody_fixture(&root);

        // The root signed APP_ID's app key; under another measured app-id the
        // recovered root is some other key.
        let err = verify_dstack_kms_receipt_custody(
            &evidence,
            &keyset,
            &[0xcd; 20],
            &policy_with_root(&root),
        )
        .unwrap_err();

        assert!(matches!(err, CustodyError::KmsRootRejected));
    }

    #[test]
    fn custody_for_another_purpose_is_rejected() {
        let root = signing_key(1);
        let (keyset, mut evidence) = custody_fixture(&root);
        evidence["key_custody"]["keys"][0]["purpose"] = json!("aci.e2ee.v1");

        let err = verify_dstack_kms_receipt_custody(
            &evidence,
            &keyset,
            &APP_ID,
            &policy_with_root(&root),
        )
        .unwrap_err();

        assert!(
            matches!(&err, CustodyError::InvalidKeyCustody(m) if m.contains("purpose")),
            "{err}"
        );
    }

    #[test]
    fn custody_chain_of_the_wrong_length_is_rejected() {
        let root = signing_key(1);
        let (keyset, mut evidence) = custody_fixture(&root);
        let chain = &mut evidence["key_custody"]["keys"][0]["signature_chain"];
        let first = chain[0].clone();
        *chain = json!([first]);

        let err = verify_dstack_kms_receipt_custody(
            &evidence,
            &keyset,
            &APP_ID,
            &policy_with_root(&root),
        )
        .unwrap_err();

        assert!(
            matches!(&err, CustodyError::InvalidKeyCustody(m) if m.contains("2 signatures")),
            "{err}"
        );
    }
}
