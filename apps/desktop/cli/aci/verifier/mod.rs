//! ACI relying-party appraisal used by the CLI and local proxy.

pub const DEFAULT_DCAP_PCCS_URL: &str = dcap_qvl::PHALA_PCCS_URL;

mod appraisal;
mod dstack;
mod quote;
mod report;

pub use appraisal::{
    appraise_report, Appraisal, AppraisalInputs, ChannelEvidence, CheckId, CheckResult,
    CustodyEvidence, FailureCause, Outcome, QuoteSource,
};
pub use dstack::{dstack_rtmr3_event, verify_dstack_event_log, DstackEventLog};
pub use quote::QuoteStepError;
pub use report::{
    validate_aci_report_binding, AciReportValidationError, ReportBinding, ValidatedAciReport,
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

fn dcap_report_data(report: &dcap_qvl::quote::Report) -> &[u8; 64] {
    match report {
        dcap_qvl::quote::Report::SgxEnclave(report) => &report.report_data,
        dcap_qvl::quote::Report::TD10(report) => &report.report_data,
        dcap_qvl::quote::Report::TD15(report) => &report.base.report_data,
    }
}
