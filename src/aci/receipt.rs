//! Receipt construction and signature math from ACI §9.
//!
//! A receipt is a signed per-request event log. The aggregator
//! computes event hashes from bytes it observed inside the TEE and
//! signs the canonical bytes of the receipt minus `signature.value`
//! with a key listed in the established workload keyset.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::canonical::{self, CanonicalError};
use super::keys::{KeyError, KeyProvider};
use super::types::{Receipt, ReceiptEvent, ReceiptSignature};

pub const EVENT_REQUEST_RECEIVED: &str = "request.received";
pub const EVENT_REQUEST_FORWARDED: &str = "request.forwarded";
pub const EVENT_MIDDLEWARE_FORWARDED: &str = "middleware.forwarded";
pub const EVENT_ROUTE_SELECTED: &str = "route.selected";
pub const EVENT_UPSTREAM_VERIFIED: &str = "upstream.verified";
pub const EVENT_RESPONSE_RECEIVED: &str = "response.received";
pub const EVENT_RESPONSE_RETURNED: &str = "response.returned";
pub const EVENT_TRANSPARENCY_REQUEST_MODIFIED: &str = "transparency.request_modified";
pub const EVENT_TRANSPARENCY_RESPONSE_MODIFIED: &str = "transparency.response_modified";

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("first event MUST be request.received, got {0}")]
    FirstEventMustBeRequestReceived(String),
    #[error("receipt is missing required event {0}")]
    MissingRequiredEvent(&'static str),
    #[error("cannot finalize an empty receipt")]
    EmptyReceipt,
    #[error("event field collides with structural field: {0}")]
    ReservedField(String),
    #[error("event type {0} is reserved for required events; use a profile-specific name instead")]
    ReservedEventType(String),
    #[error("upstream.verified result must be 'verified' or 'failed', got {0}")]
    InvalidVerificationResult(String),
    #[error("canonicalisation error: {0}")]
    Canonical(#[from] CanonicalError),
    #[error("key provider error: {0}")]
    Key(#[from] KeyError),
}

/// An aggregator verifier event suitable for `upstream.verified`.
///
/// `Default` is provided so constructions can set only the fields they care
/// about and `..Default::default()` the rest — adding a field then doesn't
/// ripple across every call site. The default `result` is fail-closed
/// ([`VerificationResult::Failed`]), so an event is never accidentally
/// "verified" by omission.
#[derive(Debug, Clone, Default)]
pub struct UpstreamVerifiedEvent {
    /// The operator's per-endpoint upstream config `name` (e.g.
    /// "tinfoil-glm51") — a label chosen by whoever wrote the config.
    pub upstream_name: String,
    /// The verifier adapter *type* the verification logic keys on (e.g.
    /// "tinfoil", "near-ai", "chutes", "privatemode", "phala-direct"). Many `upstream_name`
    /// entries can share one `provider_type` (two configs both pointing at
    /// Tinfoil). Used to map provider evidence onto typed session claims;
    /// `None` for generic/static verifiers that have no provider type.
    pub provider_type: Option<String>,
    pub model_id: String,
    pub url_origin: Option<String>,
    pub verifier_id: String,
    pub result: VerificationResult,
    pub required: bool,
    pub reason: Option<String>,
    pub evidence: Option<Value>,
    pub channel_bindings: Vec<ChannelBinding>,
    pub provider_claims: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerificationResult {
    Verified,
    /// Fail-closed default: an event with no explicit result is not "verified".
    #[default]
    Failed,
}

impl VerificationResult {
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationResult::Verified => "verified",
            VerificationResult::Failed => "failed",
        }
    }
}

/// Channel binding material verified before the aggregator forwards
/// sensitive bytes to an upstream.
///
/// `tag = "type"` serializes this as a flat, self-describing object — e.g.
/// `{"type":"tls_spki_sha256","origin":..,"spki_sha256":..}` — rather than
/// serde's default externally tagged `{"TlsSpkiSha256":{..}}`, which would
/// leak Rust variant names; `rename_all` keeps the discriminator in the
/// snake_case ACI JSON convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelBinding {
    TlsSpkiSha256 {
        origin: String,
        spki_sha256: String,
    },
    TlsCertificateSha256 {
        origin: String,
        certificate_sha256: String,
    },
    E2eePublicKeySha256 {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_id: Option<String>,
        algorithm: String,
        public_key_sha256: String,
    },
    /// Legacy binding for a gateway-supervised provider-proxy child. Retained
    /// so existing `aci/1` receipts and persisted sessions remain readable.
    ManifestSha256 {
        provider: String,
        manifest_sha256: String,
        coordinator_policy_hash: String,
        proxy_binary_sha256: String,
        proxy_tls_certificate_sha256: String,
    },
    /// Digests binding a provider proxy co-deployed in the gateway's measured
    /// dstack Compose. The proxy verifies the provider's attestation chain and
    /// owns the resulting E2EE secret. The gateway verifies the exact manifest
    /// bytes and pins the Compose-owned proxy image digest.
    ManifestImageSha256 {
        provider: String,
        manifest_sha256: String,
        coordinator_policy_hash: String,
        proxy_image_digest: String,
        /// Digest of the exact Compose secret mounted into both the measured
        /// gateway and proxy. Optional only so receipts issued by an earlier
        /// `aci/1` deployment remain deserializable; current forwarding
        /// requires it to be present and equal to static policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_sha256: Option<String>,
    },
}

