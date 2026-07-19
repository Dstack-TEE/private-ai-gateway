use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;

use super::{ReceiptOwner, ServiceError};
use crate::aci::receipt::{ReceiptBuilder, UpstreamVerifiedEvent};
use crate::aci::types::Receipt;
use crate::aggregator::metrics::RequestMode;

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
    pub(super) version: String,
    pub(super) algo: String,
    pub(super) aad_mode: E2eeAadMode,
    pub(super) request_model: String,
    pub(super) client_public_key_hex: String,
    pub(super) nonce: Option<String>,
    pub(super) timestamp: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum E2eeAadMode {
    /// The spec ACI v2 path: JCS AAD (§7.3).
    AciV2,
    /// The inherited dstack-vllm-proxy path (`X-Signing-Algo`): no AAD (§13).
    LegacyV1,
}

impl E2eeAadMode {
    /// The spec ACI v2 path (JCS AAD, §7.3), as opposed to the no-AAD legacy
    /// X-Signing-Algo compatibility mode. Per-part multimodal field paths
    /// (§7.2) exist only here.
    pub(super) fn is_aci(self) -> bool {
        matches!(self, Self::AciV2)
    }
}

pub(super) enum E2eeDecryptor<'a> {
    AciV2 { key_id: &'a str },
    Legacy { signing_algo: &'a str },
}
#[derive(Debug, Clone)]
pub struct ForwardResult {
    pub receipt: Receipt,
    /// Client-facing status: the upstream's, or 400 when the service remapped a
    /// client image-URL fetch failure. The receipt attests the matching body.
    pub upstream_status: u16,
    pub upstream_body: Vec<u8>,
    pub upstream_headers: std::collections::HashMap<String, String>,
    pub e2ee: Option<E2eeResponseInfo>,
}

pub enum MiddlewareForwardResult {
    Forwarded(Box<MiddlewareForwarded>),
    Stream(Box<MiddlewareStreamingForwarded>),
    UpstreamError(Box<MiddlewareUpstreamError>),
    /// Every candidate was attempted and failed without an HTTP response to
    /// relay. Carries the per-attempt outcomes so the caller can report each
    /// attempt (they are otherwise unrecoverable from the aggregated error)
    /// and derive an honest client status from the failure mix.
    AllFailed(Box<MiddlewareAllFailed>),
}

pub struct MiddlewareAllFailed {
    /// Every candidate attempted, as (route_id, status), in the order tried.
    /// Non-HTTP failures (prepare/verification/transport) record 502; HTTP
    /// failures record the upstream status (e.g. 429).
    ///
    /// Usage-report consumers: the caller reports these as attempt rows
    /// 0..N-1, and MAY additionally report one request-level summary row at
    /// attempt index N (currently only for 5xx aggregate errors) with an
    /// empty route and a gateway/upstream error source. A row with no route
    /// and a non-empty error source is a request-level summary, not an
    /// attempt; attempt counting must only consider rows that carry a route.
    pub failed_attempts: Vec<(String, u16)>,
    /// The highest-priority underlying failure across the attempts.
    pub error: ServiceError,
}

pub struct MiddlewareForwarded {
    pub receipt_id: String,
    pub receipt: MiddlewareReceiptDraft,
    pub upstream_status: u16,
    pub upstream_body: Vec<u8>,
    pub upstream_headers: std::collections::HashMap<String, String>,
    /// Which route served the request and the attested session id (if any).
    /// These are internal routing outcomes, not emitted as response headers;
    /// the committed reference for what happened is the receipt.
    pub selected_route: String,
    /// Failed-over candidates as (route_id, status), in the order tried. The
    /// committed route is `selected_route`; these are surfaced so the caller can
    /// observe every attempt, not just the one that served the response.
    pub failed_attempts: Vec<(String, u16)>,
    pub session_id: Option<String>,
}

pub struct MiddlewareStreamingForwarded {
    pub receipt_id: String,
    pub upstream_status: u16,
    pub upstream_headers: std::collections::HashMap<String, String>,
    pub body: ServiceResponseStream,
    /// Which route served the request and the attested session id (if any).
    /// These are internal routing outcomes, not emitted as response headers;
    /// the committed reference for what happened is the receipt.
    pub selected_route: String,
    /// Failed-over candidates as (route_id, status), in the order tried. The
    /// committed route is `selected_route`; these are surfaced so the caller can
    /// observe every attempt, not just the one that served the response.
    pub failed_attempts: Vec<(String, u16)>,
    pub session_id: Option<String>,
}

pub struct MiddlewareReceiptDraft {
    pub(super) receipt_id: String,
    pub(super) builder: ReceiptBuilder,
    pub(super) provider_response_hash: String,
    pub(super) endpoint_path: String,
    pub(super) request_mode: RequestMode,
    pub(super) response_model: Option<String>,
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

/// A streaming non-2xx on the middleware path, carrying the same route
/// attribution as the `Forwarded`/`Stream` variants. No receipt is issued (there
/// is no completed inference stream to bind), but the attempt still reached an
/// upstream, so the caller must be able to report *which* route produced the
/// status — an unattributed report cannot count against any route's health, which
/// would leave the load behind upstream 429s invisible and never shed.
pub struct MiddlewareUpstreamError {
    pub error: StreamingUpstreamError,
    /// The route that produced the non-2xx (the last one tried).
    pub selected_route: String,
    /// Candidates failed over before this one, as (route_id, status), in the
    /// order tried. Same contract as the sibling variants' field.
    pub failed_attempts: Vec<(String, u16)>,
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
    /// Optional x-user-tier value to relay to the upstream. None when the
    /// caller supplied no x-user-tier header.
    pub user_tier: Option<String>,
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
