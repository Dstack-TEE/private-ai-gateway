//! The §9.1(1) quote steps that need this gateway's DCAP library.
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
    pub report: dcap_qvl::quote::Report,
    pub advisory_ids: Vec<String>,
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
    let collateral = dcap_qvl::collateral::get_collateral(pccs_url, raw_quote)
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
        report: verified.report,
        advisory_ids: verified.advisory_ids,
        tee_type,
    })
}

/// Stage 2's complete TD10/TD15 security projection, excluding REPORTDATA.
pub(super) fn tdx_session_material(report: &dcap_qvl::quote::Report) -> Option<Value> {
    use dcap_qvl::quote::Report;
    let (base, format) = match report {
        Report::TD10(base) => (base, "td10"),
        Report::TD15(report) => (&report.base, "td15"),
        _ => return None,
    };
    let mut value = serde_json::json!({
        "report_format": format,
        "tee_tcb_svn": hex::encode(base.tee_tcb_svn),
        "mr_seam": hex::encode(base.mr_seam),
        "mr_signer_seam": hex::encode(base.mr_signer_seam),
        "seam_attributes": hex::encode(base.seam_attributes),
        "td_attributes": hex::encode(base.td_attributes),
        "xfam": hex::encode(base.xfam),
        "mr_td": hex::encode(base.mr_td),
        "mr_config_id": hex::encode(base.mr_config_id),
        "mr_owner": hex::encode(base.mr_owner),
        "mr_owner_config": hex::encode(base.mr_owner_config),
        "rt_mr0": hex::encode(base.rt_mr0),
        "rt_mr1": hex::encode(base.rt_mr1),
        "rt_mr2": hex::encode(base.rt_mr2),
        "rt_mr3": hex::encode(base.rt_mr3),
    });
    if let Report::TD15(report) = report {
        value["tee_tcb_svn2"] = hex::encode(report.tee_tcb_svn2).into();
        value["mr_service_td"] = hex::encode(report.mr_service_td).into();
    }
    Some(value)
}
