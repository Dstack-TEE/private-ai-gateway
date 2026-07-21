//! In-process completion orchestration tests: the consult-driven paths (denial,
//! control-unavailable fail-closed, rate-limit, empty candidates) which return
//! before any upstream forward, plus the success path (consult allow → candidate
//! transform → forward → receipt finalization) against a mock upstream.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod common;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::{body::to_bytes, routing::post, Json, Router};
use futures_util::StreamExt;
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, ChatCompletionRequest, FixedClock, ForwardCandidate,
    GatewayRequestContext, InMemoryReceiptStore, MiddlewareForwardResult, MiddlewareReceiptJournal,
    ServiceError, ServiceResponseStream, UpstreamVerificationError, UpstreamVerificationRequest,
    UpstreamVerifier,
};
use private_ai_gateway::aggregator::upstream_config::{
    UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::middleware::control::ControlClient;
use private_ai_gateway::middleware::errors::{SseProtocol, Surface};
use private_ai_gateway::middleware::request_transform::Endpoint;
use private_ai_gateway::middleware::sse::{MeterStream, StreamReport};
use private_ai_gateway::middleware::{CompletionInput, Middleware, MiddlewareConfig};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use common::{event_from_request, StaticKeyProvider, StubQuoter};

// A mock upstream that returns a fixed response for any forward.
struct MockUpstream {
    status: u16,
    body: Vec<u8>,
}

#[async_trait]
impl UpstreamBackend for MockUpstream {
    fn name(&self) -> &str {
        "mock-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://mock-upstream.example")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamResponse {
            status_code: self.status,
            body: self.body.clone(),
            headers,
            served_instance_id: None,
        })
    }
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Ok(UpstreamResponse {
            status_code: 200,
            body: b"{}".to_vec(),
            headers: HashMap::new(),
            served_instance_id: None,
        })
    }
}

// A mock upstream that classifies a route as attested by its `tee-` prefix and
// records every route it was actually asked to forward to. Classification
// happens in `prepare`, as the real config-driven router does it, so a route
// the ACI constraint rejects can be told apart from one never reached.
struct TeeAwareUpstream {
    forwarded: Arc<Mutex<Vec<String>>>,
    status: u16,
}

#[async_trait]
impl UpstreamBackend for TeeAwareUpstream {
    fn name(&self) -> &str {
        "tee-aware-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://tee-aware-upstream.example")
    }
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let route_id = req.target_route_id.clone().unwrap_or_default();
        if route_id.starts_with("missing-") {
            return Err(UpstreamError::Routing(format!("no route {route_id}")));
        }
        Ok(PreparedUpstreamRequest {
            upstream_name: self.name().to_string(),
            url_origin: self.url_origin().map(str::to_string),
            model_id: "gpt-test".to_string(),
            is_tee: Some(route_id.starts_with("tee-")),
            route_id: Some(route_id),
            request: req,
        })
    }
    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forwarded
            .lock()
            .unwrap()
            .push(req.route_id.clone().unwrap_or_default());
        self.forward(req.request).await
    }
    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(req).await
    }
    // Streaming resolves through `forward_stream_prepared`, not
    // `forward_prepared`, so it needs its own recording hook — otherwise a
    // streaming test cannot tell "never forwarded" from "not observed".
    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.forwarded
            .lock()
            .unwrap()
            .push(req.route_id.clone().unwrap_or_default());
        self.forward_stream(req.request).await
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamResponse {
            status_code: self.status,
            body: br#"{"choices":[]}"#.to_vec(),
            headers,
            served_instance_id: None,
        })
    }
}

// Sum every series in one Prometheus counter family.
fn metric_total(service: &AciService, name: &str) -> u64 {
    let body = String::from_utf8(service.metrics().unwrap().body).unwrap();
    body.lines()
        .filter(|line| line.starts_with(name))
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(_, value)| value.trim().parse::<f64>().ok())
        .map(|value| value as u64)
        .sum()
}

fn build_tee_aware_service() -> (Arc<AciService>, Arc<Mutex<Vec<String>>>) {
    build_tee_aware_service_with_status(200)
}

fn build_tee_aware_service_with_status(status: u16) -> (Arc<AciService>, Arc<Mutex<Vec<String>>>) {
    let forwarded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(TeeAwareUpstream {
                forwarded: forwarded.clone(),
                status,
            }),
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    (service, forwarded)
}

