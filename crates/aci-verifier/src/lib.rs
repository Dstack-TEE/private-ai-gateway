//! The ACI relying-party verifier (§9.1): the checks, their order, and what
//! each outcome means.
//!
//! Private AI Gateway (verifying its upstreams) and Private AI Proxy
//! (verifying the service it fronts) both appraise reports through
//! [`appraise_report`] — the gateway folds the outcomes into one
//! accept/reject, the proxy renders each as a transcript line. One
//! implementation is what keeps the two from drifting while each keeps
//! passing its own tests. Each caller still supplies its own verifier
//! policy (§1.3); wire types and encoding come from `aci-protocol`.

// Re-exported because its quote and report types appear in this crate's API.
pub use dcap_qvl;

mod appraisal;
mod channel;
mod dstack;
mod policy;
mod quote;
mod report;
#[cfg(test)]
mod tests;

pub use appraisal::{
    appraise_report, Appraisal, AppraisalInputs, ChannelEvidence, CheckId, CheckResult,
    CustodyEvidence, FailureCause, Outcome, QuoteSource,
};
pub use dstack::{dstack_rtmr3_event, verify_dstack_event_log, DstackEventLog};
pub use policy::{AciServiceVerifierPolicy, VerifierPolicyError};
pub use quote::QuoteStepError;
pub use report::{
    raw_evidence, validate_aci_report_binding, AciReportValidationError, ReportBinding,
    ValidatedAciReport,
};

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|e| e.to_string())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = decode_hex(value)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 32 bytes, got {}", bytes.len()))
}

/// The `report_data` field of a parsed DCAP quote, across report variants.
pub fn dcap_report_data(report: &dcap_qvl::quote::Report) -> &[u8; 64] {
    match report {
        dcap_qvl::quote::Report::SgxEnclave(report) => &report.report_data,
        dcap_qvl::quote::Report::TD10(report) => &report.report_data,
        dcap_qvl::quote::Report::TD15(report) => &report.base.report_data,
    }
}

fn dcap_rtmr3(report: &dcap_qvl::quote::Report) -> Option<&[u8; 48]> {
    match report {
        dcap_qvl::quote::Report::TD10(report) => Some(&report.rt_mr3),
        dcap_qvl::quote::Report::TD15(report) => Some(&report.base.rt_mr3),
        dcap_qvl::quote::Report::SgxEnclave(_) => None,
    }
}
