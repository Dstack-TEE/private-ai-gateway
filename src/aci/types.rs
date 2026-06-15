//! Typed protocol structures for ACI v1.
//!
//! Each struct serialises through `serde` to a wire shape that
//! matches the ACI draft. Digest computations in [`super::identity`]
//! and signature coverage in [`super::receipt`] go through
//! [`to_canonical_value`], which converts the typed structure to a
//! `serde_json::Value` tree with the exact field order/contents the
//! protocol requires. That tree is then canonicalised by
//! [`super::canonical`].
//!
//! Keeping the canonical projection separate from the wire `Serialize`
//! impl lets `serde` produce additional non-protocol fields in the
//! future (debug-only annotations, etc.) without changing the digest
//! input.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ---------- §4.1 Workload identity and keyset ----------

/// A keyset entry that names a public key by its raw bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicKeyMaterial {
    pub algo: String,
    #[serde(rename = "public_key")]
    pub public_key_hex: String,
}

impl PublicKeyMaterial {
    pub fn to_canonical_value(&self) -> Value {
        json!({
            "algo": self.algo,
            "public_key": self.public_key_hex,
        })
    }
}

/// A public key with a stable `key_id` for receipt/E2EE selectors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyedPublicKey {
    pub key_id: String,
    pub algo: String,
    #[serde(rename = "public_key")]
    pub public_key_hex: String,
}

impl KeyedPublicKey {
    pub fn to_canonical_value(&self) -> Value {
        json!({
            "key_id": self.key_id,
            "algo": self.algo,
            "public_key": self.public_key_hex,
        })
    }
}

/// SPKI digest of a TLS endpoint certificate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TlsSpki {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(rename = "spki_sha256")]
    pub spki_sha256_hex: String,
}

impl TlsSpki {
    pub fn to_canonical_value(&self) -> Value {
        let mut value = json!({ "spki_sha256": self.spki_sha256_hex });
        if let (Some(domain), Some(obj)) = (&self.domain, value.as_object_mut()) {
            obj.insert("domain".to_string(), Value::String(domain.clone()));
        }
        value
    }
}

/// Stable identity public key plus optional profile-interpreted subject.
///
/// `workload_id` covers only `public_key`; `subject` is included in
/// `workload_keyset_digest` (because it lives at the top of the
/// keyset), but a verifier MUST NOT trust it without a profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadIdentity {
    pub public_key: PublicKeyMaterial,
    /// `Option<String>` becomes JSON `null` on the wire when `None`.
    pub subject: Option<String>,
}

impl WorkloadIdentity {
    pub fn to_canonical_value(&self) -> Value {
        json!({
            "public_key": self.public_key.to_canonical_value(),
            "subject": self.subject,
        })
    }
}

/// Monotonic version plus expiry for one keyset binding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeysetEpoch {
    pub version: u64,
    pub not_after: u64,
}

impl KeysetEpoch {
    pub fn to_canonical_value(&self) -> Value {
        json!({ "version": self.version, "not_after": self.not_after })
    }
}

/// The full canonical keyset bound by `workload_keyset_digest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadKeyset {
    pub workload_identity: WorkloadIdentity,
    pub keyset_epoch: KeysetEpoch,
    pub receipt_signing_keys: Vec<KeyedPublicKey>,
    pub e2ee_public_keys: Vec<KeyedPublicKey>,
    pub tls_public_keys: Vec<TlsSpki>,
}

impl WorkloadKeyset {
    pub fn to_canonical_value(&self) -> Value {
        json!({
            "workload_identity": self.workload_identity.to_canonical_value(),
            "keyset_epoch": self.keyset_epoch.to_canonical_value(),
            "receipt_signing_keys": self
                .receipt_signing_keys
                .iter()
                .map(KeyedPublicKey::to_canonical_value)
                .collect::<Vec<_>>(),
            "e2ee_public_keys": self
                .e2ee_public_keys
                .iter()
                .map(KeyedPublicKey::to_canonical_value)
                .collect::<Vec<_>>(),
            "tls_public_keys": self
                .tls_public_keys
                .iter()
                .map(TlsSpki::to_canonical_value)
                .collect::<Vec<_>>(),
        })
    }
}

// ---------- §4.2 Attestation statement ----------

pub const REPORT_DATA_PURPOSE: &str = "aci.report_data.v1";
pub const KEYSET_ENDORSEMENT_PURPOSE: &str = "aci.keyset.endorsement.v1";

/// The named report-data payload that the TEE quote MUST cover.
///
/// `nonce` is the URL-decoded UTF-8 value of the `nonce` query
/// parameter, or `None` if the parameter was omitted. `None` here
/// becomes a JSON `null` in canonicalisation, never the string
/// `"null"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationStatement {
    pub workload_id: String,
    pub workload_keyset_digest: String,
    pub nonce: Option<String>,
}

impl AttestationStatement {
    pub fn to_canonical_value(&self) -> Value {
        json!({
            "purpose": REPORT_DATA_PURPOSE,
            "workload_id": self.workload_id,
            "workload_keyset_digest": self.workload_keyset_digest,
            "nonce": self.nonce,
        })
    }
}

