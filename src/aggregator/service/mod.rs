//! ACI aggregator service.
//!
//! `AciService` is thin:
//!
//! * `attestation_report(nonce)` builds a fresh report.
//! * `forward_chat_completion(...)` runs the ACI §3 hot path for
//!   buffered responses.
//! * `forward_chat_completion_stream_request(...)` runs the same path
//!   for SSE responses and hashes bytes incrementally until the stream
//!   ends.
//! * `get_receipt(...)` returns a previously-issued receipt by id.
//!
//! Upstream verification is **fail-closed by default**. If
//! `X-Upstream-Verification: required` (the default) and no verifier
//! event is supplied for the chosen upstream, the service refuses to
//! forward sensitive bytes and surfaces
//! [`UpstreamVerificationError`].

use std::sync::{Arc, RwLock};

use crate::aci::identity;
use crate::aci::keys::{KeyProvider, Quoter};
use crate::aci::types::{WorkloadIdentity, WorkloadKeyset};
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
mod receipt_store;
mod receipts;
mod streaming;
mod wire;

pub use clock::*;
pub use config::*;
pub use errors::*;
pub use receipt_store::*;
pub use wire::*;

pub struct AciService {
    keys: Arc<dyn KeyProvider>,
    quoter: Arc<dyn Quoter>,
    upstream: Arc<dyn UpstreamBackend>,
    upstream_verifier: Option<Arc<dyn UpstreamVerifier>>,
    receipt_store: Arc<dyn ReceiptStore>,
    session_store: Arc<dyn SessionStore>,
    keyset: WorkloadKeyset,
    workload_id: String,
    workload_keyset_digest: String,
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

/// Input supplied to the attested upstream verifier before any sensitive
/// bytes are forwarded.
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

        let identity = WorkloadIdentity {
            public_key: keys.identity_public_key(),
            subject: config.identity_subject.clone(),
        };
        let tls_public_keys = config
            .tls_public_keys
            .clone()
            .unwrap_or_else(|| keys.tls_spkis());
        let keyset = WorkloadKeyset {
            workload_identity: identity,
            keyset_epoch: config.keyset_epoch.clone(),
            receipt_signing_keys: keys.receipt_keys(),
            e2ee_public_keys: keys.e2ee_keys(),
            tls_public_keys,
        };

        let workload_id = identity::workload_id(&keyset.workload_identity)?;
        let workload_keyset_digest = identity::workload_keyset_digest(&keyset)?;

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
            workload_id,
            workload_keyset_digest,
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

    pub fn workload_id(&self) -> &str {
        &self.workload_id
    }

    pub fn workload_keyset_digest(&self) -> &str {
        &self.workload_keyset_digest
    }

    pub fn keyset(&self) -> &WorkloadKeyset {
        &self.keyset
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
