//! In-process completion orchestration tests: pre-consult reasoning validation,
//! consult-driven denials, fail-closed control errors, rate limits, empty
//! candidates, and successful forwarding through receipt finalization.

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
    AciService, AciServiceConfig, ChatCompletionRequest, FailedAttempt, FixedClock,
    ForwardCandidate, GatewayRequestContext, InMemoryReceiptStore, MiddlewareForwardResult,
    MiddlewareReceiptJournal, ServiceError, ServiceResponseStream, UpstreamVerificationError,
    UpstreamVerificationRequest, UpstreamVerifier,
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
    /// Headers an upstream may add on top of `content-type` — the kind the
    /// allowlist must drop so none of them can reach a client.
    extra_headers: Vec<(&'static str, &'static str)>,
}

#[derive(Debug, Clone)]
struct RecordedRequest {
    path: Option<String>,
    body: Value,
}

struct RecordingUpstream {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
}

#[async_trait]
impl UpstreamBackend for RecordingUpstream {
    fn name(&self) -> &str {
        "recording-upstream"
    }

    fn url_origin(&self) -> Option<&str> {
        Some("https://recording-upstream.example")
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.requests.lock().unwrap().push(RecordedRequest {
            path: req.path,
            body: serde_json::from_slice(&req.body).unwrap(),
        });
        Ok(UpstreamResponse {
            status_code: self.status,
            body: self.body.clone(),
            headers: HashMap::from([("content-type".to_string(), self.content_type.to_string())]),
            served_instance_id: None,
        })
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(req).await
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.forward_stream_prepared(req).await
    }
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
        for (name, value) in &self.extra_headers {
            headers.insert(name.to_string(), value.to_string());
        }
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
    // Stands in for a backend that enforces the verifier's channel binding on
    // its connection; the trait default fails closed.
    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(req).await
    }
    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.forward_stream_prepared(req).await
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
    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.forward_stream_prepared(req).await
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
            AciServiceConfig::for_test(),
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
    let forwarded = Arc::new(Mutex::new(Vec::new()));
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(TeeAwareUpstream {
                forwarded,
                status: 200,
            }),
            Arc::new(FailVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

fn build_service_with_upstream(status: u16, body: Vec<u8>) -> Arc<AciService> {
    build_service_with_upstream_headers(status, body, Vec::new())
}

fn build_service_with_upstream_headers(
    status: u16,
    body: Vec<u8>,
    extra_headers: Vec<(&'static str, &'static str)>,
) -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(MockUpstream {
                status,
                body,
                extra_headers,
            }),
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

fn build_recording_service(
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
) -> (Arc<AciService>, Arc<Mutex<Vec<RecordedRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(RecordingUpstream {
                requests: requests.clone(),
                status,
                body,
                content_type,
            }),
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    (service, requests)
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
        accepted_subjects: vec![],
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
            AciServiceConfig::for_test(),
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

// Stub control plane that captures /consult/pre bodies AND /consult/post
// reports — for asserting what the gateway sends, not only what it does.
async fn spawn_control_capturing_pre(
    pre_body: Value,
) -> (String, Arc<Mutex<Vec<Value>>>, Arc<Mutex<Vec<Value>>>) {
    let pres: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let posts: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let pre = Arc::new(pre_body);
    let pres_route = pres.clone();
    let posts_route = posts.clone();
    let app = Router::new()
        .route(
            "/consult/pre",
            post(move |Json(body): Json<Value>| {
                let pre = pre.clone();
                let pres = pres_route.clone();
                async move {
                    pres.lock().unwrap().push(body);
                    (axum::http::StatusCode::OK, Json((*pre).clone()))
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
    (format!("http://{addr}"), pres, posts)
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
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
    })
    .unwrap()
}

/// The (route, status) view of a failover chain; per-attempt timing is not
/// deterministic and is asserted separately where it matters.
fn attempts(chain: &[FailedAttempt]) -> Vec<(String, u16)> {
    chain
        .iter()
        .map(|a| (a.route_id.clone(), a.status))
        .collect()
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
        aci_required: false,
        aci_session_ids: Vec::new(),
        request_id: "req-1".to_string(),
        user_model: Some("gpt-test".to_string()),
        stream: false,
        tee_only: false,
    }
}

fn responses_input(stream: bool) -> CompletionInput {
    let params = json!({
        "model": "gpt-test",
        "input": "hello",
        "stream": stream,
        "max_output_tokens": 16
    });
    CompletionInput {
        endpoint: Endpoint::CreateModelResponse,
        endpoint_path: "/v1/responses",
        surface: Surface::Openai,
        received_body: serde_json::to_vec(&params).unwrap(),
        params,
        api_key_hash: Some("deadbeef".to_string()),
        requester: None,
        e2ee: None,
        aci_required: false,
        aci_session_ids: Vec::new(),
        request_id: "req-responses".to_string(),
        user_model: Some("gpt-test".to_string()),
        stream,
        tee_only: false,
    }
}

async fn response_parts(response: axum::response::Response) -> (u16, axum::http::HeaderMap, Value) {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}

/// The shape of headers an upstream sends that identify it: one names the
/// serving provider outright, one names its edge, one is a set-cookie that would
/// otherwise land on our own domain. Synthetic values — the test proves the
/// allowlist drops arbitrary upstream headers, not any specific provider.
const LEAKY_UPSTREAM_HEADERS: &[(&str, &str)] = &[
    ("x-acme-serving", "acme"),
    ("server", "acme-edge/1.0"),
    ("inference-id", "29abc41a-c5c0-56d7-818a-c56c8c0fb272"),
    ("x-request-id", "fa57b5a8-4967-4e5c-9ab8-103a9feeeb14"),
    ("alt-svc", r#"h3=":8443"; ma=86400"#),
    ("x-vendor-call-gateway", "true"),
    ("set-cookie", "edge_sess=0893731c1786104409;path=/"),
];

fn assert_no_upstream_headers(headers: &axum::http::HeaderMap) {
    for (name, _) in LEAKY_UPSTREAM_HEADERS {
        assert!(
            headers.get(*name).is_none(),
            "{name} reached the client; response headers must be an allowlist"
        );
    }
}

async fn raw_body(response: axum::response::Response) -> (axum::http::HeaderMap, String) {
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn sse_events(body: &str) -> Vec<Value> {
    body.split("\n\n")
        .filter_map(|block| block.lines().find_map(|line| line.strip_prefix("data: ")))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

#[tokio::test]
async fn buffered_success_hides_which_upstream_served_it() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "acme:model-a", "format": "openai" }],
            "pricing": { "inputCostPerToken": "0", "outputCostPerToken": "0" }
        }),
    )
    .await;
    let mw = middleware(control_url);
    // An engine-served body: `matched_stop` is the engine's own field, the id
    // is its own format, and `model` is the upstream's internal name.
    let service = build_service_with_upstream_headers(
        200,
        br#"{"id":"7bdaaade50304502b0fe7e66e9a4bec2","object":"chat.completion","model":"vendor-model-int","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop","matched_stop":424242}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.to_vec(),
        LEAKY_UPSTREAM_HEADERS.to_vec(),
    );

    let (status, headers, body) =
        response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 200);
    assert_no_upstream_headers(&headers);
    assert_eq!(body["id"], json!("req-1"));
    assert_eq!(body["model"], json!("gpt-test"));
    assert!(body["choices"][0].get("matched_stop").is_none());
    // The parts that are the client's answer are untouched.
    assert_eq!(body["choices"][0]["message"]["content"], json!("hi"));
    assert_eq!(body["choices"][0]["finish_reason"], json!("stop"));
    assert_eq!(body["usage"]["completion_tokens"], json!(1));
}

