//! dstack verification: the event-log replay and the KMS custody chain come
//! from `aci-verify`; this module feeds them the DCAP quote and applies this
//! verifier's accepted KMS roots.

use aci_verify::dstack::verify_dstack_kms_receipt_chain;
pub use aci_verify::dstack::{dstack_rtmr3_event, DstackEventLog};
use serde_json::Value;

use super::aci_service::{dcap_rtmr3, AciServiceVerificationError, AciServiceVerifierPolicy};
use crate::aci::types::WorkloadKeyset;

/// Replay the dstack event log to RTMR3 and require it to match the quote,
/// returning the verified events.
pub fn verify_dstack_event_log(
    evidence: &Value,
    report: &dcap_qvl::quote::Report,
) -> Result<Vec<DstackEventLog>, String> {
    aci_verify::dstack::verify_dstack_event_log(evidence, dcap_rtmr3(report))
}

/// §9.1(5): the receipt key's KMS chain must end at a root this verifier's
/// policy accepts.
pub(super) fn verify_dstack_kms_receipt_custody(
    evidence: &Value,
    keyset: &WorkloadKeyset,
    app_id: &[u8],
    policy: &AciServiceVerifierPolicy,
) -> Result<(), AciServiceVerificationError> {
    let root = verify_dstack_kms_receipt_chain(evidence, keyset, app_id)?;
    if !policy.accepted_kms_root_public_keys.contains(&root) {
        return Err(AciServiceVerificationError::KmsRootRejected);
    }
    Ok(())
}
