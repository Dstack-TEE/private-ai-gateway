//! ACI attestation-report binding validation (§9.1 steps 2–3), from
//! `aci-verify`. The tests check it against this gateway's own report
//! producer.

pub use aci_verify::report::{
    validate_aci_report_binding, AciReportValidationError, ReportBinding, ValidatedAciReport,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::identity::{self, SealedWorkloadKeyset};
    use crate::aci::types::{
        AttestationEnvelope, AttestationReport, KeyedPublicKey, SourceProvenance, WorkloadKeyset,
    };

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
                workload_keyset: sealed.to_value(),
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
        assert_eq!(validated.workload_keyset_digest, sealed_keyset().digest());
        assert_eq!(validated.keyset.receipt_signing_keys.len(), 1);
    }

    #[test]
    fn rejects_a_stale_nonce_binding() {
        // A quote over a different nonce cannot satisfy a fresh challenge.
        let stale = "0d".repeat(32);
        let fresh = "9e".repeat(32);
        let err = validate_aci_report_binding(&report(Some(&stale)), Some(&fresh), 1_000, None)
            .unwrap_err();
        assert!(matches!(
            err,
            AciReportValidationError::ReportDataMismatch { .. }
        ));
    }

    #[test]
    fn rejects_a_tampered_keyset_digest() {
        let mut tampered = report(None);
        tampered.workload_keyset_digest = format!("sha256:{}", "00".repeat(32));
        let err = validate_aci_report_binding(&tampered, None, 1_000, None).unwrap_err();
        assert!(matches!(
            err,
            AciReportValidationError::WorkloadKeysetDigestMismatch { .. }
        ));
    }

    #[test]
    fn rejects_an_expired_keyset() {
        let err =
            validate_aci_report_binding(&report(None), None, 2_000_000_000, None).unwrap_err();
        assert!(matches!(err, AciReportValidationError::KeysetExpired));
    }
}
