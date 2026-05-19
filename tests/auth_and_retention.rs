//! Coverage for the new authn/authz layer on receipts, body retention,
//! and the ACI headers stamped on every response.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use private_ai_gateway::aci::types::ServiceCapabilities;
use private_ai_gateway::aci::upstream::{
    UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aci::verifier::PreverifiedUpstreamVerifier;
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::http::build_router;
use serde_json::Value;
use tower::ServiceExt;

use common::{StaticKeyProvider, StubQuoter};

const CHAT_REQUEST: &[u8] =
    br#"{"model":"aci-model","messages":[{"role":"user","content":"hello"}]}"#;
const CHAT_RESPONSE: &[u8] = br#"{"id":"chat-auth-1","object":"chat.completion","choices":[]}"#;

struct StubUpstream;

#[async_trait]
impl UpstreamBackend for StubUpstream {
    fn name(&self) -> &str {
        "stub-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://stub-upstream.example")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamResponse {
            status_code: 200,
            body: CHAT_RESPONSE.to_vec(),
            headers,
        })
    }
}

struct Harness {
    service: Arc<AciService>,
    router: axum::Router,
    clock: Arc<TestClock>,
}

struct TestClock {
    inner: Mutex<u64>,
}

impl TestClock {
    fn new(t0: u64) -> Self {
        Self {
            inner: Mutex::new(t0),
        }
    }
    fn advance(&self, by: u64) {
        let mut guard = self.inner.lock().unwrap();
        *guard += by;
    }
}

impl private_ai_gateway::aggregator::service::Clock for TestClock {
    fn now_secs(&self) -> u64 {
        *self.inner.lock().unwrap()
    }
}

fn harness(body_retention_seconds: u64) -> Harness {
    harness_with_ttl(body_retention_seconds, 3600)
}

fn harness_with_ttl(body_retention_seconds: u64, receipt_ttl_seconds: u64) -> Harness {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let upstream = Arc::new(StubUpstream);
    let verifier = Arc::new(PreverifiedUpstreamVerifier::new("test-verifier/v1"));
    let mut cfg = AciServiceConfig::for_test("auth-and-retention");
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: vec![],
        body_retention_seconds,
    };
    cfg.receipt_ttl_seconds = receipt_ttl_seconds;
    let clock = Arc::new(TestClock::new(1_700_000_000));
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            quoter,
            upstream,
            verifier,
            Arc::new(InMemoryReceiptStore::default()),
            cfg,
            clock.clone(),
        )
        .unwrap(),
    );
    Harness {
        router: build_router(service.clone()),
        service,
        clock,
    }
}

async fn call(router: &axum::Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

// ---------- Receipt auth ----------

#[tokio::test]
async fn anonymous_receipt_is_publicly_retrievable() {
    let h = harness(0);
    let (status, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    // No Authorization on the lookup; should succeed because the
    // receipt has no recorded owner.
    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["receipt"]["receipt_id"], rid);
    assert_eq!(json(&body)["receipt"]["chat_id"], "chat-auth-1");
    assert!(json(&body)["signature"].is_string());
}

#[tokio::test]
async fn owned_receipt_lookup_unauthenticated_returns_401() {
    let h = harness(0);
    let (status, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", "Bearer requester-a")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let _rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&body)["error"]["type"], "unauthorized");
}

#[tokio::test]
async fn owned_receipt_lookup_wrong_bearer_returns_403() {
    let h = harness(0);
    let (_, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", "Bearer requester-a")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    let _rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1")
            .header("authorization", "Bearer requester-b")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&body)["error"]["type"], "redaction_required");
}

#[tokio::test]
async fn owned_receipt_lookup_with_matching_bearer_returns_receipt() {
    let h = harness(0);
    let (_, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", "Bearer requester-a")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    let rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1")
            .header("authorization", "Bearer requester-a")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["receipt"]["receipt_id"], rid);
}

// ---------- Body retention ----------

#[tokio::test]
async fn retention_disabled_body_endpoint_returns_receipt_body_not_retained() {
    let h = harness(0);
    let (_, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    let _rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1/body")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"]["type"], "receipt_body_not_retained");
}

#[tokio::test]
async fn retention_enabled_body_endpoint_returns_retained_bytes_to_owner() {
    let h = harness(60);
    let (_, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", "Bearer requester-a")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    let _rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1/body")
            .header("authorization", "Bearer requester-a")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, CHAT_REQUEST);
}

#[tokio::test]
async fn retained_body_rejects_wrong_bearer() {
    let h = harness(60);
    let (_, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", "Bearer requester-a")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    let _rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1/body")
            .header("authorization", "Bearer requester-b")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&body)["error"]["type"], "redaction_required");
}

#[tokio::test]
async fn retained_body_expires_after_window() {
    let h = harness(60);
    let (_, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    let _rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    h.clock.advance(120);

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1/body")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"]["type"], "receipt_body_not_retained");
}

#[tokio::test]
async fn receipt_expires_after_store_ttl() {
    let h = harness_with_ttl(60, 30);
    let (_, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    let rid = headers.get("x-receipt-id").unwrap().to_str().unwrap();

    let (status, _, _) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    h.clock.advance(31);

    let (status, _, body) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/chat-auth-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"]["type"], "not_found");
    assert!(h.service.get_receipt_by_receipt_id(rid).is_none());
}

#[tokio::test]
async fn retention_propagates_into_service_capabilities() {
    let h = harness(86400);
    let report = h.service.attestation_report(None).await.unwrap();
    assert_eq!(report.service_capabilities.body_retention_seconds, 86400);
}

// ---------- X-ACI headers everywhere ----------

#[tokio::test]
async fn aci_headers_present_on_success_responses() {
    let h = harness(0);
    let (status, headers, _) = call(
        &h.router,
        Request::builder()
            .uri("/v1/attestation/report")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("x-aci-version").unwrap(), "aci/1");
    assert_eq!(
        headers.get("x-aci-identity").unwrap(),
        h.service.workload_id()
    );
    assert_eq!(
        headers.get("x-aci-keyset-digest").unwrap(),
        h.service.workload_keyset_digest()
    );
}

#[tokio::test]
async fn aci_headers_present_on_not_found_error() {
    let h = harness(0);
    let (status, headers, _) = call(
        &h.router,
        Request::builder()
            .uri("/v1/receipt/nope")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers.get("x-aci-identity").unwrap(),
        h.service.workload_id()
    );
    assert_eq!(
        headers.get("x-aci-keyset-digest").unwrap(),
        h.service.workload_keyset_digest()
    );
}

#[tokio::test]
async fn aci_headers_present_on_bad_request_error() {
    let h = harness(0);
    let (status, headers, _) = call(
        &h.router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from("not json".as_bytes().to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers.get("x-aci-identity").unwrap(),
        h.service.workload_id()
    );
}

// ---------- StaticUpstreamVerifier ----------

#[tokio::test]
async fn static_verifier_failed_with_required_blocks_forwarding() {
    use private_ai_gateway::aci::verifier::StaticUpstreamVerifier;
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let upstream = Arc::new(StubUpstream);
    let verifier = Arc::new(StaticUpstreamVerifier::failed(
        "test-verifier/v1",
        "deliberate failure",
    ));
    let mut cfg = AciServiceConfig::for_test("static-failed");
    cfg.service_capabilities = ServiceCapabilities::default();
    let svc = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            quoter,
            upstream,
            verifier,
            Arc::new(InMemoryReceiptStore::default()),
            cfg,
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let router = build_router(svc);
    let (status, _, body) = call(
        &router,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(CHAT_REQUEST.to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json(&body)["error"]["type"], "upstream_verification_failed");
}
