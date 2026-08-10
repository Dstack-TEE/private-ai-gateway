//! ACI aggregator service.
//!
//! `AciService` is thin:
//!
//! * `attestation_report(nonce)` builds a fresh report over the sealed keyset.
//! * `forward_chat_completion(...)` runs the receipt-issuing hot path for
//!   buffered responses.
//! * `forward_chat_completion_stream_request(...)` runs the same path
//!   for SSE responses and hashes bytes incrementally until the stream
//!   ends.
//! * `get_receipt(...)` returns a previously-issued receipt by id.
//!
//! Requests constrained by `provider.aci_verified` (or served on a TEE-only
//! endpoint) are fail-closed: when no verifier event is supplied for the
//! chosen attested upstream, the service refuses to forward sensitive bytes
//! and surfaces [`UpstreamVerificationError`], which the HTTP layer answers
//! with `upstream_verification_failed` plus a refusal receipt (§7.5).

use std::sync::{Arc, RwLock};

use crate::aci::identity::SealedWorkloadKeyset;
use crate::aci::keys::{KeyProvider, Quoter};
use crate::aci::types::WorkloadKeyset;
use crate::aci::upstream::UpstreamBackend;
use crate::aggregator::metrics::{MetricsSnapshot, ServiceMetrics};
use crate::aggregator::session_store::{InMemorySessionStore, SessionStore};

pub const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
pub const COMPLETIONS_PATH: &str = "/v1/completions";
pub const EMBEDDINGS_PATH: &str = "/v1/embeddings";
pub const MESSAGES_PATH: &str = "/v1/messages";
pub const RESPONSES_PATH: &str = "/v1/responses";
const CHANNEL_BINDING_REVERIFY_ATTEMPTS: usize = 2;

mod claims;
mod clock;
mod config;
mod e2ee;
mod e2ee_crypto;
mod errors;
mod forward;
mod helpers;
mod middleware;
mod receipt_store;
mod receipts;
mod streaming;
mod wire;

pub use clock::{Clock, FixedClock, SystemClock};
pub use config::{
    validate_source_provenance, AciServiceConfig, ReceiptOwner, DEFAULT_KEYSET_NOT_AFTER_SECONDS,
};
pub use errors::{E2eeError, ServiceError, UpstreamVerificationError};
pub use receipt_store::{InMemoryReceiptStore, ReceiptStore};
pub use wire::{
    ChatCompletionRequest, E2eePreparedRequest, E2eeRequestContext, E2eeRequestParts,
    E2eeResponseInfo, ForwardCandidate, ForwardResult, GatewayRequestContext,
    LegacySignatureResult, MiddlewareAllFailed, MiddlewareForwardResult, MiddlewareForwarded,
    MiddlewareGeneratedFinalization, MiddlewareReceiptDraft, MiddlewareReceiptFinalization,
    MiddlewareReceiptJournal, MiddlewareStreamFinalization, MiddlewareStreamingForwarded,
    MiddlewareUpstreamError, ServiceResponseStream, StreamingForwardResult, StreamingForwardStream,
    StreamingUpstreamError, UpstreamVerificationRequest, UpstreamVerifier,
};