#[tokio::test]
async fn streamed_success_hides_which_upstream_served_it() {
    // Same-format streaming is the path that relayed provider bytes verbatim,
    // so it gets its own end-to-end check rather than only a unit test.
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "acme:model-a", "format": "openai" }],
            "pricing": { "inputCostPerToken": "0", "outputCostPerToken": "0" }
        }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_upstream_headers(
        200,
        b"data: {\"id\":\"b4fa5a1dc59c4b41\",\"model\":\"vendor-model-int\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"matched_stop\":424242}]}\n\ndata: [DONE]\n\n".to_vec(),
        LEAKY_UPSTREAM_HEADERS.to_vec(),
    );
    let mut input = chat_input();
    input.stream = true;

    let (headers, body) = raw_body(mw.handle_completion(&service, input).await).await;
    assert_no_upstream_headers(&headers);
    assert!(!body.contains("matched_stop"), "{body}");
    assert!(!body.contains("vendor-model-int"), "{body}");
    assert!(!body.contains("b4fa5a1dc59c4b41"), "{body}");
    assert!(body.contains("req-1"), "{body}");
    assert!(body.contains("gpt-test"), "{body}");
    // Framing and content survive intact.
    assert!(body.contains(r#""content":"hi""#), "{body}");
    assert!(body.contains("data: [DONE]"), "{body}");
}

#[tokio::test]
async fn responses_stream_converts_chat_protocol_and_keeps_receipt_and_cost() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "compatible:gpt-test", "format": "openai" }],
            "pricing": { "inputCostPerToken": "1", "outputCostPerToken": "2" }
        }),
    )
    .await;
    let upstream = concat!(
        "data: {\"id\":\"chatcmpl-upstream\",\"created\":1700000000,\"model\":\"internal-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    );
    let (service, requests) =
        build_recording_service(200, upstream.as_bytes().to_vec(), "text/event-stream");
    let response = middleware(control_url)
        .handle_completion(&service, responses_input(true))
        .await;
    assert_eq!(response.status(), 200);
    let (headers, body) = raw_body(response).await;
    assert!(headers.get("x-receipt-id").is_some());
    assert!(!body.contains("[DONE]"), "{body}");

    let events = sse_events(&body);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    for (sequence, event) in events.iter().enumerate() {
        assert_eq!(event["sequence_number"], json!(sequence));
    }
    let terminal = events.last().unwrap();
    assert_eq!(terminal["response"]["status"], json!("completed"));
    assert_eq!(terminal["response"]["usage"]["input_tokens"], json!(1));
    assert_eq!(terminal["response"]["usage"]["output_tokens"], json!(2));
    assert_eq!(terminal["response"]["usage"]["cost"], json!(5));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path.as_deref(), Some("/v1/chat/completions"));
    assert!(requests[0].body.get("messages").is_some());
    assert!(requests[0].body.get("input").is_none());
}

#[tokio::test]
async fn native_responses_candidate_keeps_protocol_and_path() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "openai:gpt-test",
                "format": "openai",
                "nativeResponses": true
            }]
        }),
    )
    .await;
    let upstream = br#"{
        "id":"resp-upstream",
        "object":"response",
        "created_at":1700000000,
        "model":"internal-model",
        "status":"completed",
        "incomplete_details":null,
        "error":null,
        "output":[],
        "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
    }"#;
    let (service, requests) = build_recording_service(200, upstream.to_vec(), "application/json");
    let (status, _, body) = response_parts(
        middleware(control_url)
            .handle_completion(&service, responses_input(false))
            .await,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], json!("req-responses"));
    assert_eq!(body["object"], json!("response"));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path.as_deref(), Some("/v1/responses"));
    assert_eq!(requests[0].body["input"], json!("hello"));
    assert!(requests[0].body.get("messages").is_none());
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
async fn identity_bearing_denial_is_reported_as_a_control_failure() {
    // A 4xx denial the control plane attributed to a key must reach the
    // usage pipeline: reported with errorSource "control", the identity, and
    // no route — not a 5xx, not a 429, yet still accounted for.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": false,
            "status": 400,
            "message": "This model does not support image input.",
            "userId": 7,
            "virtualKeyId": 3
        }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service();

    let (status, _, _) = response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 400);

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(400)).await;
    assert_eq!(report["errorSource"], json!("control"));
    assert_eq!(report["userId"], json!(7));
    assert_eq!(report["virtualKeyId"], json!(3));
    assert!(report["selectedRouteId"].is_null());
    assert!(report["usage"].is_null());
}

#[tokio::test]
async fn empty_candidates_is_reported_as_a_control_failure() {
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [], "userId": 7, "virtualKeyId": 3 }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service();

    let (status, _, _) = response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 404);

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(404)).await;
    assert_eq!(report["errorSource"], json!("control"));
    assert_eq!(report["userId"], json!(7));
    assert!(report["selectedRouteId"].is_null());
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
async fn reasoning_conflict_precedes_control() {
    let mw = middleware("http://127.0.0.1:1".to_string());
    let service = build_service();
    let mut input = chat_input();
    input.params["reasoning"] = json!({"effort":"high"});
    input.params["reasoning_effort"] = json!("low");
    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 400);
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

    let input = chat_input();
    let (status, headers, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 200);
    assert!(
        headers.get("x-receipt-id").is_some(),
        "buffered success must carry a receipt id"
    );
    // Our request id, not the upstream's `chat-1`: the shape of a provider's id
    // identifies its backend (`chatcmpl-<uuid>`, bare 32-hex, timestamp-prefixed
    // each belong to a different one), so it is replaced rather than relayed.
    assert_eq!(body["id"], json!("req-1"));
}

