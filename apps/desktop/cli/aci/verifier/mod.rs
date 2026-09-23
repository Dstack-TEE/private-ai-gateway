//! ACI relying-party appraisal used by the CLI and local proxy.

pub const DEFAULT_DCAP_PCCS_URL: &str = dcap_qvl::PHALA_PCCS_URL;

mod appraisal;

pub use aci_verify::dstack::{dstack_rtmr3_event, verify_dstack_event_log, DstackEventLog};
pub use aci_verify::quote::QuoteStepError;
pub use aci_verify::report::{
    validate_aci_report_binding, AciReportValidationError, ReportBinding, ValidatedAciReport,
};
pub use appraisal::{
    appraise_report, Appraisal, AppraisalInputs, ChannelEvidence, CheckId, CheckResult,
    CustodyEvidence, FailureCause, Outcome, QuoteSource,
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
