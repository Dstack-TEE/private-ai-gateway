//! ACI attestation-report binding validation (§10.1 steps 2–3).

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;

use super::decode_hex_32;
use crate::aci::digest::sha256_hex;
use crate::aci::identity;
use crate::aci::types::{AttestationReport, WorkloadKeyset};

#[derive(Debug, Clone)]
pub struct ValidatedAciReport {
    pub workload_keyset_digest: String,
    /// The keyset parsed from the exact decoded `workload_keyset_b64` bytes.
    pub keyset: WorkloadKeyset,
    pub report_data: [u8; 32],
    pub evidence: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum AciReportValidationError {
    #[error("unsupported ACI api_version: {0}")]
    UnsupportedApiVersion(String),
    #[error("invalid workload_keyset_b64: {0}")]
    InvalidKeysetBase64(String),
    #[error("workload keyset does not parse: {0}")]
    InvalidKeyset(String),
    #[error("workload_keyset_digest mismatch")]
    WorkloadKeysetDigestMismatch,
    #[error("report_data mismatch")]
    ReportDataMismatch,
    #[error("invalid report_data hex: {0}")]
    InvalidReportDataHex(String),
    #[error("invalid attestation nonce: {0}")]
    InvalidNonce(#[from] identity::InvalidNonce),
    #[error("workload keyset is expired (not_after passed)")]
    KeysetExpired,
}

/// Verify the ACI binding chain inside an attestation report (§10.1):
/// base64-decode the keyset, recompute its digest, rebuild the §4.2 statement
/// for the nonce the caller supplied, check `report_data`, and check keyset
/// expiry. It deliberately does not verify the vendor quote; provider
/// adapters compose this with their own hardware-verification step (which
/// must bind the returned `report_data`).
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

    // The keyset is verified as served bytes (§3): hash the decoded bytes,
    // never a re-serialization.
    let keyset_bytes = BASE64
        .decode(report.attestation.workload_keyset_b64.as_bytes())
        .map_err(|e| AciReportValidationError::InvalidKeysetBase64(e.to_string()))?;
    let computed_digest = identity::workload_keyset_digest(&keyset_bytes);
    if computed_digest != report.workload_keyset_digest {
        return Err(AciReportValidationError::WorkloadKeysetDigestMismatch);
    }
    let keyset: WorkloadKeyset = serde_json::from_slice(&keyset_bytes)
        .map_err(|e| AciReportValidationError::InvalidKeyset(e.to_string()))?;

    let statement = identity::attestation_statement(&computed_digest, nonce)?;
    let expected_report_data = identity::report_data(&statement);
    let reported_report_data = decode_hex_32(&report.attestation.report_data_hex)
        .map_err(AciReportValidationError::InvalidReportDataHex)?;
    if reported_report_data != expected_report_data {
        return Err(AciReportValidationError::ReportDataMismatch);
    }

    if now_secs >= keyset.not_after {
        return Err(AciReportValidationError::KeysetExpired);
    }

    Ok(ValidatedAciReport {
        workload_keyset_digest: computed_digest,
        keyset,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::identity::SealedWorkloadKeyset;
    use crate::aci::types::{AttestationEnvelope, KeyedPublicKey, SourceProvenance};

    fn sealed_keyset() -> SealedWorkloadKeyset {
        SealedWorkloadKeyset::seal(WorkloadKeyset {
            subject: None,
            not_after: 2_000_000_000,
            receipt_signing_keys: vec![KeyedPublicKey {
                key_id: "r1".to_string(),
                algo: "ed25519".to_string(),
                public_key_hex: "aa".repeat(32),
            }],
            e2ee_public_keys: Vec::new(),
            tls_public_keys: Vec::new(),
        })
        .unwrap()
    }

    fn report(nonce: Option<&str>) -> AttestationReport {
        let sealed = sealed_keyset();
        let statement = identity::attestation_statement(sealed.digest(), nonce).unwrap();
        AttestationReport {
            api_version: "aci/1".to_string(),
            workload_keyset_digest: sealed.digest().to_string(),
            attestation: AttestationEnvelope {
                tee_type: "tdx".to_string(),
                workload_keyset_b64: BASE64.encode(sealed.bytes()),
                report_data_hex: hex::encode(identity::report_data(&statement)),
                source_provenance: SourceProvenance::default(),
                evidence: serde_json::json!({}),
            },
            service_capabilities: Default::default(),
        }
    }

    #[test]
    fn accepts_a_well_bound_report() {
        let validated =
            validate_aci_report_binding(&report(Some("nonce-1")), Some("nonce-1"), 1_000, None)
                .unwrap();
        assert_eq!(validated.workload_keyset_digest, sealed_keyset().digest());
        assert_eq!(validated.keyset.receipt_signing_keys.len(), 1);
    }

    #[test]
    fn rejects_a_stale_nonce_binding() {
        // A quote over a different nonce cannot satisfy a fresh challenge.
        let err = validate_aci_report_binding(&report(Some("old")), Some("fresh"), 1_000, None)
            .unwrap_err();
        assert!(matches!(err, AciReportValidationError::ReportDataMismatch));
    }

    #[test]
    fn rejects_a_tampered_keyset_digest() {
        let mut tampered = report(None);
        tampered.workload_keyset_digest = format!("sha256:{}", "00".repeat(32));
        let err = validate_aci_report_binding(&tampered, None, 1_000, None).unwrap_err();
        assert!(matches!(
            err,
            AciReportValidationError::WorkloadKeysetDigestMismatch
        ));
    }

    #[test]
    fn rejects_an_expired_keyset() {
        let err =
            validate_aci_report_binding(&report(None), None, 2_000_000_000, None).unwrap_err();
        assert!(matches!(err, AciReportValidationError::KeysetExpired));
    }
}