#[tokio::test]
async fn control_override_reconciles_reasoning_before_forwarding() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "phala:z-ai/glm-5.2",
                "format": "openai",
                "engine": "sglang",
                "reasoningFormat": "reasoning_effort",
                "reasoningPolicy": {
                    "override": { "effort": "none" }
                }
            }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let (service, _, forwarded) = build_sequenced_service(vec![200]);
    let mut input = chat_input();
    input.params = json!({
        "model": "phala/glm-5.2",
        "messages": [{ "role": "user", "content": "Return JSON" }],
        "reasoning_effort": "high",
        "response_format": { "type": "json_object" },
        "max_tokens": 256,
        "chat_template_kwargs": {
            "thinking": true,
            "enable_thinking": true,
            "tokenize": false
        }
    });

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 200);
    let body: Value = serde_json::from_slice(&forwarded.lock().unwrap()[0]).unwrap();
    assert_eq!(body["reasoning_effort"], "none");
    assert_eq!(
        body["chat_template_kwargs"],
        json!({ "thinking": false, "enable_thinking": false, "tokenize": false })
    );
    assert_eq!(body["response_format"], json!({ "type": "json_object" }));
    assert_eq!(body["max_tokens"], 256);
}

#[tokio::test]
async fn control_selects_native_kimi_reasoning_switch() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "chutes:moonshotai/kimi-k2.6",
                "format": "openai",
                "engine": "vllm",
                "reasoningFormat": "chat_template_thinking"
            }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let (service, _, forwarded) = build_sequenced_service(vec![200]);
    let mut input = chat_input();
    input.params = json!({
        "model": "moonshotai/kimi-k2.6",
        "messages": [{ "role": "user", "content": "Return JSON" }],
        "reasoning_effort": "none",
        "response_format": { "type": "json_object" },
        "max_tokens": 64
    });

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 200);
    let body: Value = serde_json::from_slice(&forwarded.lock().unwrap()[0]).unwrap();
    assert_eq!(body["chat_template_kwargs"], json!({ "thinking": false }));
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("reasoning").is_none());
}

/// End to end for the shape that exposed the gap: a caller whose only way to
/// say "no thinking" is the chat-template switch, and a route whose upstream
/// ignores it. The switch has to reach that upstream as the dialect the route
/// declared.
#[tokio::test]
async fn chat_template_switch_reaches_a_managed_route_as_its_own_dialect() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "vendor:acme/model-a",
                "format": "openai",
                "engine": "sglang",
                "reasoningFormat": "reasoning_effort",
                "reasoningPolicy": { "threshold": 2048 }
            }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let (service, _, forwarded) = build_sequenced_service(vec![200]);
    let mut input = chat_input();
    input.params = json!({
        "model": "acme/model-a",
        "messages": [{ "role": "user", "content": "How many apples?" }],
        "max_tokens": 65536,
        "temperature": 1,
        "top_p": 0.95,
        "chat_template_kwargs": { "thinking": false, "enable_thinking": false }
    });

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 200);
    let body: Value = serde_json::from_slice(&forwarded.lock().unwrap()[0]).unwrap();
    assert_eq!(body["reasoning_effort"], "none");
    // Still forwarded for an upstream that does act on it.
    assert_eq!(
        body["chat_template_kwargs"],
        json!({ "thinking": false, "enable_thinking": false })
    );
}

