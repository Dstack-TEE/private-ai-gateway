//! Pins the channel-binding reverify/retry loop in
//! `AciService::forward_with_binding_reverify` and its invalidate-vs-not policy
//! at the two public entry points (`forward_chat_completion_request` and
//! `forward_chat_completion_for_middleware`).
//!
//! `CHANNEL_BINDING_REVERIFY_ATTEMPTS == 2`, so a gateway-owned event that keeps
//! mismatching does: 1 initial verify + 2 reverify rounds, each round
//! invalidating-then-verifying, then a terminal invalidate.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use private_ai_gateway::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, ChatCompletionRequest, FixedClock, ForwardCandidate,
    GatewayRequestContext, InMemoryReceiptStore, MiddlewareReceiptJournal,
    UpstreamVerificationRequest, UpstreamVerifier,
};

use common::{StaticKeyProvider, StubQuoter};

const UPSTREAM_NAME: &str = "mismatch-upstream";
const UPSTREAM_ORIGIN: &str = "https://mismatch-upstream.example";
const CHAT_BODY: &[u8] = br#"{"model":"model-a","messages":[]}"#;

/// Recording `UpstreamVerifier`.
///
/// `verify` returns a canned *passing* event (so the flow reaches the forward
/// step) and bumps `verify_calls`. `invalidate` bumps `invalidate_calls`.
/// `refresh` is left as the trait default (invalidate-then-verify), so a single
/// reverify round shows up as one bump on each counter.
#[derive(Default)]
struct RecordingVerifier {
    verify_calls: AtomicUsize,
    invalidate_calls: AtomicUsize,
}

#[async_trait]
impl UpstreamVerifier for RecordingVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verify_calls.fetch_add(1, Ordering::SeqCst);
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            provider: None,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "recording-verifier/v1".to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            reason: None,
            evidence: None,
            // No channel bindings: the canned event itself never makes the
            // backend's default fail-closed guard fire; the mock backend below
            // raises the mismatch deterministically instead.
            channel_bindings: Vec::new(),
            provider_claims: None,
        }
    }

    fn invalidate(&self, _request: &UpstreamVerificationRequest) {
        self.invalidate_calls.fetch_add(1, Ordering::SeqCst);
    }
}

impl RecordingVerifier {
    fn verify_calls(&self) -> usize {
        self.verify_calls.load(Ordering::SeqCst)
    }

    fn invalidate_calls(&self) -> usize {
        self.invalidate_calls.load(Ordering::SeqCst)
    }
}

/// Mock `UpstreamBackend` that returns `ChannelBindingMismatch` for the first
/// `mismatches_remaining` verified-forward calls, then succeeds.
///
/// `usize::MAX` means "always mismatch". The same counter governs the buffered
/// and streaming verified-forward entry points.
struct MismatchingBackend {
    mismatches_remaining: AtomicUsize,
}

impl MismatchingBackend {
    fn always_mismatch() -> Self {
        Self {
            mismatches_remaining: AtomicUsize::new(usize::MAX),
        }
    }

    fn mismatch_times(n: usize) -> Self {
        Self {
            mismatches_remaining: AtomicUsize::new(n),
        }
    }

    /// Consume one "mismatch budget" unit; returns true while the budget lasts.
    fn take_mismatch(&self) -> bool {
        loop {
            let current = self.mismatches_remaining.load(Ordering::SeqCst);
            if current == 0 {
                return false;
            }
            // Saturate at MAX so "always mismatch" never decrements to success.
            let next = if current == usize::MAX {
                usize::MAX
            } else {
                current - 1
            };
            if self
                .mismatches_remaining
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn ok_response() -> UpstreamResponse {
        UpstreamResponse {
            status_code: 200,
            body: br#"{"id":"chat-ok","object":"chat.completion","model":"model-a","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#.to_vec(),
            headers: std::collections::HashMap::new(),
        }
    }
}

#[async_trait]
impl UpstreamBackend for MismatchingBackend {
    fn name(&self) -> &str {
        UPSTREAM_NAME
    }

    fn url_origin(&self) -> Option<&str> {
        Some(UPSTREAM_ORIGIN)
    }

    // Default `prepare` derives `model_id` from the request body and copies the
    // backend `name`/`url_origin`, so `recorded_upstream_event` and the
    // verification request build consistently against this backend.

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Ok(Self::ok_response())
    }

    async fn forward_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        if self.take_mismatch() {
            return Err(UpstreamError::ChannelBindingMismatch(
                "mock channel binding mismatch".to_string(),
            ));
        }
        Ok(Self::ok_response())
    }

    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        if self.take_mismatch() {
            return Err(UpstreamError::ChannelBindingMismatch(
                "mock channel binding mismatch".to_string(),
            ));
        }
        let response = Self::ok_response();
        let body = bytes::Bytes::from(response.body);
        Ok(UpstreamStreamResponse {
            status_code: response.status_code,
            headers: response.headers,
            body: Box::pin(futures_util::stream::once(async move { Ok(body) })),
        })
    }
}

