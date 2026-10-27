//! Freshly established renewal members and exact receipt identity selection.

use std::sync::Arc;

use crate::aci::types::AttestationReport;
use crate::checks::EstablishedIdentity;
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct EstablishedDigest {
    pub digest: String,
    pub not_after: u64,
    pub identity: Arc<EstablishedIdentity>,
    pub report: Arc<AttestationReport>,
}

impl EstablishedDigest {
    pub fn new(report: AttestationReport, identity: EstablishedIdentity) -> Self {
        Self {
            digest: identity.keyset_digest.clone(),
            not_after: identity.keyset.not_after,
            identity: Arc::new(identity),
            report: Arc::new(report),
        }
    }
}

pub(crate) fn same_renewal_family(left: &AttestationReport, right: &AttestationReport) -> bool {
    let mut left_keyset = left.attestation.workload_keyset.clone();
    let mut right_keyset = right.attestation.workload_keyset.clone();
    let (Some(left_keyset), Some(right_keyset)) =
        (left_keyset.as_object_mut(), right_keyset.as_object_mut())
    else {
        return false;
    };
    left_keyset.remove("not_after");
    right_keyset.remove("not_after");
    left_keyset == right_keyset
        && left.attestation.tee_type == right.attestation.tee_type
        && left.attestation.source_provenance == right.attestation.source_provenance
        && left.attestation.evidence.get("app_compose")
            == right.attestation.evidence.get("app_compose")
}

/// A hint never establishes a member. Select only an exactly attested digest
/// in the admitted family that was valid when the receipt says it was served.
pub(crate) fn receipt_identity(
    admitted: &AttestationReport,
    established: &[EstablishedDigest],
    receipt: &Value,
) -> Option<EstablishedDigest> {
    let digest = receipt["workload_keyset_digest"].as_str()?;
    let served_at = receipt["served_at"].as_u64()?;
    established
        .iter()
        .find(|entry| {
            entry.digest == digest
                && served_at < entry.not_after
                && same_renewal_family(admitted, &entry.report)
        })
        .cloned()
}