/// A route that declares no dialect keeps today's behavior exactly: the switch
/// is a passthrough and nothing is synthesized. This is what keeps the change
/// off the managed surfaces, where an invented reasoning parameter would be
/// rejected rather than ignored.
#[tokio::test]
async fn chat_template_switch_stays_a_passthrough_without_a_declared_dialect() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "self-hosted:acme/model-a",
                "format": "openai",
                "engine": "vllm"
            }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let (service, _, forwarded) = build_sequenced_service(vec![200]);
    let mut input = chat_input();
    input.params = json!({
        "model": "acme/model-a",
        "messages": [{ "role": "user", "content": "How many apples?" }],
        "chat_template_kwargs": { "thinking": false, "enable_thinking": false }
    });

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 200);
    let body: Value = serde_json::from_slice(&forwarded.lock().unwrap()[0]).unwrap();
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("reasoning").is_none());
    assert_eq!(
        body["chat_template_kwargs"],
        json!({ "thinking": false, "enable_thinking": false })
    );
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

    let input = chat_input();
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
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
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
        prefix_hash: None,
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
    let input = chat_input();

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
        json!({ "allow": true, "candidates": [{ "routeId": "tee-a:gpt-test", "format": "openai" }] }),
    )
    .await;
    let mw = middleware(control_url);
    // ACI verification fails for the constrained candidate, so forwarding fails.
    let service = build_service_failing_verify();
    let mut input = chat_input();
    input.aci_required = true;

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
    // A retryable non-2xx that is NOT a capacity signal, so the walk is a single
    // pass and this stays a test about attribution. 429 would take the delayed
    // capacity-retry path, which the capacity_retry_* suite covers on its own.
    let service = build_service_with_upstream(503, br#"{"error":"unavailable"}"#.to_vec());
    let mut input = chat_input();
    input.stream = true;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 503, "the upstream status must reach the client");

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(503)).await;
    assert_eq!(
        report["selectedRouteId"],
        json!("openai:gpt"),
        "an unattributed upstream failure counts against no route's health"
    );
    assert_eq!(report["isStreaming"], json!(true));
    assert_eq!(report["attemptIndex"], json!(0));
    // The record carries the upstream's own words (bounded, unscrubbed): the
    // client-facing envelope is the scrubbed surface, the report is not.
    assert_eq!(report["errorMessage"], json!("unavailable"));
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
    // Failover exhausted: every candidate fails, and each attempt must reach
    // control as its own attributed report — the failed-over one via
    // failed_attempts, the last via upstream_error. The gateway does not dedupe
    // `candidates` — it forwards whatever control supplies, and control
    // implementations are swappable (the open-source stack ships its own). A
    // route id repeated in the list must still yield one report per attempt:
    // control dedupes by (request_id, attempt, status), so two failures sharing
    // an attempt index would silently collapse into one, under-counting the
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
    // Retryable but not a capacity signal, so the walk is a single pass and the
    // indices under test are the walk's own (see the note in the test above).
    let service = build_service_with_upstream(503, br#"{"error":"unavailable"}"#.to_vec());
    let mut input = chat_input();
    input.stream = true;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 503);

    wait_for_post(&posts, |r| r["attemptIndex"].as_i64() == Some(1)).await;
    let indices: Vec<i64> = posts
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["status"].as_i64() == Some(503))
        .filter_map(|r| r["attemptIndex"].as_i64())
        .collect();
    assert_eq!(
        indices,
        vec![0, 1],
        "both failures against the repeated route must carry distinct attempt indices"
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
        AciServiceConfig::for_test(),
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
        aci_required: true,
        aci_session_ids: session_ids,
        upstream_verification_event: None,
        requester: None,
        e2ee: None,
    };
    let candidate = || ForwardCandidate {
        route_id: "tee-a:gpt-test".to_string(),
        path: "/v1/chat/completions",
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
    // Plain failover, no ACI constraint: the first candidate answers a real
    // failure, the walk moves on, and the second cannot even be routed. A route
    // that never reached an upstream must not overwrite one that did — the
    // client's status is the real 503, not a 404 synthesized from the second
    // candidate's own failure. Both streaming and buffered, which commit
    // through separate paths.
    //
    // 503 rather than 429 so the walk stays a single pass: a capacity signal
    // would take the delayed retry, and the precedence under test here is the
    // walk's own. The capacity_retry_* suite covers that path.
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
        let (service, forwarded) = build_tee_aware_service_with_status(503);
        let mut input = chat_input();
        input.stream = stream;

        let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
        assert_eq!(
            status, 503,
            "stream={stream}: the real upstream status wins"
        );
        assert_eq!(
            *forwarded.lock().unwrap(),
            vec!["plain:gpt-test".to_string()],
            "stream={stream}: only the routable candidate was contacted"
        );

        // The committed failure must be reported last: dashboards read a request's
        // user-facing status as the one at the highest attempt index, so a
        // committed response sitting behind a later attempt would be misread.
        let report = wait_for_post(&posts, |r| {
            r["status"].as_i64() == Some(503) && r["selectedRouteId"] == json!("plain:gpt-test")
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
async fn aci_constraint_fails_closed_when_attestation_fails() {
    // A route classed as TEE is not the same as a route whose attestation held.
    // `provider.aci_verified` pins the request to an attested upstream. The
    // constraint must not degrade into a static provider-name check: a TEE route
    // with a failed or missing attestation must not receive the prompt.
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
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let mut input = chat_input();
    input.aci_required = true;

    let (status, headers, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 503, "a failed attestation must still fail closed");
    // §7.5/§10 on the middleware path too: the named error type plus a
    // fetchable refusal receipt.
    assert_eq!(body["error"]["type"], json!("upstream_verification_failed"));
    let receipt_id = headers
        .get("x-receipt-id")
        .expect("a refusal carries X-Receipt-Id")
        .to_str()
        .unwrap();
    let receipt = service
        .get_receipt_by_receipt_id(receipt_id)
        .expect("the refusal receipt must be retrievable");
    let uv = receipt.document_json().unwrap()["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "upstream.verified")
        .cloned()
        .expect("refusal receipts record upstream.verified");
    assert_eq!(uv["result"], "failed");
    assert_eq!(uv["required"], true);
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
    assert_eq!(status, 404);
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
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
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
        prefix_hash: None,
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
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
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
        prefix_hash: None,
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

// A mock upstream that answers each forward with the next status in a script,
// recording every contacted route — the capacity-retry tests need "429 first,
// 200 on the delayed second pass" to be observable per attempt.
struct SequencedUpstream {
    forwarded: Arc<Mutex<Vec<String>>>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    statuses: Mutex<std::collections::VecDeque<u16>>,
}

#[async_trait]
impl UpstreamBackend for SequencedUpstream {
    fn name(&self) -> &str {
        "sequenced-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://sequenced-upstream.example")
    }
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let route_id = req.target_route_id.clone().unwrap_or_default();
        Ok(PreparedUpstreamRequest {
            upstream_name: self.name().to_string(),
            url_origin: self.url_origin().map(str::to_string),
            model_id: "gpt-test".to_string(),
            is_tee: Some(false),
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
        self.bodies.lock().unwrap().push(req.request.body.clone());
        let status = self
            .statuses
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let body = match status {
            200 => br#"{"id":"ok","choices":[]}"#.to_vec(),
            // The recognized upstream capacity/no-targets signal, under a
            // status OUTSIDE the retryable whitelist — guards the shared
            // classification contract, not the whitelist path.
            520 => br#"{"error":"exhausted all available targets"}"#.to_vec(),
            _ => br#"{"error":{"message":"capacity"}}"#.to_vec(),
        };
        Ok(UpstreamResponse {
            status_code: status,
            body,
            headers,
            served_instance_id: None,
        })
    }
    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(req).await
    }
    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let response = self.forward_prepared(req).await?;
        let body = Bytes::from(response.body);
        Ok(UpstreamStreamResponse {
            status_code: response.status_code,
            headers: response.headers,
            body: Box::pin(futures_util::stream::once(async move { Ok(body) })),
            served_instance_id: response.served_instance_id,
        })
    }
    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.forward_stream_prepared(req).await
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        unreachable!("sequenced upstream forwards via prepared paths only")
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

#[allow(clippy::type_complexity)]
fn build_sequenced_service(
    statuses: Vec<u16>,
) -> (
    Arc<AciService>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<Vec<u8>>>>,
) {
    let forwarded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(SequencedUpstream {
                forwarded: forwarded.clone(),
                bodies: bodies.clone(),
                statuses: Mutex::new(statuses.into_iter().collect()),
            }),
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    (service, forwarded, bodies)
}

fn capacity_retry_request(user_tier: Option<&str>) -> ChatCompletionRequest<'static> {
    ChatCompletionRequest {
        context: GatewayRequestContext {
            user_model: Some("gpt-test".to_string()),
            user_tier: user_tier.map(str::to_string),
            ..GatewayRequestContext::default()
        },
        endpoint_path: "/v1/chat/completions",
        received_body: br#"{"model":"gpt-test","messages":[]}"#,
        forwarded_body: None,
        aci_required: false,
        aci_session_ids: Vec::new(),
        upstream_verification_event: None,
        requester: None,
        e2ee: None,
    }
}

fn plain_candidate(route: &str) -> ForwardCandidate {
    ForwardCandidate {
        route_id: route.to_string(),
        path: "/v1/chat/completions",
        body: br#"{"model":"gpt-test","messages":[]}"#.to_vec(),
    }
}

// Virtual time: the retry sleep auto-advances, so the test runs instantly.
#[tokio::test(start_paused = true)]
async fn capacity_retry_gives_non_preemptible_traffic_a_delayed_second_pass() {
    let (service, forwarded, _) = build_sequenced_service(vec![429, 200]);
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![plain_candidate("plain:gpt-test")],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::Forwarded(forward) => {
            // The first attempt's 429 stays observable behind the commit.
            assert_eq!(
                attempts(&forward.failed_attempts),
                vec![("plain:gpt-test".to_string(), 429)]
            );
        }
        _ => panic!("the delayed second pass must commit the 200"),
    }
    assert_eq!(
        *forwarded.lock().unwrap(),
        vec!["plain:gpt-test".to_string(), "plain:gpt-test".to_string()],
        "exactly one delayed re-contact"
    );
}

/// The tier used to suppress this pass. Asserting the reversal rather than
/// deleting the case keeps the old rule from being reintroduced by accident.
#[tokio::test(start_paused = true)]
async fn capacity_retry_covers_preemptible_traffic_too() {
    let (service, forwarded, _) = build_sequenced_service(vec![429, 200]);
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(Some("basic")),
            vec![plain_candidate("plain:gpt-test")],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::Forwarded(forward) => {
            assert_eq!(
                attempts(&forward.failed_attempts),
                vec![("plain:gpt-test".to_string(), 429)]
            );
        }
        _ => panic!("the delayed second pass must commit the 200"),
    }
    assert_eq!(
        *forwarded.lock().unwrap(),
        vec!["plain:gpt-test".to_string(), "plain:gpt-test".to_string()],
        "the tier no longer suppresses the re-contact"
    );
}

#[tokio::test(start_paused = true)]
async fn capacity_retry_replays_only_the_capacity_rejections() {
    // Pass 1: a answers 500, b answers 429. The second pass replays b alone —
    // re-hitting the hard-failed a would feed the breaker another failure for
    // a request it cannot serve.
    let (service, forwarded, _) = build_sequenced_service(vec![500, 429, 200]);
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![plain_candidate("a:gpt-test"), plain_candidate("b:gpt-test")],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::Forwarded(forward) => {
            assert_eq!(forward.selected_route, "b:gpt-test");
            assert_eq!(
                attempts(&forward.failed_attempts),
                vec![
                    ("a:gpt-test".to_string(), 500),
                    ("b:gpt-test".to_string(), 429)
                ]
            );
        }
        _ => panic!("the retried capacity rejection must commit"),
    }
    assert_eq!(
        *forwarded.lock().unwrap(),
        vec![
            "a:gpt-test".to_string(),
            "b:gpt-test".to_string(),
            "b:gpt-test".to_string()
        ],
        "the hard failure is not replayed"
    );
}

