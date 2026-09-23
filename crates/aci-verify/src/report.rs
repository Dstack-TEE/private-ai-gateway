//! ACI attestation-report binding validation (§9.1 steps 2–3).

use aci_protocol::digest::sha256_hex;
use aci_protocol::identity;
use aci_protocol::types::{AttestationReport, WorkloadKeyset};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;

use crate::decode_hex_32;

#[derive(Debug, Clone)]
pub struct ValidatedAciReport {
    pub workload_keyset_digest: String,
    /// The keyset parsed from the served `workload_keyset` object.
    pub keyset: WorkloadKeyset,
    pub report_data: [u8; 32],
    pub evidence: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum AciReportValidationError {
    #[error("unsupported ACI api_version: {0}")]
    UnsupportedApiVersion(String),
    #[error("workload keyset does not parse: {0}")]
    InvalidKeyset(String),
    #[error("sha256 over the keyset JCS form is {computed}, report claims {claimed}")]
    WorkloadKeysetDigestMismatch { computed: String, claimed: String },
    #[error("statement digest is {computed}, report carries {reported}")]
    ReportDataMismatch { computed: String, reported: String },
    #[error("invalid report_data hex: {0}")]
    InvalidReportDataHex(String),
    #[error("invalid attestation nonce: {0}")]
    InvalidNonce(#[from] identity::InvalidStatementInput),
    #[error("workload keyset is expired (not_after passed)")]
    KeysetExpired,
}

/// The §9.1(2) binding chain's intermediate values.
#[derive(Debug, Clone)]
pub struct ReportBinding {
    pub keyset_digest: String,
    pub keyset: WorkloadKeyset,
    pub report_data: [u8; 32],
    /// The §3.2 statement bytes the digest was taken over.
    pub statement: Vec<u8>,
    /// The keyset's JCS form, the digest preimage.
    pub keyset_jcs: Vec<u8>,
}

/// Recompute the §9.1(2) chain: keyset JCS -> digest -> §3.2 statement ->
/// `report_data`. This checks neither the vendor quote nor keyset expiry.
pub fn verify_report_binding(
    report: &AttestationReport,
    nonce: Option<&str>,
) -> Result<ReportBinding, AciReportValidationError> {
    // Canonicalize exactly what was parsed, including unknown members (§3.1).
    let keyset_jcs = aci_protocol::digest::jcs_bytes(&report.attestation.workload_keyset)
        .map_err(|error| AciReportValidationError::InvalidKeyset(error.to_string()))?;
    let keyset_digest = format!(
        "sha256:{}",
        hex::encode(aci_protocol::digest::sha256_raw(&keyset_jcs))
    );
    if keyset_digest != report.workload_keyset_digest {
        return Err(AciReportValidationError::WorkloadKeysetDigestMismatch {
            computed: keyset_digest,
            claimed: report.workload_keyset_digest.clone(),
        });
    }
    let keyset: WorkloadKeyset = serde_json::from_value(report.attestation.workload_keyset.clone())
        .map_err(|error| AciReportValidationError::InvalidKeyset(error.to_string()))?;

    let statement = identity::attestation_statement(&keyset_digest, nonce)?;
    let report_data = identity::report_data(&statement);
    let reported = decode_hex_32(&report.attestation.report_data_hex)
        .map_err(AciReportValidationError::InvalidReportDataHex)?;
    if reported != report_data {
        return Err(AciReportValidationError::ReportDataMismatch {
            computed: hex::encode(report_data),
            reported: hex::encode(reported),
        });
    }

    Ok(ReportBinding {
        keyset_digest,
        keyset,
        report_data,
        statement,
        keyset_jcs,
    })
}

/// Verify the ACI binding chain and keyset expiry. Vendor quote verification
/// remains a separate mechanism composed by the consumer's appraisal policy.
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

    let ReportBinding {
        keyset_digest,
        keyset,
        report_data,
        ..
    } = verify_report_binding(report, nonce)?;

    if keyset.is_expired_at(now_secs) {
        return Err(AciReportValidationError::KeysetExpired);
    }

    // §3.1: key material reused across receipt and E2EE roles collapses the
    // domains and is invalid.
    for signing in &keyset.receipt_signing_keys {
        if keyset
            .e2ee_public_keys
            .iter()
            .any(|e2ee| e2ee.public_key_hex == signing.public_key_hex)
        {
            return Err(AciReportValidationError::InvalidKeyset(format!(
                "key {} is listed for both the receipt and E2EE roles (§3.1)",
                signing.key_id
            )));
        }
    }

    Ok(ValidatedAciReport {
        workload_keyset_digest: keyset_digest,
        keyset,
        report_data,
        evidence: raw_report_body.map(|body| raw_evidence(body, "application/json", None)),
    })
}

pub fn raw_evidence(data: &[u8], content_type: &str, source_url: Option<&str>) -> Value {
    let mut evidence = serde_json::json!({
        "digest": sha256_hex(data),
        "data": format!("data:{content_type};base64,{}", BASE64.encode(data)),
    });
    if let Some(source_url) = source_url {
        evidence["source_url"] = Value::String(source_url.to_string());
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use aci_protocol::types::{AttestationEnvelope, SourceProvenance};

    fn keyset() -> Value {
        serde_json::json!({
            "subject": null,
            "not_after": 2_000_000_000u64,
            "receipt_signing_keys": [{
                "key_id": "r1",
                "algo": "ed25519",
                "public_key": "aa".repeat(32),
            }],
            "e2ee_public_keys": [],
            "tls_public_keys": [],
        })
    }

    fn report(nonce: Option<&str>) -> AttestationReport {
        let keyset = keyset();
        let digest = identity::workload_keyset_digest(&keyset).unwrap();
        let statement = identity::attestation_statement(&digest, nonce).unwrap();
        AttestationReport {
            api_version: "aci/1".to_string(),
            workload_keyset_digest: digest,
            attestation: AttestationEnvelope {
                tee_type: "tdx".to_string(),
                workload_keyset: keyset,
                report_data_hex: hex::encode(identity::report_data(&statement)),
                source_provenance: SourceProvenance::default(),
                evidence: serde_json::json!({}),
            },
            service_capabilities: Default::default(),
        }
    }

    #[test]
    fn accepts_a_well_bound_report() {
        let nonce = "1a".repeat(32);
        let validated =
            validate_aci_report_binding(&report(Some(&nonce)), Some(&nonce), 1_000, None).unwrap();
        assert_eq!(
            validated.workload_keyset_digest,
            identity::workload_keyset_digest(&keyset()).unwrap()
        );
        assert_eq!(validated.keyset.receipt_signing_keys.len(), 1);
    }

    #[test]
    fn rejects_a_stale_nonce_binding() {
        let stale = "0d".repeat(32);
        let fresh = "9e".repeat(32);
        let error = validate_aci_report_binding(&report(Some(&stale)), Some(&fresh), 1_000, None)
            .unwrap_err();
        assert!(matches!(
            error,
            AciReportValidationError::ReportDataMismatch { .. }
        ));
    }

    #[test]
    fn rejects_a_tampered_keyset_digest() {
        let mut tampered = report(None);
        tampered.workload_keyset_digest = format!("sha256:{}", "00".repeat(32));
        let error = validate_aci_report_binding(&tampered, None, 1_000, None).unwrap_err();
        assert!(matches!(
            error,
            AciReportValidationError::WorkloadKeysetDigestMismatch { .. }
        ));
    }

    #[test]
    fn rejects_an_expired_keyset() {
        let error =
            validate_aci_report_binding(&report(None), None, 2_000_000_000, None).unwrap_err();
        assert!(matches!(error, AciReportValidationError::KeysetExpired));
    }
}
