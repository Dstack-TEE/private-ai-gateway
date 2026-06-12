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

use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::aci::canonical::CanonicalError;
use crate::aci::e2ee::{
    encrypt_for_public_key, encrypt_legacy_for_public_key, normalize_secp256k1_public_key_hex,
    E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519, E2EE_ALGO_SECP256K1_AESGCM, E2EE_VERSION_V1,
    E2EE_VERSION_V2,
};
use crate::aci::identity::{self, attestation_statement, report_data};
use crate::aci::keys::{KeyError, KeyProvider, LegacySignature, Quoter, LEGACY_ALGO_ECDSA};
use crate::aci::receipt::{
    ReceiptBuilder, ReceiptError, TransparencyEventKind, UpstreamVerifiedEvent, VerificationResult,
    EVENT_REQUEST_RECEIVED, EVENT_RESPONSE_RETURNED,
};
use crate::aci::types::{
    AttestationEnvelope, AttestationReport, Freshness, KeysetEndorsement, KeysetEpoch, Receipt,
    ServiceCapabilities, SourceProvenance, TlsSpki, WorkloadIdentity, WorkloadKeyset,
};
use crate::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamBodyStream, UpstreamError, UpstreamRequest,
};
use crate::aggregator::metrics::{MetricsSnapshot, RequestMode, ServiceMetrics, StreamErrorKind};
use crate::aggregator::session::{
    AttestedSession, Claim, ClaimSource, EvidenceRef, SessionClaims, WorkloadIdentityRef,
};
use crate::aggregator::session_store::{InMemorySessionStore, SessionStore};
use crate::aggregator::upstream_config::UpstreamSessionSink;