/// Minimal transparency event names for operations the workload applied.
///
/// The receipt's existing hash events carry the actual before/after
/// evidence. These events intentionally name only the operation class
/// so the protocol stays extensible without defining a transform DSL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransparencyEventKind {
    RequestModified,
    ResponseModified,
}

impl TransparencyEventKind {
    pub fn event_type(self) -> &'static str {
        match self {
            TransparencyEventKind::RequestModified => EVENT_TRANSPARENCY_REQUEST_MODIFIED,
            TransparencyEventKind::ResponseModified => EVENT_TRANSPARENCY_RESPONSE_MODIFIED,
        }
    }
}

impl UpstreamVerifiedEvent {
    fn to_fields(&self) -> Value {
        // `evidence` is never inlined in a receipt. It can be large, and the
        // receipt store keeps every receipt for the receipt TTL, so inlining it
        // would let that store grow with traffic. Its system of record is the
        // attested session: a verified receipt commits to the evidence through
        // the content-addressed `session_id` (a deep audit re-fetches it from the
        // session store). A failed or unbound verification has no session, so its
        // evidence is not retained here either — `reason` records why it failed.
        serde_json::json!({
            "upstream_name": self.upstream_name,
            "provider_type": self.provider_type,
            "model_id": self.model_id,
            "url_origin": self.url_origin,
            "verifier_id": self.verifier_id,
            "result": self.result.as_str(),
            "required": self.required,
            "reason": self.reason,
            "channel_bindings": self.channel_bindings,
            "provider_claims": self.provider_claims.clone(),
        })
    }
}

/// Assemble a receipt event log inside the TEE.
pub struct ReceiptBuilder {
    receipt_id: String,
    chat_id: Option<String>,
    /// The user-requested model (received request's top-level `model`), before
    /// any service-side rewrite; `None` when the request carried none (§8.2).
    model: Option<String>,
    workload_id: String,
    workload_keyset_digest: String,
    endpoint: String,
    method: String,
    served_at: u64,
    events: Vec<ReceiptEvent>,
    next_seq: u64,
}

#[allow(clippy::too_many_arguments)]
impl ReceiptBuilder {
    pub fn new(
        receipt_id: String,
        chat_id: Option<String>,
        model: Option<String>,
        workload_id: String,
        workload_keyset_digest: String,
        endpoint: String,
        method: String,
        served_at: u64,
    ) -> Self {
        Self {
            receipt_id,
            chat_id,
            model,
            workload_id,
            workload_keyset_digest,
            endpoint,
            method,
            served_at,
            events: Vec::new(),
            next_seq: 0,
        }
    }

    fn append(&mut self, event_type: &str, fields: Value) -> Result<(), ReceiptError> {
        if self.next_seq == 0 && event_type != EVENT_REQUEST_RECEIVED {
            return Err(ReceiptError::FirstEventMustBeRequestReceived(
                event_type.to_string(),
            ));
        }
        if let Value::Object(obj) = &fields {
            for k in obj.keys() {
                if k == "seq" || k == "type" {
                    return Err(ReceiptError::ReservedField(k.clone()));
                }
            }
        }
        self.events.push(ReceiptEvent {
            seq: self.next_seq,
            event_type: event_type.to_string(),
            fields,
        });
        self.next_seq += 1;
        Ok(())
    }