struct OkVerifier;

#[async_trait]
impl UpstreamVerifier for OkVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        event_from_request(&request, VerificationResult::Verified)
    }
}

struct FailVerifier;

#[async_trait]
impl UpstreamVerifier for FailVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        event_from_request(&request, VerificationResult::Failed)
    }
}

struct SessionVerifier;

#[async_trait]
impl UpstreamVerifier for SessionVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            verifier_id: "session-verifier/v1".to_string(),
            channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
                origin: "https://tee-aware-upstream.example".to_string(),
                spki_sha256: "ab".repeat(32),
            }],
            ..event_from_request(&request, VerificationResult::Verified)
        }
    }
}

fn build_service_failing_verify() -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(MockUpstream {
                status: 200,
                body: b"{}".to_vec(),
            }),
            Arc::new(FailVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

fn build_service_with_upstream(status: u16, body: Vec<u8>) -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(MockUpstream { status, body }),
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

fn temp_config_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "private-ai-gateway-middleware-completion-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn runtime_options() -> UpstreamRuntimeOptions {
    UpstreamRuntimeOptions {
        verifier_mode: UpstreamVerifierMode::Preverified,
        accepted_workload_ids: vec![],
        accepted_image_digests: vec![],
        accepted_dstack_kms_root_public_keys: vec![],
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 600,
        verifier_request_timeout_seconds: 60,
    }
}

fn build_service() -> Arc<AciService> {
    let path = temp_config_path();
    let manager = Arc::new(UpstreamConfigManager::load(&path, runtime_options()).unwrap());
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            manager.backend(),
            manager.verifier(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

// Stub control plane: POST /consult/pre returns the configured JSON + status.
async fn spawn_control(status: u16, body: Value) -> String {
    let response = Arc::new((status, body));
    let app = Router::new().route(
        "/consult/pre",
        post(move || {
            let response = response.clone();
            async move {
                let code = axum::http::StatusCode::from_u16(response.0).unwrap();
                (code, Json(response.1.clone()))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// Stub control plane that also captures /consult/post reports.
async fn spawn_control_capturing(
    pre_status: u16,
    pre_body: Value,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let posts: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let pre = Arc::new((pre_status, pre_body));
    let posts_route = posts.clone();
    let app = Router::new()
        .route(
            "/consult/pre",
            post(move || {
                let pre = pre.clone();
                async move {
                    let code = axum::http::StatusCode::from_u16(pre.0).unwrap();
                    (code, Json(pre.1.clone()))
                }
            }),
        )
        .route(
            "/consult/post",
            post(move |Json(body): Json<Value>| {
                let posts = posts_route.clone();
                async move {
                    posts.lock().unwrap().push(body);
                    axum::http::StatusCode::OK
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), posts)
}

// Poll the captured reports for one matching `pred` (consult_post is fire-and-forget).
async fn wait_for_post(posts: &Arc<Mutex<Vec<Value>>>, pred: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..40 {
        if let Some(found) = posts.lock().unwrap().iter().find(|r| pred(r)).cloned() {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no matching consult_post report captured");
}

fn middleware(control_url: String) -> Middleware {
    Middleware::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
    })
    .unwrap()
}

fn chat_input() -> CompletionInput {
    CompletionInput {
        endpoint: Endpoint::ChatComplete,
        endpoint_path: "/v1/chat/completions",
        surface: Surface::Openai,
        params: json!({ "model": "gpt-test", "messages": [{ "role": "user", "content": "hi" }] }),
        received_body: br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#
            .to_vec(),
        api_key_hash: Some("deadbeef".to_string()),
        requester: None,
        e2ee: None,
        upstream_required: true,
        aci_required: false,
        aci_session_ids: Vec::new(),
        request_id: "req-1".to_string(),
        user_model: Some("gpt-test".to_string()),
        stream: false,
    }
}

async fn response_parts(response: axum::response::Response) -> (u16, axum::http::HeaderMap, Value) {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}

#[tokio::test]
async fn denial_returns_forbidden_envelope() {
    let control_url = spawn_control(200, json!({ "allow": false })).await;
    let mw = middleware(control_url);
    let service = build_service();

    let (status, _, body) =
        response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 403);
    assert_eq!(body["error"]["type"], json!("permission_error"));
    assert_eq!(body["error"]["message"], json!("forbidden"));
}

#[tokio::test]
async fn control_unavailable_fails_closed() {
    // Unreachable control plane -> consult_pre fails closed with a 503 denial.
    let mw = middleware("http://127.0.0.1:1".to_string());
    let service = build_service();

    let (status, _, body) =
        response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 503);
    assert_eq!(body["error"]["type"], json!("service_unavailable"));
    assert_eq!(body["error"]["message"], json!("control plane unavailable"));
}

#[tokio::test]
async fn rate_limit_denial_sets_headers_and_code() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": false,
            "status": 429,
            "message": "slow down",
            "rateLimit": { "limit": 5, "resetAt": 4_000_000_000_i64 }
        }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service();

    let (status, headers, body) =
        response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 429);
    assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "5");
    assert_eq!(headers.get("x-ratelimit-remaining").unwrap(), "0");
    assert!(headers.get("retry-after").is_some());
    assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));
}

#[tokio::test]
async fn allow_forwards_and_finalizes_receipt() {
    // consult allows with one candidate; the request is shaped, forwarded to the
    // mock upstream, and the buffered receipt is finalized.
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "openai:gpt-test", "format": "openai" }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let upstream_body = br#"{"id":"chat-1","object":"chat.completion","choices":[]}"#.to_vec();
    let service = build_service_with_upstream(200, upstream_body);

    let mut input = chat_input();
    input.upstream_required = false;
    let (status, headers, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 200);
    assert!(
        headers.get("x-receipt-id").is_some(),
        "buffered success must carry a receipt id"
    );
    assert_eq!(body["id"], json!("chat-1"));
}