pub const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
pub const COMPLETIONS_PATH: &str = "/v1/completions";
pub const EMBEDDINGS_PATH: &str = "/v1/embeddings";
pub const MESSAGES_PATH: &str = "/v1/messages";
pub const RESPONSES_PATH: &str = "/v1/responses";
const CHANNEL_BINDING_REVERIFY_ATTEMPTS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(
        "refusing to start AciService with a test-only KeyProvider; set \
         allow_test_keys only in tests"
    )]
    TestKeysInProduction,
    #[error(
        "ACI §5.2 requires at least one source provenance arm: \
         (repo_url + repo_commit) or image_digest"
    )]
    InvalidSourceProvenance,
    #[error("upstream verification failed: {0}")]
    UpstreamVerification(#[from] UpstreamVerificationError),
    #[error("E2EE request failed: {0}")]
    E2ee(#[from] E2eeError),
    #[error("canonicalisation error: {0}")]
    Canonical(#[from] CanonicalError),
    #[error("key provider error: {0}")]
    Key(#[from] KeyError),
    #[error("receipt builder error: {0}")]
    Receipt(#[from] ReceiptError),
    #[error("upstream error: {0}")]
    Upstream(#[from] UpstreamError),
    #[error("metrics error: {0}")]
    Metrics(String),
    #[error("missing receipt signing key in keyset")]
    NoReceiptKey,
}

#[derive(Debug, thiserror::Error, Clone)]
pub enum UpstreamVerificationError {
    #[error("upstream verification required but no verifier result supplied")]
    NoVerifierResult,
    #[error("upstream verifier reported failed: {0}")]
    VerifierFailed(String),
}

#[derive(Debug, thiserror::Error, Clone)]
pub enum E2eeError {
    #[error("missing E2EE header")]
    HeaderMissing,
    #[error("invalid E2EE signing algorithm")]
    InvalidSigningAlgo,
    #[error("invalid E2EE version")]
    InvalidVersion,
    #[error("invalid E2EE public key")]
    InvalidPublicKey,
    #[error("X-Model-Pub-Key does not match this ACI service")]
    ModelKeyMismatch,
    #[error("invalid E2EE nonce")]
    InvalidNonce,
    #[error("E2EE replay detected")]
    ReplayDetected,
    #[error("invalid E2EE timestamp")]
    InvalidTimestamp,
    #[error("invalid E2EE payload model")]
    InvalidPayloadModel,
    #[error("E2EE decryption failed")]
    DecryptionFailed,
    #[error("E2EE encryption failed")]
    EncryptionFailed,
}

pub struct E2eeRequestParts<'a> {
    pub signing_algo: Option<&'a str>,
    pub client_public_key: Option<&'a str>,
    pub model_public_key: Option<&'a str>,
    pub version: Option<&'a str>,
    pub nonce: Option<&'a str>,
    pub timestamp: Option<&'a str>,
}

pub struct E2eePreparedRequest {
    pub decrypted_body: Vec<u8>,
    pub context: E2eeRequestContext,
}

#[derive(Debug, Clone)]
pub struct E2eeRequestContext {
    version: String,
    algo: String,
    aad_mode: E2eeAadMode,
    request_model: String,
    client_public_key_hex: String,
    nonce: Option<String>,
    timestamp: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum E2eeAadMode {
    AciV2,
    LegacyV2,
    LegacyV1,
}

impl E2eeAadMode {
    fn uses_aad(self) -> bool {
        matches!(self, Self::AciV2 | Self::LegacyV2)
    }
}

enum E2eeDecryptor<'a> {
    AciV2 { key_id: &'a str },
    Legacy { signing_algo: &'a str },
}

/// Validate ACI §5.2 source-provenance arms.
pub fn validate_source_provenance(sp: &SourceProvenance) -> Result<(), ServiceError> {
    let has_repo = sp.repo_url.as_deref().is_some_and(|s| !s.is_empty())
        && sp.repo_commit.as_deref().is_some_and(|s| !s.is_empty());
    let has_image = sp.image_digest.as_deref().is_some_and(|s| !s.is_empty());
    if has_repo || has_image {
        Ok(())
    } else {
        Err(ServiceError::InvalidSourceProvenance)
    }
}

fn normalize_downstream_domain(raw: &str) -> Option<String> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.contains('/')
        || domain.contains(':')
        || domain.contains('=')
        || domain.contains(',')
        || domain.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(domain)
}

/// Configuration accepted by [`AciService::new`].
pub struct AciServiceConfig {
    pub vendor: String,
    pub tee_type: String,
    pub source_provenance: SourceProvenance,
    pub keyset_epoch: KeysetEpoch,
    pub identity_subject: Option<String>,
    pub service_capabilities: ServiceCapabilities,
    pub freshness_seconds: u64,
    /// How long receipts stay queryable in the in-memory store.
    pub receipt_ttl_seconds: u64,
    pub upstream_required_default: bool,
    pub allow_test_keys: bool,
    /// Overrides the TLS-SPKI digests reported by the key provider.
    /// Production deployments should derive this from the mounted
    /// client-facing TLS certificate path when plaintext HTTPS
    /// terminates for this workload.
    pub tls_public_keys: Option<Vec<TlsSpki>>,
}

impl AciServiceConfig {
    pub fn for_test(vendor: &str) -> Self {
        Self {
            vendor: vendor.to_string(),
            tee_type: "tdx".to_string(),
            source_provenance: SourceProvenance {
                repo_url: Some("https://github.com/Dstack-TEE/private-ai-gateway".to_string()),
                repo_commit: Some("deadbeef".to_string()),
                image_digest: None,
                image_provenance: None,
            },
            keyset_epoch: KeysetEpoch {
                version: 1,
                not_after: 2_000_000_000,
            },
            identity_subject: None,
            service_capabilities: ServiceCapabilities::default(),
            freshness_seconds: 3600,
            receipt_ttl_seconds: 3600,
            upstream_required_default: true,
            allow_test_keys: true,
            tls_public_keys: None,
        }
    }
}

/// Identifier the service records alongside a receipt so a relying party
/// can prove it was the original requester (ACI §9.1, §9.5).
///
/// The aggregator never stores raw bearer tokens; it stores the SHA-256
/// digest of whatever credential the requester presented at chat time.
/// Lookups must present the same credential, whose digest must match.
/// Receipts with `None` owner are anonymous and publicly retrievable;
/// in production a deployment should require auth on `POST
/// /v1/chat/completions`, after which every receipt is owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptOwner {
    pub auth_token_sha256: String,
}

impl ReceiptOwner {
    /// Build the receipt-owner record from a raw `Authorization: Bearer ...`
    /// token. The raw bytes are hashed immediately and never kept.
    pub fn from_bearer(token: &str) -> Self {
        Self {
            auth_token_sha256: crate::aci::canonical::sha256_hex(token.as_bytes()),
        }
    }
}

/// Stored signed receipts. The default in-memory implementation is enough for
/// the prototype; a durable store comes in a follow-up. The gateway never
/// stores request bodies — only the receipt (which holds hashes, not content).
pub trait ReceiptStore: Send + Sync {
    /// Store a signed receipt. `owner` is the requester's hashed bearer
    /// credential, or `None` for anonymous calls. The store MUST keep
    /// the owner alongside the receipt so lookups can authenticate.
    fn put(&self, receipt: Receipt, owner: Option<ReceiptOwner>, expires_at: u64);
    fn get_by_receipt_id(&self, receipt_id: &str, now: u64) -> Option<Receipt>;
    fn get_by_chat_id(&self, chat_id: &str, now: u64) -> Option<Receipt>;
    /// Return the owner recorded at `put` time, if any.
    fn owner_of(&self, receipt_id: &str, now: u64) -> Option<ReceiptOwner>;
}

#[derive(Default)]
pub struct InMemoryReceiptStore {
    inner: RwLock<InMemoryReceiptStoreInner>,
}

#[derive(Default)]
struct InMemoryReceiptStoreInner {
    by_receipt: std::collections::HashMap<String, StoredReceipt>,
    by_chat: std::collections::HashMap<String, String>,
}

struct StoredReceipt {
    receipt: Receipt,
    owner: Option<ReceiptOwner>,
    expires_at: u64,
}

impl ReceiptStore for InMemoryReceiptStore {
    fn put(&self, receipt: Receipt, owner: Option<ReceiptOwner>, expires_at: u64) {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        if let Some(cid) = receipt.chat_id.clone() {
            guard.by_chat.insert(cid, receipt.receipt_id.clone());
        }
        guard.by_receipt.insert(
            receipt.receipt_id.clone(),
            StoredReceipt {
                receipt,
                owner,
                expires_at,
            },
        );
    }

    fn get_by_receipt_id(&self, receipt_id: &str, now: u64) -> Option<Receipt> {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        let expires_at = guard.by_receipt.get(receipt_id)?.expires_at;
        if now >= expires_at {
            remove_receipt_locked(&mut guard, receipt_id);
            return None;
        }
        guard
            .by_receipt
            .get(receipt_id)
            .map(|entry| entry.receipt.clone())
    }

    fn get_by_chat_id(&self, chat_id: &str, now: u64) -> Option<Receipt> {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        let receipt_id = guard.by_chat.get(chat_id)?.clone();
        let expires_at = guard.by_receipt.get(&receipt_id)?.expires_at;
        if now >= expires_at {
            remove_receipt_locked(&mut guard, &receipt_id);
            return None;
        }
        guard
            .by_receipt
            .get(&receipt_id)
            .map(|entry| entry.receipt.clone())
    }

    fn owner_of(&self, receipt_id: &str, now: u64) -> Option<ReceiptOwner> {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        let expires_at = guard.by_receipt.get(receipt_id)?.expires_at;
        if now >= expires_at {
            remove_receipt_locked(&mut guard, receipt_id);
            return None;
        }
        guard
            .by_receipt
            .get(receipt_id)
            .and_then(|entry| entry.owner.clone())
    }
}

fn remove_receipt_locked(inner: &mut InMemoryReceiptStoreInner, receipt_id: &str) {
    if let Some(entry) = inner.by_receipt.remove(receipt_id) {
        if let Some(chat_id) = entry.receipt.chat_id {
            inner.by_chat.remove(&chat_id);
        }
    }
}

/// Returned by [`AciService::forward_chat_completion`].
#[derive(Debug, Clone)]
pub struct ForwardResult {
    pub receipt: Receipt,
    pub upstream_status: u16,
    pub upstream_body: Vec<u8>,
    pub upstream_headers: std::collections::HashMap<String, String>,
    pub e2ee: Option<E2eeResponseInfo>,
}

pub enum MiddlewareForwardResult {
    Forwarded(Box<MiddlewareForwarded>),
    Stream(Box<MiddlewareStreamingForwarded>),
    UpstreamError(StreamingUpstreamError),
}

pub struct MiddlewareForwarded {
    pub receipt_id: String,
    pub receipt: MiddlewareReceiptDraft,
    pub upstream_status: u16,
    pub upstream_body: Vec<u8>,
    pub upstream_headers: std::collections::HashMap<String, String>,
    /// Which route served the request, how many routes were tried, and the
    /// attested session id (if any) — returned to the caller as headers.
    pub selected_route: String,
    pub attempts: usize,
    pub session_id: Option<String>,
}

pub struct MiddlewareStreamingForwarded {
    pub receipt_id: String,
    pub upstream_status: u16,
    pub upstream_headers: std::collections::HashMap<String, String>,
    pub body: ServiceResponseStream,
    /// Which route served the request, how many routes were tried, and the
    /// attested session id (if any) — returned to the caller as headers.
    pub selected_route: String,
    pub attempts: usize,
    pub session_id: Option<String>,
}

pub struct MiddlewareReceiptDraft {
    receipt_id: String,
    builder: ReceiptBuilder,
    provider_response_hash: String,
    endpoint_path: String,
    request_mode: RequestMode,
    response_model: Option<String>,
}

#[derive(Clone, Default)]
pub struct MiddlewareReceiptJournal {
    inner: Arc<Mutex<MiddlewareReceiptJournalState>>,
}

#[derive(Default)]
struct MiddlewareReceiptJournalState {
    receipt_id: Option<String>,
    draft: Option<MiddlewareReceiptDraft>,
}

impl MiddlewareReceiptJournal {
    pub fn reserve_receipt_id(&self, receipt_id: String) {
        self.inner
            .lock()
            .expect("middleware receipt journal poisoned")
            .receipt_id = Some(receipt_id);
    }

    pub fn set(&self, draft: MiddlewareReceiptDraft) {
        let mut inner = self
            .inner
            .lock()
            .expect("middleware receipt journal poisoned");
        inner.receipt_id = Some(draft.receipt_id.clone());
        inner.draft = Some(draft);
    }

    pub fn take(&self) -> Option<MiddlewareReceiptDraft> {
        self.inner
            .lock()
            .expect("middleware receipt journal poisoned")
            .draft
            .take()
    }

    pub fn peek_receipt_id(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("middleware receipt journal poisoned")
            .receipt_id
            .clone()
    }
}

pub struct MiddlewareReceiptFinalization {
    pub receipt: Receipt,
    pub wire_body: Vec<u8>,
    pub e2ee: Option<E2eeResponseInfo>,
}

pub type ServiceResponseStream = Pin<Box<dyn Stream<Item = Result<Bytes, ServiceError>> + Send>>;

pub struct MiddlewareStreamFinalization {
    pub body: ServiceResponseStream,
    pub e2ee: Option<E2eeResponseInfo>,
}

pub struct MiddlewareGeneratedFinalization {
    pub wire_body: Vec<u8>,
    pub e2ee: Option<E2eeResponseInfo>,
}

#[derive(Debug, Clone)]
pub struct E2eeResponseInfo {
    pub version: String,
    pub algo: String,
}

/// Returned by [`AciService::forward_chat_completion_stream_request`].
pub enum StreamingForwardResult {
    Stream(StreamingForwardStream),
    UpstreamError(StreamingUpstreamError),
}

pub struct StreamingForwardStream {
    /// Receipt id reserved before the upstream stream starts. The
    /// receipt becomes queryable after the response stream finishes
    /// and the final hash is known.
    pub receipt_id: String,
    pub upstream_status: u16,
    pub upstream_headers: std::collections::HashMap<String, String>,
    pub e2ee: Option<E2eeResponseInfo>,
    pub body: Pin<Box<dyn Stream<Item = Result<Bytes, ServiceError>> + Send>>,
}

pub struct StreamingUpstreamError {
    pub upstream_status: u16,
    pub upstream_headers: std::collections::HashMap<String, String>,
    pub upstream_body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct LegacySignatureResult {
    pub text: String,
    pub signature: String,
    pub signing_address: String,
    pub signing_algo: String,
}

/// Bundle of inputs accepted by [`AciService::forward_chat_completion_request`].
///
/// Adding fields here is the path of least resistance for new
/// hot-path concerns, including request rewrites. The 4-arg
/// [`AciService::forward_chat_completion`] is a thin wrapper that
/// forwards `requester: None`.
pub struct ChatCompletionRequest<'a> {
    pub context: GatewayRequestContext,
    pub endpoint_path: &'a str,
    /// Bytes the service observed after TLS / E2EE termination.
    pub received_body: &'a [u8],
    /// Optional post-rewrite body the service will forward upstream.
    /// `None` means "forward `received_body` verbatim" and produces an
    /// `request.received.body_hash == request.forwarded.body_hash` receipt
    /// pair.
    pub forwarded_body: Option<Vec<u8>>,
    /// Override the configured default upstream-verification mode.
    pub upstream_required: Option<bool>,
    /// Verifier event already produced by the caller. When `None`,
    /// the service consults its configured `UpstreamVerifier` (if any)
    /// to compute one before forwarding.
    pub upstream_verification_event: Option<UpstreamVerifiedEvent>,
    /// Authenticated requester recorded with the receipt. Lookups must
    /// present the same credential. `None` produces an anonymous
    /// receipt that any caller can retrieve.
    pub requester: Option<ReceiptOwner>,
    pub e2ee: Option<E2eeRequestContext>,
}

#[derive(Debug, Clone, Default)]
pub struct GatewayRequestContext {
    pub request_id: String,
    pub user_model: Option<String>,
    pub target_route_id: Option<String>,
}

/// One ordered failover candidate: a route id to try plus the request
/// body to send to it. Callers may share a single body across candidates
/// or give each candidate its own body. Candidates are tried in order
/// until one succeeds.
#[derive(Debug, Clone)]
pub struct ForwardCandidate {
    pub route_id: String,
    pub body: Vec<u8>,
}

/// Provider HTTP statuses that trigger failover to the next candidate when
/// returned before the first response byte. Tentative set, subject to change.
fn is_retryable_provider_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Track the highest-priority failover error so that, when every candidate
/// fails, the returned error reflects the most informative failure.
/// Priority order: verification (3), then transport (2), then routing (1).
fn upgrade_err(slot: &mut Option<(u8, ServiceError)>, priority: u8, err: ServiceError) {
    if slot.as_ref().map(|(p, _)| priority >= *p).unwrap_or(true) {
        *slot = Some((priority, err));
    }
}

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
#[derive(Debug, Clone)]
pub struct UpstreamVerificationRequest {
    pub upstream_name: String,
    pub url_origin: Option<String>,
    pub model_id: String,
    pub forwarded_body_hash: String,
    pub required: bool,
}

/// Verifies that the selected upstream is acceptable for this request.
///
/// Production implementations cache provider attestation state and emit a
/// deterministic `verifier_id` traceable to source provenance. Tests use this
/// trait to exercise the real HTTP hot path without talking to a live upstream.
#[async_trait]
pub trait UpstreamVerifier: Send + Sync {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent;

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.invalidate(&request);
        self.verify(request).await
    }

    fn invalidate(&self, _request: &UpstreamVerificationRequest) {}
}

/// Source of `served_at` / freshness timestamps. Tests inject a
/// fixed clock; production uses [`SystemClock`].
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> u64;
}

#[derive(Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_secs(&self) -> u64 {
        self.0
    }
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

    /// Build a fresh attestation report for this service.
    pub async fn attestation_report(
        &self,
        nonce: Option<String>,
    ) -> Result<AttestationReport, ServiceError> {
        self.attestation_report_for_domain(nonce, None).await
    }

    /// Build a fresh attestation report and annotate it with the downstream
    /// TLS binding selected for `domain`, when the configured keyset contains
    /// an exact domain match.
    pub async fn attestation_report_for_domain(
        &self,
        nonce: Option<String>,
        domain: Option<&str>,
    ) -> Result<AttestationReport, ServiceError> {
        let statement = attestation_statement(&self.keyset, nonce)?;
        let rd = report_data(&statement)?;
        let quote = self.quoter.get_quote(rd).await?;

        let endorsement_payload = identity::keyset_endorsement_payload(&self.keyset)?;
        let endorsement_sig = self.keys.sign_keyset_endorsement(&endorsement_payload)?;
        let endorsement = KeysetEndorsement {
            algo: self.keys.identity_public_key().algo,
            value_hex: hex::encode(endorsement_sig),
        };

        let now = self.clock.now_secs();
        let freshness = Freshness {
            fetched_at: now,
            stale_after: now + self.config.freshness_seconds,
        };

        let mut evidence = json!({
            "quote": hex::encode(&quote.raw_quote),
            "quote_report_data": hex::encode(&quote.report_data),
            "event_log": quote.event_log,
            "vm_config": quote.vm_config,
        });
        let key_custody = self.keys.key_custody_evidence();
        if !key_custody.is_null() {
            evidence["key_custody"] = key_custody;
        }
        if let Some(binding) = domain.and_then(|domain| self.downstream_tls_binding(domain)) {
            evidence["downstream_tls_binding"] = binding;
        }

        let envelope = AttestationEnvelope {
            vendor: self.config.vendor.clone(),
            tee_type: self.config.tee_type.clone(),
            workload_keyset: self.keyset.clone(),
            report_data_hex: hex::encode(rd),
            keyset_endorsement: endorsement,
            source_provenance: self.config.source_provenance.clone(),
            freshness,
            evidence,
        };

        Ok(AttestationReport {
            api_version: "aci/1".to_string(),
            workload_id: self.workload_id.clone(),
            workload_keyset_digest: self.workload_keyset_digest.clone(),
            attestation: envelope,
            service_capabilities: self.config.service_capabilities.clone(),
        })
    }

    fn downstream_tls_binding(&self, domain: &str) -> Option<Value> {
        let domain = normalize_downstream_domain(domain)?;
        self.keyset
            .tls_public_keys
            .iter()
            .find(|key| key.domain.as_deref() == Some(domain.as_str()))
            .map(|key| {
                json!({
                    "domain": domain,
                    "spki_sha256": key.spki_sha256_hex,
                })
            })
    }

    pub fn prepare_e2ee_v2_request(
        &self,
        parts: E2eeRequestParts<'_>,
        body: &[u8],
        endpoint_path: &str,
    ) -> Result<E2eePreparedRequest, E2eeError> {
        if parts.signing_algo.is_some() {
            return self.prepare_legacy_e2ee_request(parts, body, endpoint_path);
        }

        let version = parts.version.ok_or(E2eeError::HeaderMissing)?;
        if version != E2EE_VERSION_V2 {
            return Err(E2eeError::InvalidVersion);
        }
        let client_public_key = parts.client_public_key.ok_or(E2eeError::HeaderMissing)?;
        let model_public_key = parts.model_public_key.ok_or(E2eeError::HeaderMissing)?;
        let nonce = parts.nonce.ok_or(E2eeError::HeaderMissing)?;
        let timestamp = parts.timestamp.ok_or(E2eeError::HeaderMissing)?;

        validate_e2ee_nonce(nonce)?;
        let timestamp = timestamp
            .parse::<u64>()
            .map_err(|_| E2eeError::InvalidTimestamp)?;
        let now = self.clock.now_secs();
        if now.abs_diff(timestamp) > 300 {
            return Err(E2eeError::InvalidTimestamp);
        }

        let client_public_key_hex = normalize_secp256k1_public_key_hex(client_public_key)
            .map_err(|_| E2eeError::InvalidPublicKey)?;
        let model_public_key_hex = normalize_secp256k1_public_key_hex(model_public_key)
            .map_err(|_| E2eeError::InvalidPublicKey)?;
        let selected_key = self
            .keyset
            .e2ee_public_keys
            .iter()
            .find(|key| {
                key.algo == E2EE_ALGO_SECP256K1_AESGCM
                    && normalize_secp256k1_public_key_hex(&key.public_key_hex)
                        .is_ok_and(|normalized| normalized == model_public_key_hex)
            })
            .ok_or(E2eeError::ModelKeyMismatch)?;

        let mut payload: Value =
            serde_json::from_slice(body).map_err(|_| E2eeError::DecryptionFailed)?;
        let request_model = validate_payload_model(&payload)?;
        self.claim_e2ee_replay(
            client_public_key_hex.clone(),
            model_public_key_hex.clone(),
            nonce.to_string(),
            now,
        )?;
        let crypto = E2eeFieldCrypto {
            keys: self.keys.as_ref(),
            decryptor: E2eeDecryptor::AciV2 {
                key_id: selected_key.key_id.as_str(),
            },
            algo: selected_key.algo.as_str(),
            aad_mode: E2eeAadMode::AciV2,
            model: &request_model,
            nonce: Some(nonce),
            timestamp: Some(timestamp),
        };
        decrypt_request_payload(&crypto, endpoint_path, &mut payload)?;
        let decrypted_body =
            serde_json::to_vec(&payload).map_err(|_| E2eeError::DecryptionFailed)?;
        Ok(E2eePreparedRequest {
            decrypted_body,
            context: E2eeRequestContext {
                version: E2EE_VERSION_V2.to_string(),
                algo: selected_key.algo.clone(),
                aad_mode: E2eeAadMode::AciV2,
                request_model,
                client_public_key_hex,
                nonce: Some(nonce.to_string()),
                timestamp: Some(timestamp),
            },
        })
    }

    fn prepare_legacy_e2ee_request(
        &self,
        parts: E2eeRequestParts<'_>,
        body: &[u8],
        endpoint_path: &str,
    ) -> Result<E2eePreparedRequest, E2eeError> {
        let signing_algo = parts
            .signing_algo
            .ok_or(E2eeError::HeaderMissing)?
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            signing_algo.as_str(),
            E2EE_ALGO_LEGACY_ECDSA | E2EE_ALGO_LEGACY_ED25519
        ) {
            return Err(E2eeError::InvalidSigningAlgo);
        }
        let client_public_key = parts.client_public_key.ok_or(E2eeError::HeaderMissing)?;
        let model_public_key = parts.model_public_key.ok_or(E2eeError::HeaderMissing)?;
        let _selected_key = self
            .keyset
            .e2ee_public_keys
            .iter()
            .find(|key| {
                key.algo == signing_algo
                    && legacy_public_keys_match(
                        &signing_algo,
                        &key.public_key_hex,
                        model_public_key,
                    )
            })
            .ok_or(E2eeError::ModelKeyMismatch)?;

        let version_header = parts.version.unwrap_or("").trim();
        if !version_header.is_empty()
            && version_header != E2EE_VERSION_V1
            && version_header != E2EE_VERSION_V2
        {
            return Err(E2eeError::InvalidVersion);
        }
        let has_nonce = parts.nonce.is_some_and(|nonce| !nonce.is_empty());
        let has_timestamp = parts.timestamp.is_some_and(|ts| !ts.is_empty());
        if has_nonce ^ has_timestamp {
            return Err(E2eeError::HeaderMissing);
        }
        let use_v2 = version_header == E2EE_VERSION_V2 || (has_nonce && has_timestamp);
        let (version, aad_mode, nonce, timestamp) = if use_v2 {
            let nonce = parts.nonce.ok_or(E2eeError::HeaderMissing)?;
            validate_legacy_e2ee_nonce(nonce)?;
            let timestamp = parts
                .timestamp
                .ok_or(E2eeError::HeaderMissing)?
                .parse::<u64>()
                .map_err(|_| E2eeError::InvalidTimestamp)?;
            let now = self.clock.now_secs();
            if now.abs_diff(timestamp) > 300 {
                return Err(E2eeError::InvalidTimestamp);
            }
            self.claim_e2ee_replay(
                normalize_legacy_public_key_for_replay(&signing_algo, client_public_key)?,
                normalize_legacy_public_key_for_replay(&signing_algo, model_public_key)?,
                nonce.to_string(),
                now,
            )?;
            (
                E2EE_VERSION_V2.to_string(),
                E2eeAadMode::LegacyV2,
                Some(nonce.to_string()),
                Some(timestamp),
            )
        } else {
            (
                E2EE_VERSION_V1.to_string(),
                E2eeAadMode::LegacyV1,
                None,
                None,
            )
        };

        let mut payload: Value =
            serde_json::from_slice(body).map_err(|_| E2eeError::DecryptionFailed)?;
        let request_model = validate_payload_model(&payload)?;
        let crypto = E2eeFieldCrypto {
            keys: self.keys.as_ref(),
            decryptor: E2eeDecryptor::Legacy {
                signing_algo: &signing_algo,
            },
            algo: &signing_algo,
            aad_mode,
            model: &request_model,
            nonce: nonce.as_deref(),
            timestamp,
        };
        decrypt_request_payload(&crypto, endpoint_path, &mut payload)?;
        let decrypted_body =
            serde_json::to_vec(&payload).map_err(|_| E2eeError::DecryptionFailed)?;
        let client_public_key_hex =
            normalize_legacy_public_key_for_replay(&signing_algo, client_public_key)?;
        Ok(E2eePreparedRequest {
            decrypted_body,
            context: E2eeRequestContext {
                version,
                algo: signing_algo,
                aad_mode,
                request_model,
                client_public_key_hex,
                nonce,
                timestamp,
            },
        })
    }

    fn claim_e2ee_replay(
        &self,
        client_public_key_hex: String,
        model_public_key_hex: String,
        nonce: String,
        now: u64,
    ) -> Result<(), E2eeError> {
        let mut guard = self
            .e2ee_replay
            .write()
            .expect("E2EE replay cache poisoned");
        guard.retain(|_, expires_at| *expires_at > now);
        let key = E2eeReplayKey {
            client_public_key_hex,
            model_public_key_hex,
            nonce,
        };
        if guard.contains_key(&key) {
            return Err(E2eeError::ReplayDetected);
        }
        guard.insert(key, now.saturating_add(300));
        Ok(())
    }

    /// Run the ACI §3 hot path for one non-streaming chat completion.
    pub async fn forward_chat_completion(
        &self,
        received_body: &[u8],
        forwarded_body: Option<Vec<u8>>,
        upstream_required: Option<bool>,
        upstream_verification_event: Option<UpstreamVerifiedEvent>,
    ) -> Result<ForwardResult, ServiceError> {
        self.forward_chat_completion_request(ChatCompletionRequest {
            context: GatewayRequestContext::default(),
            endpoint_path: CHAT_COMPLETIONS_PATH,
            received_body,
            forwarded_body,
            upstream_required,
            upstream_verification_event,
            requester: None,
            e2ee: None,
        })
        .await
    }

    /// Rich variant of [`Self::forward_chat_completion`] that also takes
    /// the receipt owner so the receipt store can authenticate later
    /// lookups (ACI §9.1, §9.5).
    pub async fn forward_chat_completion_request(
        &self,
        req: ChatCompletionRequest<'_>,
    ) -> Result<ForwardResult, ServiceError> {
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        self.metrics.record_request(
            endpoint_path,
            RequestMode::Buffered,
            req.e2ee.as_ref().is_some(),
        );
        let target_route_id = req.context.target_route_id.clone();
        let backend_input_body = req.forwarded_body.unwrap_or_else(|| received_body.to_vec());
        let middleware_forwarded_body =
            target_route_id.as_ref().map(|_| backend_input_body.clone());
        let prepared = self.upstream.prepare(UpstreamRequest {
            body: backend_input_body,
            path: Some(endpoint_path.to_string()),
            target_route_id: target_route_id.clone(),
            ..Default::default()
        })?;
        let forwarded_body = prepared.request.body.clone();
        let caller_supplied_upstream_event = req.upstream_verification_event.is_some();
        let mut recorded_event = self
            .recorded_upstream_event(
                &prepared,
                req.upstream_required,
                req.upstream_verification_event,
            )
            .await?;

        let mut reverify_attempts = 0;
        let upstream_response = loop {
            match self
                .upstream
                .forward_verified_prepared(prepared.clone(), &recorded_event)
                .await
            {
                Ok(response) => break response,
                Err(UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event
                        && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                {
                    reverify_attempts += 1;
                    recorded_event = self
                        .refresh_upstream_event(&prepared, req.upstream_required)
                        .await?;
                }
                Err(err @ UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event =>
                {
                    self.invalidate_upstream_event(&prepared, req.upstream_required);
                    return Err(err.into());
                }
                Err(err) => return Err(err.into()),
            }
        };
        let response_model =
            accepted_response_model(upstream_response.status_code, &upstream_response.body);
        self.metrics.record_upstream_response(
            endpoint_path,
            RequestMode::Buffered,
            upstream_response.status_code,
            response_model.as_deref(),
        );

        let e2ee = req.e2ee.as_ref();
        let wire_response_body = match e2ee {
            Some(ctx) => encrypt_e2ee_response_body(&upstream_response.body, ctx, endpoint_path)?,
            None => upstream_response.body.clone(),
        };
        let e2ee_response = e2ee.map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });

        // Receipt construction with bytes the service actually
        // observed. X-Request-Hash is never trusted here because we
        // do not even consult it; the byte source is the body the
        // service received from axum.
        let receipt_id = generate_receipt_id();
        let chat_id = extract_chat_id(&upstream_response.body);
        let served_at = self.clock.now_secs();
        let mut builder = ReceiptBuilder::new(
            receipt_id,
            chat_id,
            self.workload_id.clone(),
            self.workload_keyset_digest.clone(),
            endpoint_path.to_string(),
            "POST".to_string(),
            served_at,
        );
        builder.add_request_received(received_body)?;
        if let Some(body) = middleware_forwarded_body.as_deref() {
            builder.add_middleware_forwarded(body)?;
        }
        if let Some(route_id) = target_route_id.as_deref() {
            builder.add_route_selected(route_id)?;
        }
        builder.add_request_forwarded(&forwarded_body)?;
        if received_body != forwarded_body.as_slice() {
            builder.add_transparency_event(TransparencyEventKind::RequestModified)?;
        }
        let recorded = self.record_attested_upstream_session(&recorded_event)?;
        Self::append_upstream_verified(&mut builder, recorded_event, recorded)?;
        // The session is keyed on the requested (routed) model; record the exact
        // upstream-served model in the receipt's upstream.verified event.
        builder.set_upstream_verified_model_id(response_model.clone());
        if upstream_response.body != wire_response_body {
            builder.add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        builder.add_response_returned(&upstream_response.body, &wire_response_body)?;

        let receipt = builder.finalize(self.keys.as_ref(), &self.default_receipt_key_id)?;
        self.store_receipt(receipt.clone(), req.requester.clone());
        self.metrics.record_receipt_issued(
            endpoint_path,
            RequestMode::Buffered,
            response_model.as_deref(),
        );

        Ok(ForwardResult {
            receipt,
            upstream_status: upstream_response.status_code,
            upstream_body: wire_response_body,
            upstream_headers: upstream_response.headers,
            e2ee: e2ee_response,
        })
    }

    /// Forward a middleware-selected request without finalizing the receipt.
    ///
    /// The backend records trust-critical provider facts into the returned
    /// draft. The public frontend must append `response.returned`, sign, and
    /// store the receipt after middleware returns the final user-visible body.
    /// Build the receipt event prefix shared by the buffered and
    /// streaming commit paths: request.received → middleware.forwarded →
    /// route.selected → request.forwarded (+transparency) →
    /// upstream.verified. The caller appends response.received afterwards
    /// (buffered now, streaming at end). Failover is not recorded in the
    /// receipt — the receipt attests only the served (selected) route; the
    /// attempt count is surfaced to ops via an attribution header.
    #[allow(clippy::too_many_arguments)]
    fn build_middleware_receipt_prefix(
        &self,
        receipt_id: &str,
        chat_id: Option<String>,
        served_at: u64,
        endpoint_path: &str,
        received_body: &[u8],
        middleware_forwarded_body: &[u8],
        selected_route_id: &str,
        forwarded_body: &[u8],
        recorded_event: UpstreamVerifiedEvent,
        recorded: Option<(String, SessionClaims)>,
    ) -> Result<ReceiptBuilder, ServiceError> {
        let mut builder = ReceiptBuilder::new(
            receipt_id.to_string(),
            chat_id,
            self.workload_id.clone(),
            self.workload_keyset_digest.clone(),
            endpoint_path.to_string(),
            "POST".to_string(),
            served_at,
        );
        builder.add_request_received(received_body)?;
        builder.add_middleware_forwarded(middleware_forwarded_body)?;
        builder.add_route_selected(selected_route_id)?;
        builder.add_request_forwarded(forwarded_body)?;
        if received_body != forwarded_body {
            builder.add_transparency_event(TransparencyEventKind::RequestModified)?;
        }
        Self::append_upstream_verified(&mut builder, recorded_event, recorded)?;
        Ok(builder)
    }

    pub async fn forward_chat_completion_for_middleware(
        &self,
        req: ChatCompletionRequest<'_>,
        candidates: Vec<ForwardCandidate>,
        stream: bool,
        receipt_journal: MiddlewareReceiptJournal,
    ) -> Result<MiddlewareForwardResult, ServiceError> {
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        let mode = if stream {
            RequestMode::Streaming
        } else {
            RequestMode::Buffered
        };
        self.metrics
            .record_request(endpoint_path, mode, req.e2ee.as_ref().is_some());

        if candidates.is_empty() {
            return Err(ServiceError::Upstream(UpstreamError::Routing(
                "no candidate routes supplied".to_string(),
            )));
        }
        // A caller-supplied verifier event only applies to a single
        // explicit candidate (non-failover). With an ordered list the
        // backend always computes per-candidate events.
        let caller_supplied_upstream_event =
            req.upstream_verification_event.is_some() && candidates.len() == 1;
        let single_caller_event = if caller_supplied_upstream_event {
            req.upstream_verification_event.clone()
        } else {
            None
        };
        let candidate_route_ids: Vec<String> =
            candidates.iter().map(|c| c.route_id.clone()).collect();
        let last_index = candidates.len() - 1;

        // Highest-priority error across exhausted candidates, returned if
        // no candidate succeeds.
        //
        // The number of candidates attempted (`index + 1` when one succeeds)
        // is surfaced via a response header for the caller's metrics. Failover
        // is internal to this forwarder and is NOT recorded in the user-facing
        // receipt; the receipt attests only the served request (route.selected
        // + upstream.verified + hashes).
        let mut aggregated_err: Option<(u8, ServiceError)> = None;

        for (index, candidate) in candidates.iter().enumerate() {
            let route_id = candidate.route_id.clone();
            let is_last = index == last_index;

            let prepared = match self.upstream.prepare(UpstreamRequest {
                body: candidate.body.clone(),
                path: Some(endpoint_path.to_string()),
                target_route_id: Some(route_id.clone()),
                ..Default::default()
            }) {
                Ok(prepared) => prepared,
                Err(UpstreamError::Routing(message)) => {
                    upgrade_err(
                        &mut aggregated_err,
                        1,
                        ServiceError::Upstream(UpstreamError::Routing(message)),
                    );
                    continue;
                }
                Err(err) => {
                    upgrade_err(&mut aggregated_err, 2, err.into());
                    continue;
                }
            };

            // Per-route fail-closed mode: explicitly non-TEE routes never
            // fail closed; TEE and unclassified routes honour the
            // request-level `upstream_required` flag.
            let non_tee = prepared.is_tee == Some(false);
            let candidate_required = if non_tee {
                Some(false)
            } else {
                req.upstream_required
            };

            let mut recorded_event = match self
                .recorded_upstream_event(&prepared, candidate_required, single_caller_event.clone())
                .await
            {
                Ok(event) => event,
                Err(ServiceError::UpstreamVerification(uv)) => {
                    upgrade_err(
                        &mut aggregated_err,
                        3,
                        ServiceError::UpstreamVerification(uv),
                    );
                    continue;
                }
                Err(err) => return Err(err),
            };

            let forwarded_body = prepared.request.body.clone();

            if stream {
                let mut reverify_attempts = 0;
                let upstream_response = loop {
                    match self
                        .upstream
                        .forward_stream_verified_prepared(prepared.clone(), &recorded_event)
                        .await
                    {
                        Ok(response) => break Some(response),
                        Err(UpstreamError::ChannelBindingMismatch(_))
                            if !caller_supplied_upstream_event
                                && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                        {
                            reverify_attempts += 1;
                            match self
                                .refresh_upstream_event(&prepared, candidate_required)
                                .await
                            {
                                Ok(event) => recorded_event = event,
                                // A reverify failure must fail over to the
                                // next candidate, not abort the whole list.
                                Err(err) => {
                                    let priority =
                                        if matches!(err, ServiceError::UpstreamVerification(_)) {
                                            3
                                        } else {
                                            2
                                        };
                                    upgrade_err(&mut aggregated_err, priority, err);
                                    break None;
                                }
                            }
                        }
                        Err(err @ UpstreamError::ChannelBindingMismatch(_)) => {
                            self.invalidate_upstream_event(&prepared, candidate_required);
                            upgrade_err(&mut aggregated_err, 2, err.into());
                            break None;
                        }
                        Err(err) => {
                            upgrade_err(&mut aggregated_err, 2, err.into());
                            break None;
                        }
                    }
                };
                let Some(upstream_response) = upstream_response else {
                    continue;
                };

                let status = upstream_response.status_code;
                if status != 200 {
                    self.metrics.record_upstream_response(
                        endpoint_path,
                        RequestMode::Streaming,
                        status,
                        None,
                    );
                    if is_retryable_provider_status(status) && !is_last {
                        continue;
                    }
                    self.metrics
                        .record_stream_error(endpoint_path, StreamErrorKind::UpstreamNon2xx);
                    let upstream_headers = upstream_response.headers;
                    let upstream_body = collect_upstream_body(upstream_response.body).await?;
                    return Ok(MiddlewareForwardResult::UpstreamError(
                        StreamingUpstreamError {
                            upstream_status: status,
                            upstream_headers,
                            upstream_body,
                        },
                    ));
                }

                // Commit this candidate.
                let upstream_headers = upstream_response.headers;
                let receipt_id = generate_receipt_id();
                let served_at = self.clock.now_secs();
                let recorded = self.record_attested_upstream_session(&recorded_event)?;
                let session_id = recorded.as_ref().map(|(id, _)| id.clone());
                let builder = self.build_middleware_receipt_prefix(
                    &receipt_id,
                    None,
                    served_at,
                    endpoint_path,
                    received_body,
                    &candidate.body,
                    &route_id,
                    &forwarded_body,
                    recorded_event,
                    recorded,
                )?;
                receipt_journal.reserve_receipt_id(receipt_id.clone());

                let body = MiddlewareProviderResponseDraftingStream {
                    inner: upstream_response.body,
                    builder: Some(builder),
                    journal: receipt_journal,
                    provider_response_hasher: Sha256::new(),
                    receipt_id: receipt_id.clone(),
                    endpoint_path: endpoint_path.to_string(),
                    sse_parser: SseChatIdParser::default(),
                    metrics: self.metrics.clone(),
                    upstream_status: status,
                    upstream_ended: false,
                    finished: false,
                };

                return Ok(MiddlewareForwardResult::Stream(Box::new(
                    MiddlewareStreamingForwarded {
                        receipt_id: receipt_id.clone(),
                        upstream_status: status,
                        upstream_headers,
                        body: Box::pin(body),
                        selected_route: route_id.clone(),
                        attempts: index + 1,
                        session_id,
                    },
                )));
            }

            // Buffered forward.
            let mut reverify_attempts = 0;
            let upstream_response = loop {
                match self
                    .upstream
                    .forward_verified_prepared(prepared.clone(), &recorded_event)
                    .await
                {
                    Ok(response) => break Some(response),
                    Err(UpstreamError::ChannelBindingMismatch(_))
                        if !caller_supplied_upstream_event
                            && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                    {
                        reverify_attempts += 1;
                        match self
                            .refresh_upstream_event(&prepared, candidate_required)
                            .await
                        {
                            Ok(event) => recorded_event = event,
                            // A reverify failure must fail over to the next
                            // candidate, not abort the whole list.
                            Err(err) => {
                                let priority =
                                    if matches!(err, ServiceError::UpstreamVerification(_)) {
                                        3
                                    } else {
                                        2
                                    };
                                upgrade_err(&mut aggregated_err, priority, err);
                                break None;
                            }
                        }
                    }
                    Err(err @ UpstreamError::ChannelBindingMismatch(_)) => {
                        self.invalidate_upstream_event(&prepared, candidate_required);
                        upgrade_err(&mut aggregated_err, 2, err.into());
                        break None;
                    }
                    Err(err) => {
                        upgrade_err(&mut aggregated_err, 2, err.into());
                        break None;
                    }
                }
            };
            let Some(upstream_response) = upstream_response else {
                continue;
            };

            let status = upstream_response.status_code;
            if is_retryable_provider_status(status) && !is_last {
                self.metrics.record_upstream_response(
                    endpoint_path,
                    RequestMode::Buffered,
                    status,
                    None,
                );
                continue;
            }

            // Commit this candidate.
            let response_model = accepted_response_model(status, &upstream_response.body);
            self.metrics.record_upstream_response(
                endpoint_path,
                RequestMode::Buffered,
                status,
                response_model.as_deref(),
            );

            let receipt_id = generate_receipt_id();
            let served_at = self.clock.now_secs();
            let chat_id = extract_chat_id(&upstream_response.body);
            let recorded = self.record_attested_upstream_session(&recorded_event)?;
            let session_id = recorded.as_ref().map(|(id, _)| id.clone());
            let mut builder = self.build_middleware_receipt_prefix(
                &receipt_id,
                chat_id,
                served_at,
                endpoint_path,
                received_body,
                &candidate.body,
                &route_id,
                &forwarded_body,
                recorded_event,
                recorded,
            )?;
            // The session is keyed on the requested (routed) model; record the
            // exact upstream-served model in the receipt's upstream.verified.
            builder.set_upstream_verified_model_id(response_model.clone());
            let provider_response_hash = builder.add_response_received(&upstream_response.body)?;

            return Ok(MiddlewareForwardResult::Forwarded(Box::new(
                MiddlewareForwarded {
                    receipt_id: receipt_id.clone(),
                    receipt: MiddlewareReceiptDraft {
                        receipt_id: receipt_id.clone(),
                        builder,
                        provider_response_hash,
                        endpoint_path: endpoint_path.to_string(),
                        request_mode: RequestMode::Buffered,
                        response_model,
                    },
                    upstream_status: status,
                    upstream_body: upstream_response.body,
                    upstream_headers: upstream_response.headers,
                    selected_route: route_id.clone(),
                    attempts: index + 1,
                    session_id,
                },
            )));
        }

        // No candidate succeeded. Return the highest-priority failure, with
        // the attempted route ids for context.
        Err(aggregated_err.map(|(_, err)| err).unwrap_or_else(|| {
            ServiceError::Upstream(UpstreamError::Routing(format!(
                "all upstream routes failed (attempted: {})",
                candidate_route_ids.join(", ")
            )))
        }))
    }

    /// Start a streaming chat completion. The response stream hashes
    /// every byte in order and stores the receipt only after the
    /// upstream stream completes.
    pub async fn forward_chat_completion_stream_request(
        &self,
        req: ChatCompletionRequest<'_>,
    ) -> Result<StreamingForwardResult, ServiceError> {
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        self.metrics.record_request(
            endpoint_path,
            RequestMode::Streaming,
            req.e2ee.as_ref().is_some(),
        );
        let target_route_id = req.context.target_route_id.clone();
        let backend_input_body = req.forwarded_body.unwrap_or_else(|| received_body.to_vec());
        let middleware_forwarded_body =
            target_route_id.as_ref().map(|_| backend_input_body.clone());
        let prepared = self.upstream.prepare(UpstreamRequest {
            body: backend_input_body,
            path: Some(endpoint_path.to_string()),
            target_route_id: target_route_id.clone(),
            ..Default::default()
        })?;
        let forwarded_body = prepared.request.body.clone();
        let caller_supplied_upstream_event = req.upstream_verification_event.is_some();
        let mut recorded_event = self
            .recorded_upstream_event(
                &prepared,
                req.upstream_required,
                req.upstream_verification_event,
            )
            .await?;

        let mut reverify_attempts = 0;
        let upstream_response = loop {
            match self
                .upstream
                .forward_stream_verified_prepared(prepared.clone(), &recorded_event)
                .await
            {
                Ok(response) => break response,
                Err(UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event
                        && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                {
                    reverify_attempts += 1;
                    recorded_event = self
                        .refresh_upstream_event(&prepared, req.upstream_required)
                        .await?;
                }
                Err(err @ UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event =>
                {
                    self.invalidate_upstream_event(&prepared, req.upstream_required);
                    return Err(err.into());
                }
                Err(err) => return Err(err.into()),
            }
        };
        // Match dstack-vllm-proxy compatibility behavior: streaming
        // requests whose upstream response is not exactly HTTP 200
        // are returned as ordinary buffered error responses. No
        // receipt is issued because there is no completed inference
        // stream to bind.
        if upstream_response.status_code != 200 {
            self.metrics.record_upstream_response(
                endpoint_path,
                RequestMode::Streaming,
                upstream_response.status_code,
                None,
            );
            self.metrics
                .record_stream_error(endpoint_path, StreamErrorKind::UpstreamNon2xx);
            let upstream_status = upstream_response.status_code;
            let upstream_headers = upstream_response.headers;
            let upstream_body = collect_upstream_body(upstream_response.body).await?;
            return Ok(StreamingForwardResult::UpstreamError(
                StreamingUpstreamError {
                    upstream_status,
                    upstream_headers,
                    upstream_body,
                },
            ));
        }

        let receipt_id = generate_receipt_id();
        let served_at = self.clock.now_secs();
        let mut builder = ReceiptBuilder::new(
            receipt_id.clone(),
            None,
            self.workload_id.clone(),
            self.workload_keyset_digest.clone(),
            endpoint_path.to_string(),
            "POST".to_string(),
            served_at,
        );
        builder.add_request_received(received_body)?;
        if let Some(body) = middleware_forwarded_body.as_deref() {
            builder.add_middleware_forwarded(body)?;
        }
        if let Some(route_id) = target_route_id.as_deref() {
            builder.add_route_selected(route_id)?;
        }
        builder.add_request_forwarded(&forwarded_body)?;
        if received_body != forwarded_body.as_slice() {
            builder.add_transparency_event(TransparencyEventKind::RequestModified)?;
        }
        let recorded = self.record_attested_upstream_session(&recorded_event)?;
        Self::append_upstream_verified(&mut builder, recorded_event, recorded)?;

        let e2ee_response = req.e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });
        let response_modified = req.e2ee.is_some();
        let e2ee_transformer = req
            .e2ee
            .clone()
            .map(|ctx| E2eeSseTransformer::new(ctx, endpoint_path.to_string()));

        let body = ReceiptFinalizingStream {
            inner: upstream_response.body,
            builder: Some(builder),
            cleartext_hasher: Sha256::new(),
            wire_hasher: Sha256::new(),
            keys: self.keys.clone(),
            receipt_store: self.receipt_store.clone(),
            key_id: self.default_receipt_key_id.clone(),
            requester: req.requester,
            receipt_ttl_seconds: self.config.receipt_ttl_seconds,
            clock: self.clock.clone(),
            metrics: self.metrics.clone(),
            endpoint_path: endpoint_path.to_string(),
            sse_parser: SseChatIdParser::default(),
            e2ee_transformer,
            response_modified,
            upstream_ended: false,
            finished: false,
        };

        Ok(StreamingForwardResult::Stream(StreamingForwardStream {
            receipt_id,
            upstream_status: upstream_response.status_code,
            upstream_headers: upstream_response.headers,
            e2ee: e2ee_response,
            body: Box::pin(body),
        }))
    }

    pub fn finalize_middleware_receipt(
        &self,
        mut draft: MiddlewareReceiptDraft,
        final_cleartext_body: &[u8],
        content_type: Option<&str>,
        requester: Option<ReceiptOwner>,
        e2ee: Option<E2eeRequestContext>,
    ) -> Result<MiddlewareReceiptFinalization, ServiceError> {
        let is_sse = is_sse_content_type(content_type);
        if is_sse {
            let mut parser = SseChatIdParser::default();
            parser.observe(final_cleartext_body);
            if parser.chat_id.is_some() {
                draft.builder.set_chat_id(parser.chat_id);
            }
        } else if let Some(chat_id) = extract_chat_id(final_cleartext_body) {
            draft.builder.set_chat_id(Some(chat_id));
        }

        let wire_body = match e2ee.as_ref() {
            Some(ctx) => encrypt_e2ee_final_response(
                final_cleartext_body,
                ctx,
                &draft.endpoint_path,
                is_sse,
            )?,
            None => final_cleartext_body.to_vec(),
        };
        let e2ee_response = e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });

        let final_cleartext_hash = crate::aci::canonical::sha256_hex(final_cleartext_body);
        if draft.provider_response_hash != final_cleartext_hash || wire_body != final_cleartext_body
        {
            draft
                .builder
                .add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        draft
            .builder
            .add_response_returned(final_cleartext_body, &wire_body)?;
        let receipt = draft
            .builder
            .finalize(self.keys.as_ref(), &self.default_receipt_key_id)?;
        self.store_receipt(receipt.clone(), requester);
        self.metrics.record_receipt_issued(
            &draft.endpoint_path,
            draft.request_mode,
            draft.response_model.as_deref(),
        );

        Ok(MiddlewareReceiptFinalization {
            receipt,
            wire_body,
            e2ee: e2ee_response,
        })
    }

    pub fn finalize_middleware_generated_response(
        &self,
        endpoint_path: &str,
        cleartext_body: &[u8],
        content_type: Option<&str>,
        e2ee: Option<E2eeRequestContext>,
    ) -> Result<MiddlewareGeneratedFinalization, ServiceError> {
        let is_sse = is_sse_content_type(content_type);
        let wire_body = match e2ee.as_ref() {
            Some(ctx) => encrypt_e2ee_final_response(cleartext_body, ctx, endpoint_path, is_sse)?,
            None => cleartext_body.to_vec(),
        };
        let e2ee_response = e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });
        Ok(MiddlewareGeneratedFinalization {
            wire_body,
            e2ee: e2ee_response,
        })
    }

    pub fn finalize_middleware_response_stream(
        &self,
        journal: MiddlewareReceiptJournal,
        cleartext_stream: ServiceResponseStream,
        endpoint_path: &str,
        content_type: Option<&str>,
        requester: Option<ReceiptOwner>,
        e2ee: Option<E2eeRequestContext>,
    ) -> Result<MiddlewareStreamFinalization, ServiceError> {
        let is_sse = is_sse_content_type(content_type);
        if e2ee.is_some() && !is_sse {
            return Err(E2eeError::EncryptionFailed.into());
        }
        let e2ee_response = e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });
        let e2ee_transformer = e2ee
            .clone()
            .map(|ctx| E2eeSseTransformer::new(ctx, endpoint_path.to_string()));
        let body = MiddlewareResponseFinalizingStream {
            inner: cleartext_stream,
            journal,
            cleartext_hasher: Sha256::new(),
            wire_hasher: Sha256::new(),
            keys: self.keys.clone(),
            receipt_store: self.receipt_store.clone(),
            key_id: self.default_receipt_key_id.clone(),
            requester,
            receipt_ttl_seconds: self.config.receipt_ttl_seconds,
            clock: self.clock.clone(),
            metrics: self.metrics.clone(),
            endpoint_path: endpoint_path.to_string(),
            sse_parser: SseChatIdParser::default(),
            e2ee_transformer,
            response_modified_by_wire: e2ee_response.is_some(),
            upstream_ended: false,
            finished: false,
        };
        Ok(MiddlewareStreamFinalization {
            body: Box::pin(body),
            e2ee: e2ee_response,
        })
    }

    async fn recorded_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        upstream_required: Option<bool>,
        upstream_verification_event: Option<UpstreamVerifiedEvent>,
    ) -> Result<UpstreamVerifiedEvent, ServiceError> {
        let upstream_required = upstream_required.unwrap_or(self.config.upstream_required_default);
        let mut upstream_verification_event = match upstream_verification_event {
            Some(event) => Some(event),
            None => match &self.upstream_verifier {
                Some(verifier) => {
                    let request = self.upstream_verification_request(prepared, upstream_required);
                    Some(verifier.verify(request).await)
                }
                None => None,
            },
        };
        if let Some(event) = upstream_verification_event.as_mut() {
            // `required` is the client's effective mode for this request. The
            // verifier may report the upstream result, but the service owns the
            // client-facing downgrade decision recorded in the receipt.
            event.required = upstream_required;
        }

        let missing_verifier_result = upstream_verification_event.is_none();
        let event = upstream_verification_event.unwrap_or_else(|| UpstreamVerifiedEvent {
            upstream_name: prepared.upstream_name.clone(),
            provider: None,
            model_id: prepared.model_id.clone(),
            url_origin: prepared.url_origin.clone(),
            verifier_id: "none".to_string(),
            result: VerificationResult::Failed,
            required: upstream_required,
            reason: Some("no upstream verifier configured".to_string()),
            evidence: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
        });
        self.metrics.record_upstream_verification(&event);

        // Fail-closed gate. Run before any upstream IO.
        if upstream_required {
            if missing_verifier_result {
                return Err(ServiceError::UpstreamVerification(
                    UpstreamVerificationError::NoVerifierResult,
                ));
            }
            if event.result != VerificationResult::Verified {
                let reason = event
                    .reason
                    .clone()
                    .unwrap_or_else(|| "upstream verification failed".to_string());
                return Err(ServiceError::UpstreamVerification(
                    UpstreamVerificationError::VerifierFailed(reason),
                ));
            }
        }

        // Aggregator receipts always carry an `upstream.verified`
        // event. The opt-out path records a synthesized failed event
        // so downstream verifiers see the actual state.
        Ok(event)
    }

    async fn refresh_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        upstream_required: Option<bool>,
    ) -> Result<UpstreamVerifiedEvent, ServiceError> {
        let upstream_required = upstream_required.unwrap_or(self.config.upstream_required_default);
        self.invalidate_upstream_event(prepared, Some(upstream_required));
        self.recorded_upstream_event(prepared, Some(upstream_required), None)
            .await
    }

    fn invalidate_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        upstream_required: Option<bool>,
    ) {
        let Some(verifier) = &self.upstream_verifier else {
            return;
        };
        let required = upstream_required.unwrap_or(self.config.upstream_required_default);
        let request = self.upstream_verification_request(prepared, required);
        verifier.invalidate(&request);
    }

    fn upstream_verification_request(
        &self,
        prepared: &PreparedUpstreamRequest,
        required: bool,
    ) -> UpstreamVerificationRequest {
        UpstreamVerificationRequest {
            upstream_name: prepared.upstream_name.clone(),
            url_origin: prepared.url_origin.clone(),
            model_id: prepared.model_id.clone(),
            forwarded_body_hash: crate::aci::canonical::sha256_hex(&prepared.request.body),
            required,
        }
    }

    /// Seal + persist the attested session for a verified event, and return its
    /// `(session_id, claim-verdicts)`. The verdicts are surfaced inline in the
    /// receipt's `upstream.verified` (shallow audit), while the persisted session
    /// also carries the evidence + reasons (deep audit).
    fn record_attested_upstream_session(
        &self,
        event: &UpstreamVerifiedEvent,
    ) -> Result<Option<(String, SessionClaims)>, ServiceError> {
        if event.result != VerificationResult::Verified || event.channel_bindings.is_empty() {
            return Ok(None);
        }

        let now = self.clock.now_secs();
        // Retention window (`receipt_ttl_seconds`), so a relying party verifying a
        // citing receipt can resolve its `session_id`. The session is sealed
        // slightly before its receipt, so it expires up to one request-processing
        // interval (sub-second) sooner than that receipt — both use the same TTL
        // off a per-call `now`. This is a retention deadline, not a binding
        // validity one (the forwarding path only ever uses a fresh lease).
        let expires_at = now.saturating_add(self.config.receipt_ttl_seconds);

        let channel_binding = AttestedSession::bindings_to_values(&event.channel_bindings);
        let claims = session_claims_for_event(event);

        // Lift the response-signing address into the verified identity when present.
        let mut identity = WorkloadIdentityRef::default();
        if let Some(Value::Object(map)) = event.provider_claims.as_ref() {
            if let Some(addr) = map.get("signing_address").and_then(Value::as_str) {
                identity.signing_address = Some(addr.to_string());
            }
        }
        let identity = (!identity.is_empty()).then_some(identity);

        let evidence = event
            .evidence
            .as_ref()
            .map(EvidenceRef::from_value)
            .unwrap_or_default();

        let session = AttestedSession::seal(
            event.upstream_name.clone(),
            event.url_origin.clone(),
            event.verifier_id.clone(),
            identity,
            channel_binding,
            claims.clone(),
            evidence,
            now,
            expires_at,
        )?;

        let session_id = session.session_id.clone();
        if let Err(err) = self.session_store.put_session(session, now) {
            // Persisting the audit record must not break inference; a missing
            // session simply resolves to "not found" for relying parties.
            tracing::warn!(error = %err, session_id = %session_id, "failed to persist attested session");
        }
        Ok(Some((session_id, claims)))
    }

    /// Append the `upstream.verified` receipt event, attaching the session id and
    /// the typed claim verdicts when a verified session was recorded.
    fn append_upstream_verified(
        builder: &mut ReceiptBuilder,
        event: UpstreamVerifiedEvent,
        recorded: Option<(String, SessionClaims)>,
    ) -> Result<(), ReceiptError> {
        let (session_id, claims) = match recorded {
            Some((id, claims)) => (Some(id), Some(claims)),
            None => (None, None),
        };
        builder.add_upstream_verified_with_session(event, session_id.as_deref(), claims.as_ref())
    }

    fn store_receipt(&self, receipt: Receipt, requester: Option<ReceiptOwner>) {
        let now = self.clock.now_secs();
        let expires_at = now.saturating_add(self.config.receipt_ttl_seconds);
        self.receipt_store.put(receipt, requester, expires_at);
    }

    pub fn get_receipt_by_receipt_id(&self, id: &str) -> Option<Receipt> {
        self.receipt_store
            .get_by_receipt_id(id, self.clock.now_secs())
    }

    pub fn get_receipt_by_chat_id(&self, id: &str) -> Option<Receipt> {
        self.receipt_store.get_by_chat_id(id, self.clock.now_secs())
    }

    pub fn legacy_signature_for_receipt(
        &self,
        receipt: &Receipt,
        signing_algo: Option<&str>,
    ) -> Result<LegacySignatureResult, ServiceError> {
        let Some(text) = legacy_signature_text(receipt) else {
            return Err(ReceiptError::MissingRequiredEvent(EVENT_RESPONSE_RETURNED).into());
        };
        let LegacySignature {
            signing_algo,
            signing_address,
            signature,
        } = self
            .keys
            .sign_legacy_message(signing_algo.unwrap_or(LEGACY_ALGO_ECDSA), &text)?;
        Ok(LegacySignatureResult {
            text,
            signature,
            signing_address,
            signing_algo,
        })
    }

    /// Read the recorded owner for a receipt, if any.
    pub fn owner_of_receipt(&self, receipt_id: &str) -> Option<ReceiptOwner> {
        self.receipt_store
            .owner_of(receipt_id, self.clock.now_secs())
    }

    pub fn get_attested_session(&self, session_id: &str) -> Option<AttestedSession> {
        self.session_store
            .get_session(session_id, self.clock.now_secs())
    }

    /// List attested sessions (TEE channels), optionally filtered by provider
    /// (the upstream config name). A model→channel lookup belongs to the caller,
    /// since a session is per-channel, not per-model.
    pub fn list_attested_sessions(&self, provider: Option<&str>) -> Vec<AttestedSession> {
        self.session_store
            .list_sessions(provider, self.clock.now_secs())
    }

    /// E2EE protocol versions this workload has actually wired.
    pub fn supported_e2ee_versions(&self) -> &[String] {
        &self.config.service_capabilities.supported_e2ee_versions
    }
}