    pub fn add_request_received(&mut self, body: &[u8]) -> Result<String, ReceiptError> {
        let digest = canonical::sha256_hex(body);
        self.append(
            EVENT_REQUEST_RECEIVED,
            serde_json::json!({ "body_hash": digest }),
        )?;
        Ok(digest)
    }

    pub fn add_request_forwarded(&mut self, body: &[u8]) -> Result<String, ReceiptError> {
        let digest = canonical::sha256_hex(body);
        self.append(
            EVENT_REQUEST_FORWARDED,
            serde_json::json!({ "body_hash": digest }),
        )?;
        Ok(digest)
    }

    pub fn add_middleware_forwarded(&mut self, body: &[u8]) -> Result<String, ReceiptError> {
        let digest = canonical::sha256_hex(body);
        self.append(
            EVENT_MIDDLEWARE_FORWARDED,
            serde_json::json!({ "body_hash": digest }),
        )?;
        Ok(digest)
    }

    pub fn add_route_selected(&mut self, target_route_id: &str) -> Result<(), ReceiptError> {
        self.append(
            EVENT_ROUTE_SELECTED,
            serde_json::json!({ "target_route_id": target_route_id }),
        )
    }

    /// Append `upstream.verified` with **no** attested session — used when
    /// verification failed (or produced no enforceable channel binding), so there
    /// is no session to reference. The event is still recorded so a downstream
    /// verifier sees the (failed) outcome. The verified case uses
    /// [`Self::add_upstream_verified_with_session`].
    pub fn add_upstream_verified(
        &mut self,
        event: UpstreamVerifiedEvent,
    ) -> Result<(), ReceiptError> {
        self.append(EVENT_UPSTREAM_VERIFIED, event.to_fields())
    }

    /// Append `upstream.verified` for a verified upstream, attaching the
    /// attested-session id and the typed claim verdicts (shallow audit). The two
    /// are **inseparable** — a sealed session always carries its claims — so they
    /// are taken together and required, not as two independent options. The
    /// session id is content-addressed, so it commits the receipt to the exact
    /// session (with its evidence + reasons) a deep audit would re-fetch.
    pub fn add_upstream_verified_with_session<C: serde::Serialize>(
        &mut self,
        event: UpstreamVerifiedEvent,
        session_id: &str,
        claims: &C,
    ) -> Result<(), ReceiptError> {
        // `claims` is typed at the call site (the aggregator's `SessionClaims`);
        // it is serialized here only because the event-log fields are a generic
        // JSON object. The ACI core stays decoupled from the aggregator's claim
        // types via the `Serialize` bound rather than a `Value` parameter.
        let mut fields = event.to_fields();
        if let Value::Object(obj) = &mut fields {
            obj.insert(
                "session_id".to_string(),
                Value::String(session_id.to_string()),
            );
            obj.insert(
                "claims".to_string(),
                serde_json::to_value(claims).unwrap_or(Value::Null),
            );
        }
        self.append(EVENT_UPSTREAM_VERIFIED, fields)
    }

    pub fn add_transparency_event(
        &mut self,
        kind: TransparencyEventKind,
    ) -> Result<(), ReceiptError> {
        self.append(kind.event_type(), serde_json::json!({}))
    }

    pub fn add_response_received(&mut self, cleartext: &[u8]) -> Result<String, ReceiptError> {
        let digest = canonical::sha256_hex(cleartext);
        self.add_response_received_hash(digest.clone())?;
        Ok(digest)
    }

    pub fn add_response_received_hash(
        &mut self,
        cleartext_hash: String,
    ) -> Result<(), ReceiptError> {
        self.append(
            EVENT_RESPONSE_RECEIVED,
            serde_json::json!({ "cleartext_hash": cleartext_hash }),
        )
    }

    pub fn add_response_returned(
        &mut self,
        cleartext: &[u8],
        wire: &[u8],
    ) -> Result<(String, String), ReceiptError> {
        let cleartext_hash = canonical::sha256_hex(cleartext);
        let wire_hash = canonical::sha256_hex(wire);
        self.add_response_returned_hashes(cleartext_hash.clone(), wire_hash.clone())?;
        Ok((cleartext_hash, wire_hash))
    }

