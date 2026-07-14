//! Immutable, content-addressed attested-session records (ACI §9).
//!
//! An *attested session* records **one** verified upstream TEE channel for
//! one validity period: identity, enforceable channel binding, typed claims,
//! and evidence. The served bytes are the artifact — the document is
//! serialized exactly once when sealed, the store keeps those bytes, and
//!
//! ```text
//! session_id = "sha256:" || hex(sha256(exact served session document bytes))
//! ```
//!
//! The id is not inside the document; the signed receipt commits to it, so a
//! relying party recomputes the hash of the fetched bytes to prove the record
//! is exactly what the receipt cited. Sessions are never updated in place —
//! re-verification produces a new document, period, and id.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::aci::digest;
use crate::aci::receipt::ChannelBinding;

/// `api_version` stamped on session documents — `aci/1`, uniform with the
/// rest of the ACI surface.
pub const SESSION_API_VERSION: &str = "aci/1";

/// Tri-state truth value for a claim. Missing evidence is [`ClaimStatus::Unknown`]
/// — transparency, never a silent pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Asserted,
    Refuted,
    #[default]
    Unknown,
}

/// Who vouches for a claim — sets its assurance level honestly. A
/// hardware-proven TCB status and an operator-asserted weight provenance must
/// never look alike in the audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSource {
    /// Derived from the verified quote/collateral itself (e.g. TDX `TcbStatus`).
    HardwareProven,
    /// Computed by the verifier from verified evidence.
    VerifierDerived,
    /// Published by the provider but not independently proven by the gateway.
    ProviderAsserted,
    /// Declared by the gateway operator.
    OperatorAsserted,
}

/// One claim about a verified workload, as asserted by a verifier. `source` and
/// `reason` are populated only when the claim is [`ClaimStatus::Asserted`] or
/// [`ClaimStatus::Refuted`]; an `Unknown` claim carries neither (§9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub status: ClaimStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ClaimSource>,
    /// The verifier's plain reason, e.g. "matches hard-coded known measurements".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Default for Claim {
    fn default() -> Self {
        Self::unknown()
    }
}

impl Claim {
    /// An unknown claim: no evidence either way.
    pub fn unknown() -> Self {
        Self {
            status: ClaimStatus::Unknown,
            source: None,
            reason: None,
        }
    }

    pub fn asserted(source: ClaimSource, reason: impl Into<String>) -> Self {
        Self {
            status: ClaimStatus::Asserted,
            source: Some(source),
            reason: Some(reason.into()),
        }
    }

    pub fn refuted(source: ClaimSource, reason: impl Into<String>) -> Self {
        Self {
            status: ClaimStatus::Refuted,
            source: Some(source),
            reason: Some(reason.into()),
        }
    }
}

/// The §9.3 typed claim vocabulary. Every field defaults to
/// [`Claim::unknown`]; `extra` carries the raw provider facts verbatim — its
/// key names are a stable contract for a given verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionClaims {
    /// A genuine CPU TEE, with the recorded identity bound to the channel.
    pub tee_attested: Claim,
    /// The provider's NVIDIA confidential-computing GPU attestation, when
    /// verified and nonce-bound. Attests a genuine CC GPU, not (on its own)
    /// that GPU's binding to the serving CPU TEE.
    pub gpu_attested: Claim,
    /// Platform TCB freshness (TDX/SGX `TcbStatus`, SEV reported TCB).
    pub tcb_up_to_date: Claim,
    /// Platform/OS provenance (guest OS, kernel, firmware).
    pub os_known_good: Claim,
    /// Serving-software provenance, verifier-asserted.
    pub serving_software_known_good: Claim,
    /// Served weights / quantization honesty.
    pub model_weights_provenance: Claim,
    /// Raw provider facts, recorded verbatim from the verifier's
    /// `provider_claims`. Inputs to the typed claims, not claims themselves.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

/// The §9.2 evidence object: a `sha256:` digest over the decoded
/// verifier-input bytes plus a data URI preserving those bytes. A multipart
/// bundle is carried as a single `data:multipart/mixed;...;base64,...` URI
/// with the digest over the whole decoded payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EvidenceRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// `data:` URI carrying the exact bytes and content type.
    #[serde(rename = "data", default, skip_serializing_if = "Option::is_none")]
    pub data_uri: Option<String>,
}

