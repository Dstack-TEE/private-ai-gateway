//! Pins the channel-binding reverify/retry loop in
//! `AciService::forward_with_binding_reverify` and its invalidate-vs-not policy
//! at the two public entry points (`forward_chat_completion_request` and
//! `forward_chat_completion_for_middleware`).
//!
//! `CHANNEL_BINDING_REVERIFY_ATTEMPTS == 2`, so a gateway-owned event that keeps
//! mismatching does: 1 initial verify + 2 reverify rounds, each round
//! invalidating-then-verifying, then a terminal invalidate.

use common::admission;
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, ChatCompletionRequest, FixedClock, ForwardCandidate,
    GatewayRequestContext, InMemoryReceiptStore, MiddlewareForwardResult, MiddlewareReceiptJournal,
    ServiceError, UpstreamVerificationError, UpstreamVerificationRequest, UpstreamVerifier,
};

use common::{event_from_request, verified_event, StaticKeyProvider, StubQuoter};

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
        // No channel bindings (left at default): the canned event itself never
        // makes the backend's default fail-closed guard fire; the mock backend
        // below raises the mismatch deterministically instead.
        UpstreamVerifiedEvent {
            verifier_id: "recording-verifier/v1".to_string(),
            ..event_from_request(&request, VerificationResult::Verified)
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

/// Returns a two-key TLS group for seed + pinned initial admission, then
/// replaces one member on refresh. Sharing a key must not authorize the new group.
#[derive(Default)]
struct RotatingVerifier {
    verify_calls: AtomicUsize,
}

#[async_trait]
impl UpstreamVerifier for RotatingVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let call = self.verify_calls.fetch_add(1, Ordering::SeqCst);
        let byte = if call < 2 { "bb" } else { "cc" };
        UpstreamVerifiedEvent {
            verifier_id: "rotating-verifier/v1".to_string(),
            channel_bindings: vec![
                ChannelBinding::TlsSpkiSha256 {
                    origin: UPSTREAM_ORIGIN.to_string(),
                    spki_sha256: "aa".repeat(32),
                },
                ChannelBinding::TlsSpkiSha256 {
                    origin: UPSTREAM_ORIGIN.to_string(),
                    spki_sha256: byte.repeat(32),
                },
            ],
            ..event_from_request(&request, VerificationResult::Verified)
        }
    }
}

/// Mock `UpstreamBackend` that returns `ChannelBindingMismatch` for the first
/// `mismatches_remaining` verified-forward calls, then succeeds.
///
/// `usize::MAX` means "always mismatch". The same counter governs the buffered
/// and streaming verified-forward entry points.
struct MismatchingBackend {
    calls: AtomicUsize,
    mismatches_remaining: AtomicUsize,
}

impl MismatchingBackend {
    fn always_mismatch() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            mismatches_remaining: AtomicUsize::new(usize::MAX),
        }
    }

    fn mismatch_times(n: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            mismatches_remaining: AtomicUsize::new(n),
        }
    }

    fn set_mismatches(&self, n: usize) {
        self.mismatches_remaining.store(n, Ordering::SeqCst);
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
            served_instance_id: None,
        }
    }
}

#[async_trait]
impl UpstreamBackend for MismatchingBackend {
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let model_id = serde_json::from_slice::<serde_json::Value>(&req.body)
            .ok()
            .and_then(|body| {
                body.get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        Ok(PreparedUpstreamRequest {
            request: req,
            upstream_name: UPSTREAM_NAME.to_string(),
            url_origin: Some(UPSTREAM_ORIGIN.to_string()),
            model_id,
            route_id: None,
            // A TEE-attesting route, so an `aci_required` request selects it.
            is_tee: Some(true),
        })
    }

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
        self.calls.fetch_add(1, Ordering::SeqCst);
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
        self.calls.fetch_add(1, Ordering::SeqCst);
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
            served_instance_id: None,
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
            AciServiceConfig::for_test(),
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
        url_origin: Some(UPSTREAM_ORIGIN.to_string()),
        verifier_id: "caller-supplied/v1".to_string(),
        ..verified_event(UPSTREAM_NAME, "model-a")
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
        aci_required: true,
        aci_session_ids: Vec::new(),
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
    assert_eq!(
        verifier.invalidate_calls(),
        3,
        "2 reverify-round invalidations + 1 terminal flush"
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
        path: "/v1/chat/completions",
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

    // main surfaces exhaustion as `AllFailed` (carrying the per-attempt
    // outcomes) rather than an opaque Err.
    assert!(
        matches!(result, Ok(MiddlewareForwardResult::AllFailed(_)) | Err(_)),
        "single candidate that always mismatches must be exhausted"
    );
    assert!(
        verifier.invalidate_calls() >= 1,
        "middleware path must flush a possibly-stale binding even for a caller-supplied event"
    );
}
#[tokio::test]
async fn reverify_cannot_escape_the_requested_aci_group() {
    let backend = Arc::new(MismatchingBackend::mismatch_times(0));
    let verifier = Arc::new(RotatingVerifier::default());
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            backend.clone(),
            verifier.clone(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );

    // First establish and expose the id for binding A.
    let mut seed = forward_request(None);
    seed.aci_required = true;
    let seeded = service.forward_chat_completion_request(seed).await.unwrap();
    let session_id = seeded.receipt.document_json().unwrap()["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == "upstream.verified")
        .and_then(|event| event["session_id"].as_str())
        .expect("seeded verified binding must expose a session id")
        .to_string();

    // The next initial verification still returns A and may forward once. Its
    // binding mismatch refreshes to B; B is not in the allowlist, so there must
    // be no retry carrying the prompt to the rotated session.
    backend.set_mismatches(1);
    let mut pinned = forward_request(None);
    pinned.aci_required = true;
    pinned.aci_session_ids = vec![session_id];
    let err = service
        .forward_chat_completion_request(pinned)
        .await
        .expect_err("a rotated session must not satisfy the original allowlist");
    assert!(matches!(
        err,
        ServiceError::UpstreamVerification(UpstreamVerificationError::NoEligibleAttestedSession(_))
    ));
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        2,
        "seed and initial pinned attempt only; rotated retry never forwards"
    );
    assert_eq!(
        verifier.verify_calls.load(Ordering::SeqCst),
        3,
        "seed + pinned initial verify + one refresh"
    );
}

struct TimedRotatingVerifier {
    calls: AtomicUsize,
    clock: Arc<admission::TestClock>,
}
#[async_trait]
impl UpstreamVerifier for TimedRotatingVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.clock.set(if call == 0 { 100 } else { 120 });
        UpstreamVerifiedEvent {
            channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
                origin: UPSTREAM_ORIGIN.to_string(),
                spki_sha256: if call == 0 { "aa" } else { "bb" }.repeat(32),
            }],
            ..event_from_request(&request, VerificationResult::Verified)
        }
    }
}

