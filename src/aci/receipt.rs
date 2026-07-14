//! Inference receipts (ACI §8).
//!
//! A receipt is a signed per-request event log. The payload is serialized
//! exactly once at finalization; those bytes are what the Ed25519 signature
//! covers and what the service serves (base64-encoded) in the §8.2 envelope.
//! Verifiers check the signature over the decoded bytes — never over any
//! re-serialization.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::digest;
use super::keys::{KeyError, KeyProvider, ALGO_ED25519};

pub const RECEIPT_API_VERSION: &str = "aci/1";

// §8.4 required event vocabulary.
pub const EVENT_REQUEST_RECEIVED: &str = "request.received";
pub const EVENT_REQUEST_FORWARDED: &str = "request.forwarded";
pub const EVENT_UPSTREAM_VERIFIED: &str = "upstream.verified";
pub const EVENT_RESPONSE_RETURNED: &str = "response.returned";

// Reference-implementation extension events (§8.4: verifiers ignore unknown
// types). They record routing decisions and the upstream's response bytes.
pub const EVENT_MIDDLEWARE_FORWARDED: &str = "middleware.forwarded";
pub const EVENT_ROUTE_SELECTED: &str = "route.selected";
pub const EVENT_RESPONSE_RECEIVED: &str = "response.received";

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
    #[error("event type {0} is reserved for required events; use an extension name instead")]
    ReservedEventType(String),
    #[error("receipt payload serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("key provider error: {0}")]
    Key(#[from] KeyError),
}

/// The verification outcome an upstream verifier hands the aggregator.
///
/// The receipt records only the slim §8.5 event; everything else here —
/// bindings, provider facts, evidence — becomes the attested session the
/// event cites. `Default` keeps `result` fail-closed
/// ([`VerificationResult::Failed`]).
#[derive(Debug, Clone, Default)]
pub struct UpstreamVerifiedEvent {
    /// The operator's per-endpoint upstream config `name` (e.g.
    /// "tinfoil-glm51").
    pub upstream_name: String,
    /// The verifier adapter *type* the verification logic keys on (e.g.
    /// "tinfoil", "chutes"); maps provider evidence onto typed session claims.
    /// `None` for generic/static verifiers.
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
/// sensitive bytes to an upstream (§9.2 shapes).
///
/// `tag = "type"` serializes this as a flat, self-describing object — e.g.
/// `{"type":"tls_spki_sha256","origin":..,"spki_sha256":..}` — rather than
/// serde's default externally tagged form, which would leak Rust variant
/// names; `rename_all` keeps the discriminator in the snake_case ACI JSON
/// convention.
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
}

/// One flat event: `type` plus type-specific fields, in insertion order.
#[derive(Debug, Clone)]
pub struct ReceiptEvent {
    pub event_type: String,
    pub fields: Map<String, Value>,
}

impl ReceiptEvent {
    fn to_value(&self) -> Value {
        let mut obj = Map::with_capacity(self.fields.len() + 1);
        obj.insert("type".to_string(), Value::from(self.event_type.clone()));
        for (k, v) in &self.fields {
            obj.insert(k.clone(), v.clone());
        }
        Value::Object(obj)
    }
}

/// The §8.3 payload shape; serialized exactly once by
/// [`ReceiptBuilder::finalize`].
#[derive(Serialize)]
struct ReceiptPayload<'a> {
    api_version: &'a str,
    receipt_id: &'a str,
    chat_id: &'a Option<String>,
    model: &'a Option<String>,
    workload_keyset_digest: &'a str,
    endpoint: &'a str,
    method: &'a str,
    served_at: u64,
    event_log: Vec<Value>,
}

/// A finalized receipt: the exact payload bytes plus the Ed25519 signature
/// over them. The store keeps this whole; the §8.2 envelope is derived from
/// the stored bytes, so serving is byte-stable by construction.
#[derive(Debug, Clone)]
pub struct SignedReceipt {
    /// Lookup ids, duplicated out of the payload so stores never parse it.
    pub receipt_id: String,
    pub chat_id: Option<String>,
    /// The exact §8.3 payload bytes the signature covers.
    pub payload: Vec<u8>,
    pub key_id: String,
    pub algo: String,
    pub signature_hex: String,
}