#[tokio::test(start_paused = true)]
async fn capacity_retry_triggers_regardless_of_candidate_order() {
    // The mirror of the mixed-chain test: the capacity rejection comes FIRST
    // and the hard failure last. The retry decision keys on "did this pass see
    // any capacity signal", not on which candidate happened to sit last.
    let (service, forwarded, _) = build_sequenced_service(vec![429, 500, 200]);
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![plain_candidate("a:gpt-test"), plain_candidate("b:gpt-test")],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::Forwarded(forward) => {
            assert_eq!(forward.selected_route, "a:gpt-test");
            assert_eq!(
                attempts(&forward.failed_attempts),
                vec![
                    ("a:gpt-test".to_string(), 429),
                    ("b:gpt-test".to_string(), 500)
                ]
            );
        }
        _ => panic!("the retried capacity rejection must commit"),
    }
    assert_eq!(
        *forwarded.lock().unwrap(),
        vec![
            "a:gpt-test".to_string(),
            "b:gpt-test".to_string(),
            "a:gpt-test".to_string()
        ],
        "only the capacity rejection is replayed, wherever it sits in the chain"
    );
}

#[tokio::test(start_paused = true)]
async fn capacity_retry_tracks_candidates_by_index_not_route_id() {
    // Callers may repeat a route id with distinct bodies. The first twin hard-
    // fails, the second hits capacity: the replay must re-send the SECOND
    // twin's body only — reconstructing the target set from route ids would
    // replay the hard-failed twin too.
    let (service, forwarded, bodies) = build_sequenced_service(vec![500, 429, 200]);
    let twin = |body: &[u8]| ForwardCandidate {
        route_id: "dup:gpt-test".to_string(),
        path: "/v1/chat/completions",
        body: body.to_vec(),
    };
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![twin(br#"{"n":1}"#), twin(br#"{"n":2}"#)],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    assert!(matches!(result, MiddlewareForwardResult::Forwarded(_)));
    assert_eq!(forwarded.lock().unwrap().len(), 3, "exactly one replay");
    assert_eq!(
        *bodies.lock().unwrap(),
        vec![
            br#"{"n":1}"#.to_vec(),
            br#"{"n":2}"#.to_vec(),
            br#"{"n":2}"#.to_vec()
        ],
        "the replay carries the capacity-rejected twin's body, not the hard-failed one"
    );
}

#[tokio::test(start_paused = true)]
async fn capacity_retry_covers_the_recognized_5xx_capacity_signal() {
    // An upstream that reports "exhausted all available targets" under a 5xx
    // is surfaced to clients as 429 by error normalization — the retry (and
    // failover) must treat it as capacity too, even when the status sits
    // outside the retryable whitelist (520 here), or a request could be told
    // 429 without ever getting the second chance that 429s get.
    let (service, forwarded, _) = build_sequenced_service(vec![520, 200]);
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![plain_candidate("plain:gpt-test")],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::Forwarded(forward) => {
            assert_eq!(
                attempts(&forward.failed_attempts),
                vec![("plain:gpt-test".to_string(), 520)]
            );
        }
        _ => panic!("the capacity-signal 5xx must be replayed and commit the 200"),
    }
    assert_eq!(forwarded.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn capacity_retry_streaming_replays_and_commits_the_final_429() {
    // The streaming walk has its own retention/exit code: a single-candidate
    // 429 must take the delayed second pass, and a second 429 must commit as
    // the terminal upstream error with both contacts observable.
    let (service, forwarded, _) = build_sequenced_service(vec![429, 429]);
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![plain_candidate("plain:gpt-test")],
            true,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::UpstreamError(err) => {
            assert_eq!(err.error.upstream_status, 429);
            assert_eq!(err.selected_route, "plain:gpt-test");
            // The pass-1 429 stays observable behind the terminal report.
            assert_eq!(
                attempts(&err.failed_attempts),
                vec![("plain:gpt-test".to_string(), 429)]
            );
        }
        _ => panic!("both passes 429 must surface the terminal upstream error"),
    }
    assert_eq!(forwarded.lock().unwrap().len(), 2, "exactly one replay");
}

#[tokio::test(start_paused = true)]
async fn capacity_retry_streaming_second_pass_commits_the_stream() {
    let (service, forwarded, _) = build_sequenced_service(vec![429, 200]);
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![plain_candidate("plain:gpt-test")],
            true,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::Stream(stream) => {
            assert_eq!(stream.upstream_status, 200);
            assert_eq!(
                attempts(&stream.failed_attempts),
                vec![("plain:gpt-test".to_string(), 429)]
            );
        }
        _ => panic!("the delayed second pass must commit the stream"),
    }
    assert_eq!(forwarded.lock().unwrap().len(), 2);
}

// ── Request features (Phase C) ───────────────────────────────────────────────

#[tokio::test]
async fn consult_pre_carries_request_features_and_post_echoes_prefix_hash() {
    let (control_url, pres, posts) = spawn_control_capturing_pre(json!({
        "allow": true,
        "candidates": [{ "routeId": "acme:model-a", "format": "openai" }],
        "pricing": { "inputCostPerToken": "0", "outputCostPerToken": "0" }
    }))
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_upstream(
        200,
        br#"{"id":"x","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.to_vec(),
    );

    // Long enough to fill the 4KB prefix cap — sub-cap conversations emit no
    // affinity hash on purpose (worthless to key, and dictionary-guessable).
    let mut input = chat_input();
    input.params["messages"][0]["content"] = json!("s".repeat(5000));
    let response = mw.handle_completion(&service, input).await;
    assert_eq!(response.status().as_u16(), 200);

    let pre = pres.lock().unwrap().first().cloned().unwrap();
    let features = &pre["request"];
    assert!(
        features["estimatedPromptTokens"].is_u64(),
        "pre body must carry request features: {pre}"
    );
    assert_eq!(features["reasoning"], json!("unspecified"));
    assert_eq!(features["responseFormat"], json!("text"));
    assert_eq!(features["inputModalities"], json!(["text"]));
    let hash = features["prefixHash"]
        .as_str()
        .expect("prefix hash")
        .to_string();
    assert_eq!(hash.len(), 32);

    // The post report echoes the hash — billing keys cache affinity on it.
    let report = wait_for_post(&posts, |r| r["selectedRouteId"].is_string()).await;
    assert_eq!(report["prefixHash"].as_str(), Some(hash.as_str()));
}

#[tokio::test]
async fn send_request_features_off_restores_the_featureless_pre_body() {
    let (control_url, pres, _posts) = spawn_control_capturing_pre(json!({
        "allow": false, "status": 403, "message": "denied"
    }))
    .await;
    let mw = Middleware::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
        send_request_features: Some(false),
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
    })
    .unwrap();
    let service = build_service();

    let _ = mw.handle_completion(&service, chat_input()).await;
    let pre = pres.lock().unwrap().first().cloned().unwrap();
    // The rollback lever: off must mean the pre body of the featureless era,
    // not a request field with empty innards.
    assert!(
        pre.get("request").is_none(),
        "unexpected request field: {pre}"
    );
}

