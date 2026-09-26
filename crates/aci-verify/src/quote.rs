//! The §9.1(1) quote steps that do not depend on a DCAP library.
//!
//! Each verifier parses and verifies the quote with its own DCAP library and
//! passes the parsed fields here, so the evidence handling and the
//! report-data comparison are the same in both.

use aci_protocol::identity;
use serde_json::Value;

use crate::decode_hex;

#[derive(Debug, thiserror::Error)]
pub enum QuoteStepError {
    #[error("report evidence carries no quote")]
    MissingQuote,
    #[error("evidence quote is not valid hex: {0}")]
    InvalidQuoteHex(String),
    #[error("quote does not parse: {0}")]
    UnparsableQuote(String),
    #[error("evidence quote_report_data is not valid hex: {0}")]
    InvalidEvidenceReportDataHex(String),
    #[error("evidence quote_report_data does not match the quote's report-data slot")]
    EvidenceReportDataMismatch,
    #[error(
        "the quote's report-data slot ({slot}) does not bind the report's report_data \
         zero-padded to 64 bytes"
    )]
    ReportDataSlotMismatch { slot: String },
    #[error("quote collateral fetch from {url} failed: {reason}")]
    Collateral { url: String, reason: String },
    #[error("DCAP quote verification failed: {0}")]
    Verification(String),
    #[error("report claims tee_type {reported:?}, the verified quote is {verified:?}")]
    TeeTypeMismatch {
        reported: String,
        verified: &'static str,
    },
}

/// The raw quote bytes the evidence carries, for the caller's DCAP library.
pub fn quote_bytes(evidence: &Value) -> Result<Vec<u8>, QuoteStepError> {
    let quote_hex = evidence
        .get("quote")
        .and_then(Value::as_str)
        .ok_or(QuoteStepError::MissingQuote)?;
    decode_hex(quote_hex).map_err(QuoteStepError::InvalidQuoteHex)
}

/// Check the quote's 64-byte report-data `slot` carries `report_data`, and
/// that `evidence.quote_report_data`, when published, agrees with the quote
/// itself.
pub fn quote_binds_report_data(
    evidence: &Value,
    slot: &[u8; 64],
    report_data: [u8; 32],
) -> Result<(), QuoteStepError> {
    if let Some(published) = evidence.get("quote_report_data").and_then(Value::as_str) {
        let published =
            decode_hex(published).map_err(QuoteStepError::InvalidEvidenceReportDataHex)?;
        if published.as_slice() != slot {
            return Err(QuoteStepError::EvidenceReportDataMismatch);
        }
    }
    if slot != &identity::report_data_slot(report_data) {
        return Err(QuoteStepError::ReportDataSlotMismatch {
            slot: hex::encode(slot),
        });
    }
    Ok(())
}