#[tokio::test]
async fn buffered_success_transforms_injects_cost_and_meters() {
    // Anthropic upstream over /v1/chat/completions: response is transformed to the
    // OpenAI shape, cost is injected into the client body, and the metering report
    // carries raw (pre-cost) usage.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "anthropic:claude", "format": "anthropic" }],
            "pricing": { "inputCostPerToken": "0.000001", "outputCostPerToken": "0.000002" },
            "userId": 7
        }),
    )
    .await;
    let mw = middleware(control_url);
    let anthropic_body = json!({
        "id": "msg_1", "model": "claude-3", "stop_reason": "end_turn",
        "content": [{ "type": "text", "text": "hi" }],
        "usage": { "input_tokens": 100, "output_tokens": 20 }
    });
    let service = build_service_with_upstream(200, serde_json::to_vec(&anthropic_body).unwrap());

    let mut input = chat_input();
    input.upstream_required = false;
    let (status, _headers, body) =
        response_parts(mw.handle_completion(&service, input).await).await;

    assert_eq!(status, 200);
    // Transformed to the OpenAI chat surface.
    assert_eq!(body["object"], json!("chat.completion"));
    assert_eq!(body["usage"]["prompt_tokens"], json!(100));
    // cost = 100*1e-6 + 20*2e-6 = 0.00014, injected into the client body.
    assert!((body["usage"]["cost"].as_f64().unwrap() - 0.00014).abs() < 1e-12);

    // The metering report carries raw usage (no cost) and the selected route.
    let report = wait_for_post(&posts, |r| {
        r.get("errorSource").map(Value::is_null).unwrap_or(true)
            && r["status"].as_i64() == Some(200)
    })
    .await;
    assert_eq!(report["selectedRouteId"], json!("anthropic:claude"));
    assert_eq!(report["usage"]["prompt_tokens"], json!(100));
    assert!(
        report["usage"].get("cost").is_none(),
        "report usage must be pre-cost-injection"
    );
    assert_eq!(report["userId"], json!(7));
    assert_eq!(report["isStreaming"], json!(false));
}