/// Derive the typed claim verdicts for a verified upstream event. A verified
/// result means the provider's verifier proved a genuine TEE workload identity
/// and bound the request channel to it, so `tee_attested` is asserted
/// (`VerifierDerived`). The remaining typed claims stay `Unknown` until a
/// per-provider mapper fills them; the verifier's raw `provider_claims` are
/// preserved verbatim as scope facts under `claims.extra`.
/// The background upstream verification writes attested sessions into the store
/// through this sink, keeping it fresh from the same verification that keeps the
/// gateway's attestation fresh — independent of traffic. The live completion
/// path also writes (the session it served). Both are safe because sealing is
/// content-addressed and idempotent: an unchanged endpoint resolves to the same
/// record, so the two writers converge on one logical session per verified state
/// rather than duplicating it.
impl UpstreamSessionSink for AciService {
    fn record_session(&self, event: &UpstreamVerifiedEvent) {
        if let Err(err) = self.record_attested_upstream_session(event) {
            tracing::warn!(error = %err, "failed to record attested session from verification");
        }
    }
}

/// Maps a verified `UpstreamVerifiedEvent` onto the typed claim vocabulary for
/// one provider. Each provider implements it, so the honesty rules for a
/// provider live with that provider instead of in one central match. `claims`
/// is only invoked for a `Verified` result; the caller folds the raw
/// `provider_claims` into `claims.extra` afterward. A mapper asserts only what
/// its verifier's evidence proves; everything else stays `Unknown`.
trait ProviderClaimMapper {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims;
}

