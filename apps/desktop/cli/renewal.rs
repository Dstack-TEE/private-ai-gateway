//! Freshly established renewal members and exact receipt identity selection.

use std::sync::Arc;

use crate::aci::types::AttestationReport;
use crate::checks::EstablishedIdentity;
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct EstablishedMember {
    pub identity: Arc<EstablishedIdentity>,
    pub report: Arc<AttestationReport>,
}

impl EstablishedMember {
    pub fn new(report: AttestationReport, identity: EstablishedIdentity) -> Self {
        Self {
            identity: Arc::new(identity),
            report: Arc::new(report),
        }
    }

    pub fn digest(&self) -> &str {
        &self.identity.keyset_digest
    }

    pub fn not_after(&self) -> u64 {
        self.identity.keyset.not_after
    }
}

// Reports are verified within one API origin and unchanged verifier policy.
pub(crate) fn same_renewal_family(left: &AttestationReport, right: &AttestationReport) -> bool {
    let mut left_keyset = left.attestation.workload_keyset.clone();
    let mut right_keyset = right.attestation.workload_keyset.clone();
    let (Some(left_keyset), Some(right_keyset)) =
        (left_keyset.as_object_mut(), right_keyset.as_object_mut())
    else {
        return false;
    };
    let (Some(left_compose), Some(right_compose)) = (
        left.attestation.evidence["app_compose"].as_str(),
        right.attestation.evidence["app_compose"].as_str(),
    ) else {
        return false;
    };
    left_keyset.remove("not_after");
    right_keyset.remove("not_after");
    left_keyset == right_keyset
        && left.attestation.tee_type == right.attestation.tee_type
        && left.attestation.source_provenance == right.attestation.source_provenance
        && left_compose == right_compose
}

/// A hint never establishes a member. Select only an exactly attested digest
/// in the admitted family. Receipt-2 checks its validity at served_at.
pub(crate) fn receipt_identity(
    admitted: &AttestationReport,
    established: &[EstablishedMember],
    receipt: &Value,
) -> Option<EstablishedMember> {
    let digest = receipt["workload_keyset_digest"].as_str()?;
    established
        .iter()
        .find(|entry| entry.digest() == digest && same_renewal_family(admitted, &entry.report))
        .cloned()
}
