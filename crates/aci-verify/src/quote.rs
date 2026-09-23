//! DCAP quote verification steps for ACI §9.1(1).

use aci_protocol::identity;
use dcap_qvl::quote::{Quote, Report};
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

pub struct VerifiedQuote {
    /// The collateral's TCB status, for the caller to appraise (§8.3).
    pub status: String,
    pub tee_type: &'static str,
}

pub fn parse_quote_evidence(evidence: &Value) -> Result<(Vec<u8>, Quote), QuoteStepError> {
    let quote_hex = evidence
        .get("quote")
        .and_then(Value::as_str)
        .ok_or(QuoteStepError::MissingQuote)?;
    let raw = decode_hex(quote_hex).map_err(QuoteStepError::InvalidQuoteHex)?;
    let quote =
        Quote::parse(&raw).map_err(|error| QuoteStepError::UnparsableQuote(error.to_string()))?;
    Ok((raw, quote))
}

/// Check the quote's 64-byte report-data slot carries `report_data`, and that
/// `evidence.quote_report_data`, when published, agrees with the quote itself.
pub fn quote_binds_report_data(
    evidence: &Value,
    quote_report: &Report,
    report_data: [u8; 32],
) -> Result<(), QuoteStepError> {
    let slot = dcap_report_data(quote_report);
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

/// Verify the quote to its vendor root and confirm the report's claimed
/// `tee_type` is the one the quote actually carries.
pub async fn verify_quote_to_root(
    raw_quote: &[u8],
    pccs_url: &str,
    now_secs: u64,
    claimed_tee_type: &str,
) -> Result<VerifiedQuote, QuoteStepError> {
    let collateral = dcap_qvl::collateral::get_collateral(pccs_url, raw_quote)
        .await
        .map_err(|error| QuoteStepError::Collateral {
            url: pccs_url.to_string(),
            reason: error.to_string(),
        })?;
    let verified = dcap_qvl::verify::rustcrypto::verify(raw_quote, &collateral, now_secs)
        .map_err(|error| QuoteStepError::Verification(error.to_string()))?;
    let tee_type = if verified.report.is_sgx() {
        "sgx"
    } else {
        "tdx"
    };
    if claimed_tee_type != tee_type {
        return Err(QuoteStepError::TeeTypeMismatch {
            reported: claimed_tee_type.to_string(),
            verified: tee_type,
        });
    }
    Ok(VerifiedQuote {
        status: verified.status,
        tee_type,
    })
}

pub fn dcap_report_data(report: &Report) -> &[u8; 64] {
    match report {
        Report::SgxEnclave(report) => &report.report_data,
        Report::TD10(report) => &report.report_data,
        Report::TD15(report) => &report.base.report_data,
    }
}