#[tokio::test]
async fn meter_stream_injects_cost_classifies_completed_and_reports() {
    let (control_url, posts) = spawn_control_capturing(200, json!({})).await;
    let control = ControlClient::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
    })
    .unwrap();
    let report = StreamReport {
        control,
        request_id: "r1".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        request_model: "gpt".to_string(),
        pricing: Some(json!({ "inputCostPerToken": "0.000001", "outputCostPerToken": "0.000002" })),
        spend_mode: None,
        user_id: Some(9),
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        started: std::time::Instant::now(),
        downstream_abort: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        settled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let events: Vec<Result<Bytes, _>> = vec![
        Ok(Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        )),
        Ok(Bytes::from(
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20}}\n\n",
        )),
        Ok(Bytes::from("data: [DONE]\n\n")),
    ];
    let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
    let metered = MeterStream::new(inner, report, SseProtocol::OpenaiChat);
    let collected: Vec<Bytes> = metered.map(|r| r.unwrap()).collect().await;
    let text: String = collected
        .iter()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect();

    // Cost injected into the usage chunk; [DONE] preserved.
    assert!(text.contains("\"cost\""), "cost not injected: {text}");
    assert!(text.contains("[DONE]"));

    let report = wait_for_post(&posts, |r| {
        r["isStreaming"] == json!(true) && r["status"].as_i64() == Some(200)
    })
    .await;
    assert_eq!(report["selectedRouteId"], json!("openai:gpt"));
    assert_eq!(report["usage"]["prompt_tokens"], json!(10));
    assert!(
        report["usage"].get("cost").is_none(),
        "report usage must be pre-cost"
    );
    assert!(report["ttftMs"].is_number(), "ttft must be recorded");
    assert_eq!(report["userId"], json!(9));
}

#[tokio::test]
async fn malformed_2xx_body_returns_502_upstream() {
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "anthropic:claude", "format": "anthropic" }] }),
    )
    .await;
    let mw = middleware(control_url);
    // Upstream returns HTTP 200 with a non-JSON body.
    let service = build_service_with_upstream(200, b"<html>not json</html>".to_vec());
    let mut input = chat_input();
    input.upstream_required = false;

    let (status, _, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(
        status, 502,
        "malformed 2xx must not be a fabricated success"
    );
    assert_eq!(body["error"]["type"], json!("upstream_error"));

    let report = wait_for_post(&posts, |r| r["errorSource"] == json!("upstream")).await;
    assert_eq!(report["status"].as_i64(), Some(502));
}

#[tokio::test]
async fn total_forward_failure_reports_upstream_failure() {
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let mw = middleware(control_url);
    // Upstream verification fails for every candidate, so the forward returns Err.
    let service = build_service_failing_verify();
    let mut input = chat_input();
    input.upstream_required = true;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 503);

    let report = wait_for_post(&posts, |r| r["errorSource"] == json!("upstream")).await;
    assert_eq!(report["status"].as_i64(), Some(503));
    assert_eq!(report["selectedRouteId"], Value::Null);
}

#[tokio::test]
async fn streaming_upstream_non_2xx_reports_the_serving_route() {
    // A streaming request whose upstream answers non-2xx issues no receipt, but it
    // did reach an upstream: the report must name the route that produced the
    // status, or the failure cannot count against that route's health and the
    // load behind the 429s is never shed.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_upstream(429, br#"{"error":"rate limited"}"#.to_vec());
    let mut input = chat_input();
    input.upstream_required = false;
    input.stream = true;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 429, "the upstream status must reach the client");

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(429)).await;
    assert_eq!(
        report["selectedRouteId"],
        json!("openai:gpt"),
        "an unattributed upstream 429 counts against no route's health"
    );
    assert_eq!(report["isStreaming"], json!(true));
    assert_eq!(report["attemptIndex"], json!(0));
    // A real upstream attempt, not a gateway-generated failure: error_source
    // stays empty so the status is attributed to the route itself.
    assert!(
        report
            .get("errorSource")
            .map(Value::is_null)
            .unwrap_or(true),
        "a real upstream attempt must not be tagged as a gateway failure"
    );
}