pub struct AciService {
    keys: Arc<dyn KeyProvider>,
    quoter: Arc<dyn Quoter>,
    upstream: Arc<dyn UpstreamBackend>,
    upstream_verifier: Option<Arc<dyn UpstreamVerifier>>,
    receipt_store: Arc<dyn ReceiptStore>,
    session_store: Arc<dyn SessionStore>,
    keyset: SealedWorkloadKeyset,
    default_receipt_key_id: String,
    config: AciServiceConfig,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    e2ee_replay: RwLock<std::collections::HashMap<E2eeReplayKey, u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct E2eeReplayKey {
    client_public_key_hex: String,
    model_public_key_hex: String,
    nonce: String,
}

impl AciService {
    pub fn new(
        keys: Arc<dyn KeyProvider>,
        quoter: Arc<dyn Quoter>,
        upstream: Arc<dyn UpstreamBackend>,
        receipt_store: Arc<dyn ReceiptStore>,
        config: AciServiceConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ServiceError> {
        Self::new_inner(keys, quoter, upstream, None, receipt_store, config, clock)
    }

    pub fn new_with_upstream_verifier(
        keys: Arc<dyn KeyProvider>,
        quoter: Arc<dyn Quoter>,
        upstream: Arc<dyn UpstreamBackend>,
        upstream_verifier: Arc<dyn UpstreamVerifier>,
        receipt_store: Arc<dyn ReceiptStore>,
        config: AciServiceConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ServiceError> {
        Self::new_inner(
            keys,
            quoter,
            upstream,
            Some(upstream_verifier),
            receipt_store,
            config,
            clock,
        )
    }

    fn new_inner(
        keys: Arc<dyn KeyProvider>,
        quoter: Arc<dyn Quoter>,
        upstream: Arc<dyn UpstreamBackend>,
        upstream_verifier: Option<Arc<dyn UpstreamVerifier>>,
        receipt_store: Arc<dyn ReceiptStore>,
        config: AciServiceConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ServiceError> {
        if keys.is_test_only() && !config.allow_test_keys {
            return Err(ServiceError::TestKeysInProduction);
        }
        validate_source_provenance(&config.source_provenance)?;

        let tls_public_keys = config
            .tls_public_keys
            .clone()
            .unwrap_or_else(|| keys.tls_spkis());
        let unsealed = WorkloadKeyset {
            subject: config.subject.clone(),
            not_after: config.keyset_not_after,
            receipt_signing_keys: keys.receipt_keys(),
            e2ee_public_keys: keys.e2ee_keys(),
            tls_public_keys,
        };
        validate_keyset(&unsealed, &config)?;
        // Sealed once: these exact bytes (and their digest) are what every
        // report serves for the lifetime of the process (Appendix A, §3.1).
        let keyset = SealedWorkloadKeyset::seal(unsealed)
            .map_err(|e| ServiceError::Keyset(e.to_string()))?;

        let default_receipt_key_id = keys
            .receipt_keys()
            .first()
            .ok_or(ServiceError::NoReceiptKey)?
            .key_id
            .clone();

        Ok(Self {
            keys,
            quoter,
            upstream,
            upstream_verifier,
            receipt_store,
            session_store: Arc::new(InMemorySessionStore::default()),
            keyset,
            default_receipt_key_id,
            config,
            clock,
            metrics: Arc::new(
                ServiceMetrics::new().map_err(|e| ServiceError::Metrics(e.to_string()))?,
            ),
            e2ee_replay: RwLock::new(std::collections::HashMap::new()),
        })
    }

    /// Swap in a durable session store (e.g. [`crate::aggregator::session_store::JsonlSessionStore`]).
    /// Defaults to an in-memory store, which keeps the prior no-persistence behavior.
    pub fn with_session_store(mut self, session_store: Arc<dyn SessionStore>) -> Self {
        self.session_store = session_store;
        self
    }

    pub fn workload_keyset_digest(&self) -> &str {
        self.keyset.digest()
    }

    pub fn keyset(&self) -> &WorkloadKeyset {
        self.keyset.keyset()
    }

    /// Inference runs inside this attested workload: no upstream hop, so no
    /// `upstream.verified` event and no attested sessions (§7.5, §8).
    pub fn serves_directly(&self) -> bool {
        self.config.service_capabilities.serving == "direct"
    }

    /// The keyset object the report serves as `workload_keyset` (§4.1).
    pub fn keyset_value(&self) -> serde_json::Value {
        self.keyset.to_value()
    }

    pub fn upstream(&self) -> &dyn UpstreamBackend {
        self.upstream.as_ref()
    }

    pub fn metrics(&self) -> Result<MetricsSnapshot, ServiceError> {
        self.metrics
            .render()
            .map_err(|e| ServiceError::Metrics(e.to_string()))
    }
}

/// Keyset seal-time rules a library consumer could otherwise violate: this
/// v2-capable gateway must list an E2EE v2 §4 key, and keys must be distinct
/// per role. (The shipped launcher satisfies both by construction.)
fn validate_keyset(
    keyset: &WorkloadKeyset,
    _config: &AciServiceConfig,
) -> Result<(), ServiceError> {
    use crate::aci::digest::sha256_raw;
    use crate::aci::e2ee::is_aci_e2ee_suite;

    // The reference gateway always provisions at least one recognized E2EE v2
    // §4 suite, even while extension termination is explicitly disabled.
    // X25519 is recommended, not required; existing v2 clients may select the
    // secp256k1 suite on its own.
    if !keyset
        .e2ee_public_keys
        .iter()
        .any(|key| is_aci_e2ee_suite(&key.algo))
    {
        return Err(ServiceError::Keyset(
            "e2ee_public_keys has no recognized E2EE v2 suite (E2EE v2 spec §4)".to_string(),
        ));
    }
    for receipt_key in &keyset.receipt_signing_keys {
        if keyset
            .e2ee_public_keys
            .iter()
            .any(|e2ee_key| e2ee_key.public_key_hex == receipt_key.public_key_hex)
        {
            return Err(ServiceError::Keyset(format!(
                "receipt signing key {:?} doubles as an E2EE key; keys must be distinct \
                 per role (§3.1)",
                receipt_key.key_id
            )));
        }
    }
    // §3.1: nor may a receipt or E2EE key double as a TLS key. The TLS role
    // is published as SPKI digests, and the DER SPKI of an Ed25519/X25519
    // raw key is deterministic, so the digest each keyset key WOULD have as
    // a TLS entry is computable exactly.
    for (role, key, der_prefix) in keyset
        .receipt_signing_keys
        .iter()
        .map(|k| {
            (
                "receipt",
                k,
                &b"\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00"[..],
            )
        })
        .chain(keyset.e2ee_public_keys.iter().map(|k| {
            (
                "E2EE",
                k,
                &b"\x30\x2a\x30\x05\x06\x03\x2b\x65\x6e\x03\x21\x00"[..],
            )
        }))
    {
        let Ok(raw) = hex::decode(&key.public_key_hex) else {
            continue;
        };
        if raw.len() != 32 {
            continue;
        }
        let spki = [der_prefix, &raw].concat();
        let spki_digest = hex::encode(sha256_raw(&spki));
        if keyset
            .tls_public_keys
            .iter()
            .any(|tls| tls.spki_sha256_hex.eq_ignore_ascii_case(&spki_digest))
        {
            return Err(ServiceError::Keyset(format!(
                "{role} key {:?} doubles as a TLS key; keys must be distinct per role (§3.1)",
                key.key_id
            )));
        }
    }
    Ok(())
}
