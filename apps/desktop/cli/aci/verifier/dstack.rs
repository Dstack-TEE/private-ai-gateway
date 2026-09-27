//! dstack verification: the event-log replay and the KMS custody chain come
//! from `aci-verify`; this module feeds them the DCAP quote and applies the
//! custody policy's accepted KMS roots.

pub use aci_verify::dstack::{dstack_rtmr3_event, DstackEventLog, VerifiedEventLog};
use aci_verify::dstack::{verify_dstack_kms_receipt_chain, KeyCustodyError};
use serde_json::Value;

use super::policy::CustodyPolicy;
use crate::aci::types::WorkloadKeyset;

/// Replay the dstack event log to RTMR3 and require it to match the quote,
/// returning the verified log.
pub fn verify_dstack_event_log(
    evidence: &Value,
    report: &dcap_qvl::quote::Report,
) -> Result<VerifiedEventLog, String> {
    aci_verify::dstack::verify_dstack_event_log(evidence, dcap_rtmr3(report))
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CustodyError {
    #[error(transparent)]
    Chain(#[from] KeyCustodyError),
    #[error("dstack KMS root public key is not accepted by the custody policy")]
    KmsRootRejected,
}

/// §9.1(5) under the dstack KMS policy: the receipt key's custody chain must
/// lead from the measured `app_id` to a KMS root the policy accepts.
pub(super) fn verify_dstack_kms_receipt_custody(
    evidence: &Value,
    keyset: &WorkloadKeyset,
    app_id: &[u8],
    policy: &CustodyPolicy,
) -> Result<(), CustodyError> {
    let root = verify_dstack_kms_receipt_chain(evidence, keyset, app_id)?;
    if !policy.accepts_kms_root(&root) {
        return Err(CustodyError::KmsRootRejected);
    }
    Ok(())
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
    use sha3::{Digest as _, Keccak256};

    use super::*;
    use crate::aci::types::KeyedPublicKey;

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

        assert!(matches!(
            err,
            CustodyError::Chain(KeyCustodyError::ReceiptKeyCustodyMismatch)
        ));
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
            matches!(&err, CustodyError::Chain(KeyCustodyError::InvalidKeyCustody(m)) if m.contains("purpose")),
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
            matches!(&err, CustodyError::Chain(KeyCustodyError::InvalidKeyCustody(m)) if m.contains("2 signatures")),
            "{err}"
        );
    }
}