/// Route a provider *type* to its claim mapper; an absent/unknown provider gets
/// the generic mapper. This is the only place that branches on the provider
/// string — the per-provider logic lives in the `ProviderClaimMapper` impls.
fn claim_mapper(provider: Option<&str>) -> &'static dyn ProviderClaimMapper {
    match provider {
        Some("tinfoil") => &TinfoilClaims,
        Some("near-ai") | Some("chutes") | Some("phala-direct") => &DstackClaims,
        _ => &GenericClaims,
    }
}

/// Build the typed claim set for a verified event. Raw `provider_claims` are
/// always preserved verbatim in `claims.extra` so a deep auditor sees the full
/// provider scope, typed or not.
fn session_claims_for_event(event: &UpstreamVerifiedEvent) -> SessionClaims {
    let mut claims = if event.result == VerificationResult::Verified {
        claim_mapper(event.provider.as_deref()).claims(event)
    } else {
        SessionClaims::default()
    };
    if let Some(Value::Object(map)) = event.provider_claims.as_ref() {
        for (key, value) in map {
            claims.extra.insert(key.clone(), value.clone());
        }
    }
    claims
}

/// `tee_attested` rooted in a verified hardware quote with the request channel
/// bound to it. Shared by the providers that verify a real TEE quote.
fn hardware_tee_attested(event: &UpstreamVerifiedEvent) -> Claim {
    Claim::asserted(
        ClaimSource::HardwareProven,
        format!(
            "{} verified the TEE quote and bound the request channel",
            event.verifier_id
        ),
    )
}

