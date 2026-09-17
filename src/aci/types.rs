//! Typed protocol structures for ACI v1.
//!
//! The keyset travels inside the attestation report as a plain
//! `workload_keyset` object; its digest is the SHA-256 of the JCS form of
//! the parsed object (§3.1, Appendix A), so the served encoding is free.
//! Foreign bytes (HTTP bodies, evidence data) are hashed exactly as
//! observed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- §5.3 Serving constraints ----------

/// Members of the request body's `provider` object that state serving
/// constraints. The gateway's parser and the `aci` client both name them from
/// here: an unrecognized `aci_`-prefixed member is refused, so a rename on one
/// side alone would silently break the constraint end to end.
pub const PROVIDER_ACI_VERIFIED: &str = "aci_verified";
pub const PROVIDER_ACI_SESSION_IDS: &str = "aci_session_ids";

// ---------- §3.1 Workload keyset ----------

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

/// The workload keyset — the unit of workload identity (§3.1). The hardware
/// quote binds the digest of the serialized keyset bytes; every keyset change
/// requires a fresh quote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadKeyset {
    /// Policy-interpreted name (dstack app-id URI, SPIFFE ID, DNS name).
    /// Serialized as JSON `null` when absent; never trusted without a policy.
    pub subject: Option<String>,
    /// Unix timestamp after which a verifier MUST NOT accept the keyset.
    pub not_after: u64,
    pub receipt_signing_keys: Vec<KeyedPublicKey>,
    pub e2ee_public_keys: Vec<KeyedPublicKey>,
    /// Required only for services accepting sensitive plaintext over HTTPS
    /// (§3.1), so an E2EE-only keyset may omit the member entirely.
    #[serde(default)]
    pub tls_public_keys: Vec<TlsSpki>,
}

impl WorkloadKeyset {
    /// Whether the keyset has expired at `now_secs` (§3.1): `not_after` is
    /// the first second a verifier stops accepting it.
    ///
    /// Shared so the two verifiers cannot disagree on the boundary.
    pub fn is_expired_at(&self, now_secs: u64) -> bool {
        now_secs >= self.not_after
    }

    /// The TLS entries that apply to `host`, per §3.1: an entry without a
    /// `domain` is unrestricted, and one with a `domain` applies only to that
    /// hostname. Comparison is case-insensitive and ignores a trailing dot.
    ///
    /// Both verifiers select candidates through this so they cannot disagree
    /// about which attested SPKI a hostname may present.
    pub fn tls_keys_for_host(&self, host: &str) -> Vec<&TlsSpki> {
        let host = host.trim().trim_end_matches('.');
        self.tls_public_keys
            .iter()
            .filter(|key| match key.domain.as_deref() {
                Some(domain) => domain
                    .trim()
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(host),
                None => true,
            })
            .collect()
    }
}

// ---------- §4 Attestation report ----------

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceCapabilities {
    /// Client-facing E2EE extension versions the service terminates (ACI §4.1).
    /// Only services that actually wired E2EE termination should populate this.
    pub supported_e2ee_versions: Vec<String>,
    /// `"direct"` when inference runs inside this attested workload, so there
    /// is no upstream hop: receipts carry no `upstream.verified` event and no
    /// attested sessions exist. `"aggregator"` for an aggregator (§1.2).
    #[serde(default = "default_serving")]
    pub serving: String,
}

fn default_serving() -> String {
    "aggregator".to_string()
}

impl Default for ServiceCapabilities {
    fn default() -> Self {
        Self {
            supported_e2ee_versions: Vec::new(),
            serving: default_serving(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationEnvelope {
    pub tee_type: String,
    /// The keyset as a plain JSON object (§3.1). Verifiers canonicalize the
    /// parsed value and hash its JCS form (Appendix A).
    pub workload_keyset: Value,
    /// Bare hex of the 32-byte §3.2 statement digest the TEE evidence binds.
    #[serde(rename = "report_data")]
    pub report_data_hex: String,
    /// Absent on the wire only for non-conformant or development deployments;
    /// a verifier rejects reports without acceptable provenance (§4.1).
    #[serde(default, skip_serializing_if = "SourceProvenance::is_unknown")]
    pub source_provenance: SourceProvenance,
    /// TEE-type-specific evidence, interpreted by the verifier policy (§4.2).
    /// Absent evidence is an id-1 failure, not a parse error.
    #[serde(default)]
    pub evidence: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationReport {
    pub api_version: String,
    /// Restated digest of the embedded `workload_keyset` (JCS form);
    /// verifiers MUST recompute it (Appendix A).
    pub workload_keyset_digest: String,
    pub attestation: AttestationEnvelope,
    /// Appendix B: an unknown or absent `serving` value is treated as
    /// `"aggregator"`, so upstream-1 (which reads it) stays decidable.
    #[serde(default)]
    pub service_capabilities: ServiceCapabilities,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_envelope(source_provenance: SourceProvenance) -> AttestationEnvelope {
        AttestationEnvelope {
            tee_type: "tdx".to_string(),
            workload_keyset: json!({}),
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

    #[test]
    fn tls_candidates_follow_the_domain_scoping_rule() {
        let keyset = WorkloadKeyset {
            subject: None,
            not_after: 0,
            receipt_signing_keys: vec![],
            e2ee_public_keys: vec![],
            tls_public_keys: vec![
                TlsSpki {
                    spki_sha256_hex: "aa".repeat(32),
                    domain: None,
                },
                TlsSpki {
                    spki_sha256_hex: "bb".repeat(32),
                    domain: Some("API.example.com.".to_string()),
                },
                TlsSpki {
                    spki_sha256_hex: "cc".repeat(32),
                    domain: Some("other.example.com".to_string()),
                },
            ],
        };
        let spkis = |host| {
            keyset
                .tls_keys_for_host(host)
                .iter()
                .map(|k| k.spki_sha256_hex.clone())
                .collect::<Vec<_>>()
        };
        // §3.1: the unscoped entry is unrestricted, so it stays a candidate
        // even alongside domain-scoped siblings; matching ignores case and a
        // trailing dot.
        assert_eq!(
            spkis("api.example.com"),
            vec!["aa".repeat(32), "bb".repeat(32)]
        );
        assert_eq!(
            spkis("other.example.com"),
            vec!["aa".repeat(32), "cc".repeat(32)]
        );
        assert_eq!(spkis("unlisted.example.com"), vec!["aa".repeat(32)]);
    }
}
