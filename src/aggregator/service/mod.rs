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
//! Upstream verification is **fail-closed** (§1.2): when service policy
//! requires verification for a route and the verifier does not produce an
//! enforceable verified binding, the service refuses to forward sensitive
//! bytes and surfaces [`UpstreamVerificationError`], which the HTTP layer
//! answers with `upstream_verification_failed` plus a refusal receipt.

use std::sync::Arc;

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
    ForwardCandidate, ForwardResult, GatewayRequestContext, LegacySignatureResult,
    MiddlewareForwardResult, MiddlewareForwarded, MiddlewareGeneratedFinalization,
    MiddlewareReceiptDraft, MiddlewareReceiptFinalization, MiddlewareReceiptJournal,
    MiddlewareStreamFinalization, MiddlewareStreamingForwarded, ServiceResponseStream,
    StreamingForwardResult, StreamingForwardStream, StreamingUpstreamError,
    UpstreamVerificationRequest, UpstreamVerifier,
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
        // report serves for the lifetime of the process (§3, §4.1).
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

    /// The exact keyset bytes the report serves as `workload_keyset_b64`.
    pub fn keyset_bytes(&self) -> &[u8] {
        self.keyset.bytes()
    }

    pub fn upstream(&self) -> &dyn UpstreamBackend {
        self.upstream.as_ref()
    }

    pub fn metrics(&self) -> Result<MetricsSnapshot, ServiceError> {
        self.metrics
            .render()
            .map_err(|e| ServiceError::Metrics(e.to_string()))
    }

    pub fn upstream_required_default(&self) -> bool {
        self.config.upstream_required_default
    }
}

/// §4.1 seal-time rules a library consumer could otherwise violate: a service
/// that terminates E2EE must list a §7.1 key, and keys must be distinct per
/// role. (The shipped launcher satisfies both by construction.)
fn validate_keyset(keyset: &WorkloadKeyset, config: &AciServiceConfig) -> Result<(), ServiceError> {
    use crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM;

    let supports_e2ee = !config
        .service_capabilities
        .supported_e2ee_versions
        .is_empty();
    if supports_e2ee
        && !keyset
            .e2ee_public_keys
            .iter()
            .any(|key| key.algo == E2EE_ALGO_X25519_AESGCM)
    {
        return Err(ServiceError::Keyset(format!(
            "supported_e2ee_versions is non-empty but e2ee_public_keys has no \
             {E2EE_ALGO_X25519_AESGCM} entry (§4.1)"
        )));
    }
    for receipt_key in &keyset.receipt_signing_keys {
        if keyset
            .e2ee_public_keys
            .iter()
            .any(|e2ee_key| e2ee_key.public_key_hex == receipt_key.public_key_hex)
        {
            return Err(ServiceError::Keyset(format!(
                "receipt signing key {:?} doubles as an E2EE key; keys must be distinct \
                 per role (§4.1)",
                receipt_key.key_id
            )));
        }
    }
    Ok(())
}