// A mock upstream whose forward never resolves: it stands in for a provider
// that accepted the connection but has not answered (a long prefill queue),
// so a client that gives up disconnects before any response header.
struct HangingUpstream;

#[async_trait]
impl UpstreamBackend for HangingUpstream {
    fn name(&self) -> &str {
        "hang-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://hang-upstream.example")
    }
    // Classified attested so an `aci_verified` request passes route
    // eligibility and reaches the (never-resolving) header wait.
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        Ok(PreparedUpstreamRequest {
            upstream_name: self.name().to_string(),
            url_origin: self.url_origin().map(str::to_string),
            model_id: "gpt-test".to_string(),
            is_tee: Some(true),
            route_id: req.target_route_id.clone(),
            request: req,
        })
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        std::future::pending().await
    }
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        std::future::pending().await
    }
    async fn forward_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        std::future::pending().await
    }
    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        std::future::pending().await
    }
}

// A mock upstream that waits, then reports a gateway-enforced read timeout —
// the reqwest deadline the real client applies, surfaced as `Timeout`.
struct TimingOutUpstream {
    delay: Duration,
}

#[async_trait]
impl UpstreamBackend for TimingOutUpstream {
    fn name(&self) -> &str {
        "timeout-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://timeout-upstream.example")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        tokio::time::sleep(self.delay).await;
        Err(UpstreamError::Timeout("read timeout".to_string()))
    }
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Timeout("read timeout".to_string()))
    }
    async fn forward_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        tokio::time::sleep(self.delay).await;
        Err(UpstreamError::Timeout("read timeout".to_string()))
    }
    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        tokio::time::sleep(self.delay).await;
        Err(UpstreamError::Timeout("read timeout".to_string()))
    }
}

fn build_service_with_backend(backend: Arc<dyn UpstreamBackend>) -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            backend,
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn client_disconnect_before_upstream_response_reports_499_with_route() {
    // The upstream never answers; the client gives up. Dropping the handler
    // future (as hyper does on a closed connection) must still leave exactly
    // one usage report: a 499 attributed to the candidate in flight, with no
    // TTFT because no byte ever arrived.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "mock:hang", "format": "openai" }] }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_backend(Arc::new(HangingUpstream));
    let mut input = chat_input();
    input.stream = true;

    let result = tokio::time::timeout(
        Duration::from_millis(300),
        mw.handle_completion(&service, input),
    )
    .await;
    assert!(
        result.is_err(),
        "the handler must still be waiting on the upstream"
    );

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(499)).await;
    assert_eq!(report["selectedRouteId"], json!("mock:hang"));
    assert_eq!(report["attemptIndex"], json!(0));
    assert_eq!(report["isStreaming"], json!(true));
    assert!(report.get("ttftMs").map(Value::is_null).unwrap_or(true));
    assert!(report
        .get("errorSource")
        .map(Value::is_null)
        .unwrap_or(true));
    assert!(report["errorMessage"]
        .as_str()
        .unwrap_or("")
        .contains("before upstream response"));

    // Exactly one report for this request: the drop must not race a second row.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let count = posts
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["requestId"] == json!("req-1"))
        .count();
    assert_eq!(count, 1, "the guard fires once, not per poll");
}

#[tokio::test]
async fn upstream_timeout_is_recorded_as_504_per_attempt_and_summary() {
    // Two candidates, both timing out: the client sees a 504 timeout envelope,
    // each attempt is recorded as 504 with its own elapsed time, and the
    // request-level summary is a 504 attributed to the upstream.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [
            { "routeId": "a:gpt-test", "format": "openai" },
            { "routeId": "b:gpt-test", "format": "openai" }
        ] }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_backend(Arc::new(TimingOutUpstream {
        delay: Duration::from_millis(20),
    }));

    let (status, _, body) =
        response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 504);
    assert_eq!(body["error"]["type"], json!("timeout_error"));

    for (index, route) in [(0, "a:gpt-test"), (1, "b:gpt-test")] {
        let report = wait_for_post(&posts, move |r| {
            r["attemptIndex"].as_i64() == Some(index) && r["selectedRouteId"] == json!(route)
        })
        .await;
        assert_eq!(
            report["status"].as_i64(),
            Some(504),
            "attempt {index} status"
        );
        assert!(
            report["durationMs"].as_i64().unwrap_or(0) >= 20,
            "attempt {index} must record how long it was waited for"
        );
    }
    let summary = wait_for_post(&posts, |r| {
        r["attemptIndex"].as_i64() == Some(2) && r["status"].as_i64() == Some(504)
    })
    .await;
    assert_eq!(summary["errorSource"], json!("upstream"));
    assert_eq!(summary["selectedRouteId"], Value::Null);
}

#[tokio::test]
async fn mid_stream_read_timeout_settles_504_with_message() {
    // A streaming response that begins, then the upstream read deadline fires
    // mid-body. The client already has 200 headers, so only the usage report
    // carries the failure: 504, with the timeout's own words.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let control = ControlClient::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
    })
    .unwrap();
    let report = StreamReport {
        control,
        request_id: "r-readtimeout".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        request_model: "gpt".to_string(),
        pricing: None,
        spend_mode: None,
        user_id: None,
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        started: std::time::Instant::now(),
        downstream_abort: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        settled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let events: Vec<Result<Bytes, ServiceError>> = vec![
        Ok(Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        )),
        Err(ServiceError::Upstream(UpstreamError::Timeout(
            "upstream timed out".to_string(),
        ))),
    ];
    let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
    let mut metered = MeterStream::new(inner, report, SseProtocol::OpenaiChat);
    while metered.next().await.is_some() {}

    let report = wait_for_post(&posts, |r| r["requestId"] == json!("r-readtimeout")).await;
    assert_eq!(report["status"], json!(504));
    assert!(report["errorMessage"]
        .as_str()
        .unwrap_or("")
        .contains("timed out"));
}