impl SignedReceipt {
    /// The §8.2 signed-bytes envelope served at `GET /v1/aci/receipts/{id}`.
    pub fn envelope(&self) -> Value {
        serde_json::json!({
            "payload_b64": BASE64.encode(&self.payload),
            "key_id": self.key_id,
            "algo": self.algo,
            "signature": self.signature_hex,
        })
    }

    /// Parse the stored payload bytes (for legacy surfaces and tests; the
    /// protocol artifact stays [`Self::payload`]).
    pub fn payload_json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }
}

/// Assemble a receipt event log inside the TEE.
pub struct ReceiptBuilder {
    receipt_id: String,
    chat_id: Option<String>,
    /// The model the client requested (for E2EE, the envelope `model`);
    /// `None` when the request carried none (§8.3).
    model: Option<String>,
    workload_keyset_digest: String,
    endpoint: String,
    method: String,
    served_at: u64,
    events: Vec<ReceiptEvent>,
}

impl ReceiptBuilder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        receipt_id: String,
        chat_id: Option<String>,
        model: Option<String>,
        workload_keyset_digest: String,
        endpoint: String,
        method: String,
        served_at: u64,
    ) -> Self {
        Self {
            receipt_id,
            chat_id,
            model,
            workload_keyset_digest,
            endpoint,
            method,
            served_at,
            events: Vec::new(),
        }
    }

    fn append(&mut self, event_type: &str, fields: Map<String, Value>) -> Result<(), ReceiptError> {
        if self.events.is_empty() && event_type != EVENT_REQUEST_RECEIVED {
            return Err(ReceiptError::FirstEventMustBeRequestReceived(
                event_type.to_string(),
            ));
        }
        if let Some(key) = fields.keys().find(|k| *k == "type") {
            return Err(ReceiptError::ReservedField(key.clone()));
        }
        self.events.push(ReceiptEvent {
            event_type: event_type.to_string(),
            fields,
        });
        Ok(())
    }

    fn append_body_hash(&mut self, event_type: &str, body: &[u8]) -> Result<String, ReceiptError> {
        let hash = digest::sha256_hex(body);
        self.append_hash(event_type, hash.clone())?;
        Ok(hash)
    }

    fn append_hash(&mut self, event_type: &str, body_hash: String) -> Result<(), ReceiptError> {
        let mut fields = Map::new();
        fields.insert("body_hash".to_string(), Value::String(body_hash));
        self.append(event_type, fields)
    }

    pub fn add_request_received(&mut self, body: &[u8]) -> Result<String, ReceiptError> {
        self.append_body_hash(EVENT_REQUEST_RECEIVED, body)
    }

    pub fn add_request_forwarded(&mut self, body: &[u8]) -> Result<String, ReceiptError> {
        self.append_body_hash(EVENT_REQUEST_FORWARDED, body)
    }

    pub fn add_middleware_forwarded(&mut self, body: &[u8]) -> Result<String, ReceiptError> {
        self.append_body_hash(EVENT_MIDDLEWARE_FORWARDED, body)
    }

    pub fn add_route_selected(&mut self, target_route_id: &str) -> Result<(), ReceiptError> {
        let mut fields = Map::new();
        fields.insert(
            "target_route_id".to_string(),
            Value::String(target_route_id.to_string()),
        );
        self.append(EVENT_ROUTE_SELECTED, fields)
    }

    /// Append the §8.5 verified form: the event cites the content-addressed
    /// attested session holding every other verification detail.
    pub fn add_upstream_verified_with_session(
        &mut self,
        event: &UpstreamVerifiedEvent,
        session_id: &str,
    ) -> Result<(), ReceiptError> {
        let mut fields = Map::new();
        fields.insert(
            "result".to_string(),
            Value::String(VerificationResult::Verified.as_str().to_string()),
        );
        fields.insert("required".to_string(), Value::Bool(event.required));
        fields.insert(
            "model_id".to_string(),
            Value::String(event.model_id.clone()),
        );
        fields.insert(
            "session_id".to_string(),
            Value::String(session_id.to_string()),
        );
        self.append(EVENT_UPSTREAM_VERIFIED, fields)
    }

    /// Append the §8.5 failed form: `reason` instead of a session, plus the
    /// optional upstream label. Also used when a nominally verified result
    /// produced no enforceable binding — without a session there is nothing a
    /// relying party could check, so recording it "verified" would overstate.
    pub fn add_upstream_verified_failed(
        &mut self,
        event: &UpstreamVerifiedEvent,
    ) -> Result<(), ReceiptError> {
        let mut fields = Map::new();
        fields.insert(
            "result".to_string(),
            Value::String(VerificationResult::Failed.as_str().to_string()),
        );
        fields.insert("required".to_string(), Value::Bool(event.required));
        fields.insert(
            "model_id".to_string(),
            Value::String(event.model_id.clone()),
        );
        let reason = event
            .reason
            .clone()
            .unwrap_or_else(|| "no enforceable verified binding".to_string());
        fields.insert("reason".to_string(), Value::String(reason));
        if !event.upstream_name.is_empty() {
            fields.insert(
                "upstream_name".to_string(),
                Value::String(event.upstream_name.clone()),
            );
        }
        self.append(EVENT_UPSTREAM_VERIFIED, fields)
    }

    pub fn add_response_received(&mut self, body: &[u8]) -> Result<String, ReceiptError> {
        self.append_body_hash(EVENT_RESPONSE_RECEIVED, body)
    }

    pub fn add_response_received_hash(&mut self, body_hash: String) -> Result<(), ReceiptError> {
        self.append_hash(EVENT_RESPONSE_RECEIVED, body_hash)
    }

    /// The exact response body bytes emitted on the wire (§8.4): raw SSE
    /// stream bytes for streaming, the sealed envelope bytes for E2EE.
    pub fn add_response_returned(&mut self, wire: &[u8]) -> Result<String, ReceiptError> {
        self.append_body_hash(EVENT_RESPONSE_RETURNED, wire)
    }

    pub fn add_response_returned_hash(&mut self, body_hash: String) -> Result<(), ReceiptError> {
        self.append_hash(EVENT_RESPONSE_RETURNED, body_hash)
    }

    pub fn add_extension_event(
        &mut self,
        event_type: &str,
        fields: Map<String, Value>,
    ) -> Result<(), ReceiptError> {
        match event_type {
            EVENT_REQUEST_RECEIVED
            | EVENT_REQUEST_FORWARDED
            | EVENT_UPSTREAM_VERIFIED
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

    /// Record the model the upstream actually served on the (first)
    /// `upstream.verified` event; the session is keyed on the channel, not the
    /// model, so this lands on the receipt.
    pub fn set_upstream_verified_model_id(&mut self, model_id: Option<String>) {
        let model_id = model_id.unwrap_or_else(|| "unknown".to_string());
        for event in &mut self.events {
            if event.event_type == EVENT_UPSTREAM_VERIFIED {
                event
                    .fields
                    .insert("model_id".to_string(), Value::String(model_id));
                return;
            }
        }
    }

    /// Serialize the payload once, sign the exact bytes, and return the
    /// finalized receipt. The signing key MUST be an attested Ed25519 receipt
    /// key (§8.2); other algorithms are rejected.
    pub fn finalize(
        self,
        keys: &dyn KeyProvider,
        key_id: &str,
    ) -> Result<SignedReceipt, ReceiptError> {
        if self.events.is_empty() {
            return Err(ReceiptError::EmptyReceipt);
        }
        // A refusal receipt (§8.5: a failed `upstream.verified` accompanying an
        // `upstream_verification_failed` error) never forwarded the prompt, so
        // `request.forwarded` is required only on the inference path.
        let refused = self.events.iter().any(|e| {
            e.event_type == EVENT_UPSTREAM_VERIFIED
                && e.fields.get("result").and_then(Value::as_str)
                    == Some(VerificationResult::Failed.as_str())
        });
        for required in [
            EVENT_REQUEST_RECEIVED,
            EVENT_REQUEST_FORWARDED,
            EVENT_RESPONSE_RETURNED,
        ] {
            if required == EVENT_REQUEST_FORWARDED && refused {
                continue;
            }
            if !self.events.iter().any(|e| e.event_type == required) {
                return Err(ReceiptError::MissingRequiredEvent(required));
            }
        }

        let receipt_key = keys
            .receipt_keys()
            .into_iter()
            .find(|k| k.key_id == key_id)
            .ok_or_else(|| KeyError::UnknownReceiptKeyId(key_id.to_string()))?;
        if receipt_key.algo != ALGO_ED25519 {
            return Err(ReceiptError::Key(KeyError::UnsupportedAlgo(
                receipt_key.algo,
            )));
        }

        let payload = serde_json::to_vec(&ReceiptPayload {
            api_version: RECEIPT_API_VERSION,
            receipt_id: &self.receipt_id,
            chat_id: &self.chat_id,
            model: &self.model,
            workload_keyset_digest: &self.workload_keyset_digest,
            endpoint: &self.endpoint,
            method: &self.method,
            served_at: self.served_at,
            event_log: self.events.iter().map(ReceiptEvent::to_value).collect(),
        })?;
        let signature = keys.sign_receipt(&receipt_key.key_id, &payload)?;

        Ok(SignedReceipt {
            receipt_id: self.receipt_id,
            chat_id: self.chat_id,
            payload,
            key_id: receipt_key.key_id,
            algo: receipt_key.algo,
            signature_hex: hex::encode(signature),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::keys::verify_receipt_signature;
    use crate::aci::types::{KeyedPublicKey, TlsSpki};
    use ed25519_dalek::SigningKey;

    struct TestKeys {
        key: SigningKey,
    }

    impl TestKeys {
        fn new() -> Self {
            Self {
                key: SigningKey::from_bytes(&[0x11; 32]),
            }
        }

        fn public_entry(&self) -> KeyedPublicKey {
            KeyedPublicKey {
                key_id: "test-ed25519".to_string(),
                algo: ALGO_ED25519.to_string(),
                public_key_hex: hex::encode(self.key.verifying_key().as_bytes()),
            }
        }
    }

    impl KeyProvider for TestKeys {
        fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
            vec![self.public_entry()]
        }

        fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
            if key_id != "test-ed25519" {
                return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
            }
            use ed25519_dalek::Signer;
            Ok(self.key.sign(payload).to_bytes().to_vec())
        }

        fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
            Vec::new()
        }

        fn tls_spkis(&self) -> Vec<TlsSpki> {
            Vec::new()
        }

        fn is_test_only(&self) -> bool {
            true
        }
    }

    fn verified_event() -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: "phala-direct".to_string(),
            model_id: "demo-model".to_string(),
            verifier_id: "test/v1".to_string(),
            result: VerificationResult::Verified,
            required: true,
            ..Default::default()
        }
    }

    fn build_receipt(keys: &TestKeys) -> SignedReceipt {
        let mut builder = ReceiptBuilder::new(
            "rcpt-1".to_string(),
            Some("chatcmpl-1".to_string()),
            Some("demo-model".to_string()),
            format!("sha256:{}", "ab".repeat(32)),
            "/v1/chat/completions".to_string(),
            "POST".to_string(),
            1_750_000_000,
        );
        builder
            .add_request_received(b"{\"model\":\"demo-model\"}")
            .unwrap();
        builder
            .add_request_forwarded(b"{\"model\":\"demo-model\"}")
            .unwrap();
        builder
            .add_upstream_verified_with_session(
                &verified_event(),
                &format!("sha256:{}", "cd".repeat(32)),
            )
            .unwrap();
        builder
            .add_response_returned(b"{\"id\":\"chatcmpl-1\"}")
            .unwrap();
        builder.finalize(keys, "test-ed25519").unwrap()
    }

    #[test]
    fn payload_bytes_are_signed_and_envelope_round_trips() {
        let keys = TestKeys::new();
        let receipt = build_receipt(&keys);

        let signature = hex::decode(&receipt.signature_hex).unwrap();
        assert!(verify_receipt_signature(
            &keys.public_entry(),
            &receipt.payload,
            &signature
        ));

        // The envelope's payload_b64 decodes to the exact stored bytes.
        let envelope = receipt.envelope();
        let decoded = BASE64
            .decode(envelope["payload_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, receipt.payload);
        assert_eq!(envelope["algo"], "ed25519");
        assert_eq!(envelope["key_id"], "test-ed25519");

        // A single flipped payload byte invalidates the signature.
        let mut tampered = receipt.payload.clone();
        tampered[0] ^= 1;
        assert!(!verify_receipt_signature(
            &keys.public_entry(),
            &tampered,
            &signature
        ));
    }

    #[test]
    fn payload_has_spec_shape_and_event_order() {
        let keys = TestKeys::new();
        let receipt = build_receipt(&keys);
        let payload = receipt.payload_json().unwrap();

        assert_eq!(payload["api_version"], "aci/1");
        assert_eq!(payload["receipt_id"], "rcpt-1");
        assert_eq!(payload["model"], "demo-model");
        assert!(payload.get("workload_id").is_none(), "no workload_id field");
        let events = payload["event_log"].as_array().unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0]["type"], "request.received");
        assert!(events[0].get("seq").is_none(), "events carry no seq");
        assert_eq!(events[2]["type"], "upstream.verified");
        assert_eq!(events[2]["result"], "verified");
        assert_eq!(
            events[2]["session_id"],
            format!("sha256:{}", "cd".repeat(32))
        );
        assert!(events[2].get("reason").is_none());
        assert_eq!(events[3]["type"], "response.returned");
        assert!(events[3]["body_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));

        // Payload text starts with the spec member order.
        let text = String::from_utf8(receipt.payload.clone()).unwrap();
        assert!(text.starts_with(r#"{"api_version":"aci/1","receipt_id":"rcpt-1","chat_id":"#));
    }

    #[test]
    fn failed_event_carries_reason_and_never_a_session() {
        let keys = TestKeys::new();
        let mut builder = ReceiptBuilder::new(
            "rcpt-2".to_string(),
            None,
            None,
            "sha256:00".to_string(),
            "/v1/chat/completions".to_string(),
            "POST".to_string(),
            1,
        );
        builder.add_request_received(b"x").unwrap();
        builder.add_request_forwarded(b"x").unwrap();
        let event = UpstreamVerifiedEvent {
            upstream_name: "tinfoil".to_string(),
            model_id: "m".to_string(),
            required: true,
            reason: Some("quote verification failed".to_string()),
            ..Default::default()
        };
        builder.add_upstream_verified_failed(&event).unwrap();
        builder.add_response_returned(b"err").unwrap();
        let receipt = builder.finalize(&keys, "test-ed25519").unwrap();

        let payload = receipt.payload_json().unwrap();
        let verified = &payload["event_log"][2];
        assert_eq!(verified["result"], "failed");
        assert_eq!(verified["reason"], "quote verification failed");
        assert_eq!(verified["upstream_name"], "tinfoil");
        assert!(verified.get("session_id").is_none());
    }

    #[test]
    fn finalize_enforces_required_events_and_ordering() {
        let keys = TestKeys::new();

        // First event must be request.received.
        let mut builder = ReceiptBuilder::new(
            "r".into(),
            None,
            None,
            "sha256:00".into(),
            "/v1/chat/completions".into(),
            "POST".into(),
            1,
        );
        assert!(matches!(
            builder.add_request_forwarded(b"x"),
            Err(ReceiptError::FirstEventMustBeRequestReceived(_))
        ));

        // Missing response.returned is rejected at finalize.
        builder.add_request_received(b"x").unwrap();
        builder.add_request_forwarded(b"x").unwrap();
        assert!(matches!(
            builder.finalize(&keys, "test-ed25519"),
            Err(ReceiptError::MissingRequiredEvent(EVENT_RESPONSE_RETURNED))
        ));
    }

    #[test]
    fn extension_events_cannot_reuse_required_types() {
        let mut builder = ReceiptBuilder::new(
            "r".into(),
            None,
            None,
            "sha256:00".into(),
            "/v1/chat/completions".into(),
            "POST".into(),
            1,
        );
        builder.add_request_received(b"x").unwrap();
        assert!(matches!(
            builder.add_extension_event(EVENT_RESPONSE_RETURNED, Map::new()),
            Err(ReceiptError::ReservedEventType(_))
        ));
        let mut fields = Map::new();
        fields.insert("type".to_string(), Value::String("smuggled".to_string()));
        assert!(matches!(
            builder.add_extension_event("custom.event", fields),
            Err(ReceiptError::ReservedField(_))
        ));
    }
}