    pub fn add_response_returned_hashes(
        &mut self,
        cleartext_hash: String,
        wire_hash: String,
    ) -> Result<(), ReceiptError> {
        self.append(
            EVENT_RESPONSE_RETURNED,
            serde_json::json!({
                "cleartext_hash": cleartext_hash,
                "wire_hash": wire_hash,
            }),
        )
    }

    pub fn add_extension_event(
        &mut self,
        event_type: &str,
        fields: Value,
    ) -> Result<(), ReceiptError> {
        match event_type {
            EVENT_REQUEST_RECEIVED
            | EVENT_REQUEST_FORWARDED
            | EVENT_UPSTREAM_VERIFIED
            | EVENT_RESPONSE_RECEIVED
            | EVENT_RESPONSE_RETURNED => {
                Err(ReceiptError::ReservedEventType(event_type.to_string()))
            }
            _ => self.append(event_type, fields),
        }
    }

    pub fn events(&self) -> &[ReceiptEvent] {
        &self.events
    }

    pub fn set_chat_id(&mut self, chat_id: Option<String>) {
        self.chat_id = chat_id;
    }

    pub fn set_upstream_verified_model_id(&mut self, model_id: Option<String>) {
        let model_id = model_id.unwrap_or_else(|| "unknown".to_string());
        for event in &mut self.events {
            if event.event_type == EVENT_UPSTREAM_VERIFIED {
                if let Value::Object(fields) = &mut event.fields {
                    fields.insert("model_id".to_string(), Value::String(model_id));
                }
                return;
            }
        }
    }

    /// Produce a signed receipt.
    ///
    /// Validates that all required events for the chosen receipt
    /// shape are present. The caller has already decided whether
    /// this is an aggregator or individual-LLM receipt by choosing
    /// whether to append [`EVENT_UPSTREAM_VERIFIED`].
    pub fn finalize(self, keys: &dyn KeyProvider, key_id: &str) -> Result<Receipt, ReceiptError> {
        if self.events.is_empty() {
            return Err(ReceiptError::EmptyReceipt);
        }

        let present: std::collections::HashSet<&str> =
            self.events.iter().map(|e| e.event_type.as_str()).collect();
        for required in [
            EVENT_REQUEST_RECEIVED,
            EVENT_REQUEST_FORWARDED,
            EVENT_RESPONSE_RETURNED,
        ] {
            if !present.contains(required) {
                return Err(ReceiptError::MissingRequiredEvent(required));
            }
        }

        let receipt_key = keys
            .receipt_keys()
            .into_iter()
            .find(|k| k.key_id == key_id)
            .ok_or_else(|| KeyError::UnknownReceiptKeyId(key_id.to_string()))?;

        // Build the receipt with an empty signature value so the
        // canonical-bytes computation matches §9.4 ("whole receipt
        // with only signature.value omitted"). The placeholder never
        // reaches the wire because we re-sign below.
        let unsigned = Receipt {
            api_version: "aci/1".to_string(),
            receipt_id: self.receipt_id,
            chat_id: self.chat_id,
            model: self.model,
            workload_id: self.workload_id,
            workload_keyset_digest: self.workload_keyset_digest,
            endpoint: self.endpoint,
            method: self.method,
            served_at: self.served_at,
            event_log: self.events,
            signature: ReceiptSignature {
                algo: receipt_key.algo.clone(),
                key_id: receipt_key.key_id.clone(),
                value_hex: String::new(),
            },
        };

        let canonical_bytes = canonical::canonicalize(&unsigned.to_canonical_value(false))?;
        let sig = keys.sign_receipt(&receipt_key.key_id, &canonical_bytes)?;
        let value_hex = hex::encode(sig);

        Ok(Receipt {
            signature: ReceiptSignature {
                algo: receipt_key.algo,
                key_id: receipt_key.key_id,
                value_hex,
            },
            ..unsigned
        })
    }
}

/// Return the bytes a verifier MUST use to check the receipt signature.
pub fn canonical_bytes_for_signing(receipt: &Receipt) -> Result<Vec<u8>, ReceiptError> {
    Ok(canonical::canonicalize(&receipt.to_canonical_value(false))?)
}
