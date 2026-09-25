//! The dstack verifier policy (§1.3): which KMS roots custody may chain to
//! (§9.1(5)) and which measured workloads are acceptable.

use std::collections::BTreeSet;

use aci_protocol::types::{SourceProvenance, WorkloadKeyset};

use crate::dstack::compressed_k256_public_key_hex;

#[derive(Debug, thiserror::Error)]
pub enum VerifierPolicyError {
    #[error(
        "ACI service upstream verifier requires at least one accepted subject or image digest"
    )]
    EmptyPolicy,
    #[error(
        "ACI service upstream verifier requires at least one accepted dstack KMS root public key"
    )]
    EmptyKmsRootPolicy,
    #[error("invalid dstack KMS root public key: {0}")]
    InvalidKmsRootPublicKey(String),
}

#[derive(Debug, Clone)]
pub struct AciServiceVerifierPolicy {
    accepted_subjects: BTreeSet<String>,
    accepted_image_digests: BTreeSet<String>,
    pub(crate) accepted_kms_root_public_keys: BTreeSet<String>,
}

impl AciServiceVerifierPolicy {
    pub fn new(
        accepted_subjects: impl IntoIterator<Item = String>,
        accepted_image_digests: impl IntoIterator<Item = String>,
        accepted_kms_root_public_keys: impl IntoIterator<Item = String>,
    ) -> Result<Self, VerifierPolicyError> {
        let accepted_subjects = accepted_subjects
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        let accepted_image_digests = accepted_image_digests
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        let accepted_kms_root_public_keys = accepted_kms_root_public_keys
            .into_iter()
            .map(|key| {
                compressed_k256_public_key_hex(&key)
                    .map_err(VerifierPolicyError::InvalidKmsRootPublicKey)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if accepted_subjects.is_empty() && accepted_image_digests.is_empty() {
            return Err(VerifierPolicyError::EmptyPolicy);
        }
        if accepted_kms_root_public_keys.is_empty() {
            return Err(VerifierPolicyError::EmptyKmsRootPolicy);
        }
        Ok(Self {
            accepted_subjects,
            accepted_image_digests,
            accepted_kms_root_public_keys,
        })
    }

    /// §3 identity anchors: the attested keyset `subject` — accepted only
    /// when it names the app-id the verified event log measured, since a
    /// free-form subject is otherwise the workload's own claim (§3.1:
    /// "Generic verifiers MUST NOT trust it by itself") — or the report's
    /// source-provenance image digest (uncorroborated; conformance-gaps
    /// item 17).
    pub(crate) fn accepts_measured(
        &self,
        keyset: &WorkloadKeyset,
        provenance: &SourceProvenance,
        measured_app_id: &[u8],
    ) -> bool {
        // The anchor is the measurement, not what the report says about
        // itself: `subject` is self-asserted, so a declared one may only
        // restate the measured app-id, and acceptance is decided by whether
        // that measured value is allowlisted.
        let measured_subject = format!("app-id:0x{}", hex::encode(measured_app_id));
        if keyset
            .subject
            .as_ref()
            .is_some_and(|subject| *subject != measured_subject)
        {
            return false;
        }
        self.accepted_subjects.contains(&measured_subject)
            || provenance
                .image_digest
                .as_ref()
                .is_some_and(|digest| self.accepted_image_digests.contains(digest))
    }
}