#[tokio::test]
async fn repeated_route_id_still_reports_distinct_attempt_indices() {
    // Failover exhausted: every candidate 429s, and each attempt must reach
    // control as its own attributed report — the failed-over one via
    // failed_attempts, the last via upstream_error. The gateway does not dedupe
    // `candidates` — it forwards whatever control supplies, and control
    // implementations are swappable (the open-source stack ships its own). A
    // route id repeated in the list must still yield one report per attempt:
    // control dedupes by (request_id, attempt, status), so two 429s sharing an
    // attempt index would silently collapse into one, under-counting the
    // pressure signal by half. The index is therefore derived from the number
    // of prior attempts, never looked up by route id.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [
                { "routeId": "openai:dup", "format": "openai" },
                { "routeId": "openai:dup", "format": "openai" }
            ]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_upstream(429, br#"{"error":"rate limited"}"#.to_vec());
    let mut input = chat_input();
    input.upstream_required = false;
    input.stream = true;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 429);

    wait_for_post(&posts, |r| r["attemptIndex"].as_i64() == Some(1)).await;
    let indices: Vec<i64> = posts
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["status"].as_i64() == Some(429))
        .filter_map(|r| r["attemptIndex"].as_i64())
        .collect();
    assert_eq!(
        indices,
        vec![0, 1],
        "both 429s against the repeated route must carry distinct attempt indices"
    );
}

#[tokio::test]
async fn image_fetch_5xx_becomes_400_and_is_not_failed_over() {
    // The upstream can't fetch the client's image URL and (wrongly) reports it as a
    // 500. That is a bad-input error: the client must get a 400, it must not fail
    // over across candidates (it would fail identically), and the provider must not
    // be charged for it (the report carries 400, which control excludes from health).
    let url = "https://halleonard.example/wl/02116757-wl.jpg";
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [
                { "routeId": "openai:a", "format": "openai" },
                { "routeId": "openai:b", "format": "openai" }
            ]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let upstream_body = format!(
        r#"{{"error":{{"message":"403, message='Forbidden', url='{url}'","type":"InternalServerError","param":null,"code":500}}}}"#
    );
    let service = build_service_with_upstream(500, upstream_body.into_bytes());

    let mut input = chat_input();
    input.upstream_required = false;
    input.params = json!({
        "model": "gpt-test",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "describe" },
                { "type": "image_url", "image_url": { "url": url } }
            ]
        }]
    });
    input.received_body = serde_json::to_vec(&input.params).unwrap();

    let (status, _, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 400, "a bad client image URL is a 400, not a 5xx");
    assert_eq!(body["error"]["type"], json!("invalid_request_error"));
    assert!(body["error"]["message"].as_str().unwrap().contains(url));

    // The committed attempt is reported as 400 (client-attributable, not provider).
    let report = wait_for_post(&posts, |r| {
        r["status"].as_i64() == Some(400)
            && r.get("errorSource").map(Value::is_null).unwrap_or(true)
    })
    .await;
    assert_eq!(report["status"].as_i64(), Some(400));
    // And the request was never failed over: no attempt is reported with the raw 500.
    let failed_over = posts
        .lock()
        .unwrap()
        .iter()
        .any(|r| r["status"].as_i64() == Some(500));
    assert!(
        !failed_over,
        "an image-input error must not trigger failover attempts"
    );
}

#[tokio::test]
async fn aci_constraint_skips_non_tee_routes_without_blaming_them() {
    // `provider.aci_verified` must not be satisfiable by a plaintext route,
    // even when one is offered ahead of an attested route. Nor may the skipped
    // route be reported as a failed attempt: being ineligible is a policy
    // decision, and counting it would penalize a provider that never got the
    // chance to fail.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [
                { "routeId": "plain:gpt-test", "format": "openai" },
                { "routeId": "tee-a:gpt-test", "format": "openai" }
            ]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let (service, forwarded) = build_tee_aware_service();
    let mut input = chat_input();
    input.aci_required = true;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 200, "the attested candidate must still serve");
    assert_eq!(
        *forwarded.lock().unwrap(),
        vec!["tee-a:gpt-test".to_string()],
        "the non-TEE candidate must never receive the prompt"
    );

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(200)).await;
    assert_eq!(
        report["attemptIndex"].as_i64(),
        Some(0),
        "the skipped route must not count as a failed attempt"
    );
    assert!(
        !posts
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["selectedRouteId"] == json!("plain:gpt-test")),
        "no attempt may be attributed to the rejected non-TEE route"
    );
}

