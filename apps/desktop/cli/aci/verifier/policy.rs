//! The custody policy (§1.3, §9.1(5)): which dstack KMS roots the receipt
//! key's custody chain may end at, and which measured app-ids are acceptable.
//! The caller supplies every trust anchor.
//!
//! The only workload anchor is the app-id the verified RTMR3 event log
//! measures. A report's `source_provenance.image_digest` is self-asserted and
//! nothing measured corroborates it (§4.1; conformance gaps item 16), so it is
//! never an anchor here: an app under the same KMS root could claim it.

use std::collections::BTreeSet;

use super::dstack::compressed_k256_public_key_hex;
use crate::aci::types::WorkloadKeyset;

const APP_ID_SUBJECT_PREFIX: &str = "app-id:0x";

#[derive(Debug, thiserror::Error)]
pub enum CustodyPolicyError {
    #[error("a custody policy needs at least one accepted app-id")]
    EmptySubjects,
    #[error("a custody policy needs at least one accepted dstack KMS root public key")]
    EmptyKmsRoots,
    #[error("invalid app-id subject {0:?}: expected app-id:0x followed by hex")]
    InvalidSubject(String),
    #[error("invalid dstack KMS root public key: {0}")]
    InvalidKmsRootPublicKey(String),
}

#[derive(Clone, Debug)]
pub struct CustodyPolicy {
    accepted_app_ids: BTreeSet<Vec<u8>>,
    accepted_kms_root_public_keys: BTreeSet<String>,
}

impl CustodyPolicy {
    /// `accepted_subjects` are `app-id:0x<hex>` (hex in either case);
    /// `accepted_kms_root_public_keys` are secp256k1 keys, compressed or not.
    pub fn new(
        accepted_subjects: impl IntoIterator<Item = String>,
        accepted_kms_root_public_keys: impl IntoIterator<Item = String>,
    ) -> Result<Self, CustodyPolicyError> {
        let accepted_app_ids = accepted_subjects
            .into_iter()
            .map(|subject| parse_app_id_subject(&subject))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let accepted_kms_root_public_keys = accepted_kms_root_public_keys
            .into_iter()
            .map(|key| {
                compressed_k256_public_key_hex(&key)
                    .map_err(CustodyPolicyError::InvalidKmsRootPublicKey)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if accepted_app_ids.is_empty() {
            return Err(CustodyPolicyError::EmptySubjects);
        }
        if accepted_kms_root_public_keys.is_empty() {
            return Err(CustodyPolicyError::EmptyKmsRoots);
        }
        Ok(Self {
            accepted_app_ids,
            accepted_kms_root_public_keys,
        })
    }

    /// `root` is a compressed secp256k1 public key in lowercase hex.
    pub(super) fn accepts_kms_root(&self, root: &str) -> bool {
        self.accepted_kms_root_public_keys.contains(root)
    }

    /// §3 identity anchor: the app-id the verified event log measured must be
    /// accepted, and a declared keyset `subject` — the workload's own claim
    /// (§3.1) — may only restate it.
    pub(super) fn check_measured(
        &self,
        keyset: &WorkloadKeyset,
        measured_app_id: &[u8],
    ) -> Result<(), String> {
        let measured = hex::encode(measured_app_id);
        let measured_subject = format!("{APP_ID_SUBJECT_PREFIX}{measured}");
        if let Some(subject) = keyset
            .subject
            .as_ref()
            .filter(|subject| **subject != measured_subject)
        {
            return Err(format!(
                "keyset subject {subject} does not name the measured app-id 0x{measured}"
            ));
        }
        if !self.accepted_app_ids.contains(measured_app_id) {
            return Err(format!(
                "measured app-id 0x{measured} is not accepted by the custody policy"
            ));
        }
        Ok(())
    }
}

fn parse_app_id_subject(subject: &str) -> Result<Vec<u8>, CustodyPolicyError> {
    subject
        .strip_prefix(APP_ID_SUBJECT_PREFIX)
        .filter(|hex| !hex.is_empty())
        .and_then(|hex| hex::decode(hex).ok())
        .ok_or_else(|| CustodyPolicyError::InvalidSubject(subject.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The generator point, a valid compressed secp256k1 key.
    const ROOT: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    fn keyset(subject: Option<String>) -> WorkloadKeyset {
        WorkloadKeyset {
            subject,
            not_after: u64::MAX,
            receipt_signing_keys: Vec::new(),
            e2ee_public_keys: Vec::new(),
            tls_public_keys: Vec::new(),
        }
    }

    #[test]
    fn subject_anchor_requires_the_measured_app_id() {
        let measured = [0xab; 20];
        let measured_subject = format!("app-id:0x{}", hex::encode(measured));
        let policy = CustodyPolicy::new(
            [measured_subject.clone(), "app-id:0xcd".to_string()],
            [ROOT.to_string()],
        )
        .unwrap();

        assert!(policy
            .check_measured(&keyset(Some(measured_subject)), &measured)
            .is_ok());
        // Accepted by the policy, but not what the event log measured: a
        // declared subject is the workload's own claim and must not anchor.
        assert!(policy
            .check_measured(&keyset(Some("app-id:0xcd".to_string())), &measured)
            .is_err());
        // No declared subject: the measured app-id is the anchor.
        assert!(policy.check_measured(&keyset(None), &measured).is_ok());
        // A measurement the policy never accepted is rejected either way.
        assert!(policy.check_measured(&keyset(None), &[0xee; 20]).is_err());
    }

    #[test]
    fn app_id_subjects_are_normalized_or_rejected() {
        let policy = CustodyPolicy::new(["app-id:0xABCD".to_string()], [ROOT.to_string()]).unwrap();
        assert!(policy.check_measured(&keyset(None), &[0xab, 0xcd]).is_ok());

        for subject in [
            "0xabcd",
            "app-id:abcd",
            "app-id:0x",
            "app-id:0xabc",
            "app-id:0xzz",
        ] {
            assert!(
                matches!(
                    CustodyPolicy::new([subject.to_string()], [ROOT.to_string()]),
                    Err(CustodyPolicyError::InvalidSubject(_))
                ),
                "{subject} must be rejected"
            );
        }
    }

    #[test]
    fn a_policy_needs_an_app_id_and_a_root() {
        assert!(matches!(
            CustodyPolicy::new([], [ROOT.to_string()]),
            Err(CustodyPolicyError::EmptySubjects)
        ));
        assert!(matches!(
            CustodyPolicy::new(["app-id:0xab".to_string()], []),
            Err(CustodyPolicyError::EmptyKmsRoots)
        ));
        assert!(matches!(
            CustodyPolicy::new(["app-id:0xab".to_string()], ["02zz".to_string()]),
            Err(CustodyPolicyError::InvalidKmsRootPublicKey(_))
        ));
    }
}