/// The named payload signed by the workload identity key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeysetEndorsementPayload {
    pub workload_keyset_digest: String,
}

impl KeysetEndorsementPayload {
    pub fn to_canonical_value(&self) -> Value {
        json!({
            "purpose": KEYSET_ENDORSEMENT_PURPOSE,
            "workload_keyset_digest": self.workload_keyset_digest,
        })
    }
}

/// `keyset_endorsement` field in the attestation envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeysetEndorsement {
    pub algo: String,
    #[serde(rename = "value")]
    pub value_hex: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Freshness {
    pub fetched_at: u64,
    pub stale_after: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ServiceCapabilities {
    /// Defaults to empty. Only services that have actually wired
    /// client-facing ACI E2EE termination should populate this;
    /// advertising a version the workload cannot decrypt would
    /// mislead verifiers about the trust surface.
    pub supported_e2ee_versions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationEnvelope {
    pub vendor: String,
    pub tee_type: String,
    pub workload_keyset: WorkloadKeyset,
    #[serde(rename = "report_data")]
    pub report_data_hex: String,
    pub keyset_endorsement: KeysetEndorsement,
    #[serde(default, skip_serializing_if = "SourceProvenance::is_unknown")]
    pub source_provenance: SourceProvenance,
    pub freshness: Freshness,
    pub evidence: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationReport {
    pub api_version: String,
    pub workload_id: String,
    pub workload_keyset_digest: String,
    pub attestation: AttestationEnvelope,
    pub service_capabilities: ServiceCapabilities,
}

// ---------- §9 Receipts ----------

/// One signed event in the per-request event log.
///
/// The wire shape flattens `seq` and `type` to the top of the JSON
/// object plus any number of type-specific `fields`. The canonical
/// value rebuilds that flat object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptEvent {
    pub seq: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    /// Free-form per-type fields; canonicalised verbatim into the
    /// same JSON object as `seq` and `type`. Field collisions with
    /// `seq` / `type` are rejected at insertion time.
    pub fields: Value,
}

impl ReceiptEvent {
    /// Flatten `seq` and `type` into the same JSON object as `fields`.
    pub fn to_canonical_value(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("seq".to_string(), Value::from(self.seq));
        obj.insert("type".to_string(), Value::from(self.event_type.clone()));
        if let Value::Object(map) = &self.fields {
            for (k, v) in map.iter() {
                obj.insert(k.clone(), v.clone());
            }
        }
        Value::Object(obj)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptSignature {
    pub algo: String,
    pub key_id: String,
    /// Signature value as hex. `to_canonical_for_signing` strips this
    /// when computing the bytes that the signature itself covers
    /// (ACI §9.4: "JCS of the whole receipt with only signature.value
    /// omitted").
    #[serde(rename = "value")]
    pub value_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub api_version: String,
    pub receipt_id: String,
    pub chat_id: Option<String>,
    pub workload_id: String,
    pub workload_keyset_digest: String,
    pub endpoint: String,
    pub method: String,
    pub served_at: u64,
    pub event_log: Vec<ReceiptEvent>,
    pub signature: ReceiptSignature,
}

impl Receipt {
    /// Build the canonical JSON tree used for digest / display.
    pub fn to_canonical_value(&self, include_signature_value: bool) -> Value {
        let sig = if include_signature_value {
            json!({
                "algo": self.signature.algo,
                "key_id": self.signature.key_id,
                "value": self.signature.value_hex,
            })
        } else {
            json!({
                "algo": self.signature.algo,
                "key_id": self.signature.key_id,
            })
        };
        json!({
            "api_version": self.api_version,
            "receipt_id": self.receipt_id,
            "chat_id": self.chat_id,
            "workload_id": self.workload_id,
            "workload_keyset_digest": self.workload_keyset_digest,
            "endpoint": self.endpoint,
            "method": self.method,
            "served_at": self.served_at,
            "event_log": self.event_log.iter().map(ReceiptEvent::to_canonical_value).collect::<Vec<_>>(),
            "signature": sig,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttestationEnvelope, Freshness, KeysetEndorsement, KeysetEpoch, PublicKeyMaterial,
        SourceProvenance, WorkloadIdentity, WorkloadKeyset,
    };
    use serde_json::json;

    fn minimal_envelope(source_provenance: SourceProvenance) -> AttestationEnvelope {
        AttestationEnvelope {
            vendor: "test".to_string(),
            tee_type: "tdx".to_string(),
            workload_keyset: WorkloadKeyset {
                workload_identity: WorkloadIdentity {
                    public_key: PublicKeyMaterial {
                        algo: "test".to_string(),
                        public_key_hex: "00".to_string(),
                    },
                    subject: None,
                },
                keyset_epoch: KeysetEpoch {
                    version: 1,
                    not_after: u64::MAX,
                },
                receipt_signing_keys: Vec::new(),
                e2ee_public_keys: Vec::new(),
                tls_public_keys: Vec::new(),
            },
            report_data_hex: "00".to_string(),
            keyset_endorsement: KeysetEndorsement {
                algo: "test".to_string(),
                value_hex: "00".to_string(),
            },
            source_provenance,
            freshness: Freshness {
                fetched_at: 0,
                stale_after: u64::MAX,
            },
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
}