#[tokio::test]
async fn successful_retry_cites_its_own_admission() {
    use std::sync::atomic::AtomicU64;
    let backend = Arc::new(MismatchingBackend::mismatch_times(1));
    let clock = Arc::new(admission::TestClock(AtomicU64::new(100)));
    let verifier = Arc::new(TimedRotatingVerifier {
        calls: AtomicUsize::new(0),
        clock: clock.clone(),
    });
    let mut config = AciServiceConfig::for_test();
    config.attested_session_ttl_seconds = 10;
    config.receipt_ttl_seconds = 5;
    let service = AciService::new_with_upstream_verifier(
        Arc::new(StaticKeyProvider::default()),
        Arc::new(StubQuoter::default()),
        backend.clone(),
        verifier,
        Arc::new(InMemoryReceiptStore::default()),
        config,
        clock,
    )
    .unwrap();
    let result = service
        .forward_chat_completion_request(forward_request(None))
        .await
        .unwrap();
    let document = result.receipt.document_json().unwrap();
    assert_eq!(document["served_at"], 120);
    let id = document["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == "upstream.verified")
        .unwrap()["session_id"]
        .as_str()
        .unwrap();
    let session = service.get_attested_session(id).unwrap();
    assert_eq!(session.document().established_at, 120);
    assert_eq!(
        session.document().channel_binding,
        vec![ChannelBinding::TlsSpkiSha256 {
            origin: UPSTREAM_ORIGIN.to_string(),
            spki_sha256: "bb".repeat(32)
        }]
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reverify_accepts_a_replacement_group_only_with_its_explicit_id() {
    let backend = Arc::new(MismatchingBackend::mismatch_times(0));
    let verifier = Arc::new(RotatingVerifier::default());
    let service = AciService::new_with_upstream_verifier(
        Arc::new(StaticKeyProvider::default()),
        Arc::new(StubQuoter::default()),
        backend.clone(),
        verifier.clone(),
        Arc::new(InMemoryReceiptStore::default()),
        AciServiceConfig::for_test(),
        Arc::new(FixedClock(1_700_000_000)),
    )
    .unwrap();
    let cited = |receipt: &private_ai_gateway::aci::receipt::SignedReceipt| {
        receipt.document_json().unwrap()["event_log"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["type"] == "upstream.verified")
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let original = cited(
        &service
            .forward_chat_completion_request(forward_request(None))
            .await
            .unwrap()
            .receipt,
    );
    verifier.verify_calls.store(2, Ordering::SeqCst);
    let replacement = cited(
        &service
            .forward_chat_completion_request(forward_request(None))
            .await
            .unwrap()
            .receipt,
    );
    assert_ne!(original, replacement);
    verifier.verify_calls.store(1, Ordering::SeqCst);
    backend.set_mismatches(1);
    let mut request = forward_request(None);
    request.aci_session_ids = vec![original, replacement.clone()];
    let result = service
        .forward_chat_completion_request(request)
        .await
        .unwrap();
    assert_eq!(cited(&result.receipt), replacement);
    let document = service.get_attested_session(&replacement).unwrap();
    assert_eq!(document.document().channel_binding.len(), 2);
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        4,
        "two seeds plus both admitted retry attempts"
    );
}