/// dstack-based providers (NEAR AI, Chutes, Phala-direct): a real hardware quote
/// plus a `TcbStatus` from DCAP collateral (a HardwareProven tri-state) and OS
/// provenance from the attested image hash.
struct DstackClaims;
impl ProviderClaimMapper for DstackClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        // gpu_attested / model_weights_provenance stay Unknown: a GPU/NRAS token
        // proves only that a CC-capable GPU exists, not that it served this
        // request, and no verifier here checks the served weights.
        SessionClaims {
            tee_attested: hardware_tee_attested(event),
            tcb_up_to_date: tcb_up_to_date_claim(event),
            os_known_good: os_known_good_claim(event),
            ..SessionClaims::default()
        }
    }
}

/// Tinfoil: a verified hardware quote, but its official verifier gates on TCB
/// internally (no separable `TcbStatus`, so freshness is VerifierDerived, never
/// HardwareProven), and it traces serving software to a reviewed Sigstore release.
struct TinfoilClaims;
impl ProviderClaimMapper for TinfoilClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        SessionClaims {
            tee_attested: hardware_tee_attested(event),
            tcb_up_to_date: Claim::asserted(
                ClaimSource::VerifierDerived,
                "Tinfoil's verifier requires an up-to-date TCB for a verified \
                 result; no separable TcbStatus is surfaced",
            ),
            serving_software_known_good: tinfoil_software_claim(event),
            os_known_good: os_known_good_claim(event),
            ..SessionClaims::default()
        }
    }
}

/// Generic verifier (static / preverified / DCAP test path): we only know it
/// returned Verified with an enforceable channel binding.
struct GenericClaims;
impl ProviderClaimMapper for GenericClaims {
    fn claims(&self, event: &UpstreamVerifiedEvent) -> SessionClaims {
        SessionClaims {
            tee_attested: Claim::asserted(
                ClaimSource::VerifierDerived,
                format!(
                    "{} verified the workload identity and bound the channel",
                    event.verifier_id
                ),
            ),
            ..SessionClaims::default()
        }
    }
}

/// Platform TCB freshness as an honest tri-state from the verifier's reported
/// `tcb_status` (TDX/SEV `TcbStatus`): `UpToDate` asserts, any other reported
/// status refutes — the quote proves a stale TCB even though the gateway does
/// not hard-reject it — and an absent status is Unknown. Freshness is never
/// asserted by policy: a verifier that does not surface a status leaves the
/// claim Unknown, because we cannot prove it is current, not because it is.
fn tcb_up_to_date_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let status = event
        .provider_claims
        .as_ref()
        .and_then(|c| c.get("tcb_status"))
        .and_then(Value::as_str);
    match status {
        Some(status) if status.eq_ignore_ascii_case("uptodate") => Claim::asserted(
            ClaimSource::HardwareProven,
            format!("platform TCB status {status}"),
        ),
        Some(status) => Claim::refuted(
            ClaimSource::HardwareProven,
            format!("platform TCB status {status}"),
        ),
        None => Claim::unknown(),
    }
}