fn build_service(
    backend: Arc<MismatchingBackend>,
    verifier: Arc<RecordingVerifier>,
) -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            backend,
            verifier,
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("forward-binding-reverify-test"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

/// Canned passing event the caller "owns" — mirrors what the recording verifier
/// would have produced, so the fail-closed gate (upstream_required defaults to
/// true) is satisfied and the flow reaches the forward step.
fn caller_event() -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: UPSTREAM_NAME.to_string(),
        provider: None,
        model_id: "model-a".to_string(),
        url_origin: Some(UPSTREAM_ORIGIN.to_string()),
        verifier_id: "caller-supplied/v1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: None,
        channel_bindings: Vec::new(),
        provider_claims: None,
    }
}

fn forward_request(
    upstream_verification_event: Option<UpstreamVerifiedEvent>,
) -> ChatCompletionRequest<'static> {
    ChatCompletionRequest {
        context: GatewayRequestContext::default(),
        endpoint_path: "/v1/chat/completions",
        received_body: CHAT_BODY,
        forwarded_body: None,
        upstream_required: Some(true),
        upstream_verification_event,
        requester: None,
        e2ee: None,
    }
}

// Test 1: single forward, caller-supplied event, backend always mismatches.
// Caller-supplied suppresses reverify entirely (no verify, no refresh) and the
// single-forward path never flushes an event it does not own.
#[tokio::test]
async fn forward_caller_supplied_always_mismatch_no_verify_no_invalidate() {
    let backend = Arc::new(MismatchingBackend::always_mismatch());
    let verifier = Arc::new(RecordingVerifier::default());
    let service = build_service(backend, verifier.clone());

    let result = service
        .forward_chat_completion_request(forward_request(Some(caller_event())))
        .await;

    assert!(result.is_err(), "always-mismatch forward must fail");
    assert_eq!(
        verifier.verify_calls(),
        0,
        "caller-supplied event suppresses verify and reverify"
    );
    assert_eq!(
        verifier.invalidate_calls(),
        0,
        "single forward must not flush an event it does not own"
    );
}

// Test 2: single forward, gateway-owned event, backend always mismatches.
// The loop reverifies CHANNEL_BINDING_REVERIFY_ATTEMPTS (2) times, then flushes
// on the terminal mismatch because the gateway owns the event.
//   verify_calls     = 1 initial + 2 reverify rounds            = 3
//   invalidate_calls = 2 reverify rounds + 1 terminal flush     = 3
#[tokio::test]
async fn forward_gateway_owned_always_mismatch_reverifies_then_flushes() {
    let backend = Arc::new(MismatchingBackend::always_mismatch());
    let verifier = Arc::new(RecordingVerifier::default());
    let service = build_service(backend, verifier.clone());

    let result = service
        .forward_chat_completion_request(forward_request(None))
        .await;

    assert!(result.is_err(), "always-mismatch forward must fail");
    assert_eq!(
        verifier.verify_calls(),
        3,
        "1 initial verify + 2 reverify rounds"
    );
    assert!(
        verifier.verify_calls() > 1,
        "retries must have happened (reverify loop ran)"
    );
    assert_eq!(
        verifier.invalidate_calls(),
        3,
        "2 reverify-round invalidations + 1 terminal flush"
    );
    assert!(
        verifier.invalidate_calls() >= 1,
        "terminal mismatch must flush the gateway-owned cached event"
    );
}

// Test 3: single forward, gateway-owned event, mismatch once then OK (N=1).
// Exactly one reverify round happens, then the retry succeeds.
//   verify_calls = 1 initial + 1 reverify round = 2
#[tokio::test]
async fn forward_gateway_owned_mismatch_once_then_ok() {
    let backend = Arc::new(MismatchingBackend::mismatch_times(1));
    let verifier = Arc::new(RecordingVerifier::default());
    let service = build_service(backend, verifier.clone());

    let result = service
        .forward_chat_completion_request(forward_request(None))
        .await;

    assert!(result.is_ok(), "second attempt should succeed");
    assert_eq!(
        verifier.verify_calls(),
        2,
        "1 initial verify + exactly 1 reverify round"
    );
}

// Test 4 (security-relevant): middleware path, single candidate, caller-supplied
// event, backend always mismatches. The single candidate is exhausted (Err),
// and the failover path's `always_invalidate_on_mismatch = true` flushes the
// possibly-stale binding even though the event was caller-supplied.
#[tokio::test]
async fn middleware_single_candidate_caller_supplied_always_mismatch_flushes() {
    let backend = Arc::new(MismatchingBackend::always_mismatch());
    let verifier = Arc::new(RecordingVerifier::default());
    let service = build_service(backend, verifier.clone());

    let candidates = vec![ForwardCandidate {
        route_id: "route-a".to_string(),
        body: CHAT_BODY.to_vec(),
    }];
    let journal = MiddlewareReceiptJournal::default();

    let result = service
        .forward_chat_completion_for_middleware(
            forward_request(Some(caller_event())),
            candidates,
            false,
            journal,
        )
        .await;

    assert!(
        result.is_err(),
        "single candidate that always mismatches must be exhausted into an error"
    );
    assert!(
        verifier.invalidate_calls() >= 1,
        "middleware path must flush a possibly-stale binding even for a caller-supplied event"
    );
}
