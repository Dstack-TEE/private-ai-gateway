//! Typed protocol structures for ACI v1.
//!
//! Artifacts are verified as served bytes (§3): the keyset travels inside the
//! attestation report as `workload_keyset_b64` — the base64 of the exact JSON
//! bytes the service sealed once at startup — and its digest is over those
//! bytes. Nothing here is canonicalized; `serde` field order is the wire order.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- §4.1 Workload keyset ----------

/// A keyset public-key entry with a stable `key_id` selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyedPublicKey {
    pub key_id: String,
    pub algo: String,
    #[serde(rename = "public_key")]
    pub public_key_hex: String,
}

/// SPKI digest of a TLS endpoint certificate, optionally scoped to one
/// public hostname.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TlsSpki {
    #[serde(rename = "spki_sha256")]
    pub spki_sha256_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// The workload keyset — the unit of workload identity (§4.1). The hardware
/// quote binds the digest of the serialized keyset bytes; every keyset change
/// requires a fresh quote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadKeyset {
    /// Profile-interpreted name (dstack app-id URI, SPIFFE ID, DNS name).
    /// Serialized as JSON `null` when absent; never trusted without a profile.
    pub subject: Option<String>,
    /// Unix timestamp after which a verifier MUST NOT accept the keyset.
    pub not_after: u64,
    pub receipt_signing_keys: Vec<KeyedPublicKey>,
    pub e2ee_public_keys: Vec<KeyedPublicKey>,
    pub tls_public_keys: Vec<TlsSpki>,
}

// ---------- §5 Attestation report ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SourceProvenance {
    pub repo_url: Option<String>,
    pub repo_commit: Option<String>,
    pub image_digest: Option<String>,
    pub image_provenance: Option<Value>,
}

impl SourceProvenance {
    pub fn is_unknown(&self) -> bool {
        self.repo_url.is_none()
            && self.repo_commit.is_none()
            && self.image_digest.is_none()
            && self.image_provenance.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ServiceCapabilities {
    /// Client-facing ACI E2EE scheme versions the service terminates (§5.1).
    /// Only services that actually wired E2EE termination should populate this.
    pub supported_e2ee_versions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationEnvelope {
    pub tee_type: String,
    /// Base64 of the exact keyset JSON bytes (§4.1). Verifiers hash the
    /// decoded bytes; they never re-serialize.
    pub workload_keyset_b64: String,
    /// Bare hex of the 32-byte §4.2 statement digest the TEE evidence binds.
    #[serde(rename = "report_data")]
    pub report_data_hex: String,
    /// Absent on the wire only for non-conformant or development deployments;
    /// a verifier rejects reports without acceptable provenance (§5.1).
    #[serde(default, skip_serializing_if = "SourceProvenance::is_unknown")]
    pub source_provenance: SourceProvenance,
    /// TEE-type-specific evidence, interpreted by the verifier profile (§5.2).
    pub evidence: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationReport {
    pub api_version: String,
    /// Restated digest of the decoded `workload_keyset_b64` bytes; verifiers
    /// MUST recompute it (§3).
    pub workload_keyset_digest: String,
    pub attestation: AttestationEnvelope,
    pub service_capabilities: ServiceCapabilities,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_envelope(source_provenance: SourceProvenance) -> AttestationEnvelope {
        AttestationEnvelope {
            tee_type: "tdx".to_string(),
            workload_keyset_b64: "e30=".to_string(),
            report_data_hex: "00".to_string(),
            source_provenance,
            evidence: json!({}),
        }
    }

    #[test]
    fn unknown_source_provenance_is_hidden_on_the_wire() {
        let value = serde_json::to_value(minimal_envelope(SourceProvenance::default())).unwrap();
        assert!(value.get("source_provenance").is_none());
    }

    #[test]
    fn known_source_provenance_is_reported_on_the_wire() {
        let value = serde_json::to_value(minimal_envelope(SourceProvenance {
            repo_url: Some("https://github.com/Dstack-TEE/private-ai-gateway.git".to_string()),
            repo_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            image_digest: None,
            image_provenance: None,
        }))
        .unwrap();

        assert_eq!(
            value["source_provenance"]["repo_commit"],
            "0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[test]
    fn missing_source_provenance_deserializes_as_unknown() {
        let value = serde_json::to_value(minimal_envelope(SourceProvenance::default())).unwrap();
        let envelope: AttestationEnvelope = serde_json::from_value(value).unwrap();
        assert!(envelope.source_provenance.is_unknown());
    }

    #[test]
    fn keyset_serializes_subject_null_and_spec_field_order() {
        let keyset = WorkloadKeyset {
            subject: None,
            not_after: 1_790_000_000,
            receipt_signing_keys: vec![KeyedPublicKey {
                key_id: "r1".to_string(),
                algo: "ed25519".to_string(),
                public_key_hex: "aa".repeat(32),
            }],
            e2ee_public_keys: Vec::new(),
            tls_public_keys: vec![TlsSpki {
                spki_sha256_hex: "bb".repeat(32),
                domain: None,
            }],
        };
        let text = serde_json::to_string(&keyset).unwrap();
        assert!(text.starts_with(r#"{"subject":null,"not_after":1790000000,"#));
        assert!(text.contains(r#""receipt_signing_keys":[{"key_id":"r1","algo":"ed25519","#));
        // No domain member on an unscoped TLS entry.
        assert!(text.contains(&format!(
            r#""tls_public_keys":[{{"spki_sha256":"{}"}}]"#,
            "bb".repeat(32)
        )));
    }
}
