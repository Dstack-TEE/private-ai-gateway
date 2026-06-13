//! ACI attestation-report binding validation.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;

use super::{decode_hex, decode_hex_32};
use crate::aci::canonical::{sha256_hex, CanonicalError};
use crate::aci::identity;
use crate::aci::keys::verify_keyset_endorsement;
use crate::aci::types::AttestationReport;

#[derive(Debug, Clone)]
pub struct ValidatedAciReport {
    pub workload_id: String,
    pub workload_keyset_digest: String,
    pub report_data: [u8; 32],
    pub evidence: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum AciReportValidationError {
    #[error("unsupported ACI api_version: {0}")]
    UnsupportedApiVersion(String),
    #[error("workload_id mismatch")]
    WorkloadIdMismatch,
    #[error("workload_keyset_digest mismatch")]
    WorkloadKeysetDigestMismatch,
    #[error("report_data mismatch")]
    ReportDataMismatch,
    #[error("invalid report_data hex: {0}")]
    InvalidReportDataHex(String),
    #[error("keyset_endorsement algo does not match workload identity algo")]
    KeysetEndorsementAlgoMismatch,
    #[error("invalid keyset_endorsement signature hex: {0}")]
    InvalidKeysetEndorsementHex(String),
    #[error("keyset_endorsement signature verification failed")]
    KeysetEndorsementInvalid,
    #[error("attestation report is not fresh at verifier time")]
    StaleReport,
    #[error("canonicalisation error: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Verify the ACI-level identity binding inside an attestation report.
///
/// This checks the workload id, keyset digest, nonce-bound report_data,
/// identity-key endorsement, and freshness. It deliberately does not
/// verify the vendor quote; provider adapters compose this with their
/// own hardware-verification step.
pub fn validate_aci_report_binding(
    report: &AttestationReport,
    nonce: Option<&str>,
    now_secs: u64,
    raw_report_body: Option<&[u8]>,
) -> Result<ValidatedAciReport, AciReportValidationError> {
    if report.api_version != "aci/1" {
        return Err(AciReportValidationError::UnsupportedApiVersion(
            report.api_version.clone(),
        ));
    }

    let computed_workload_id =
        identity::workload_id(&report.attestation.workload_keyset.workload_identity)?;
    if computed_workload_id != report.workload_id {
        return Err(AciReportValidationError::WorkloadIdMismatch);
    }

    let computed_keyset_digest =
        identity::workload_keyset_digest(&report.attestation.workload_keyset)?;
    if computed_keyset_digest != report.workload_keyset_digest {
        return Err(AciReportValidationError::WorkloadKeysetDigestMismatch);
    }

    let statement = identity::attestation_statement(
        &report.attestation.workload_keyset,
        nonce.map(str::to_string),
    )?;
    let expected_report_data = identity::report_data(&statement)?;
    let reported_report_data = decode_hex_32(&report.attestation.report_data_hex)
        .map_err(AciReportValidationError::InvalidReportDataHex)?;
    if reported_report_data != expected_report_data {
        return Err(AciReportValidationError::ReportDataMismatch);
    }

    let identity_key = &report
        .attestation
        .workload_keyset
        .workload_identity
        .public_key;
    if report.attestation.keyset_endorsement.algo != identity_key.algo {
        return Err(AciReportValidationError::KeysetEndorsementAlgoMismatch);
    }
    let endorsement_payload =
        identity::keyset_endorsement_payload(&report.attestation.workload_keyset)?;
    let endorsement_sig = decode_hex(&report.attestation.keyset_endorsement.value_hex)
        .map_err(AciReportValidationError::InvalidKeysetEndorsementHex)?;
    if !verify_keyset_endorsement(identity_key, &endorsement_payload, &endorsement_sig) {
        return Err(AciReportValidationError::KeysetEndorsementInvalid);
    }

    let freshness = &report.attestation.freshness;
    if now_secs < freshness.fetched_at || now_secs >= freshness.stale_after {
        return Err(AciReportValidationError::StaleReport);
    }

    Ok(ValidatedAciReport {
        workload_id: report.workload_id.clone(),
        workload_keyset_digest: report.workload_keyset_digest.clone(),
        report_data: expected_report_data,
        evidence: raw_report_body.map(|body| raw_evidence(body, "application/json", None)),
    })
}

fn raw_evidence(data: &[u8], content_type: &str, source_url: Option<&str>) -> Value {
    let mut evidence = serde_json::json!({
        "digest": sha256_hex(data),
        "data": format!("data:{content_type};base64,{}", BASE64.encode(data)),
    });
    if let Some(source_url) = source_url {
        evidence["source_url"] = Value::String(source_url.to_string());
    }
    evidence
}