/// OS-image provenance from the attested `os_image_hash`. Phala-direct resolves
/// that hash to dstack's published image and reads its prod-vs-dev flag, so
/// `production_os_image` is a verifier-derived verdict: a known production image
/// asserts; a dev image (SSH / serial console enabled — an operator shell that
/// defeats the confidentiality guarantee) **refutes**, recorded rather than
/// hard-rejected; an unresolved hash stays Unknown. Providers that surface no
/// such fact are Unknown.
fn os_known_good_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let production = event
        .provider_claims
        .as_ref()
        .and_then(|c| c.get("production_os_image"))
        .and_then(Value::as_bool);
    match production {
        Some(true) => Claim::asserted(
            ClaimSource::VerifierDerived,
            "attested OS image resolves to a known production image",
        ),
        Some(false) => Claim::refuted(
            ClaimSource::VerifierDerived,
            "attested OS image is a dev image (SSH / serial console enabled), not production",
        ),
        None => Claim::unknown(),
    }
}

/// Tinfoil traces its serving software to reviewed source: the SEV-SNP launch
/// measurement is compared against the Sigstore golden values published for the
/// build's repo. Cite the source repo and release digest when the verifier
/// reported them.
fn tinfoil_software_claim(event: &UpstreamVerifiedEvent) -> Claim {
    let field = |key: &str| {
        event
            .provider_claims
            .as_ref()
            .and_then(|c| c.get(key))
            .and_then(Value::as_str)
    };
    let reason = match (field("config_repo"), field("release_digest")) {
        (Some(repo), Some(digest)) => {
            format!("Sigstore-verified code measurement matches {repo} (release {digest})")
        }
        (Some(repo), None) => format!("Sigstore-verified code measurement matches {repo}"),
        _ => "Sigstore-verified code measurement matches the published golden values".to_string(),
    };
    Claim::asserted(ClaimSource::VerifierDerived, reason)
}

struct MiddlewareProviderResponseDraftingStream {
    inner: UpstreamBodyStream,
    builder: Option<ReceiptBuilder>,
    journal: MiddlewareReceiptJournal,
    provider_response_hasher: Sha256,
    receipt_id: String,
    endpoint_path: String,
    sse_parser: SseChatIdParser,
    metrics: Arc<ServiceMetrics>,
    upstream_status: u16,
    upstream_ended: bool,
    finished: bool,
}

impl Unpin for MiddlewareProviderResponseDraftingStream {}

impl Stream for MiddlewareProviderResponseDraftingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            if this.upstream_ended {
                this.finished = true;
                return match this.publish_draft() {
                    Ok(()) => Poll::Ready(None),
                    Err(err) => {
                        this.metrics.record_stream_error(
                            &this.endpoint_path,
                            StreamErrorKind::ReceiptFinalize,
                        );
                        Poll::Ready(Some(Err(err)))
                    }
                };
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.provider_response_hasher.update(&chunk);
                    this.sse_parser.observe(&chunk);
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(err))) => {
                    this.metrics
                        .record_stream_error(&this.endpoint_path, StreamErrorKind::UpstreamRead);
                    this.finished = true;
                    return Poll::Ready(Some(Err(ServiceError::Upstream(err))));
                }
                Poll::Ready(None) => {
                    this.upstream_ended = true;
                }
            }
        }
    }
}

impl MiddlewareProviderResponseDraftingStream {
    fn publish_draft(&mut self) -> Result<(), ServiceError> {
        let provider_response_hash = format!(
            "sha256:{}",
            hex::encode(self.provider_response_hasher.clone().finalize())
        );
        let response_model = self.sse_parser.model_id.clone();
        let mut builder = self.builder.take().ok_or(ReceiptError::EmptyReceipt)?;
        builder.set_chat_id(self.sse_parser.chat_id.clone());
        builder.set_upstream_verified_model_id(response_model.clone());
        builder.add_response_received_hash(provider_response_hash.clone())?;
        self.journal.set(MiddlewareReceiptDraft {
            receipt_id: self.receipt_id.clone(),
            builder,
            provider_response_hash,
            endpoint_path: self.endpoint_path.clone(),
            request_mode: RequestMode::Streaming,
            response_model: response_model.clone(),
        });
        self.metrics.record_upstream_response(
            &self.endpoint_path,
            RequestMode::Streaming,
            self.upstream_status,
            response_model.as_deref(),
        );
        Ok(())
    }
}

struct MiddlewareResponseFinalizingStream {
    inner: ServiceResponseStream,
    journal: MiddlewareReceiptJournal,
    cleartext_hasher: Sha256,
    wire_hasher: Sha256,
    keys: Arc<dyn KeyProvider>,
    receipt_store: Arc<dyn ReceiptStore>,
    key_id: String,
    requester: Option<ReceiptOwner>,
    receipt_ttl_seconds: u64,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    endpoint_path: String,
    sse_parser: SseChatIdParser,
    e2ee_transformer: Option<E2eeSseTransformer>,
    response_modified_by_wire: bool,
    upstream_ended: bool,
    finished: bool,
}

impl Unpin for MiddlewareResponseFinalizingStream {}

impl Stream for MiddlewareResponseFinalizingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            if this.upstream_ended {
                if let Some(mut transformer) = this.e2ee_transformer.take() {
                    let wire = match transformer.finish() {
                        Ok(wire) => wire,
                        Err(err) => {
                            this.metrics
                                .record_stream_error(&this.endpoint_path, StreamErrorKind::E2ee);
                            this.finished = true;
                            return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                        }
                    };
                    if !wire.is_empty() {
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }
                }
                this.finished = true;
                return match this.finalize_receipt() {
                    Ok(()) => Poll::Ready(None),
                    Err(err) => {
                        this.metrics.record_stream_error(
                            &this.endpoint_path,
                            StreamErrorKind::ReceiptFinalize,
                        );
                        Poll::Ready(Some(Err(err)))
                    }
                };
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.cleartext_hasher.update(&chunk);
                    this.sse_parser.observe(&chunk);

                    if let Some(transformer) = this.e2ee_transformer.as_mut() {
                        let wire = match transformer.push_chunk(&chunk) {
                            Ok(wire) => wire,
                            Err(err) => {
                                this.metrics.record_stream_error(
                                    &this.endpoint_path,
                                    StreamErrorKind::E2ee,
                                );
                                this.finished = true;
                                return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                            }
                        };
                        if wire.is_empty() {
                            continue;
                        }
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }

                    this.wire_hasher.update(&chunk);
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(err))) => {
                    this.metrics
                        .record_stream_error(&this.endpoint_path, StreamErrorKind::UpstreamRead);
                    this.finished = true;
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(None) => {
                    this.upstream_ended = true;
                }
            }
        }
    }
}

impl MiddlewareResponseFinalizingStream {
    fn finalize_receipt(&mut self) -> Result<(), ServiceError> {
        let Some(mut draft) = self.journal.take() else {
            return Ok(());
        };
        let cleartext_hash = format!(
            "sha256:{}",
            hex::encode(self.cleartext_hasher.clone().finalize())
        );
        let wire_hash = format!(
            "sha256:{}",
            hex::encode(self.wire_hasher.clone().finalize())
        );

        if self.sse_parser.chat_id.is_some() {
            draft.builder.set_chat_id(self.sse_parser.chat_id.clone());
        }
        if draft.provider_response_hash != cleartext_hash || self.response_modified_by_wire {
            draft
                .builder
                .add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        draft
            .builder
            .add_response_returned_hashes(cleartext_hash, wire_hash)?;
        let receipt = draft.builder.finalize(self.keys.as_ref(), &self.key_id)?;

        let now = self.clock.now_secs();
        let expires_at = now.saturating_add(self.receipt_ttl_seconds);
        self.receipt_store
            .put(receipt, self.requester.clone(), expires_at);

        self.metrics.record_receipt_issued(
            &draft.endpoint_path,
            draft.request_mode,
            draft.response_model.as_deref(),
        );
        Ok(())
    }
}

struct ReceiptFinalizingStream {
    inner: UpstreamBodyStream,
    builder: Option<ReceiptBuilder>,
    cleartext_hasher: Sha256,
    wire_hasher: Sha256,
    keys: Arc<dyn KeyProvider>,
    receipt_store: Arc<dyn ReceiptStore>,
    key_id: String,
    requester: Option<ReceiptOwner>,
    receipt_ttl_seconds: u64,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    endpoint_path: String,
    sse_parser: SseChatIdParser,
    e2ee_transformer: Option<E2eeSseTransformer>,
    response_modified: bool,
    upstream_ended: bool,
    finished: bool,
}

impl Unpin for ReceiptFinalizingStream {}

impl Stream for ReceiptFinalizingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            if this.upstream_ended {
                if let Some(mut transformer) = this.e2ee_transformer.take() {
                    let wire = match transformer.finish() {
                        Ok(wire) => wire,
                        Err(err) => {
                            this.metrics
                                .record_stream_error(&this.endpoint_path, StreamErrorKind::E2ee);
                            this.finished = true;
                            return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                        }
                    };
                    if !wire.is_empty() {
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }
                }
                this.finished = true;
                return match this.finalize_receipt() {
                    Ok(()) => Poll::Ready(None),
                    Err(err) => {
                        this.metrics.record_stream_error(
                            &this.endpoint_path,
                            StreamErrorKind::ReceiptFinalize,
                        );
                        Poll::Ready(Some(Err(err)))
                    }
                };
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.cleartext_hasher.update(&chunk);
                    this.sse_parser.observe(&chunk);

                    if let Some(transformer) = this.e2ee_transformer.as_mut() {
                        let wire = match transformer.push_chunk(&chunk) {
                            Ok(wire) => wire,
                            Err(err) => {
                                this.metrics.record_stream_error(
                                    &this.endpoint_path,
                                    StreamErrorKind::E2ee,
                                );
                                this.finished = true;
                                return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                            }
                        };
                        if wire.is_empty() {
                            continue;
                        }
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }

                    this.wire_hasher.update(&chunk);
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(err))) => {
                    this.metrics
                        .record_stream_error(&this.endpoint_path, StreamErrorKind::UpstreamRead);
                    this.finished = true;
                    return Poll::Ready(Some(Err(ServiceError::Upstream(err))));
                }
                Poll::Ready(None) => {
                    this.upstream_ended = true;
                }
            }
        }
    }
}

impl ReceiptFinalizingStream {
    fn finalize_receipt(&mut self) -> Result<(), ServiceError> {
        let cleartext_hash = format!(
            "sha256:{}",
            hex::encode(self.cleartext_hasher.clone().finalize())
        );
        let wire_hash = format!(
            "sha256:{}",
            hex::encode(self.wire_hasher.clone().finalize())
        );
        let mut builder = self.builder.take().ok_or(ReceiptError::EmptyReceipt)?;
        builder.set_chat_id(self.sse_parser.chat_id.clone());
        builder.set_upstream_verified_model_id(self.sse_parser.model_id.clone());
        if self.response_modified {
            builder.add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        builder.add_response_returned_hashes(cleartext_hash, wire_hash)?;
        let receipt = builder.finalize(self.keys.as_ref(), &self.key_id)?;

        let now = self.clock.now_secs();
        let expires_at = now.saturating_add(self.receipt_ttl_seconds);
        self.receipt_store
            .put(receipt, self.requester.clone(), expires_at);

        self.metrics.record_upstream_response(
            &self.endpoint_path,
            RequestMode::Streaming,
            200,
            self.sse_parser.model_id.as_deref(),
        );
        self.metrics.record_receipt_issued(
            &self.endpoint_path,
            RequestMode::Streaming,
            self.sse_parser.model_id.as_deref(),
        );

        Ok(())
    }
}

struct E2eeSseTransformer {
    line_buffer: Vec<u8>,
    event_lines: Vec<Vec<u8>>,
    ctx: E2eeRequestContext,
    endpoint_path: String,
}

impl E2eeSseTransformer {
    fn new(ctx: E2eeRequestContext, endpoint_path: String) -> Self {
        Self {
            line_buffer: Vec::new(),
            event_lines: Vec::new(),
            ctx,
            endpoint_path,
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<u8>, E2eeError> {
        let mut out = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line_buffer);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                out.extend(self.observe_line(line)?);
            } else {
                self.line_buffer.push(byte);
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<u8>, E2eeError> {
        let mut out = Vec::new();
        if !self.line_buffer.is_empty() {
            let mut line = std::mem::take(&mut self.line_buffer);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            out.extend(self.observe_line(line)?);
        }
        if !self.event_lines.is_empty() {
            out.extend(self.dispatch_event()?);
        }
        Ok(out)
    }

    fn observe_line(&mut self, line: Vec<u8>) -> Result<Vec<u8>, E2eeError> {
        if line.is_empty() {
            return self.dispatch_event();
        }
        self.event_lines.push(line);
        Ok(Vec::new())
    }

    fn dispatch_event(&mut self) -> Result<Vec<u8>, E2eeError> {
        let lines = std::mem::take(&mut self.event_lines);
        if lines.is_empty() {
            return Ok(Vec::new());
        }

        let Some(data) = sse_event_data(&lines) else {
            return Ok(serialize_original_sse_event(&lines));
        };
        if data.as_slice() == b"[DONE]" {
            return Ok(serialize_original_sse_event(&lines));
        }

        let encrypted_payload = encrypt_e2ee_stream_payload(&data, &self.ctx, &self.endpoint_path)?;
        let mut out = Vec::new();
        for line in &lines {
            if !is_sse_data_line(line) {
                out.extend_from_slice(line);
                out.push(b'\n');
            }
        }
        out.extend_from_slice(b"data: ");
        out.extend_from_slice(&encrypted_payload);
        out.extend_from_slice(b"\n\n");
        Ok(out)
    }
}

fn sse_event_data(lines: &[Vec<u8>]) -> Option<Vec<u8>> {
    let mut found = false;
    let mut out = Vec::new();
    for line in lines {
        if line.starts_with(b":") {
            continue;
        }
        let Some(rest) = line.strip_prefix(b"data:") else {
            continue;
        };
        let data = rest.strip_prefix(b" ").unwrap_or(rest);
        if found {
            out.push(b'\n');
        }
        out.extend_from_slice(data);
        found = true;
    }
    found.then_some(out)
}

fn is_sse_data_line(line: &[u8]) -> bool {
    line.strip_prefix(b"data:").is_some()
}

fn serialize_original_sse_event(lines: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out.push(b'\n');
    out
}

#[derive(Default)]
struct SseChatIdParser {
    line_buffer: Vec<u8>,
    event_data: Vec<u8>,
    chat_id: Option<String>,
    model_id: Option<String>,
}

impl SseChatIdParser {
    fn observe(&mut self, chunk: &[u8]) {
        if self.chat_id.is_some() && self.model_id.is_some() {
            return;
        }
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line_buffer);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.observe_line(&line);
                if self.chat_id.is_some() && self.model_id.is_some() {
                    return;
                }
            } else {
                self.line_buffer.push(byte);
            }
        }
    }