impl EvidenceRef {
    /// Extract an [`EvidenceRef`] from a verifier's free-form evidence value,
    /// preferring an explicit `{ "digest", "data" }` shape.
    pub fn from_value(value: &Value) -> Self {
        Self {
            digest: value
                .get("digest")
                .and_then(Value::as_str)
                .map(str::to_string),
            data_uri: value
                .get("data")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }

    /// True when there is nothing to verify (no `data_uri`, or a `data_uri`
    /// shape we do not produce) or the decoded bytes hash to `digest`. §9.2:
    /// a record whose `data` does not hash to `digest` MUST be rejected.
    pub fn digest_matches_data(&self) -> bool {
        let (Some(digest), Some(data_uri)) = (self.digest.as_deref(), self.data_uri.as_deref())
        else {
            return true;
        };
        // We only ever emit `data:<content-type>;base64,<b64>`; any other shape
        // is not ours, so there is nothing to check against our digest.
        let Some((_, b64)) = data_uri.split_once(";base64,") else {
            return true;
        };
        match BASE64.decode(b64.as_bytes()) {
            Ok(bytes) => digest::sha256_hex(&bytes) == digest,
            Err(_) => false, // claims a digest but the data is not decodable
        }
    }
}

/// Verified identity keys captured into a session (§9.2 `identity`). For
/// dstack-vllm-proxy upstreams this records the response-signing
/// `signing_address`; the TLS SPKI lives in the channel binding, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkloadIdentityRef {
    /// secp256k1 response-signing address (e.g. vllm-proxy `/v1/signature`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_address: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl WorkloadIdentityRef {
    pub fn is_empty(&self) -> bool {
        self.signing_address.is_none() && self.extra.is_empty()
    }
}

/// The §9.2 session document — exactly the members the served bytes carry.
/// The session id is NOT part of the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDocument {
    pub api_version: String,
    /// The upstream this channel belongs to (the operator's upstream config
    /// `name`) — the label a failed `upstream.verified` event would carry.
    pub upstream_name: String,
    /// Verified upstream origin, or `null` when the verifier established none.
    pub endpoint: Option<String>,
    pub verifier_id: String,
    /// When this material was verified — the start of the validity period.
    pub established_at: u64,
    /// End of the validity period for new forwarding decisions. Retention
    /// (serving the record to relying parties) outlives this (§9).
    pub expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<WorkloadIdentityRef>,
    /// Enforceable channel binding(s).
    pub channel_binding: Vec<ChannelBinding>,
    pub claims: SessionClaims,
    pub evidence: EvidenceRef,
}

/// A session frozen to its exact served bytes. Sealed once; the store keeps
/// and serves `bytes` verbatim, and `session_id` is the §9 content address
/// over them.
#[derive(Debug, Clone, PartialEq)]
pub struct AttestedSession {
    session_id: String,
    bytes: Vec<u8>,
    document: SessionDocument,
}

impl AttestedSession {
    /// Serialize `document` once and freeze the bytes and content address.
    pub fn seal(document: SessionDocument) -> Result<Self, serde_json::Error> {
        let bytes = serde_json::to_vec(&document)?;
        let session_id = digest::sha256_hex(&bytes);
        Ok(Self {
            session_id,
            bytes,
            document,
        })
    }