#[tokio::test]
async fn aci_session_ids_are_a_preforward_hard_allowlist() {
    let forwarded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let service = AciService::new_with_upstream_verifier(
        Arc::new(StaticKeyProvider::default()),
        Arc::new(StubQuoter::default()),
        Arc::new(TeeAwareUpstream {
            forwarded: forwarded.clone(),
            status: 200,
        }),
        Arc::new(SessionVerifier),
        Arc::new(InMemoryReceiptStore::default()),
        AciServiceConfig::for_test("private-ai-gateway"),
        Arc::new(FixedClock(1_700_000_000)),
    )
    .unwrap();

    let request = |session_ids| ChatCompletionRequest {
        context: GatewayRequestContext {
            user_model: Some("gpt-test".to_string()),
            ..GatewayRequestContext::default()
        },
        endpoint_path: "/v1/chat/completions",
        received_body: br#"{"model":"gpt-test","messages":[]}"#,
        forwarded_body: None,
        upstream_required: Some(false),
        aci_required: true,
        aci_session_ids: session_ids,
        upstream_verification_event: None,
        requester: None,
        e2ee: None,
    };
    let candidate = || ForwardCandidate {
        route_id: "tee-a:gpt-test".to_string(),
        body: br#"{"model":"gpt-test","messages":[]}"#.to_vec(),
    };

    // Discover the stable id derived from the current verified binding.
    let first = service
        .forward_chat_completion_for_middleware(
            request(Vec::new()),
            vec![candidate()],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    let session_id = match first {
        MiddlewareForwardResult::Forwarded(forward) => forward
            .session_id
            .expect("verified binding must seal a session"),
        _ => panic!("expected a buffered forward"),
    };

    forwarded.lock().unwrap().clear();
    let allowed = service
        .forward_chat_completion_for_middleware(
            request(vec!["as_unavailable".to_string(), session_id.clone()]),
            vec![candidate()],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match allowed {
        MiddlewareForwardResult::Forwarded(forward) => {
            assert_eq!(forward.session_id.as_deref(), Some(session_id.as_str()));
        }
        _ => panic!("expected an allowlisted buffered forward"),
    }
    assert_eq!(forwarded.lock().unwrap().len(), 1);

    forwarded.lock().unwrap().clear();
    let result = service
        .forward_chat_completion_for_middleware(
            request(vec!["as_unavailable".to_string()]),
            vec![candidate()],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await;
    match result {
        Ok(MiddlewareForwardResult::AllFailed(failure)) => assert!(matches!(
            failure.error,
            ServiceError::UpstreamVerification(
                UpstreamVerificationError::NoEligibleAttestedSession(_)
            )
        )),
        _ => panic!("an unavailable session must fail closed"),
    }
    assert!(
        forwarded.lock().unwrap().is_empty(),
        "the prompt must not reach a route before its current session matches"
    );
}

#[tokio::test]
async fn a_real_failure_outranks_a_tee_ineligible_route_in_either_order() {
    // Being ineligible is the least informative outcome — the route never got
    // the chance to fail — so a genuine failure must win whichever order the
    // candidates arrived in. Sharing a priority band with routing errors would
    // make the client-facing status depend on that order.
    for candidates in [
        json!([
            { "routeId": "missing-a:gpt-test", "format": "openai" },
            { "routeId": "plain:gpt-test", "format": "openai" }
        ]),
        json!([
            { "routeId": "plain:gpt-test", "format": "openai" },
            { "routeId": "missing-a:gpt-test", "format": "openai" }
        ]),
    ] {
        let control_url =
            spawn_control(200, json!({ "allow": true, "candidates": candidates })).await;
        let mw = middleware(control_url);
        let (service, _) = build_tee_aware_service();
        let mut input = chat_input();
        input.aci_required = true;

        let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
        assert_eq!(
            status, 404,
            "the routing failure must outrank the ineligible route"
        );
    }
}

#[tokio::test]
async fn a_later_candidate_that_never_answers_does_not_swallow_a_real_status() {
    // Plain failover, no ACI constraint: the first candidate answers 429, the
    // walk moves on, and the second cannot even be routed. A route that never
    // reached an upstream must not overwrite one that did — the client's status
    // is the real 429, not a 404 synthesized from the second candidate's own
    // failure. Both streaming and buffered, which commit through separate
    // paths.
    for stream in [false, true] {
        let (control_url, posts) = spawn_control_capturing(
            200,
            json!({
                "allow": true,
                "candidates": [
                    { "routeId": "plain:gpt-test", "format": "openai" },
                    { "routeId": "missing-b:gpt-test", "format": "openai" }
                ]
            }),
        )
        .await;
        let mw = middleware(control_url);
        let (service, forwarded) = build_tee_aware_service_with_status(429);
        let mut input = chat_input();
        input.stream = stream;

        let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
        assert_eq!(
            status, 429,
            "stream={stream}: the real upstream status wins"
        );
        assert_eq!(
            *forwarded.lock().unwrap(),
            vec!["plain:gpt-test".to_string()],
            "stream={stream}: only the routable candidate was contacted"
        );

        // The committed 429 must be reported last: dashboards read a request's
        // user-facing status as the one at the highest attempt index, so a
        // committed response sitting behind a later attempt would be misread.
        let report = wait_for_post(&posts, |r| {
            r["status"].as_i64() == Some(429) && r["selectedRouteId"] == json!("plain:gpt-test")
        })
        .await;
        let committed = report["attemptIndex"].as_i64().unwrap_or(-1);
        let highest = posts
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| r["attemptIndex"].as_i64())
            .max()
            .unwrap_or(-1);
        assert_eq!(
            committed, highest,
            "stream={stream}: the committed attempt must carry the highest index"
        );

        // Holding a response back must not change what it contributes to the
        // metrics: exactly one upstream response was observed, and a streaming
        // non-2xx is still one stream error however late it is committed.
        assert_eq!(
            metric_total(&service, "private_ai_gateway_upstream_responses_total"),
            1,
            "stream={stream}: the retained response is counted once, not twice"
        );
        if stream {
            assert_eq!(
                metric_total(&service, "private_ai_gateway_stream_errors_total"),
                1,
                "a retained streaming non-2xx is still a stream error"
            );
        }
    }
}

#[tokio::test]
async fn an_ineligible_trailing_route_does_not_swallow_a_real_upstream_status() {
    // A candidate the ACI constraint will skip is not a fallback, so it must not
    // make the attested candidate ahead of it look like a non-final attempt.
    // Otherwise the attested route's real 429 is discarded in the hope of a
    // retry that never happens, and the client gets a synthesized 503 instead —
    // with the status flipping on candidate order alone.
    for candidates in [
        json!([
            { "routeId": "tee-a:gpt-test", "format": "openai" },
            { "routeId": "plain:gpt-test", "format": "openai" }
        ]),
        json!([
            { "routeId": "plain:gpt-test", "format": "openai" },
            { "routeId": "tee-a:gpt-test", "format": "openai" }
        ]),
    ] {
        let control_url =
            spawn_control(200, json!({ "allow": true, "candidates": candidates })).await;
        let mw = middleware(control_url);
        let (service, _) = build_tee_aware_service_with_status(429);
        let mut input = chat_input();
        input.aci_required = true;

        let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
        assert_eq!(
            status, 429,
            "the attested route's real status must reach the client in either order"
        );
    }
}

#[tokio::test]
async fn aci_constraint_cannot_be_waived_by_the_verification_opt_out() {
    // A route classed as TEE is not the same as a route whose attestation held.
    // `provider.aci_verified` pins the request to an attested upstream, so the caller's
    // `X-Upstream-Verification: none` must not also waive the attestation —
    // otherwise the constraint degrades into a static provider-name check and a
    // TEE route with a failed or missing attestation still receives the prompt.
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "tee-a:gpt-test", "format": "openai" }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let forwarded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(TeeAwareUpstream {
                forwarded: forwarded.clone(),
                status: 200,
            }),
            Arc::new(FailVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let mut input = chat_input();
    input.aci_required = true;
    input.upstream_required = false;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 503, "a failed attestation must still fail closed");
    assert!(
        forwarded.lock().unwrap().is_empty(),
        "an unattested TEE route must not receive the prompt"
    );
}

#[tokio::test]
async fn aci_constraint_with_no_attested_route_is_a_diagnosable_503() {
    // Every candidate is plaintext, so the request cannot be served at all. The
    // failure names the constraint and the model rather than surfacing as a bare
    // "upstream verification failed". It is reported as a gateway failure, not an
    // upstream one: no provider was contacted, so attributing it to one would
    // make our own policy look like someone else's outage.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "plain:gpt-test", "format": "openai" }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let (service, forwarded) = build_tee_aware_service();
    let mut input = chat_input();
    input.aci_required = true;

    let (status, _, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 503);
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("no attested upstream available for model gpt-test"),
        "unexpected message: {message}"
    );
    assert!(
        forwarded.lock().unwrap().is_empty(),
        "nothing may be forwarded when no candidate is attested"
    );

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(503)).await;
    assert_eq!(
        report["errorSource"], "gateway",
        "an ineligible-route failure must not be attributed to a provider"
    );
    assert!(
        report["selectedRouteId"].is_null(),
        "no route was ever committed"
    );
}

