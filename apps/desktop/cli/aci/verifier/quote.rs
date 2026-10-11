//! The §9.1(1) quote steps that need this client's DCAP library.
//!
//! The evidence handling and the report-data comparison come from
//! `aci-verify`; parsing and verifying the quote to its vendor root happen
//! here.

use aci_verify::quote::quote_bytes;
pub use aci_verify::quote::QuoteStepError;
use dcap_qvl::quote::Quote;
use serde_json::Value;

pub(super) struct VerifiedQuote {
    /// The collateral's TCB status, for the caller to appraise (§8.3).
    pub status: String,
    pub tee_type: &'static str,
}

pub(super) fn parse_quote_evidence(evidence: &Value) -> Result<(Vec<u8>, Quote), QuoteStepError> {
    let raw = quote_bytes(evidence)?;
    let quote = Quote::parse(&raw).map_err(|e| QuoteStepError::UnparsableQuote(e.to_string()))?;
    Ok((raw, quote))
}

/// Verify the quote to its vendor root and confirm the report's claimed
/// `tee_type` is the one the quote actually carries.
pub(super) async fn verify_quote_to_root(
    raw_quote: &[u8],
    pccs_url: &str,
    now_secs: u64,
    claimed_tee_type: &str,
) -> Result<VerifiedQuote, QuoteStepError> {
    let collateral = dcap_qvl::collateral::CollateralClient::with_default_http(pccs_url)
        .map_err(|e| QuoteStepError::Collateral {
            url: pccs_url.to_string(),
            reason: e.to_string(),
        })?
        .fetch(raw_quote)
        .await
        .map_err(|e| QuoteStepError::Collateral {
            url: pccs_url.to_string(),
            reason: e.to_string(),
        })?;
    let verified = dcap_qvl::verify::rustcrypto::verify(raw_quote, &collateral, now_secs)
        .map_err(|e| QuoteStepError::Verification(e.to_string()))?;
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
