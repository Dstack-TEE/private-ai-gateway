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
//! "One provider owns many sessions" follows naturally: one per verified TEE
//! channel (endpoint), plus a new one whenever a channel's verified material
//! changes. A router fronting many models behind one TEE is a single session.
//!
//! Source-code-level provenance is the verifier's responsibility, not a schema
//! here: the verifier asserts the `serving_software_known_good` / `os_known_good`
//! claims with a plain `reason`. See `docs/attested-session-system.md`.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::aci::canonical::{self, CanonicalError};
use crate::aci::receipt::ChannelBinding;

/// `api_version` stamped on persisted session records — `aci/1`, uniform with
/// the rest of the ACI surface.
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
/// [`ClaimStatus::Refuted`]; an `Unknown` claim carries neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub status: ClaimStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ClaimSource>,
    /// The verifier's plain reason, e.g. "matches hard-coded known measurements".
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// The typed claim vocabulary, mapped to `docs/providers/audit-criteria.md`.
/// Every field defaults to [`Claim::unknown`]; `extra` holds provider-owned
/// scope facts without widening the fixed vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionClaims {
    /// §1 — a genuine CPU TEE, with the workload identity bound.
    pub tee_attested: Claim,
    /// The provider's NVIDIA confidential-computing GPU attestation, when
    /// verified and nonce-bound: asserted `VerifierDerived` — it attests a
    /// genuine CC GPU, not (on its own) that GPU's binding to the serving CPU
    /// TEE, which would need a measured-software check inside the CPU quote.
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
/// decoded verifier-input bytes plus a data URI that preserves those bytes. A
/// multipart bundle (e.g. several raw HTTP responses) is carried as a single
/// `data:multipart/mixed;boundary=...;base64,...` URI, with the digest taken
/// over the whole decoded payload — so this stays one `{digest, data}` pair
/// regardless of how many parts it contains.
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

    /// True when there is nothing to verify (no `data_uri`, or a `data_uri` shape
    /// we do not produce) or the `data_uri`'s decoded bytes hash to `digest`.
    /// The content id commits to `digest`, not the bytes, so this guards against
    /// a persisted record whose evidence `data` was substituted for bytes that
    /// do not match the digest the receipt is signed over.
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
            Ok(bytes) => canonical::sha256_hex(&bytes) == digest,
            Err(_) => false, // claims a digest but the data is not decodable
        }
    }
}

/// Verified identity keys captured into a session. For dstack-vllm-proxy this
/// records the response-signing `signing_address`; the TLS SPKI lives in the
/// channel binding, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkloadIdentityRef {
    /// secp256k1 response-signing address (e.g. vllm-proxy `/v1/signature`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_address: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl WorkloadIdentityRef {
    pub fn is_empty(&self) -> bool {
        self.signing_address.is_none() && self.extra.is_empty()
    }
}

/// One immutable, verified **TEE channel** — the attested remote service a
/// request can be bound to, identified by its endpoint + channel binding +
/// evidence, not by model. Content-addressed; never mutated. A router-based
/// upstream that serves many models behind one TEE therefore yields **one**
/// session (no per-model duplication); the specific model served is recorded on
/// the receipt's `upstream.verified` event, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedSession {
    pub api_version: String,
    /// `"as_" + hex(sha256(JCS(verified material)))`.
    pub session_id: String,
    /// The upstream this channel belongs to (the operator's upstream config name).
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub verifier_id: String,
    /// When this material was verified.
    pub established_at: u64,
    /// Retention deadline: roughly the TTL of receipts that cite this session
    /// (sealed just before its receipt, so it expires up to one sub-second
    /// request interval sooner). A retention window, not a binding-validity
    /// deadline.
    pub expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<WorkloadIdentityRef>,
    /// Enforceable channel binding(s).
    pub channel_binding: Vec<ChannelBinding>,
    pub claims: SessionClaims,
    pub evidence: EvidenceRef,
}

impl AttestedSession {
    /// Seal an immutable session, computing its content-addressed id over the
    /// verified material. Timestamps are excluded from the id so identical
    /// material dedups to one session.
    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        provider: impl Into<String>,
        endpoint: Option<String>,
        verifier_id: impl Into<String>,
        identity: Option<WorkloadIdentityRef>,
        channel_binding: Vec<ChannelBinding>,
        claims: SessionClaims,
        evidence: EvidenceRef,
        established_at: u64,
        expires_at: u64,
    ) -> Result<Self, CanonicalError> {
        let mut session = Self {
            api_version: SESSION_API_VERSION.to_string(),
            session_id: String::new(),
            provider: provider.into(),
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
        /// The immutable subset the content id commits to. Timestamps are
        /// excluded so identical material dedups to one session; field names
        /// here are load-bearing (they feed the canonical hash).
        #[derive(Serialize)]
        struct ContentMaterial<'a> {
            provider: &'a str,
            endpoint: &'a Option<String>,
            verifier_id: &'a str,
            identity: &'a Option<WorkloadIdentityRef>,
            channel_binding: &'a [ChannelBinding],
            claims: &'a SessionClaims,
            evidence_digest: &'a Option<String>,
        }
        let material = serde_json::to_value(ContentMaterial {
            provider: &self.provider,
            endpoint: &self.endpoint,
            verifier_id: &self.verifier_id,
            identity: &self.identity,
            channel_binding: &self.channel_binding,
            claims: &self.claims,
            evidence_digest: &self.evidence.digest,
        })?;
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
    use serde_json::json;

    fn binding(spki: &str) -> ChannelBinding {
        ChannelBinding::TlsSpkiSha256 {
            origin: "https://node-7.example.net".to_string(),
            spki_sha256: spki.repeat(32),
        }
    }

    fn seal_with(endpoint: &str, spki: &str, claims: SessionClaims) -> AttestedSession {
        AttestedSession::seal(
            "phala-direct",
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
        let digest = canonical::sha256_hex(b"abc"); // "sha256:..."
                                                    // base64("abc") = "YWJj" — matches the digest.
        let ok = EvidenceRef {
            digest: Some(digest.clone()),
            data_uri: Some("data:text/plain;base64,YWJj".to_string()),
        };
        assert!(ok.digest_matches_data());
        // base64("xyz") = "eHl6" — does NOT match the digest of "abc".
        let swapped = EvidenceRef {
            digest: Some(digest.clone()),
            data_uri: Some("data:text/plain;base64,eHl6".to_string()),
        };
        assert!(!swapped.digest_matches_data());
        // No data to check against ⇒ nothing to verify.
        let no_data = EvidenceRef {
            digest: Some(digest),
            data_uri: None,
        };
        assert!(no_data.digest_matches_data());
    }

    #[test]
    fn session_round_trips_through_serde() {
        let session = seal_with("https://node-7.example.net", "aa", SessionClaims::default());
        let back: AttestedSession =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(session, back);
    }
}