    /// Adopt served/persisted bytes: parse the document and recompute the id
    /// from the exact bytes. This recomputation — not any stored id — is what
    /// makes a persisted record tamper-evident.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, serde_json::Error> {
        let document: SessionDocument = serde_json::from_slice(&bytes)?;
        let session_id = digest::sha256_hex(&bytes);
        Ok(Self {
            session_id,
            bytes,
            document,
        })
    }

    /// `"sha256:" || hex` over [`Self::bytes`].
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The exact bytes `GET /v1/aci/sessions/{hex}` must serve.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn document(&self) -> &SessionDocument {
        &self.document
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding(spki: &str) -> ChannelBinding {
        ChannelBinding::TlsSpkiSha256 {
            origin: "https://node-7.example.net".to_string(),
            spki_sha256: spki.repeat(32),
        }
    }

    pub(crate) fn document(endpoint: &str, spki: &str, claims: SessionClaims) -> SessionDocument {
        SessionDocument {
            api_version: SESSION_API_VERSION.to_string(),
            upstream_name: "phala-direct".to_string(),
            endpoint: Some(endpoint.to_string()),
            verifier_id: "phala-direct/1".to_string(),
            established_at: 1_700_000_000,
            expires_at: 1_700_086_400,
            identity: None,
            channel_binding: vec![binding(spki)],
            claims,
            evidence: EvidenceRef::default(),
        }
    }

    #[test]
    fn session_id_is_the_hash_of_the_served_bytes() {
        let session = AttestedSession::seal(document(
            "https://node-7.example.net",
            "aa",
            SessionClaims::default(),
        ))
        .unwrap();
        assert_eq!(
            session.session_id(),
            digest::sha256_hex(session.bytes()),
            "id must be sha256:<hex> over the exact document bytes"
        );
        assert!(session.session_id().starts_with("sha256:"));
        assert_eq!(session.session_id().len(), 7 + 64);

        // The id is not inside the document bytes.
        let value: Value = serde_json::from_slice(session.bytes()).unwrap();
        assert!(value.get("session_id").is_none());
        assert_eq!(value["api_version"], "aci/1");
        assert_eq!(value["endpoint"], "https://node-7.example.net");
    }

    #[test]
    fn any_document_change_changes_the_id() {
        let base = AttestedSession::seal(document(
            "https://node-7.example.net",
            "aa",
            SessionClaims::default(),
        ))
        .unwrap();

        let rotated = AttestedSession::seal(document(
            "https://node-7.example.net",
            "bb",
            SessionClaims::default(),
        ))
        .unwrap();
        assert_ne!(base.session_id(), rotated.session_id());

        // A new validity period is a new session (§9): timestamps are in the bytes.
        let mut doc = document("https://node-7.example.net", "aa", SessionClaims::default());
        doc.expires_at += 1;
        let renewed = AttestedSession::seal(doc).unwrap();
        assert_ne!(base.session_id(), renewed.session_id());
    }

    #[test]
    fn from_bytes_round_trips_exactly() {
        let sealed =
            AttestedSession::seal(document("https://x", "aa", SessionClaims::default())).unwrap();
        let adopted = AttestedSession::from_bytes(sealed.bytes().to_vec()).unwrap();
        assert_eq!(adopted, sealed);
    }

    #[test]
    fn unknown_claim_serializes_minimally() {
        let json = serde_json::to_value(Claim::unknown()).unwrap();
        assert_eq!(json, json!({ "status": "unknown" }));
    }

    #[test]
    fn asserted_claim_serializes_with_source_and_reason() {
        let claim = Claim::asserted(
            ClaimSource::VerifierDerived,
            "hard-coded known measurements",
        );
        let json = serde_json::to_value(&claim).unwrap();
        assert_eq!(
            json,
            json!({
                "status": "asserted",
                "source": "verifier_derived",
                "reason": "hard-coded known measurements",
            })
        );
    }

    #[test]
    fn evidence_digest_matches_data_guards_a_swapped_payload() {
        let digest_value = digest::sha256_hex(b"abc"); // "sha256:..."
                                                       // base64("abc") = "YWJj" — matches the digest.
        let ok = EvidenceRef {
            digest: Some(digest_value.clone()),
            data_uri: Some("data:text/plain;base64,YWJj".to_string()),
        };
        assert!(ok.digest_matches_data());
        // base64("xyz") = "eHl6" — does NOT match the digest of "abc".
        let swapped = EvidenceRef {
            digest: Some(digest_value.clone()),
            data_uri: Some("data:text/plain;base64,eHl6".to_string()),
        };
        assert!(!swapped.digest_matches_data());
        // No data to check against ⇒ nothing to verify.
        let no_data = EvidenceRef {
            digest: Some(digest_value),
            data_uri: None,
        };
        assert!(no_data.digest_matches_data());
    }
}