    fn observe_line(&mut self, line: &[u8]) {
        if line.is_empty() {
            self.dispatch_event();
            return;
        }
        if line.starts_with(b":") {
            return;
        }
        let Some(rest) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = rest.strip_prefix(b" ").unwrap_or(rest);
        if !self.event_data.is_empty() {
            self.event_data.push(b'\n');
        }
        self.event_data.extend_from_slice(data);
    }

    fn dispatch_event(&mut self) {
        if self.event_data.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.event_data);
        if data.as_slice() == b"[DONE]" {
            return;
        }
        let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&data) else {
            return;
        };
        if self.chat_id.is_none() {
            self.chat_id = parsed
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
        if self.model_id.is_none() {
            self.model_id = parsed
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
    }
}

fn validate_e2ee_nonce(nonce: &str) -> Result<(), E2eeError> {
    if nonce.is_empty() || aad_component_is_ambiguous(nonce) {
        return Err(E2eeError::InvalidNonce);
    }
    Ok(())
}

fn validate_legacy_e2ee_nonce(nonce: &str) -> Result<(), E2eeError> {
    validate_e2ee_nonce(nonce)?;
    if nonce.len() < 16 {
        return Err(E2eeError::InvalidNonce);
    }
    Ok(())
}

fn legacy_public_keys_match(signing_algo: &str, expected_hex: &str, supplied_hex: &str) -> bool {
    match signing_algo {
        E2EE_ALGO_LEGACY_ECDSA => {
            normalize_secp256k1_public_key_hex(expected_hex).is_ok_and(|expected| {
                normalize_secp256k1_public_key_hex(supplied_hex)
                    .is_ok_and(|supplied| supplied == expected)
            })
        }
        E2EE_ALGO_LEGACY_ED25519 => {
            normalize_ed25519_public_key_hex(expected_hex).is_ok_and(|expected| {
                normalize_ed25519_public_key_hex(supplied_hex)
                    .is_ok_and(|supplied| supplied == expected)
            })
        }
        _ => false,
    }
}

fn normalize_legacy_public_key_for_replay(
    signing_algo: &str,
    value: &str,
) -> Result<String, E2eeError> {
    match signing_algo {
        E2EE_ALGO_LEGACY_ECDSA => {
            normalize_secp256k1_public_key_hex(value).map_err(|_| E2eeError::InvalidPublicKey)
        }
        E2EE_ALGO_LEGACY_ED25519 => {
            normalize_ed25519_public_key_hex(value).map_err(|_| E2eeError::InvalidPublicKey)
        }
        _ => Err(E2eeError::InvalidSigningAlgo),
    }
}

fn normalize_ed25519_public_key_hex(value: &str) -> Result<String, E2eeError> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .map_err(|_| E2eeError::InvalidPublicKey)?;
    if bytes.len() != 32 {
        return Err(E2eeError::InvalidPublicKey);
    }
    Ok(hex::encode(bytes))
}

fn validate_payload_model(payload: &Value) -> Result<String, E2eeError> {
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return Err(E2eeError::InvalidPayloadModel);
    };
    if aad_component_is_ambiguous(model) {
        return Err(E2eeError::InvalidPayloadModel);
    }
    Ok(model.to_string())
}

fn aad_component_is_ambiguous(value: &str) -> bool {
    value.contains('|') || value.contains('\r') || value.contains('\n')
}

fn request_aad(
    algo: &str,
    model: &str,
    message_index: usize,
    content_index: Option<usize>,
    nonce: &str,
    timestamp: u64,
) -> String {
    let content_index = content_index
        .map(|idx| idx.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "v2|req|algo={algo}|model={model}|m={message_index}|c={content_index}|n={nonce}|ts={timestamp}"
    )
}

fn completion_request_aad(
    algo: &str,
    model: &str,
    field_name: &str,
    nonce: &str,
    timestamp: u64,
) -> String {
    format!("v2|req|algo={algo}|model={model}|field={field_name}|n={nonce}|ts={timestamp}")
}

fn embedding_response_aad(
    algo: &str,
    model: &str,
    response_id: &str,
    data_index: u64,
    field_name: &str,
    nonce: &str,
    timestamp: u64,
) -> String {
    format!(
        "v2|resp|algo={algo}|model={model}|id={response_id}|data={data_index}|field={field_name}|n={nonce}|ts={timestamp}"
    )
}

fn response_aad(
    algo: &str,
    model: &str,
    response_id: &str,
    choice_index: u64,
    field_name: &str,
    nonce: &str,
    timestamp: u64,
) -> String {
    format!(
        "v2|resp|algo={algo}|model={model}|id={response_id}|choice={choice_index}|field={field_name}|n={nonce}|ts={timestamp}"
    )
}

struct E2eeFieldCrypto<'a> {
    keys: &'a dyn KeyProvider,
    decryptor: E2eeDecryptor<'a>,
    algo: &'a str,
    aad_mode: E2eeAadMode,
    model: &'a str,
    nonce: Option<&'a str>,
    timestamp: Option<u64>,
}

fn decrypt_request_payload(
    crypto: &E2eeFieldCrypto<'_>,
    endpoint_path: &str,
    payload: &mut Value,
) -> Result<(), E2eeError> {
    if endpoint_path == COMPLETIONS_PATH {
        return decrypt_completion_prompt(crypto, payload);
    }
    if endpoint_path == EMBEDDINGS_PATH {
        return decrypt_embedding_input(crypto, payload);
    }

    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return Err(E2eeError::DecryptionFailed);
    };
    let mut decrypted_count = 0usize;
    for (message_index, message) in messages.iter_mut().enumerate() {
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        decrypted_count += decrypt_content_value(crypto, message_index, content)?;
    }

    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
}

fn decrypt_completion_prompt(
    crypto: &E2eeFieldCrypto<'_>,
    payload: &mut Value,
) -> Result<(), E2eeError> {
    let Some(prompt) = payload.get_mut("prompt") else {
        return Err(E2eeError::DecryptionFailed);
    };

    let decrypted_count = match prompt {
        Value::String(ciphertext_hex) => {
            let aad = completion_request_aad_for_crypto(crypto, "prompt")?;
            let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
            *ciphertext_hex =
                String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
            1
        }
        Value::Array(items) => {
            let mut decrypted_count = 0usize;
            for (index, item) in items.iter_mut().enumerate() {
                let Value::String(ciphertext_hex) = item else {
                    continue;
                };
                let field_name = format!("prompt.{index}");
                let aad = completion_request_aad_for_crypto(crypto, &field_name)?;
                let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
                *ciphertext_hex =
                    String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
                decrypted_count += 1;
            }
            decrypted_count
        }
        _ => 0,
    };

    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
}

fn decrypt_embedding_input(
    crypto: &E2eeFieldCrypto<'_>,
    payload: &mut Value,
) -> Result<(), E2eeError> {
    let Some(input) = payload.get_mut("input") else {
        return Err(E2eeError::DecryptionFailed);
    };

    let decrypted_count = match input {
        Value::String(ciphertext_hex) => {
            let aad = completion_request_aad_for_crypto(crypto, "input")?;
            let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
            *ciphertext_hex =
                String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
            1
        }
        Value::Array(items) => {
            // OpenAI accepts string arrays AND integer token-id arrays
            // for `input`. Only encrypted strings carry E2EE field
            // ciphertext; numeric arrays pass through.
            let mut decrypted_count = 0usize;
            for (index, item) in items.iter_mut().enumerate() {
                let Value::String(ciphertext_hex) = item else {
                    continue;
                };
                let field_name = format!("input.{index}");
                let aad = completion_request_aad_for_crypto(crypto, &field_name)?;
                let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
                *ciphertext_hex =
                    String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
                decrypted_count += 1;
            }
            decrypted_count
        }
        _ => 0,
    };

    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
}

fn decrypt_content_value(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content: &mut Value,
) -> Result<usize, E2eeError> {
    match content {
        Value::String(ciphertext_hex) => {
            let aad = request_aad_for_crypto(crypto, message_index, None)?;
            let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
            let plaintext =
                String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
            *content = decrypted_chat_content_value(plaintext);
            Ok(1)
        }
        Value::Array(items) => {
            let mut decrypted_count = 0usize;
            for (content_index, item) in items.iter_mut().enumerate() {
                let Some(item) = item.as_object_mut() else {
                    continue;
                };
                if item.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(Value::String(ciphertext_hex)) = item.get_mut("text") else {
                    continue;
                };
                let aad = request_aad_for_crypto(crypto, message_index, Some(content_index))?;
                let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
                *ciphertext_hex =
                    String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
                decrypted_count += 1;
            }
            Ok(decrypted_count)
        }
        _ => Ok(0),
    }
}

fn request_aad_for_crypto(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content_index: Option<usize>,
) -> Result<Option<String>, E2eeError> {
    if !crypto.aad_mode.uses_aad() {
        return Ok(None);
    }
    let nonce = crypto.nonce.ok_or(E2eeError::DecryptionFailed)?;
    let timestamp = crypto.timestamp.ok_or(E2eeError::DecryptionFailed)?;
    Ok(Some(request_aad(
        crypto.algo,
        crypto.model,
        message_index,
        content_index,
        nonce,
        timestamp,
    )))
}

fn completion_request_aad_for_crypto(
    crypto: &E2eeFieldCrypto<'_>,
    field_name: &str,
) -> Result<Option<String>, E2eeError> {
    if !crypto.aad_mode.uses_aad() {
        return Ok(None);
    }
    let nonce = crypto.nonce.ok_or(E2eeError::DecryptionFailed)?;
    let timestamp = crypto.timestamp.ok_or(E2eeError::DecryptionFailed)?;
    Ok(Some(completion_request_aad(
        crypto.algo,
        crypto.model,
        field_name,
        nonce,
        timestamp,
    )))
}

fn decrypt_e2ee_field(
    crypto: &E2eeFieldCrypto<'_>,
    ciphertext_hex: &str,
    aad: Option<&str>,
) -> Result<Vec<u8>, E2eeError> {
    match crypto.decryptor {
        E2eeDecryptor::AciV2 { key_id } => {
            let aad = aad.ok_or(E2eeError::DecryptionFailed)?;
            crypto
                .keys
                .decrypt_e2ee(key_id, ciphertext_hex, aad.as_bytes())
                .map_err(|_| E2eeError::DecryptionFailed)
        }
        E2eeDecryptor::Legacy { signing_algo } => crypto
            .keys
            .decrypt_legacy_e2ee(signing_algo, ciphertext_hex, aad.map(str::as_bytes))
            .map_err(|_| E2eeError::DecryptionFailed),
    }
}

fn decrypted_chat_content_value(plaintext: String) -> Value {
    match serde_json::from_str::<Value>(&plaintext) {
        Ok(Value::Array(items)) => Value::Array(items),
        _ => Value::String(plaintext),
    }
}