// A mock upstream that waits before returning a normal streaming success —
// long enough for the pre-first-byte keep-alive to have committed the 200.
struct SlowStreamUpstream {
    delay: Duration,
}

#[async_trait]
impl UpstreamBackend for SlowStreamUpstream {
    fn name(&self) -> &str {
        "slow-stream-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://slow-stream-upstream.example")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        unreachable!("streaming test uses forward_stream_verified_prepared")
    }
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Transport("n/a".to_string()))
    }
    async fn forward_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        unreachable!("streaming test uses forward_stream_verified_prepared")
    }
    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        tokio::time::sleep(self.delay).await;
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        Ok(UpstreamStreamResponse {
            status_code: 200,
            headers,
            body: Box::pin(futures_util::stream::once(
                async move { Ok(Bytes::from(body)) },
            )),
            served_instance_id: None,
        })
    }
}

fn middleware_with_keepalive(control_url: String, ms: u64) -> Middleware {
    Middleware::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: Some(ms),
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn early_keepalive_commits_200_before_upstream_headers() {
    // The upstream takes longer than the keep-alive interval to answer. The
    // gateway must commit a 200 SSE body first, heartbeat, then splice in the
    // real stream once it arrives — and still meter the request as a success.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "openai:gpt", "format": "openai" }],
            "pricing": { "inputCostPerToken": "0", "outputCostPerToken": "0" }
        }),
    )
    .await;
    let mw = middleware_with_keepalive(control_url, 100);
    let service = build_service_with_backend(Arc::new(SlowStreamUpstream {
        delay: Duration::from_millis(400),
    }));
    let mut input = chat_input();
    input.stream = true;

    let started = std::time::Instant::now();
    let response = mw.handle_completion(&service, input).await;
    // Committed well before the upstream's 400ms; do not wait for the body.
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "must commit early"
    );
    let (headers, body) = raw_body(response).await;
    assert_eq!(headers.get("content-type").unwrap(), "text/event-stream");
    assert!(body.starts_with(": PROCESSING"), "body: {body}");
    assert!(body.contains("\"content\":\"hi\""), "body: {body}");
    assert!(body.contains("data: [DONE]"), "body: {body}");

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(200)).await;
    assert_eq!(report["isStreaming"], json!(true));
    assert!(report["ttftMs"].as_i64().unwrap_or(0) >= 400);
}

#[tokio::test]
async fn early_keepalive_turns_forward_failure_into_in_band_error() {
    // The upstream is slow, so the 200 commits; then it times out. The failure
    // must reach the client in-band as a 504 error event, and the usage report
    // must record 504 (not a 499 — the client did not leave).
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let mw = middleware_with_keepalive(control_url, 100);
    let service = build_service_with_backend(Arc::new(TimingOutUpstream {
        delay: Duration::from_millis(300),
    }));
    let mut input = chat_input();
    input.stream = true;

    let response = mw.handle_completion(&service, input).await;
    let (_, body) = raw_body(response).await;
    assert!(body.starts_with(": PROCESSING"), "body: {body}");
    assert!(body.contains("\"code\":504"), "body: {body}");
    assert!(body.contains("data: [DONE]"), "body: {body}");

    // The serving route's attempt is recorded as 504.
    let attempt = wait_for_post(&posts, |r| {
        r["status"].as_i64() == Some(504) && r["selectedRouteId"] == json!("openai:gpt")
    })
    .await;
    assert_eq!(attempt["attemptIndex"], json!(0));
    // The request-level summary is a 504 attributed to the upstream.
    let summary = wait_for_post(&posts, |r| {
        r["status"].as_i64() == Some(504) && r["errorSource"] == json!("upstream")
    })
    .await;
    assert_eq!(summary["selectedRouteId"], Value::Null);
    // No client-disconnect row: the client stayed, the upstream failed.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !posts
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["status"].as_i64() == Some(499)),
        "a slow upstream failure is not a client disconnect"
    );
}

#[tokio::test]
async fn unpolled_drop_is_a_client_disconnect_unless_the_pipeline_marked_itself() {
    let (control_url, posts) = spawn_control_capturing(200, json!({})).await;
    let config = MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
    };
    let report_for = |id: &str, abort: &Arc<std::sync::atomic::AtomicBool>| StreamReport {
        control: ControlClient::new(&config).unwrap(),
        request_id: id.to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        request_model: "gpt".to_string(),
        pricing: None,
        spend_mode: None,
        user_id: None,
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        started: std::time::Instant::now(),
        downstream_abort: abort.clone(),
        settled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let pending = || -> ServiceResponseStream { Box::pin(futures_util::stream::pending()) };

    // Dropped unpolled with no abort mark: hyper never read the body because
    // the client vanished right after the headers — a 499, not our failure.
    let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    drop(MeterStream::new(
        pending(),
        report_for("r-unpolled-client", &abort),
        SseProtocol::OpenaiChat,
    ));
    let report = wait_for_post(&posts, |r| r["requestId"] == json!("r-unpolled-client")).await;
    assert_eq!(report["status"], json!(499));
    assert!(report
        .get("errorSource")
        .map(Value::is_null)
        .unwrap_or(true));

    // Dropped unpolled with the abort mark set (the finalizer hand-off is the
    // one internal point that drops an unpolled pipeline): a gateway failure.
    let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    drop(MeterStream::new(
        pending(),
        report_for("r-unpolled-gateway", &abort),
        SseProtocol::OpenaiChat,
    ));
    let report = wait_for_post(&posts, |r| r["requestId"] == json!("r-unpolled-gateway")).await;
    assert_eq!(report["status"], json!(502));
    assert_eq!(report["errorSource"], json!("gateway"));
    assert_eq!(report["selectedRouteId"], json!("openai:gpt"));
}

#[tokio::test]
async fn aci_constrained_requests_are_never_committed_early() {
    // Even with the pre-upstream commit enabled, an `aci_verified` request must
    // keep HTTP semantics (refusal receipts, x-receipt-id): the gateway keeps
    // waiting for the upstream instead of committing a 200.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "tee-hang:gpt-test", "format": "openai" }] }),
    )
    .await;
    let mw = middleware_with_keepalive(control_url, 100);
    let service = build_service_with_backend(Arc::new(HangingUpstream));
    let mut input = chat_input();
    input.stream = true;
    input.aci_required = true;

    let result = tokio::time::timeout(
        Duration::from_millis(400),
        mw.handle_completion(&service, input),
    )
    .await;
    assert!(
        result.is_err(),
        "an ACI-constrained request must not be committed before the upstream answers"
    );
    // The abandoned wait still reports through the drop guard.
    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(499)).await;
    assert_eq!(report["selectedRouteId"], json!("tee-hang:gpt-test"));
}