#[tokio::test]
async fn empty_candidates_returns_model_not_found() {
    let control_url = spawn_control(200, json!({ "allow": true, "candidates": [] })).await;
    let mw = middleware(control_url);
    let service = build_service();

    let (status, _, body) =
        response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["type"], json!("model_not_found"));
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no route available"));
}

// Behavior contract for finalizer failures relative to meter settle timing.
//
// Pre-settle: a downstream finalizer error during body consumption (the
// response wrapper sets `downstream_abort` before the chain drops) must
// settle as an internal gateway failure — 502 with error_source=gateway —
// not as a client disconnect (499), and must not charge the serving route.
#[tokio::test]
async fn downstream_abort_before_settle_reports_gateway_failure_not_client_close() {
    let (control_url, posts) = spawn_control_capturing(200, json!({})).await;
    let control = ControlClient::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
    })
    .unwrap();
    let downstream_abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let report = StreamReport {
        control,
        request_id: "r-abort".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        request_model: "gpt".to_string(),
        pricing: None,
        spend_mode: None,
        user_id: None,
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        started: std::time::Instant::now(),
        downstream_abort: downstream_abort.clone(),
        settled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    // One chunk, then the stream stays open: the meter starts but never
    // reaches a terminal marker.
    let events: Vec<Result<Bytes, private_ai_gateway::aggregator::service::ServiceError>> =
        vec![Ok(Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        ))];
    let inner: ServiceResponseStream =
        Box::pin(futures_util::stream::iter(events).chain(futures_util::stream::pending()));
    let mut metered = MeterStream::new(inner, report, SseProtocol::OpenaiChat);
    let first = metered.next().await;
    assert!(first.is_some(), "meter must have started streaming");

    // The downstream finalizer errors; the wrapper marks it, then the chain
    // is dropped.
    downstream_abort.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(metered);

    let report = wait_for_post(&posts, |r| r["requestId"] == json!("r-abort")).await;
    assert_eq!(report["status"], json!(502), "internal failure, not 499");
    assert_eq!(report["errorSource"], json!("gateway"));
    assert_eq!(
        report["selectedRouteId"],
        json!("openai:gpt"),
        "route still recorded for traceability"
    );
}

