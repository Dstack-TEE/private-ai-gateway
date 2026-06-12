//! Immutable, provider-owned attested-session records.
//!
//! An *attested session* captures **one** verified state of an upstream
//! workload — its identity, the enforceable channel binding, the typed claims a
//! verifier asserted about it, and the supporting evidence. A session is never
//! mutated: its [`AttestedSession::session_id`] is content-addressed over that
//! material, so identical verifications dedup to one id while *any* change in
//! the verified material (a rotated TLS SPKI, a new measurement, a changed
//! claim) yields a different id — a new, separate session. A receipt references
//! the exact session it used, so the security context behind a receipt can
//! never silently change.
//!
//! "One provider imports many sessions" follows naturally: many model-endpoints,
//! plus a new session whenever a model-endpoint's verified material changes.
//!
//! Source-code-level provenance is the verifier's responsibility, not a schema
//! here: the verifier asserts the `serving_software_known_good` / `os_known_good`
//! claims with a plain `reason`. See `docs/attested-session-system.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::aci::canonical::{self, CanonicalError};
use crate::aci::receipt::ChannelBinding;

/// `api_version` stamped on persisted session records. Gateway-local envelopes
/// use the `aci.<resource>.v1` scheme; the signed ACI artifacts stay `aci/1`.
pub const SESSION_API_VERSION: &str = "aci.session.v1";

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
/// [`ClaimStatus::Refuted`]; an `Unknown` claim carries neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub status: ClaimStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ClaimSource>,
    /// The verifier's plain reason, e.g. "matches hard-coded known measurements".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Pointer into the session evidence backing this claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
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
            evidence_ref: None,
        }
    }

    pub fn asserted(source: ClaimSource, reason: impl Into<String>) -> Self {
        Self {
            status: ClaimStatus::Asserted,
            source: Some(source),
            reason: Some(reason.into()),
            evidence_ref: None,
        }
    }

    pub fn refuted(source: ClaimSource, reason: impl Into<String>) -> Self {
        Self {
            status: ClaimStatus::Refuted,
            source: Some(source),
            reason: Some(reason.into()),
            evidence_ref: None,
        }
    }

    pub fn with_evidence_ref(mut self, evidence_ref: impl Into<String>) -> Self {
        self.evidence_ref = Some(evidence_ref.into());
        self
    }
}

/// The typed claim vocabulary, mapped to `docs/providers/audit-criteria.md`.
/// Every field defaults to [`Claim::unknown`]; `extra` holds provider-owned
/// scope facts without widening the fixed vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionClaims {
    /// §1 — a genuine CPU TEE, with the workload identity bound.
    pub tee_attested: Claim,
    /// "The GPU is good", asserted from CPU attestation + a software source
    /// check (the measured serving software verifies the GPU). Never based on a
    /// standalone GPU/NRAS token, which only proves a CC-capable GPU exists.
    pub gpu_attested: Claim,
    /// §14 — platform TCB freshness (TDX/SGX `TcbStatus`, SEV reported TCB).
    pub tcb_up_to_date: Claim,
    /// §13 — platform/OS provenance (guest OS, kernel, firmware).
    pub os_known_good: Claim,
    /// §13 — software provenance (serving/app/gateway code), verifier-asserted.
    pub serving_software_known_good: Claim,
    /// §4 — served weights / quantization honesty.
    pub model_weights_provenance: Claim,
    /// Provider-owned scope facts, recorded verbatim from the verifier's
    /// `provider_claims` (e.g. `trust_boundary`, `gpu_verified`, `gpu_arch`).
    /// Not typed claims; the fixed vocabulary above is derived from these.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

/// The common evidence object (audit-criteria §11): a `sha256:` digest over the
/// decoded verifier-input bytes plus a data URI that preserves those bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EvidenceRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// `data:` URI carrying the exact bytes and content type.
    #[serde(rename = "data", skip_serializing_if = "Option::is_none")]
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
}

/// Verified identity keys captured into a session. For dstack-vllm-proxy this
/// records the response-signing `signing_address`; the TLS SPKI lives in the
/// channel binding, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkloadIdentityRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement: Option<String>,
    /// secp256k1 response-signing address (e.g. vllm-proxy `/v1/signature`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_address: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl WorkloadIdentityRef {
    pub fn is_empty(&self) -> bool {
        self.workload_id.is_none()
            && self.measurement.is_none()
            && self.signing_address.is_none()
            && self.extra.is_empty()
    }
}

/// One immutable, verified session. Content-addressed; never mutated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedSession {
    pub api_version: String,
    /// `"as_" + hex(sha256(JCS(verified material)))`.
    pub session_id: String,
    pub provider: String,
    pub public_model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub verifier_id: String,
    /// When this material was verified.
    pub established_at: u64,
    /// Retention deadline (>= the TTL of receipts that cite this session). A
    /// retention window, not a binding-validity deadline.
    pub expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<WorkloadIdentityRef>,
    /// Enforceable channel binding(s), serialized via [`ChannelBinding::to_value`].
    pub channel_binding: Vec<Value>,
    pub claims: SessionClaims,
    pub evidence: EvidenceRef,
}

impl AttestedSession {
    /// Serialize channel bindings to their canonical wire values.
    pub fn bindings_to_values(bindings: &[ChannelBinding]) -> Vec<Value> {
        bindings.iter().map(ChannelBinding::to_value).collect()
    }