fn encrypt_e2ee_response_body(
    cleartext_body: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Vec<u8>, E2eeError> {
    let mut payload: Value =
        serde_json::from_slice(cleartext_body).map_err(|_| E2eeError::EncryptionFailed)?;
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let response_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ctx.aad_mode.uses_aad() && aad_component_is_ambiguous(&response_id) {
        return Err(E2eeError::EncryptionFailed);
    }

    if endpoint_path == EMBEDDINGS_PATH {
        encrypt_embedding_data(&mut payload, ctx, &response_model, &response_id)?;
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    }

    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    };

    for (position, choice) in choices.iter_mut().enumerate() {
        let choice_index = choice
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(position as u64);
        let Some(choice) = choice.as_object_mut() else {
            continue;
        };
        if endpoint_path == COMPLETIONS_PATH {
            encrypt_response_field(
                choice,
                "text",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
        } else if let Some(Value::Object(message)) = choice.get_mut("message") {
            encrypt_response_field(
                message,
                "content",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
            encrypt_response_field(
                message,
                "reasoning_content",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
        }
    }

    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

fn encrypt_embedding_data(
    payload: &mut Value,
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
) -> Result<(), E2eeError> {
    let Some(items) = payload.get_mut("data").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (position, item) in items.iter_mut().enumerate() {
        let data_index = item
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(position as u64);
        let Some(entry) = item.as_object_mut() else {
            continue;
        };
        let Some(embedding) = entry.get_mut("embedding") else {
            continue;
        };
        // OpenAI emits `embedding` as a float array by default and as a
        // base64 string when the client passes `encoding_format=base64`.
        // We serialize to compact JSON before encryption so the
        // decrypted plaintext round-trips through `serde_json` back to
        // the original type, mirroring how chat content arrays are
        // recovered.
        let plaintext = serde_json::to_vec(embedding).map_err(|_| E2eeError::EncryptionFailed)?;
        let aad = embedding_response_aad_for_context(
            ctx,
            response_model,
            response_id,
            data_index,
            "embedding",
        )?;
        let ciphertext_hex = encrypt_response_plaintext(ctx, &plaintext, aad.as_deref())?;
        *embedding = Value::String(ciphertext_hex);
    }
    Ok(())
}

fn embedding_response_aad_for_context(
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    data_index: u64,
    field_name: &str,
) -> Result<Option<String>, E2eeError> {
    if !ctx.aad_mode.uses_aad() {
        return Ok(None);
    }
    if aad_component_is_ambiguous(field_name) {
        return Err(E2eeError::EncryptionFailed);
    }
    let model = match ctx.aad_mode {
        E2eeAadMode::AciV2 => ctx.request_model.as_str(),
        E2eeAadMode::LegacyV2 => response_model,
        E2eeAadMode::LegacyV1 => return Ok(None),
    };
    if aad_component_is_ambiguous(model) {
        return Err(E2eeError::EncryptionFailed);
    }
    let nonce = ctx.nonce.as_deref().ok_or(E2eeError::EncryptionFailed)?;
    let timestamp = ctx.timestamp.ok_or(E2eeError::EncryptionFailed)?;
    Ok(Some(embedding_response_aad(
        &ctx.algo,
        model,
        response_id,
        data_index,
        field_name,
        nonce,
        timestamp,
    )))
}

fn encrypt_e2ee_final_response(
    cleartext_body: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
    is_sse: bool,
) -> Result<Vec<u8>, E2eeError> {
    if !is_sse {
        return encrypt_e2ee_response_body(cleartext_body, ctx, endpoint_path);
    }
    let mut transformer = E2eeSseTransformer::new(ctx.clone(), endpoint_path.to_string());
    let mut out = transformer.push_chunk(cleartext_body)?;
    out.extend(transformer.finish()?);
    Ok(out)
}

fn is_sse_content_type(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn encrypt_e2ee_stream_payload(
    cleartext_payload: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Vec<u8>, E2eeError> {
    if endpoint_path == EMBEDDINGS_PATH {
        // OpenAI's embeddings endpoint is buffered-only; the router
        // forces stream=false, so reaching here means an internal
        // inconsistency that we fail closed on.
        return Err(E2eeError::EncryptionFailed);
    }
    let mut payload: Value =
        serde_json::from_slice(cleartext_payload).map_err(|_| E2eeError::EncryptionFailed)?;
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let response_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ctx.aad_mode.uses_aad() && aad_component_is_ambiguous(&response_id) {
        return Err(E2eeError::EncryptionFailed);
    }

    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    };

    for (position, choice) in choices.iter_mut().enumerate() {
        let choice_index = choice
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(position as u64);
        let Some(choice) = choice.as_object_mut() else {
            continue;
        };
        if endpoint_path == COMPLETIONS_PATH {
            encrypt_response_field(
                choice,
                "text",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
        } else if let Some(Value::Object(delta)) = choice.get_mut("delta") {
            if delta.get("content").and_then(Value::as_str) == Some("") {
                delta.remove("content");
            }
            encrypt_response_field(
                delta,
                "content",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
            encrypt_response_field(
                delta,
                "reasoning_content",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
        }
    }

    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

fn encrypt_response_field(
    container: &mut serde_json::Map<String, Value>,
    field_name: &str,
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    choice_index: u64,
) -> Result<(), E2eeError> {
    if aad_component_is_ambiguous(field_name) {
        return Err(E2eeError::EncryptionFailed);
    }
    let Some(Value::String(plaintext)) = container.get_mut(field_name) else {
        return Ok(());
    };
    let aad = response_aad_for_context(ctx, response_model, response_id, choice_index, field_name)?;
    *plaintext = encrypt_response_plaintext(ctx, plaintext.as_bytes(), aad.as_deref())?;
    Ok(())
}

fn response_aad_for_context(
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    choice_index: u64,
    field_name: &str,
) -> Result<Option<String>, E2eeError> {
    if !ctx.aad_mode.uses_aad() {
        return Ok(None);
    }
    if aad_component_is_ambiguous(field_name) {
        return Err(E2eeError::EncryptionFailed);
    }
    let model = match ctx.aad_mode {
        E2eeAadMode::AciV2 => ctx.request_model.as_str(),
        E2eeAadMode::LegacyV2 => response_model,
        E2eeAadMode::LegacyV1 => return Ok(None),
    };
    if aad_component_is_ambiguous(model) {
        return Err(E2eeError::EncryptionFailed);
    }
    let nonce = ctx.nonce.as_deref().ok_or(E2eeError::EncryptionFailed)?;
    let timestamp = ctx.timestamp.ok_or(E2eeError::EncryptionFailed)?;
    Ok(Some(response_aad(
        &ctx.algo,
        model,
        response_id,
        choice_index,
        field_name,
        nonce,
        timestamp,
    )))
}

fn encrypt_response_plaintext(
    ctx: &E2eeRequestContext,
    plaintext: &[u8],
    aad: Option<&str>,
) -> Result<String, E2eeError> {
    match ctx.aad_mode {
        E2eeAadMode::AciV2 => {
            let aad = aad.ok_or(E2eeError::EncryptionFailed)?;
            encrypt_for_public_key(&ctx.client_public_key_hex, plaintext, aad.as_bytes())
                .map_err(|_| E2eeError::EncryptionFailed)
        }
        E2eeAadMode::LegacyV1 | E2eeAadMode::LegacyV2 => encrypt_legacy_for_public_key(
            &ctx.algo,
            &ctx.client_public_key_hex,
            plaintext,
            aad.map(str::as_bytes),
        )
        .map_err(|_| E2eeError::EncryptionFailed),
    }
}

async fn collect_upstream_body(mut body: UpstreamBodyStream) -> Result<Vec<u8>, ServiceError> {
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

fn generate_receipt_id() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut bytes = [0u8; 12];
    rng.fill_bytes(&mut bytes);
    format!("rcpt-{}", hex::encode(bytes))
}

fn extract_chat_id(body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let trimmed = body.iter().position(|b| !b.is_ascii_whitespace())?;
    if body[trimmed] != b'{' {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let id = parsed.get("id")?.as_str()?;
    Some(id.to_string())
}

fn accepted_response_model(status_code: u16, body: &[u8]) -> Option<String> {
    if !(200..=299).contains(&status_code) || body.is_empty() {
        return None;
    }
    let trimmed = body.iter().position(|b| !b.is_ascii_whitespace())?;
    if body[trimmed] != b'{' {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    parsed.get("model")?.as_str().map(str::to_string)
}

fn legacy_signature_text(receipt: &Receipt) -> Option<String> {
    let request_hash = receipt
        .event_log
        .iter()
        .find(|e| e.event_type == EVENT_REQUEST_RECEIVED)?
        .fields
        .get("body_hash")?
        .as_str()
        .and_then(strip_sha256_prefix)?;
    let response_hash = receipt
        .event_log
        .iter()
        .find(|e| e.event_type == EVENT_RESPONSE_RETURNED)?
        .fields
        .get("wire_hash")?
        .as_str()
        .and_then(strip_sha256_prefix)?;
    Some(format!("{request_hash}:{response_hash}"))
}

fn strip_sha256_prefix(value: &str) -> Option<&str> {
    value.strip_prefix("sha256:")
}

#[cfg(test)]
mod claim_mapping_tests {
    use super::session_claims_for_event;
    use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
    use crate::aggregator::session::{ClaimSource, ClaimStatus};
    use serde_json::{json, Value};

    fn event(
        provider: Option<&str>,
        result: VerificationResult,
        provider_claims: Option<Value>,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: "operator-config-name".to_string(),
            provider: provider.map(str::to_string),
            model_id: "m".to_string(),
            url_origin: Some("https://up".to_string()),
            verifier_id: "vid/v1".to_string(),
            result,
            required: true,
            reason: None,
            evidence: None,
            channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
                origin: "https://up".to_string(),
                spki_sha256: "aa".repeat(32),
            }],
            provider_claims,
        }
    }

    #[test]
    fn tinfoil_asserts_tee_and_serving_software_with_verifier_derived_tcb() {
        let claims = session_claims_for_event(&event(
            Some("tinfoil"),
            VerificationResult::Verified,
            Some(json!({
                "config_repo": "tinfoilsh/confidential-model",
                "release_digest": "sha256:abc123",
            })),
        ));
        // TEE is hardware-proven.
        assert_eq!(claims.tee_attested.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.tee_attested.source,
            Some(ClaimSource::HardwareProven)
        );
        // TCB is asserted but VerifierDerived — Tinfoil's verifier gates on TCB
        // yet exposes no raw TcbStatus, so it must NOT be labeled HardwareProven
        // (regression guard for the fabricated-"UpToDate" bug).
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.tcb_up_to_date.source,
            Some(ClaimSource::VerifierDerived)
        );
        assert_ne!(
            claims.tcb_up_to_date.source,
            Some(ClaimSource::HardwareProven)
        );
        // Serving software is verifier-derived (Sigstore), and cites the source.
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Asserted
        );
        assert_eq!(
            claims.serving_software_known_good.source,
            Some(ClaimSource::VerifierDerived)
        );
        let reason = claims.serving_software_known_good.reason.unwrap();
        assert!(reason.contains("tinfoilsh/confidential-model"), "{reason}");
        assert!(reason.contains("sha256:abc123"), "{reason}");
        // Honest Unknowns: no OS/GPU/weights provenance proven here.
        assert_eq!(claims.os_known_good.status, ClaimStatus::Unknown);
        assert_eq!(claims.gpu_attested.status, ClaimStatus::Unknown);
        assert_eq!(claims.model_weights_provenance.status, ClaimStatus::Unknown);
        // Raw provider_claims preserved verbatim for deep audit.
        assert_eq!(
            claims.extra.get("config_repo").and_then(Value::as_str),
            Some("tinfoilsh/confidential-model")
        );
    }

    #[test]
    fn near_and_chutes_assert_tee_but_not_serving_software() {
        for provider in ["near-ai", "chutes"] {
            let claims = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                Some(json!({ "tcb_status": "UpToDate" })),
            ));
            assert_eq!(
                claims.tee_attested.status,
                ClaimStatus::Asserted,
                "{provider}"
            );
            // Neither traces serving software to reviewed source.
            assert_eq!(
                claims.serving_software_known_good.status,
                ClaimStatus::Unknown,
                "{provider}"
            );
            assert_eq!(
                claims.gpu_attested.status,
                ClaimStatus::Unknown,
                "{provider}"
            );
        }
    }

    #[test]
    fn os_known_good_refutes_a_dev_image_and_asserts_production() {
        // Phala surfaces production_os_image, resolved from the attested
        // os_image_hash. A dev image (operator console) is refuted, not silently
        // Unknown — a real platform-security signal the client can see.
        let dev = session_claims_for_event(&event(
            Some("phala-direct"),
            VerificationResult::Verified,
            Some(json!({ "production_os_image": false })),
        ));
        assert_eq!(dev.os_known_good.status, ClaimStatus::Refuted);
        assert_eq!(dev.os_known_good.source, Some(ClaimSource::VerifierDerived));

        let prod = session_claims_for_event(&event(
            Some("phala-direct"),
            VerificationResult::Verified,
            Some(json!({ "production_os_image": true })),
        ));
        assert_eq!(prod.os_known_good.status, ClaimStatus::Asserted);

        // Not surfaced / unresolved ⇒ Unknown (e.g. Tinfoil, or an unresolved hash).
        let unknown =
            session_claims_for_event(&event(Some("tinfoil"), VerificationResult::Verified, None));
        assert_eq!(unknown.os_known_good.status, ClaimStatus::Unknown);
    }

    #[test]
    fn tcb_up_to_date_is_a_hardware_proven_tri_state_for_dstack_providers() {
        // The dstack-based providers surface a real TcbStatus from DCAP
        // collateral. (Tinfoil is excluded: its verifier exposes no raw status,
        // so its TCB claim is VerifierDerived, asserted earlier in this module.)
        for provider in ["near-ai", "chutes", "phala-direct"] {
            // UpToDate asserts.
            let up = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                Some(json!({ "tcb_status": "UpToDate" })),
            ));
            assert_eq!(
                up.tcb_up_to_date.status,
                ClaimStatus::Asserted,
                "{provider}"
            );

            // A stale TCB is refuted from the quote — but the session is still
            // created (we do not hard-reject), and TEE attestation still holds.
            let stale = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                Some(json!({ "tcb_status": "OutOfDate" })),
            ));
            assert_eq!(
                stale.tcb_up_to_date.status,
                ClaimStatus::Refuted,
                "{provider}"
            );
            assert_eq!(
                stale.tcb_up_to_date.source,
                Some(ClaimSource::HardwareProven),
                "{provider}"
            );
            assert_eq!(
                stale.tee_attested.status,
                ClaimStatus::Asserted,
                "{provider}"
            );

            // No surfaced status ⇒ Unknown; freshness is never asserted by policy.
            let missing = session_claims_for_event(&event(
                Some(provider),
                VerificationResult::Verified,
                None,
            ));
            assert_eq!(
                missing.tcb_up_to_date.status,
                ClaimStatus::Unknown,
                "{provider}"
            );
            assert_eq!(
                missing.tee_attested.status,
                ClaimStatus::Asserted,
                "{provider}"
            );
        }
    }

    #[test]
    fn generic_provider_asserts_only_tee_verifier_derived() {
        let claims = session_claims_for_event(&event(None, VerificationResult::Verified, None));
        assert_eq!(claims.tee_attested.status, ClaimStatus::Asserted);
        assert_eq!(
            claims.tee_attested.source,
            Some(ClaimSource::VerifierDerived)
        );
        // No TCB/software guarantees from an unidentified verifier.
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Unknown);
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Unknown
        );
    }

    #[test]
    fn failed_result_asserts_nothing_but_preserves_evidence() {
        let claims = session_claims_for_event(&event(
            Some("tinfoil"),
            VerificationResult::Failed,
            Some(json!({ "config_repo": "x" })),
        ));
        assert_eq!(claims.tee_attested.status, ClaimStatus::Unknown);
        assert_eq!(claims.tcb_up_to_date.status, ClaimStatus::Unknown);
        assert_eq!(
            claims.serving_software_known_good.status,
            ClaimStatus::Unknown
        );
        // Raw claims are still recorded for the audit trail.
        assert!(claims.extra.contains_key("config_repo"));
    }
}
