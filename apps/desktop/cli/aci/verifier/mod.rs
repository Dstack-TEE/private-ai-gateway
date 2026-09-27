//! ACI relying-party appraisal used by the CLI and local proxy.

pub const DEFAULT_DCAP_PCCS_URL: &str = dcap_qvl::PHALA_PCCS_URL;

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
pub use quote::QuoteStepError;

fn dcap_report_data(report: &dcap_qvl::quote::Report) -> &[u8; 64] {
    match report {
        dcap_qvl::quote::Report::SgxEnclave(report) => &report.report_data,
        dcap_qvl::quote::Report::TD10(report) => &report.report_data,
        dcap_qvl::quote::Report::TD15(report) => &report.base.report_data,
    }
}