// First streaming forward times out, every later one serves a normal SSE 200 —
// the shape of a failover where the serving candidate is not the first.
struct FailoverScriptedUpstream {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl UpstreamBackend for FailoverScriptedUpstream {
    fn name(&self) -> &str {
        "failover-scripted-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://failover-scripted-upstream.example")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Transport("buffered path unused".to_string()))
    }
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Transport("unused".to_string()))
    }
    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Err(UpstreamError::Timeout("read timeout".to_string()));
        }
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        Ok(UpstreamStreamResponse {
            status_code: 200,
            headers,
            body: Box::pin(futures_util::stream::once(async {
                Ok(Bytes::from("data: [DONE]\n\n"))
            })),
            served_instance_id: None,
        })
    }
}

#[tokio::test]
async fn in_flight_tracks_the_serving_candidate_across_failover() {
    // The late-finalizer failure report reads the serving candidate back from
    // the journal; if `in_flight` were stuck on attempt 0, that report would
    // collide with the first attempt's own row under the control plane's
    // (request_id, attempt, status) dedupe and be dropped.
    let service = build_service_with_backend(Arc::new(FailoverScriptedUpstream {
        calls: std::sync::atomic::AtomicUsize::new(0),
    }));
    let journal = MiddlewareReceiptJournal::default();
    let result = service
        .forward_chat_completion_for_middleware(
            capacity_retry_request(None),
            vec![plain_candidate("a:gpt-test"), plain_candidate("b:gpt-test")],
            true,
            journal.clone(),
        )
        .await
        .unwrap();
    match result {
        MiddlewareForwardResult::Stream(stream) => {
            assert_eq!(
                attempts(&stream.failed_attempts),
                vec![("a:gpt-test".to_string(), 504)]
            );
            let in_flight = journal.in_flight().expect("serving candidate published");
            assert_eq!(in_flight.route_id, "b:gpt-test");
            assert_eq!(
                in_flight.attempt_index, 1,
                "a late report keyed on attempt 0 would collide with the failed attempt's row"
            );
        }
        other => panic!(
            "second candidate must serve the stream, got {}",
            match other {
                MiddlewareForwardResult::Forwarded(_) => "Forwarded",
                MiddlewareForwardResult::UpstreamError(_) => "UpstreamError",
                MiddlewareForwardResult::AllFailed(_) => "AllFailed",
                MiddlewareForwardResult::Stream(_) => unreachable!(),
            }
        ),
    }
}

// First streaming forward reports the gateway's read deadline, every later
// one never answers — the shape of "A failed fast, B is being waited on".
struct TimeoutThenHangUpstream {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl UpstreamBackend for TimeoutThenHangUpstream {
    fn name(&self) -> &str {
        "timeout-then-hang-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://timeout-then-hang-upstream.example")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        std::future::pending().await
    }
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        std::future::pending().await
    }
    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Err(UpstreamError::Timeout("read timeout".to_string()));
        }
        std::future::pending().await
    }
}

#[tokio::test]
async fn mid_failover_disconnect_reports_the_finished_attempts_too() {
    // Candidate a fails with the gateway's deadline, candidate b is being
    // waited on when the client disconnects. a's 504 is route evidence that
    // must not vanish with the cancelled forward; b gets the 499.
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [
            { "routeId": "a:gpt-test", "format": "openai" },
            { "routeId": "b:gpt-test", "format": "openai" }
        ] }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_backend(Arc::new(TimeoutThenHangUpstream {
        calls: std::sync::atomic::AtomicUsize::new(0),
    }));
    let mut input = chat_input();
    input.stream = true;

    let result = tokio::time::timeout(
        Duration::from_millis(300),
        mw.handle_completion(&service, input),
    )
    .await;
    assert!(
        result.is_err(),
        "the handler must still be waiting on candidate b"
    );

    let failed = wait_for_post(&posts, |r| r["status"].as_i64() == Some(504)).await;
    assert_eq!(failed["selectedRouteId"], json!("a:gpt-test"));
    assert_eq!(failed["attemptIndex"], json!(0));
    let cancelled = wait_for_post(&posts, |r| r["status"].as_i64() == Some(499)).await;
    assert_eq!(cancelled["selectedRouteId"], json!("b:gpt-test"));
    assert_eq!(cancelled["attemptIndex"], json!(1));
}

// Streams an immediate 429 for every attempt — the same-route capacity-retry
// shape whose relayable HTTP status the early commit must not demote.
struct Always429StreamUpstream;

#[async_trait]
impl UpstreamBackend for Always429StreamUpstream {
    fn name(&self) -> &str {
        "always-429-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://always-429-upstream.example")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Transport("buffered path unused".to_string()))
    }
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Transport("unused".to_string()))
    }
    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamStreamResponse {
            status_code: 429,
            headers,
            body: Box::pin(futures_util::stream::once(async {
                Ok(Bytes::from(
                    r#"{"error":{"message":"upstream at capacity"}}"#,
                ))
            })),
            served_instance_id: None,
        })
    }
}

#[tokio::test]
async fn same_route_retry_is_not_committed_early_and_relays_the_429() {
    // The whole chain (429 → capacity-retry sleep → 429 again) far outlasts the
    // keep-alive interval, but the candidate being waited on has already failed
    // once — so no early 200: the client gets the real HTTP 429 it fails over on.
    let (control_url, _posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let mw = middleware_with_keepalive(control_url, 100);
    let service = build_service_with_backend(Arc::new(Always429StreamUpstream));
    let mut input = chat_input();
    input.stream = true;

    let (status, _, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 429, "the capacity signal must stay an HTTP status");
    assert_eq!(body["error"]["type"], json!("rate_limit_error"));
}

#[tokio::test]
async fn in_band_stream_error_message_reaches_the_usage_report() {
    // A 200 stream failed by an in-band error event must settle with the
    // error's own words, not a bare 502 nothing can be asked about.
    let (control_url, posts) = spawn_control_capturing(200, json!({})).await;
    let control = ControlClient::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
        send_request_features: None,
        prefix_hash_secret: None,
        tee_only_domains: Vec::new(),
    })
    .unwrap();
    let report = StreamReport {
        control,
        request_id: "r-inband".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        request_model: "gpt".to_string(),
        pricing: None,
        spend_mode: None,
        user_id: None,
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        started: std::time::Instant::now(),
        downstream_abort: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        settled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let events: Vec<Result<Bytes, ServiceError>> = vec![Ok(Bytes::from(
        "data: {\"error\":{\"message\":\"Failed to compile json grammar: unsupported\"}}\n\ndata: [DONE]\n\n",
    ))];
    let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
    let mut metered = MeterStream::new(inner, report, SseProtocol::OpenaiChat);
    while metered.next().await.is_some() {}
    drop(metered);

    let report = wait_for_post(&posts, |r| r["requestId"] == json!("r-inband")).await;
    assert_eq!(report["status"], json!(502));
    assert!(report["errorMessage"]
        .as_str()
        .unwrap_or("")
        .contains("Failed to compile json grammar"));
}
