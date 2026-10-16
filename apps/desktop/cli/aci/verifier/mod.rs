//! ACI relying-party appraisal used by the CLI and local proxy.

mod appraisal;
mod dstack;
mod policy;
mod quote;

pub use aci_verify::report::{
    validate_aci_report_binding, AciReportValidationError, ReportBinding, ValidatedAciReport,
};
pub use appraisal::{
    appraise_report, Appraisal, AppraisalInputs, ChannelEvidence, CheckId, CheckResult,
    CustodyEvidence, FailureCause, Outcome, QuoteSource,
};
pub use dstack::{dstack_rtmr3_event, verify_dstack_event_log, DstackEventLog, VerifiedEventLog};
pub use policy::{CustodyPolicy, CustodyPolicyError};
pub use quote::{QuoteStepError, DEFAULT_PCCS_URL};

fn dcap_report_data(report: &dcap_qvl::quote::Report) -> &[u8; 64] {
    if let Some(report) = report.as_sgx() {
        &report.report_data
    } else {
        let report = report.as_td10().expect("DCAP report must be SGX or TDX");
        &report.report_data
    }
}