// Post-settle: once the meter settled Completed at a clean end-of-stream, a
// later finalizer error (flag set just before the drop) must not emit a
// second, conflicting usage report — the supplemental request_outcome line is
// the response wrapper's job, not the meter's.
#[tokio::test]
async fn downstream_abort_after_settle_does_not_double_report() {
    let (control_url, posts) = spawn_control_capturing(200, json!({})).await;
    let control = ControlClient::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
    })
    .unwrap();
    let downstream_abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let settled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let report = StreamReport {
        control,
        request_id: "r-late".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        request_model: "gpt".to_string(),
        pricing: None,
        spend_mode: None,
        user_id: None,
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        started: std::time::Instant::now(),
        downstream_abort: downstream_abort.clone(),
        settled: settled.clone(),
    };
    let events: Vec<Result<Bytes, private_ai_gateway::aggregator::service::ServiceError>> =
        vec![Ok(Bytes::from(
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ))];
    let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
    let mut metered = MeterStream::new(inner, report, SseProtocol::OpenaiChat);
    while metered.next().await.is_some() {}
    assert!(
        settled.load(std::sync::atomic::Ordering::Relaxed),
        "clean EOF settles the meter"
    );

    // Receipt/E2EE finalization now fails; the wrapper marks the abort and
    // the chain is dropped afterwards.
    downstream_abort.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(metered);

    let first = wait_for_post(&posts, |r| r["requestId"] == json!("r-late")).await;
    assert_eq!(first["status"], json!(200), "the settled outcome stands");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let count = posts
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["requestId"] == json!("r-late"))
        .count();
    assert_eq!(count, 1, "no second, conflicting report after settle");
}