    /// Seal an immutable session, computing its content-addressed id over the
    /// verified material. Timestamps are excluded from the id so identical
    /// material dedups to one session.
    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        provider: impl Into<String>,
        public_model_id: impl Into<String>,
        upstream_model_id: Option<String>,
        endpoint: Option<String>,
        verifier_id: impl Into<String>,
        identity: Option<WorkloadIdentityRef>,
        channel_binding: Vec<Value>,
        claims: SessionClaims,
        evidence: EvidenceRef,
        established_at: u64,
        expires_at: u64,
    ) -> Result<Self, CanonicalError> {
        let mut session = Self {
            api_version: SESSION_API_VERSION.to_string(),
            session_id: String::new(),
            provider: provider.into(),
            public_model_id: public_model_id.into(),
            upstream_model_id,
            endpoint,
            verifier_id: verifier_id.into(),
            established_at,
            expires_at,
            identity,
            channel_binding,
            claims,
            evidence,
        };
        session.session_id = session.content_id()?;
        Ok(session)
    }

    /// Recompute the content-addressed id from the verified material. The id is
    /// `"as_" + sha256(JCS(material))` over the immutable subset (timestamps
    /// excluded, so identical material dedups). A relying party — and the store
    /// on replay — calls this to confirm a record's `session_id` matches its
    /// contents; that recomputation, not any stored signature, is what makes the
    /// record tamper-evident.
    pub fn content_id(&self) -> Result<String, CanonicalError> {
        let material = json!({
            "provider": self.provider,
            "public_model_id": self.public_model_id,
            "upstream_model_id": self.upstream_model_id,
            "endpoint": self.endpoint,
            "verifier_id": self.verifier_id,
            "identity": self.identity,
            "channel_binding": self.channel_binding,
            "claims": self.claims,
            "evidence_digest": self.evidence.digest,
        });
        let digest = canonical::jcs_sha256_hex(&material)?;
        Ok(format!(
            "as_{}",
            digest.strip_prefix("sha256:").unwrap_or(digest.as_str())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(spki: &str) -> Value {
        ChannelBinding::TlsSpkiSha256 {
            origin: "https://node-7.example.net".to_string(),
            spki_sha256: spki.repeat(32),
        }
        .to_value()
    }

    fn seal_with(endpoint: &str, spki: &str, claims: SessionClaims) -> AttestedSession {
        AttestedSession::seal(
            "phala-direct",
            "glm51-phala",
            Some("zai-org/GLM-5.1".to_string()),
            Some(endpoint.to_string()),
            "phala-direct/1",
            None,
            vec![binding(spki)],
            claims,
            EvidenceRef::default(),
            1_700_000_000,
            1_700_086_400,
        )
        .unwrap()
    }

    #[test]
    fn session_id_is_content_addressed_and_dedups() {
        let a = seal_with("https://node-7.example.net", "aa", SessionClaims::default());
        let b = seal_with("https://node-7.example.net", "aa", SessionClaims::default());
        assert!(a.session_id.starts_with("as_"));
        assert_eq!(a.session_id.len(), 3 + 64, "as_ + 64 hex chars");
        // Identical verified material → identical id, regardless of timestamps
        // being equal here; the id excludes them.
        assert_eq!(a.session_id, b.session_id);
    }

    #[test]
    fn session_id_changes_when_verified_material_changes() {
        let base = seal_with("https://node-7.example.net", "aa", SessionClaims::default());

        // Rotated SPKI ⇒ new session (the cert-renewal case).
        let rotated = seal_with("https://node-7.example.net", "bb", SessionClaims::default());
        assert_ne!(base.session_id, rotated.session_id);

        // Different endpoint ⇒ new session.
        let other_endpoint =
            seal_with("https://node-8.example.net", "aa", SessionClaims::default());
        assert_ne!(base.session_id, other_endpoint.session_id);

        // Different claims ⇒ new session.
        let claims = SessionClaims {
            tee_attested: Claim::asserted(ClaimSource::HardwareProven, "dcap verified"),
            ..Default::default()
        };
        let other_claims = seal_with("https://node-7.example.net", "aa", claims);
        assert_ne!(base.session_id, other_claims.session_id);
    }

    #[test]
    fn id_ignores_timestamps() {
        let a = AttestedSession::seal(
            "p",
            "m",
            None,
            None,
            "v/1",
            None,
            vec![],
            SessionClaims::default(),
            EvidenceRef::default(),
            100,
            400,
        )
        .unwrap();
        let b = AttestedSession::seal(
            "p",
            "m",
            None,
            None,
            "v/1",
            None,
            vec![],
            SessionClaims::default(),
            EvidenceRef::default(),
            999,
            9999,
        )
        .unwrap();
        assert_eq!(a.session_id, b.session_id);
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
        )
        .with_evidence_ref("sha256:abc");
        let json = serde_json::to_value(&claim).unwrap();
        assert_eq!(
            json,
            json!({
                "status": "asserted",
                "source": "verifier_derived",
                "reason": "hard-coded known measurements",
                "evidence_ref": "sha256:abc",
            })
        );
    }

    #[test]
    fn session_round_trips_through_serde() {
        let session = seal_with("https://node-7.example.net", "aa", SessionClaims::default());
        let back: AttestedSession =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(session, back);
    }
}
