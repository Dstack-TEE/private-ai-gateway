//! In-process completion orchestration tests: pre-consult reasoning validation,
//! consult-driven denials, fail-closed control errors, rate limits, empty
//! candidates, and successful forwarding through receipt finalization.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::admission;
mod common;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::{body::to_bytes, routing::post, Json, Router};
use futures_util::StreamExt;
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::upstream::{
    OpenAICompatibleBackend, PreparedUpstreamRequest, UpstreamBackend, UpstreamError,
    UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
    MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES,
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
use private_ai_gateway::middleware::types::{OrganizationScope, TenantIdentity};
use private_ai_gateway::middleware::{
    CompletionInput, Middleware, MiddlewareConfig, PrefixHashKey,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use common::{event_from_request, StaticKeyProvider, StubQuoter};

/// A middleware with a fixed prefix-hash key; the gateway derives the real
/// key from dstack KMS.
fn new_middleware(config: &MiddlewareConfig) -> Result<Middleware, String> {
    Middleware::new(PrefixHashKey::new([7; 32]), config)
}

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
    headers: HashMap<String, String>,
    raw_body: bytes::Bytes,
    content_type: Option<String>,
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
        let parsed = if let Some(content_type) = req.content_type.as_deref() {
            private_ai_gateway::middleware::audio::parse(req.body.clone(), content_type)
                .await
                .unwrap()
                .0
        } else {
            serde_json::from_slice(&req.body).unwrap()
        };
        self.requests.lock().unwrap().push(RecordedRequest {
            headers: req.headers.clone(),
            raw_body: req.body.clone(),
            content_type: req.content_type.clone(),
            path: req.path,
            body: parsed,
        });
        Ok(UpstreamResponse {
            status_code: self.status,
            body: self.body.clone(),
            headers: HashMap::from([
                ("content-type".to_string(), self.content_type.to_string()),
                (
                    "x-typesafe-request-id".to_string(),
                    "upstream-id".to_string(),
                ),
            ]),
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
        if req
            .route_id
            .as_deref()
            .is_some_and(|id| id.starts_with("invalid-"))
        {
            return Err(UpstreamError::InvalidRequest(
                "unsupported field".to_string(),
            ));
        }
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
        if req
            .route_id
            .as_deref()
            .is_some_and(|id| id.starts_with("invalid-"))
        {
            return Err(UpstreamError::InvalidRequest(
                "unsupported field".to_string(),
            ));
        }
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

fn runtime_options() -> UpstreamRuntimeOptions {
    UpstreamRuntimeOptions {
        max_buffered_response_bytes:
            private_ai_gateway::aci::upstream::MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES,
        verifier_mode: UpstreamVerifierMode::Preverified,
        accepted_subjects: vec![],
        accepted_image_digests: vec![],
        accepted_dstack_kms_root_public_keys: vec![],
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 600,
        verifier_request_timeout_seconds: 60,
        privatemode_proxy: None,
    }
}

// The service retains the in-memory backend and verifier, so it no longer needs the file.
fn build_service() -> Arc<AciService> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("upstreams.json");
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

/// Marker planted in upstream error text. It stands in for caller content a
/// provider quotes back, and must reach the caller's response and nothing else.
const CANARY: &str = "CANARY-7f3a";

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Every tracing event this test binary emits, at TRACE — the most verbose
/// level there is. One process-wide subscriber: tests run in parallel, and a
/// per-thread subscriber would race tracing's callsite interest cache.
fn captured_logs() -> &'static LogCapture {
    static LOGS: std::sync::OnceLock<LogCapture> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("no other subscriber is set");
        capture
    })
}

/// The canary appears in no usage report and nowhere in the log output. A
/// probe event written first must show up in the capture, so the absence is not
/// an artefact of logging being off.
fn assert_canary_absent(posts: &Arc<Mutex<Vec<Value>>>, probe: &str) {
    for post in posts.lock().unwrap().iter() {
        assert!(!post.to_string().contains(CANARY), "usage report: {post}");
    }
    tracing::trace!(probe, "log capture probe");
    let logs = captured_logs().text();
    assert!(logs.contains(probe), "the capture records TRACE events");
    assert!(!logs.contains(CANARY), "log output carries upstream text");
}

fn middleware(control_url: String) -> Middleware {
    new_middleware(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
        send_request_features: None,
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
        audio_form: None,
        upload_permit: None,
        endpoint: Endpoint::ChatComplete,
        endpoint_path: "/v1/chat/completions",
        surface: Surface::Openai,
        params: json!({ "model": "gpt-test", "messages": [{ "role": "user", "content": "hi" }] }),
        received_body: br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#
            .to_vec()
            .into(),
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

fn systemone_input() -> CompletionInput {
    let params = json!({
        "model": "gpt-test",
        "state": { "message": "export failed" },
        "questions": {
            "kind": {
                "type": "choice",
                "instructions": "Classify the issue",
                "criteria": { "platform": "outage", "billing": null }
            }
        },
        "stream": true
    });
    CompletionInput {
        audio_form: None,
        upload_permit: None,
        endpoint: Endpoint::SystemOne,
        endpoint_path: "/v1/systemone",
        surface: Surface::Openai,
        received_body: serde_json::to_vec(&params).unwrap().into(),
        params,
        api_key_hash: Some("deadbeef".to_string()),
        requester: None,
        e2ee: None,
        aci_required: false,
        aci_session_ids: Vec::new(),
        request_id: "req-systemone".to_string(),
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
        audio_form: None,
        upload_permit: None,
        endpoint: Endpoint::CreateModelResponse,
        endpoint_path: "/v1/responses",
        surface: Surface::Openai,
        received_body: serde_json::to_vec(&params).unwrap().into(),
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
    assert!(!headers.contains_key("x-typesafe-request-id"));
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
async fn systemone_success_keeps_native_shape_and_uses_native_candidate() {
    let (control_url, _) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "acme:model-a",
                "format": "openai",
                "supportedEndpoints": ["/v1/systemone"]
            }],
            "pricing": { "inputCostPerToken": "1", "outputCostPerToken": "2" }
        }),
    )
    .await;
    let upstream = br#"{
        "model":"internal-model",
        "answers":{"kind":{"type":"choice","choice":"platform","vendor_detail":"keep"}},
        "usage":{"input_tokens":2,"output_tokens":3,"provider_trace":"drop"},
        "error":null,
        "provider":"drop"
    }"#;
    let (service, requests) = build_recording_service(200, upstream.to_vec(), "application/json");

    let (status, headers, body) = response_parts(
        middleware(control_url)
            .handle_completion(&service, systemone_input())
            .await,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(headers["x-typesafe-request-id"], "req-systemone");
    assert_eq!(body["model"], json!("gpt-test"));
    assert_eq!(body["answers"]["kind"]["vendor_detail"], json!("keep"));
    assert_eq!(body["usage"]["input_tokens"], json!(2));
    assert_eq!(body["usage"]["output_tokens"], json!(3));
    assert_eq!(body["usage"]["cost"], json!(8));
    assert!(body.get("provider").is_none());
    assert!(body["usage"].get("provider_trace").is_none());
    assert_eq!(body.as_object().unwrap().len(), 3);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path.as_deref(), Some("/v1/systemone"));
    assert_eq!(
        requests[0].body,
        json!({
            "model": "gpt-test",
            "state": { "message": "export failed" },
            "questions": {
                "kind": {
                    "type": "choice",
                    "instructions": "Classify the issue",
                    "criteria": { "platform": "outage", "billing": null }
                }
            }
        })
    );
}

#[tokio::test]
async fn systemone_unsupported_candidates_never_forward() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [
                { "routeId": "acme:chat", "format": "openai" },
                { "routeId": "acme:responses", "format": "openai",
                  "supportedEndpoints": ["/v1/responses"] }
            ]
        }),
    )
    .await;
    let (service, requests) = build_recording_service(200, b"{}".to_vec(), "application/json");
    let (status, _, body) = response_parts(
        middleware(control_url)
            .handle_completion(&service, systemone_input())
            .await,
    )
    .await;

    assert_eq!(status, 404);
    assert!(body.get("error").is_some());
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn chat_never_forwards_to_a_native_only_route() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "acme:kev", "format": "openai",
                             "supportedEndpoints": ["/v1/systemone"] }]
        }),
    )
    .await;
    let (service, requests) = build_recording_service(200, b"{}".to_vec(), "application/json");
    let (status, _, body) = response_parts(
        middleware(control_url)
            .handle_completion(&service, chat_input())
            .await,
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(body["error"]["type"], json!("model_not_found"));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn systemone_non_2xx_uses_public_error_contract() {
    for upstream_status in [400, 422] {
        let control_url = spawn_control(
            200,
            json!({
                "allow": true,
                "candidates": [{
                    "routeId": "acme:model-a", "format": "openai",
                    "supportedEndpoints": ["/v1/systemone"]
                }]
            }),
        )
        .await;
        let upstream = json!({
            "error": { "message": "question q1 is invalid", "raw": "secret-detail" },
            "trace": "secret-trace"
        });
        let (service, requests) = build_recording_service(
            upstream_status,
            serde_json::to_vec(&upstream).unwrap(),
            "application/json",
        );
        let (status, _, body) = response_parts(
            middleware(control_url)
                .handle_completion(&service, systemone_input())
                .await,
        )
        .await;

        assert_eq!(status, upstream_status);
        assert_eq!(
            body,
            json!({ "error": {
            "type": "invalid_request_error", "message": "question q1 is invalid",
            "code": null, "param": null
        } })
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn systemone_top_level_inband_error_uses_public_error_contract() {
    for code in [400, 422] {
        let control_url = spawn_control(
            200,
            json!({
                "allow": true,
                "candidates": [{
                    "routeId": "acme:model-a", "format": "openai",
                    "supportedEndpoints": ["/v1/systemone"]
                }]
            }),
        )
        .await;
        let upstream = json!({
            "type": "error", "code": code, "message": "question q1 is invalid",
            "trace": "secret-trace"
        });
        let service = build_service_with_upstream(200, serde_json::to_vec(&upstream).unwrap());
        let (status, _, body) = response_parts(
            middleware(control_url)
                .handle_completion(&service, systemone_input())
                .await,
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(
            body,
            json!({ "error": {
            "type": "invalid_request_error", "message": "question q1 is invalid",
            "code": code, "param": null
        } })
        );
    }
}

#[tokio::test]
async fn systemone_preserves_sanitized_inband_errors() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "acme:model-a",
                "format": "openai",
                "supportedEndpoints": ["/v1/systemone"]
            }]
        }),
    )
    .await;
    let upstream = json!({
        "model": "internal-model",
        "error": {
            "type": "invalid_request_error",
            "message": "question q1 is invalid",
            "metadata": { "provider_name": "secret-provider" }
        },
        "trace": "secret-trace"
    });
    let service = build_service_with_upstream(200, serde_json::to_vec(&upstream).unwrap());
    let (status, _, body) = response_parts(
        middleware(control_url)
            .handle_completion(&service, systemone_input())
            .await,
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(
        body["error"],
        json!({
            "type": "invalid_request_error",
            "message": "question q1 is invalid",
            "param": null,
            "code": 400
        })
    );
    assert!(body.get("trace").is_none());
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
        "data: {\"id\":\"chatcmpl-upstream\",\"created\":1700000000,\"model\":\"internal-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_ns\",\"type\":\"function\",\"function\":{\"name\":\"mcp__example___lookup\",\"arguments\":\"{\\\"key\\\":\\\"value\\\"}\"}},{\"index\":1,\"id\":\"call_custom\",\"type\":\"function\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"input\\\":\\\"pwd\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    );
    let (service, requests) =
        build_recording_service(200, upstream.as_bytes().to_vec(), "text/event-stream");
    let mut input = responses_input(true);
    input.params["input"] = json!([
        {
            "type": "additional_tools",
            "tools": [
                {
                    "type": "namespace",
                    "name": "mcp__example",
                    "description": "Optional plugin tools",
                    "tools": [{ "type": "function", "name": "_lookup", "parameters": { "type": "object" } }]
                },
                { "type": "custom", "name": "shell", "description": "Run shell input" }
            ]
        },
        { "type": "message", "role": "user", "content": "hello" }
    ]);
    input.params["tools"] = json!([
        {
            "type": "function",
            "name": "exec_command",
            "description": "Run a command",
            "parameters": {
                "type": "object",
                "properties": { "cmd": { "type": "string" } },
                "required": ["cmd"]
            }
        },
        { "type": "web_search", "external_web_access": false }
    ]);
    input.params["tool_choice"] = json!("auto");
    input.received_body = serde_json::to_vec(&input.params).unwrap().into();
    let response = middleware(control_url)
        .handle_completion(&service, input)
        .await;
    assert_eq!(response.status(), 200);
    let (headers, body) = raw_body(response).await;
    assert!(headers.get("x-receipt-id").is_some());
    assert!(!body.contains("[DONE]"), "{body}");
    assert!(!body.contains("chatcmpl-upstream"), "{body}");
    assert!(!body.contains("internal-model"), "{body}");

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
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.custom_tool_call_input.delta",
            "response.custom_tool_call_input.done",
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
    assert_eq!(
        terminal["response"]["output"][0],
        json!({
            "id": "fc_call_ns",
            "type": "function_call",
            "call_id": "call_ns",
            "name": "_lookup",
            "namespace": "mcp__example",
            "arguments": "{\"key\":\"value\"}",
            "status": "completed"
        })
    );
    assert_eq!(
        terminal["response"]["output"][1],
        json!({
            "id": "ctc_call_custom",
            "type": "custom_tool_call",
            "call_id": "call_custom",
            "name": "shell",
            "input": "pwd",
            "status": "completed"
        })
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path.as_deref(), Some("/v1/chat/completions"));
    assert!(requests[0].body.get("messages").is_some());
    assert!(requests[0].body.get("input").is_none());
    assert_eq!(
        requests[0].body["tools"],
        json!([
            {
                "type": "function",
                "function": {
                    "name": "exec_command",
                    "description": "Run a command",
                    "parameters": {
                        "type": "object",
                        "properties": { "cmd": { "type": "string" } },
                        "required": ["cmd"]
                    }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "mcp__example___lookup",
                    "description": "Optional plugin tools",
                    "parameters": { "type": "object" }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "shell",
                    "description": "Run shell input\n\nThis tool takes freeform text. Put the complete raw text in the \"input\" argument.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string",
                                "description": "The raw input for this tool, passed through verbatim."
                            }
                        },
                        "required": ["input"],
                        "additionalProperties": false
                    }
                }
            }
        ])
    );
    assert_eq!(requests[0].body["tool_choice"], json!("auto"));
}

#[tokio::test]
async fn supported_responses_endpoint_keeps_protocol_and_path() {
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [{
                "routeId": "openai:gpt-test",
                "format": "openai",
                "supportedEndpoints": ["/v1/responses"]
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
    let mw = middleware(control_url);
    for outcome in ["completed", "failed"] {
        let mut upstream: Value = serde_json::from_slice(upstream).unwrap();
        upstream["status"] = json!(outcome);
        if outcome == "failed" {
            upstream["error"] = json!({ "code": "server_error", "message": "Generation failed" });
        }
        let (service, requests) = build_recording_service(
            200,
            serde_json::to_vec(&upstream).unwrap(),
            "application/json",
        );
        let mut input = responses_input(false);
        input.request_id = format!("req-{outcome}");
        input.params["reasoning"] = json!({ "effort": "future-native-value" });
        input.received_body = serde_json::to_vec(&input.params).unwrap().into();
        let (status, _, body) = response_parts(mw.handle_completion(&service, input).await).await;
        assert_eq!(status, 200);
        assert_eq!(body["id"], format!("req-{outcome}"));
        assert_eq!(body["object"], json!("response"));
        assert_eq!(body["status"], outcome);
        let report = wait_for_post(&posts, |report| {
            report["requestId"] == format!("req-{outcome}")
        })
        .await;
        assert_eq!(
            report["status"],
            if outcome == "failed" { 502 } else { 200 }
        );
        assert_eq!(report["usage"]["input_tokens"], 1);

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path.as_deref(), Some("/v1/responses"));
        assert_eq!(requests[0].body["input"], json!("hello"));
        assert_eq!(
            requests[0].body["reasoning"]["effort"],
            json!("future-native-value")
        );
        assert!(requests[0].body.get("messages").is_none());
    }
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
            "organizationId": 11,
            "workspaceId": 13,
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
    assert_eq!(report["organizationId"], json!(11));
    assert_eq!(report["workspaceId"], json!(13));
    assert_eq!(report["virtualKeyId"], json!(3));
    assert!(report["selectedRouteId"].is_null());
    assert!(report["usage"].is_null());
}

#[tokio::test]
async fn empty_candidates_is_reported_as_a_control_failure() {
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({
            "allow": true,
            "candidates": [],
            "userId": 7,
            "organizationId": 11,
            "workspaceId": 13,
            "virtualKeyId": 3
        }),
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
            "userId": 7,
            "organizationId": 11,
            "workspaceId": 13,
            "virtualKeyId": 3
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
    assert_eq!(report["organizationId"], json!(11));
    assert_eq!(report["workspaceId"], json!(13));
    assert_eq!(report["isStreaming"], json!(false));
}

#[tokio::test]
async fn responses_tool_arguments_are_delivered_or_marked_incomplete() {
    // A malformed call from a model that finished reaches the client as the
    // model wrote it; only one cut off mid-way is incomplete.
    for (arguments, finish_reason, expected_status, expected_input) in [
        (
            r#"{"input":"pwd"}"#,
            Some("tool_calls"),
            "completed",
            Some("pwd"),
        ),
        (r#"{"input":""}"#, Some("tool_calls"), "completed", Some("")),
        ("{}", Some("tool_calls"), "completed", Some("{}")),
        (
            r#"{"input":42}"#,
            Some("tool_calls"),
            "completed",
            Some(r#"{"input":42}"#),
        ),
        (
            r#"{"input":"pwd"#,
            Some("tool_calls"),
            "completed",
            Some(r#"{"input":"pwd"#),
        ),
        (r#"{"input":"pwd"#, Some("length"), "incomplete", None),
        (
            r#"{"input":"pwd"#,
            None,
            "completed",
            Some(r#"{"input":"pwd"#),
        ),
    ] {
        for streaming in [false, true] {
            let (control_url, posts) = spawn_control_capturing(
                200,
                json!({
                    "allow": true,
                    "candidates": [{ "routeId": "compatible:gpt-test", "format": "openai" }],
                    "pricing": { "inputCostPerToken": "1", "outputCostPerToken": "2" },
                    "userId": 7,
                    "organizationId": 11,
                    "workspaceId": 13
                }),
            )
            .await;
            let usage = json!({ "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 });
            let call = json!({
                "index": 0, "id": "call_shell", "type": "function",
                "function": { "name": "shell", "arguments": arguments }
            });
            let payload = json!({
                "id": "upstream", "model": "internal-model",
                "choices": [{
                    "index": 0,
                    "message": { "tool_calls": [call.clone()] },
                    "delta": { "tool_calls": [call] },
                    "finish_reason": finish_reason
                }],
                "usage": usage
            });
            let (wire, content_type) = if streaming {
                (
                    format!("data: {payload}\n\ndata: [DONE]\n\n"),
                    "text/event-stream",
                )
            } else {
                (payload.to_string(), "application/json")
            };
            let (service, _) = build_recording_service(200, wire.into_bytes(), content_type);
            let mut input = responses_input(streaming);
            input.params["tools"] = json!([{ "type": "custom", "name": "shell" }]);
            input.received_body = serde_json::to_vec(&input.params).unwrap().into();
            let response = middleware(control_url)
                .handle_completion(&service, input)
                .await;
            assert_eq!(response.status(), 200);
            let (headers, wire) = raw_body(response).await;
            assert!(headers.get("x-receipt-id").is_some());
            let body = if streaming {
                let events = sse_events(&wire);
                if expected_input.is_none() {
                    assert!(
                        !events
                            .iter()
                            .any(|event| event["type"] == "response.custom_tool_call_input.done"),
                        "{wire}"
                    );
                }
                events.last().unwrap()["response"].clone()
            } else {
                serde_json::from_str::<Value>(&wire).unwrap()
            };
            assert_eq!(body["status"], expected_status, "{wire}");
            assert_eq!(body["usage"]["cost"], 5);
            match expected_input {
                Some(value) => assert_eq!(body["output"][0]["input"], value),
                // An unexecutable call is reported incomplete, with no input to run.
                None => assert_eq!(
                    body["output"],
                    json!([{
                        "id": "ctc_call_shell", "type": "custom_tool_call", "call_id": "call_shell",
                        "name": "shell", "input": "", "status": "incomplete"
                    }]),
                    "{wire}"
                ),
            }
            let report = wait_for_post(&posts, |_| true).await;
            assert_eq!(
                report["status"],
                if expected_status == "failed" {
                    502
                } else {
                    200
                }
            );
            assert_eq!(report["selectedRouteId"], "compatible:gpt-test");
            assert!(report["errorSource"].is_null());
            assert_eq!(report["isStreaming"], streaming);
            assert!(report["usage"].get("cost").is_none());
            let input_tokens = report["usage"]
                .get("prompt_tokens")
                .or_else(|| report["usage"].get("input_tokens"));
            assert_eq!(input_tokens, Some(&json!(1)));
            if expected_status == "failed" {
                let class = if streaming {
                    "stream_inband_error"
                } else {
                    "upstream_response_failed"
                };
                assert_eq!(report["errorMessage"], json!(class));
            }
        }
    }
}

// The Responses bridge reshapes usage for the client. The usage report must
// not follow it: Responses usage has no field for cache-creation tokens, so a
// report read off the client stream would bill them at the input rate. Both
// serving modes report the chat usage the upstream sent, and price the cost
// shown to the client from that same usage.
#[tokio::test]
async fn bridged_responses_report_and_price_the_upstream_usage_in_both_serving_modes() {
    let anthropic_usage = json!({
        "input_tokens": 50, "output_tokens": 5,
        "cache_read_input_tokens": 30, "cache_creation_input_tokens": 20
    });
    let anthropic_stream = [
        json!({ "type": "message_start", "message": {
            "id": "m", "model": "claude", "role": "assistant", "content": [],
            "usage": anthropic_usage
        }}),
        json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "hi" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 5 } }),
        json!({ "type": "message_stop" }),
    ]
    .iter()
    .map(|event| {
        format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        )
    })
    .collect::<String>();
    let anthropic_message = json!({
        "id": "m", "type": "message", "role": "assistant", "model": "claude",
        "content": [{ "type": "text", "text": "hi" }],
        "stop_reason": "end_turn", "usage": anthropic_usage
    });
    let chat_completion = json!({
        "id": "u", "model": "m",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "hi" },
            "delta": { "content": "hi" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });

    // Distinct rates per bucket: a token priced from the wrong bucket, or a
    // cache-creation token priced as input, changes the cost.
    let pricing = json!({
        "inputCostPerToken": "1", "outputCostPerToken": "2",
        "cacheReadCostPerToken": "0.1", "cacheCreationCostPerToken": "1.25"
    });
    for (format, route, buffered, streamed, expected, cost) in [
        (
            "anthropic",
            "anthropic:claude",
            anthropic_message.to_string(),
            anthropic_stream,
            json!({
                "prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105,
                "cache_read_input_tokens": 30, "cache_creation_input_tokens": 20
            }),
            // 50 input + 30 cache read * 0.1 + 20 cache creation * 1.25 + 5 output * 2
            88,
        ),
        (
            "openai",
            "compatible:gpt-test",
            chat_completion.to_string(),
            format!("data: {chat_completion}\n\ndata: [DONE]\n\n"),
            chat_completion["usage"].clone(),
            // 60 input + 40 cache read * 0.1 + 5 output * 2
            74,
        ),
    ] {
        for (streaming, wire, content_type) in [
            (false, buffered.clone(), "application/json"),
            (true, streamed.clone(), "text/event-stream"),
        ] {
            let (control_url, posts) = spawn_control_capturing(
                200,
                json!({
                    "allow": true,
                    "candidates": [{ "routeId": route, "format": format }],
                    "pricing": pricing,
                    "userId": 7, "organizationId": 11, "workspaceId": 13
                }),
            )
            .await;
            let (service, _) = build_recording_service(200, wire.into_bytes(), content_type);
            let response = middleware(control_url)
                .handle_completion(&service, responses_input(streaming))
                .await;
            let (_, body) = raw_body(response).await;
            let client_usage = if streaming {
                sse_events(&body)
                    .into_iter()
                    .find(|event| event["type"] == "response.completed")
                    .map(|event| event["response"]["usage"].clone())
                    .expect("response.completed")
            } else {
                serde_json::from_str::<Value>(&body).unwrap()["usage"].clone()
            };
            let context = format!("{format} upstream, streaming={streaming}");
            assert_eq!(client_usage["input_tokens"], 100, "{context}");
            assert_eq!(client_usage["cost"], cost, "{context}");

            let report = wait_for_post(&posts, |r| r["selectedRouteId"] == json!(route)).await;
            assert_eq!(report["usage"], expected, "{context}");
        }
    }
}

fn messages_input(stream: bool) -> CompletionInput {
    let params = json!({
        "model": "gpt-test",
        "stream": stream,
        "max_tokens": 16,
        "messages": [{ "role": "user", "content": "hi" }]
    });
    CompletionInput {
        audio_form: None,
        upload_permit: None,
        endpoint: Endpoint::Messages,
        endpoint_path: "/v1/messages",
        surface: Surface::Anthropic,
        received_body: serde_json::to_vec(&params).unwrap().into(),
        params,
        request_id: "req-messages".to_string(),
        stream,
        ..chat_input()
    }
}

// `/v1/messages` must report the same billable buckets as `/v1/chat/completions`
// does for the same upstream usage, whichever format the upstream speaks.
#[tokio::test]
async fn messages_report_and_price_every_usage_bucket_in_both_serving_modes() {
    let chat = |usage: Value| {
        json!({
            "id": "u", "model": "m",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "hi" },
                "delta": { "content": "hi" },
                "finish_reason": "stop"
            }],
            "usage": usage
        })
    };
    let anthropic_usage = json!({
        "input_tokens": 500, "output_tokens": 200,
        "cache_read_input_tokens": 400, "cache_creation_input_tokens": 100
    });
    // The counters arrive on message_start; message_delta states output only.
    let anthropic_stream = [
        json!({ "type": "message_start", "message": {
            "id": "m", "model": "claude", "role": "assistant", "content": [],
            "usage": {
                "input_tokens": 500, "output_tokens": 1,
                "cache_read_input_tokens": 400, "cache_creation_input_tokens": 100
            }
        }}),
        json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "hi" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 200 } }),
        json!({ "type": "message_stop" }),
    ]
    .iter()
    .map(|event| {
        format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        )
    })
    .collect::<String>();

    let pricing = json!({
        "inputCostPerToken": "1", "outputCostPerToken": "2",
        "cacheReadCostPerToken": "0.1", "cacheCreationCostPerToken": "1.25"
    });
    let both_buckets = chat(json!({
        "prompt_tokens": 1000, "completion_tokens": 200, "total_tokens": 1200,
        "prompt_tokens_details": { "cached_tokens": 400 },
        "cache_creation_input_tokens": 100
    }));
    let cached_only = chat(json!({
        "prompt_tokens": 1000, "completion_tokens": 200, "total_tokens": 1200,
        "prompt_tokens_details": { "cached_tokens": 400 }
    }));
    let uncached = chat(json!({
        "prompt_tokens": 1000, "completion_tokens": 200, "total_tokens": 1200
    }));
    let sse = |body: &Value| format!("data: {body}\n\ndata: [DONE]\n\n");
    // A delta that repeats the input counters as zeros must not erase them.
    let zero_filled_delta = anthropic_stream.replace(
        r#""usage":{"output_tokens":200}"#,
        r#""usage":{"input_tokens":0,"output_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":null}"#,
    );
    assert_ne!(zero_filled_delta, anthropic_stream);
    for (format, streaming, wire, expected, cost) in [
        // 500 input + 400 cache read * 0.1 + 100 cache creation * 1.25 + 200 output * 2
        (
            "anthropic",
            true,
            anthropic_stream,
            anthropic_usage.clone(),
            1065,
        ),
        (
            "anthropic",
            true,
            zero_filled_delta,
            anthropic_usage.clone(),
            1065,
        ),
        (
            "openai",
            false,
            both_buckets.to_string(),
            anthropic_usage.clone(),
            1065,
        ),
        ("openai", true, sse(&both_buckets), anthropic_usage, 1065),
        // 600 input + 400 cache read * 0.1 + 200 output * 2
        (
            "openai",
            false,
            cached_only.to_string(),
            json!({ "input_tokens": 600, "output_tokens": 200, "cache_read_input_tokens": 400 }),
            1040,
        ),
        (
            "openai",
            true,
            sse(&cached_only),
            json!({ "input_tokens": 600, "output_tokens": 200, "cache_read_input_tokens": 400 }),
            1040,
        ),
        (
            "openai",
            false,
            uncached.to_string(),
            json!({ "input_tokens": 1000, "output_tokens": 200 }),
            1400,
        ),
        (
            "openai",
            true,
            sse(&uncached),
            json!({ "input_tokens": 1000, "output_tokens": 200 }),
            1400,
        ),
    ] {
        let route = format!("{format}:gpt-test");
        let (control_url, posts) = spawn_control_capturing(
            200,
            json!({
                "allow": true,
                "candidates": [{ "routeId": route, "format": format }],
                "pricing": pricing,
                "userId": 7, "organizationId": 11, "workspaceId": 13
            }),
        )
        .await;
        let content_type = if streaming {
            "text/event-stream"
        } else {
            "application/json"
        };
        let (service, _) = build_recording_service(200, wire.into_bytes(), content_type);
        let response = middleware(control_url)
            .handle_completion(&service, messages_input(streaming))
            .await;
        let (_, body) = raw_body(response).await;
        let shown = if streaming {
            sse_events(&body)
                .into_iter()
                .find(|event| event["type"] == "message_delta")
                .map(|event| event["usage"]["cost"].clone())
                .expect("message_delta")
        } else {
            serde_json::from_str::<Value>(&body).unwrap()["usage"]["cost"].clone()
        };
        let context = format!("{format} upstream, streaming={streaming}, usage={expected}");
        assert_eq!(shown, cost, "{context}");

        let report = wait_for_post(&posts, |r| r["selectedRouteId"] == json!(route)).await;
        assert_eq!(report["usage"], expected, "{context}");
    }
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
        tenant: TenantIdentity {
            user_id: Some(9),
            organization: Some(OrganizationScope {
                organization_id: 11,
                workspace_id: 13,
            }),
        },
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        session_kind: None,
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
    assert_eq!(report["organizationId"], json!(11));
    assert_eq!(report["workspaceId"], json!(13));
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

// Anthropic thinking reaches a Chat route in that route's reasoning dialect,
// and the client sees the reasoning as thinking only when it asked to.
#[tokio::test]
async fn messages_thinking_is_bridged_both_ways() {
    let chunk = json!({
        "id": "u", "model": "m",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "reasoning_content": "plan", "content": "hi" },
            "delta": { "reasoning_content": "plan", "content": "hi" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 2 }
    });
    for streaming in [false, true] {
        for thinking in [
            json!({ "type": "enabled", "budget_tokens": 2048 }),
            Value::Null,
        ] {
            let control_url = spawn_control(
                200,
                json!({
                    "allow": true,
                    "candidates": [{
                        "routeId": "vllm:m", "format": "openai", "reasoningFormat": "reasoning_effort"
                    }]
                }),
            )
            .await;
            let (wire, content_type) = if streaming {
                (
                    format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                    "text/event-stream",
                )
            } else {
                (chunk.to_string(), "application/json")
            };
            let (service, requests) = build_recording_service(200, wire.into_bytes(), content_type);
            let mut input = messages_input(streaming);
            if !thinking.is_null() {
                input.params["thinking"] = thinking.clone();
            }
            input.received_body = serde_json::to_vec(&input.params).unwrap().into();
            let response = middleware(control_url)
                .handle_completion(&service, input)
                .await;
            assert_eq!(response.status(), 200);
            let (_, body) = raw_body(response).await;

            let upstream = requests.lock().unwrap()[0].body.clone();
            assert!(upstream.get("thinking").is_none(), "{upstream}");
            let thinking_shown = if streaming {
                sse_events(&body)
                    .iter()
                    .any(|event| event["delta"]["thinking"] == "plan")
            } else {
                serde_json::from_str::<Value>(&body).unwrap()["content"][0]
                    == json!({ "type": "thinking", "thinking": "plan", "signature": "" })
            };
            if thinking.is_null() {
                assert!(upstream.get("reasoning_effort").is_none(), "{upstream}");
                assert!(!thinking_shown, "{body}");
            } else {
                assert_eq!(upstream["reasoning_effort"], "medium", "{upstream}");
                assert!(thinking_shown, "{body}");
            }
            assert!(body.contains("hi"), "{body}");
        }
    }
}

// A malformed success after a failover is reported on the route and attempt
// that produced it, after the attempt that failed over.
#[tokio::test]
async fn malformed_chat_success_after_failover_keeps_attempts() {
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [
            { "routeId": "a:gpt-test", "format": "openai" },
            { "routeId": "b:gpt-test", "format": "openai" }
        ] }),
    )
    .await;
    // Route a is out of capacity; route b answers 200 with no message.
    let (service, forwarded, _) = build_sequenced_service(vec![503, 200]);
    let (status, _, body) = response_parts(
        middleware(control_url)
            .handle_completion(&service, messages_input(false))
            .await,
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "api_error");
    assert_eq!(*forwarded.lock().unwrap(), ["a:gpt-test", "b:gpt-test"]);

    let failover = wait_for_post(&posts, |r| r["attemptIndex"].as_i64() == Some(0)).await;
    assert_eq!(failover["selectedRouteId"], "a:gpt-test");
    assert_eq!(failover["status"], 503);
    let malformed = wait_for_post(&posts, |r| r["attemptIndex"].as_i64() == Some(1)).await;
    assert_eq!(malformed["selectedRouteId"], "b:gpt-test");
    assert_eq!(malformed["status"], 502);
    assert_eq!(malformed["errorSource"], "upstream");
    assert_eq!(malformed["errorMessage"], "upstream_malformed_response");
}

// A converted stream that ends in an in-band error still reports what the
// upstream spent, on the route that spent it.
#[tokio::test]
async fn messages_bridge_stream_failure_keeps_route_and_usage() {
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let chunk = json!({
        "id": "u", "model": "m",
        "choices": [{ "index": 0, "delta": { "content": "partial" } }],
        "usage": { "prompt_tokens": 3, "completion_tokens": 4 }
    });
    let error = json!({ "error": { "message": "engine failed" } });
    let (service, _) = build_recording_service(
        200,
        format!("data: {chunk}\n\ndata: {error}\n\ndata: [DONE]\n\n").into_bytes(),
        "text/event-stream",
    );
    let response = middleware(control_url)
        .handle_completion(&service, messages_input(true))
        .await;
    assert_eq!(response.status(), 200);
    let (_, body) = raw_body(response).await;
    assert_eq!(sse_events(&body).last().unwrap()["type"], "error", "{body}");

    let report = wait_for_post(&posts, |_| true).await;
    assert_eq!(report["selectedRouteId"], "openai:gpt");
    assert_eq!(report["usage"]["input_tokens"], 3, "{report}");
    assert_eq!(report["usage"]["output_tokens"], 4, "{report}");
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
async fn oversized_streaming_upstream_errors_return_502_and_fail_over() {
    for advertised_length in [false, true] {
        for failover in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let upstream_calls = calls.clone();
            let app = Router::new().route(
                "/v1/chat/completions",
                post(move || {
                    let calls = upstream_calls.clone();
                    async move {
                        if calls.fetch_add(1, Ordering::SeqCst) > 0 {
                            return axum::http::Response::builder()
                                .header("content-type", "text/event-stream")
                                .body(axum::body::Body::from("data: {\"id\":\"ok\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
                                .unwrap();
                        }
                        let mut response = axum::http::Response::builder()
                            .status(axum::http::StatusCode::BAD_REQUEST);
                        let body = if advertised_length {
                            response = response.header("content-length", MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES + 1);
                            axum::body::Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>())
                        } else {
                            let chunk = Bytes::from(vec![b'x'; 1024 * 1024]);
                            let chunks = (0..=MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES / chunk.len())
                                .map(move |_| Ok::<_, std::io::Error>(chunk.clone()));
                            axum::body::Body::from_stream(futures_util::stream::iter(chunks))
                        };
                        response.body(body).unwrap()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let backend = OpenAICompatibleBackend::new(format!("http://{address}")).unwrap();
            let service = Arc::new(
                AciService::new(
                    Arc::new(StaticKeyProvider::default()),
                    Arc::new(StubQuoter::default()),
                    Arc::new(backend),
                    Arc::new(InMemoryReceiptStore::default()),
                    AciServiceConfig::for_test(),
                    Arc::new(FixedClock(1_700_000_000)),
                )
                .unwrap(),
            );
            let mut candidates = vec![json!({ "routeId": "openai:oversized", "format": "openai" })];
            if failover {
                candidates.push(json!({ "routeId": "openai:healthy", "format": "openai" }));
            }
            let (control_url, posts) =
                spawn_control_capturing(200, json!({ "allow": true, "candidates": candidates }))
                    .await;
            let mw = middleware(control_url);
            let mut input = chat_input();
            input.stream = true;
            let response = tokio::time::timeout(Duration::from_secs(2), async {
                response_parts(mw.handle_completion(&service, input).await).await
            })
            .await;
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());

            let (status, _, _) =
                response.expect("reject or fail over without waiting for an oversized body");
            assert_eq!(
                status,
                if failover { 200 } else { 502 },
                "advertised_length={advertised_length} failover={failover}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), if failover { 2 } else { 1 });
            let rejected = wait_for_post(&posts, |report| {
                report["selectedRouteId"] == json!("openai:oversized")
            })
            .await;
            assert_eq!(rejected["status"], json!(502));
            assert_eq!(rejected["attemptIndex"], json!(0));
            assert_eq!(rejected["isStreaming"], json!(true));
            if failover {
                let accepted = wait_for_post(&posts, |report| {
                    report["selectedRouteId"] == json!("openai:healthy")
                })
                .await;
                assert_eq!(accepted["status"], json!(200));
                assert_eq!(accepted["attemptIndex"], json!(1));
            }
        }
    }
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
    // The record names the failure's class; the upstream's body stays out of it.
    assert_eq!(report["errorMessage"], json!("upstream_http_error"));
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
    input.received_body = serde_json::to_vec(&input.params).unwrap().into();

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
        audio_form: None,
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
async fn a_request_the_route_cannot_carry_is_a_400_and_no_provider_failure() {
    // The caller's own fault: answered 400 when no candidate can carry it,
    // served by a later candidate that can, and never reported against the
    // route that refused it.
    for stream in [false, true] {
        for (candidates, expected) in [
            (vec!["invalid-a:gpt-test"], 400),
            (vec!["invalid-a:gpt-test", "plain:gpt-test"], 200),
        ] {
            let candidates: Vec<Value> = candidates
                .iter()
                .map(|id| json!({ "routeId": id, "format": "openai" }))
                .collect();
            let (control_url, posts) =
                spawn_control_capturing(200, json!({ "allow": true, "candidates": candidates }))
                    .await;
            let (service, forwarded) = build_tee_aware_service();
            let mut input = chat_input();
            input.stream = stream;
            let (status, _, body) = response_parts(
                middleware(control_url)
                    .handle_completion(&service, input)
                    .await,
            )
            .await;
            assert_eq!(status, expected, "stream={stream}");
            if expected == 200 {
                wait_for_post(&posts, |r| r["selectedRouteId"] == json!("plain:gpt-test")).await;
            } else {
                assert_eq!(body["error"]["type"], json!("invalid_request_error"));
                assert!(forwarded.lock().unwrap().is_empty());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            let posts = posts.lock().unwrap();
            // A client 4xx is not recorded at all; a served request reports
            // only the route that served it.
            assert_eq!(
                posts.len(),
                usize::from(expected == 200),
                "stream={stream}: {posts:?}"
            );
            assert!(posts
                .iter()
                .all(|r| r["selectedRouteId"] != json!("invalid-a:gpt-test")));
        }
    }
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

        // The committed failure must be reported last: a request's user-facing
        // status is the one at the highest attempt index, so a committed response
        // sitting behind a later attempt would be misread.
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
        tenant: TenantIdentity::default(),
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        session_kind: None,
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
    assert!(report.get("userId").is_none());
    assert!(report.get("organizationId").is_none());
    assert!(report.get("workspaceId").is_none());
    assert_eq!(
        report["selectedRouteId"],
        json!("openai:gpt"),
        "route still recorded for traceability"
    );
}

// Post-settle: once the meter settled Completed at a clean end-of-stream, a
// later finalizer error (flag set just before the drop) must not emit a
// second, conflicting usage report — the late failure is the response
// wrapper's to report, not the meter's.
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
        tenant: TenantIdentity::default(),
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        session_kind: None,
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
// 200 on the delayed second pass" to be observable per attempt. With
// `withhold`, its verified paths answer a non-200 as the 0G and Chutes
// adapters do: the status alone, the body withheld.
struct SequencedUpstream {
    forwarded: Arc<Mutex<Vec<String>>>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    statuses: Mutex<std::collections::VecDeque<u16>>,
    withhold: bool,
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
        self.bodies.lock().unwrap().push(req.request.body.to_vec());
        let status = self
            .statuses
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let body = match status {
            200 if req.request.path.as_deref() == Some("/v1/images/generations") => {
                br#"{"data":[{"b64_json":"ok"}]}"#.to_vec()
            }
            200 => br#"{"id":"ok","choices":[]}"#.to_vec(),
            // The recognized upstream capacity/no-targets signal, under a
            // status OUTSIDE the retryable whitelist — guards the shared
            // classification contract, not the whitelist path.
            501 => br#"{"error":"exhausted all available targets"}"#.to_vec(),
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
        let response = self.forward_prepared(req).await?;
        match response.status_code {
            status if self.withhold && status != 200 => Err(UpstreamError::Upstream { status }),
            _ => Ok(response),
        }
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
        let response = self.forward_stream_prepared(req).await?;
        match response.status_code {
            status if self.withhold && status != 200 => Err(UpstreamError::Upstream { status }),
            _ => Ok(response),
        }
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
    build_sequenced_service_with(statuses, false)
}

#[allow(clippy::type_complexity)]
fn build_sequenced_service_with(
    statuses: Vec<u16>,
    withhold: bool,
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
                withhold,
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
        audio_form: None,
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

/// An adapter that withholds an unverified body (0G, Chutes) answers a
/// non-200 as `UpstreamError::Upstream`. That answer counts exactly as the same
/// status with an empty body: recorded with its status, failed over, retried on
/// capacity, and committed as the upstream's status, never as a 502.
#[tokio::test(start_paused = true)]
async fn a_withheld_upstream_status_counts_as_that_status() {
    for stream in [false, true] {
        for (script, routes, committed, recorded, contacts) in [
            // The only candidate is out of capacity on both passes.
            (
                vec![429, 429],
                vec!["zg"],
                ("zg", 429),
                vec![("zg", 429)],
                2,
            ),
            // A sibling serves after the 429.
            (
                vec![429, 200],
                vec!["zg", "plain"],
                ("plain", 200),
                vec![("zg", 429)],
                2,
            ),
            // Not a capacity signal, so no second pass; it commits as is.
            (vec![503], vec!["zg"], ("zg", 503), vec![], 1),
        ] {
            let (service, forwarded, _) = build_sequenced_service_with(script.clone(), true);
            let result = service
                .forward_chat_completion_for_middleware(
                    capacity_retry_request(None),
                    routes.iter().map(|route| plain_candidate(route)).collect(),
                    stream,
                    MiddlewareReceiptJournal::default(),
                )
                .await
                .unwrap();
            let (route, status, body, failed) = match result {
                MiddlewareForwardResult::Forwarded(f) => (
                    f.selected_route,
                    f.upstream_status,
                    f.upstream_body,
                    f.failed_attempts,
                ),
                MiddlewareForwardResult::Stream(s) => (
                    s.selected_route,
                    s.upstream_status,
                    Vec::new(),
                    s.failed_attempts,
                ),
                MiddlewareForwardResult::UpstreamError(e) => (
                    e.selected_route,
                    e.error.upstream_status,
                    e.error.upstream_body,
                    e.failed_attempts,
                ),
                MiddlewareForwardResult::AllFailed(_) => {
                    panic!("stream={stream} {script:?}: a withheld status is an answer")
                }
            };
            let case = format!("stream={stream} {script:?}");
            assert_eq!((route.as_str(), status), committed, "{case}");
            assert_eq!(
                attempts(&failed),
                recorded
                    .iter()
                    .map(|(route, status)| (route.to_string(), *status))
                    .collect::<Vec<_>>(),
                "{case}"
            );
            assert_eq!(forwarded.lock().unwrap().len(), contacts, "{case}");
            if status != 200 {
                assert!(body.is_empty(), "{case}: the withheld body stays withheld");
            }
        }
    }
}

/// The live symptom: a 0G 429 reached the client as a 5xx. A withheld status
/// reaches the client as the generic path maps it, with no provider text.
#[tokio::test]
async fn a_withheld_upstream_status_reaches_the_client_as_that_status() {
    for stream in [false, true] {
        for (script, client_status, error_type) in [
            (vec![429, 429], 429, "rate_limit_error"),
            (vec![503], 503, "service_unavailable"),
        ] {
            let control_url = spawn_control(
                200,
                json!({ "allow": true, "candidates": [{ "routeId": "zg:gpt-test", "format": "openai" }] }),
            )
            .await;
            let (service, _, _) = build_sequenced_service_with(script.clone(), true);
            let mut input = chat_input();
            input.stream = stream;
            let (status, _, body) = response_parts(
                middleware(control_url)
                    .handle_completion(&service, input)
                    .await,
            )
            .await;
            let case = format!("stream={stream} {script:?}");
            assert_eq!(status, client_status, "{case}");
            assert_eq!(body["error"]["type"], json!(error_type), "{case}: {body}");
            assert_ne!(body["error"]["message"], json!("capacity"), "{case}");
        }
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
    // outside the retryable whitelist (501 here), or a request could be told
    // 429 without ever getting the second chance that 429s get.
    let (service, forwarded, _) = build_sequenced_service(vec![501, 200]);
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
                vec![("plain:gpt-test".to_string(), 501)]
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
        .expect("session key")
        .to_string();
    assert_eq!(hash.len(), 32);
    // No prompt_cache_key: the key is inferred from the opening.
    assert_eq!(features["sessionKind"], json!("inferred"));

    // The post report echoes the key and its kind — billing keys cache
    // affinity on them.
    let report = wait_for_post(&posts, |r| r["selectedRouteId"].is_string()).await;
    assert_eq!(report["prefixHash"].as_str(), Some(hash.as_str()));
    assert_eq!(report["sessionKind"], json!("inferred"));
}

#[tokio::test]
async fn prompt_cache_key_is_explicit_and_outranks_the_opening() {
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

    // Same opening twice: once with a prompt_cache_key, once without. The
    // key names the session, so the two keys differ.
    let mut named = chat_input();
    named.params["prompt_cache_key"] = json!("sess-42");
    assert_eq!(
        mw.handle_completion(&service, named)
            .await
            .status()
            .as_u16(),
        200
    );
    assert_eq!(
        mw.handle_completion(&service, chat_input())
            .await
            .status()
            .as_u16(),
        200
    );

    let pres = pres.lock().unwrap().clone();
    assert_eq!(pres[0]["request"]["sessionKind"], json!("explicit"));
    assert_eq!(pres[1]["request"]["sessionKind"], json!("inferred"));
    assert_ne!(
        pres[0]["request"]["prefixHash"],
        pres[1]["request"]["prefixHash"]
    );
    let report = wait_for_post(&posts, |r| r["sessionKind"] == json!("explicit")).await;
    assert_eq!(report["prefixHash"], pres[0]["request"]["prefixHash"]);
}

#[tokio::test]
async fn send_request_features_off_restores_the_featureless_pre_body() {
    let (control_url, pres, _posts) = spawn_control_capturing_pre(json!({
        "allow": false, "status": 403, "message": "denied"
    }))
    .await;
    let mw = new_middleware(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
        send_request_features: Some(false),
        tee_only_domains: Vec::new(),
    })
    .unwrap();
    let service = build_service();

    let _ = mw.handle_completion(&service, chat_input()).await;
    let pre = pres.lock().unwrap().first().cloned().unwrap();
    assert_eq!(pre["endpoint"], "/v1/chat/completions");
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
    assert_eq!(report["errorMessage"], json!("client_disconnected"));

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
async fn mid_stream_read_timeout_settles_504_as_upstream_timeout() {
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
        tenant: TenantIdentity::default(),
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        session_kind: None,
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
    assert_eq!(report["errorMessage"], json!("upstream_timeout"));
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
    new_middleware(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: Some(ms),
        send_request_features: None,
        tee_only_domains: Vec::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn early_keepalive_receipt_keeps_committed_keyset_across_renewal() {
    use axum::{body::Body, http::Request};
    use std::sync::atomic::AtomicU64;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    struct PausedStreamUpstream {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl UpstreamBackend for PausedStreamUpstream {
        fn name(&self) -> &str {
            "paused-stream"
        }
        fn url_origin(&self) -> Option<&str> {
            None
        }
        async fn forward(&self, _: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
            unreachable!("test only streams")
        }
        async fn forward_stream_verified_prepared(
            &self,
            _: PreparedUpstreamRequest,
            _: &UpstreamVerifiedEvent,
        ) -> Result<UpstreamStreamResponse, UpstreamError> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(UpstreamStreamResponse {
                status_code: 200,
                headers: [("content-type".to_string(), "text/event-stream".to_string())].into(),
                body: Box::pin(futures_util::stream::once(async {
                    Ok(Bytes::from_static(
                        b"data: {\"id\":\"upstream-chat\",\"model\":\"gpt\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
                    ))
                })),
                served_instance_id: None,
            })
        }
    }

    let (control_url, _) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let clock = Arc::new(admission::TestClock(AtomicU64::new(1_000)));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut config = AciServiceConfig::for_test();
    config.keyset_not_after = 1_100;
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(PausedStreamUpstream {
                entered: entered.clone(),
                release: release.clone(),
            }),
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            config,
            clock.clone(),
        )
        .unwrap(),
    );
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        UpstreamConfigManager::load(directory.path().join("upstreams.json"), runtime_options())
            .unwrap(),
    );
    let router = private_ai_gateway::http::build_router_with_admin_and_middleware(
        service.clone(),
        manager,
        None,
        Arc::new(middleware_with_keepalive(control_url, 10)),
    );
    let original_keyset = service.keyset();
    let original_digest = service.workload_keyset_digest();
    let forward = tokio::spawn(
        router.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt","messages":[],"stream":true}"#))
                .unwrap(),
        ),
    );
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("upstream is awaiting release");
    // The real router commits the early response while upstream headers are
    // still pending. Renew only after that response has returned.
    let response = tokio::time::timeout(Duration::from_secs(5), forward)
        .await
        .expect("early response does not wait for upstream headers")
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-aci-keyset-digest"], original_digest);
    assert!(!response.headers().contains_key("x-receipt-id"));
    clock.set(1_050);
    assert_eq!(service.renew_keyset_if_due(100).unwrap(), Some(1_150));
    assert_ne!(service.workload_keyset_digest(), original_digest);
    // Complete after K0 expires: historical receipt validation depends on
    // served_at, not the time its final byte is delivered.
    clock.set(1_101);
    release.notify_one();
    let (headers, body) = raw_body(response).await;
    assert!(body.starts_with(": PROCESSING"), "{body}");
    assert!(body.contains("data: [DONE]"), "{body}");
    let chat_id = body
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.strip_prefix("data: ")?).ok())
        .find_map(|event| event["id"].as_str().map(str::to_owned))
        .expect("stream includes the gateway chat id");
    let receipt = service.get_receipt_by_chat_id(&chat_id).unwrap();
    let document = receipt.document_json().unwrap();
    assert_eq!(document["served_at"], 1_000);
    assert!(document["served_at"].as_u64().unwrap() < original_keyset.not_after);
    assert_eq!(document["workload_keyset_digest"], original_digest);
    assert_eq!(
        document["workload_keyset_digest"],
        headers["x-aci-keyset-digest"].to_str().unwrap()
    );
    let signing_key = original_keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == receipt.key_id)
        .unwrap();
    assert!(private_ai_gateway::aci::keys::verify_receipt_signature(
        signing_key,
        &private_ai_gateway::aci::receipt::receipt_signing_input(&document).unwrap(),
        &hex::decode(&receipt.signature_hex).unwrap(),
    ));
}

#[tokio::test]
async fn early_keepalive_rejects_expired_initial_admission() {
    for admitted_at in [1_101, 1_100] {
        assert_early_keepalive_rejects_expired_admission(AdmissionDelay::Initial, admitted_at)
            .await;
    }
}

#[tokio::test]
async fn early_keepalive_rejects_expired_failover_admission() {
    assert_early_keepalive_rejects_expired_admission(AdmissionDelay::Failover, 1_101).await;
}

#[tokio::test]
async fn early_keepalive_rejects_expired_binding_readmission() {
    assert_early_keepalive_rejects_expired_admission(AdmissionDelay::BindingReverify, 1_101).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AdmissionDelay {
    Initial,
    Failover,
    BindingReverify,
}

async fn assert_early_keepalive_rejects_expired_admission(delay: AdmissionDelay, admitted_at: u64) {
    use axum::{body::Body, http::Request};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tokio::sync::Notify;
    use tower::ServiceExt;

    struct PausedVerifier {
        calls: AtomicUsize,
        pause_on: usize,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl UpstreamVerifier for PausedVerifier {
        async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
            if self.calls.fetch_add(1, Ordering::SeqCst) == self.pause_on {
                self.entered.notify_one();
                self.release.notified().await;
            }
            event_from_request(&request, VerificationResult::Verified)
        }
    }

    struct CountingUpstream {
        delay: AdmissionDelay,
        forwarded: Arc<Mutex<Vec<String>>>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl UpstreamBackend for CountingUpstream {
        fn name(&self) -> &str {
            "counting-upstream"
        }
        fn url_origin(&self) -> Option<&str> {
            None
        }
        async fn forward(&self, _: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
            unreachable!("test only streams")
        }
        async fn forward_stream_verified_prepared(
            &self,
            request: PreparedUpstreamRequest,
            _: &UpstreamVerifiedEvent,
        ) -> Result<UpstreamStreamResponse, UpstreamError> {
            let first = {
                let mut forwarded = self.forwarded.lock().unwrap();
                forwarded.push(request.request.target_route_id.unwrap());
                forwarded.len() == 1
            };
            if first && self.delay != AdmissionDelay::Initial {
                self.entered.notify_one();
                self.release.notified().await;
                match self.delay {
                    AdmissionDelay::Failover => {
                        return Err(UpstreamError::Transport("try next route".into()));
                    }
                    AdmissionDelay::BindingReverify => {
                        return Err(UpstreamError::ChannelBindingMismatch(
                            "refresh binding".into(),
                        ));
                    }
                    AdmissionDelay::Initial => {}
                }
            }
            Ok(UpstreamStreamResponse {
                status_code: 200,
                headers: [("content-type".to_string(), "text/event-stream".to_string())].into(),
                body: Box::pin(futures_util::stream::once(async {
                    Ok(Bytes::from_static(b"data: {\"id\":\"unexpected-inference\",\"choices\":[]}\n\ndata: [DONE]\n\n"))
                })),
                served_instance_id: None,
            })
        }
    }

    let mut candidates = vec![json!({ "routeId": "openai:first", "format": "openai" })];
    if delay == AdmissionDelay::Failover {
        candidates.push(json!({ "routeId": "openai:second", "format": "openai" }));
    }
    let (control_url, posts) =
        spawn_control_capturing(200, json!({ "allow": true, "candidates": candidates })).await;
    let clock = Arc::new(admission::TestClock(AtomicU64::new(1_000)));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let upstream_entered = Arc::new(Notify::new());
    let upstream_release = Arc::new(Notify::new());
    let forwarded = Arc::new(Mutex::new(Vec::new()));
    let mut config = AciServiceConfig::for_test();
    config.keyset_not_after = 1_100;
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(CountingUpstream {
                delay,
                forwarded: forwarded.clone(),
                entered: upstream_entered.clone(),
                release: upstream_release.clone(),
            }),
            Arc::new(PausedVerifier {
                calls: AtomicUsize::new(0),
                pause_on: usize::from(delay != AdmissionDelay::Initial),
                entered: entered.clone(),
                release: release.clone(),
            }),
            Arc::new(InMemoryReceiptStore::default()),
            config,
            clock.clone(),
        )
        .unwrap(),
    );
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        UpstreamConfigManager::load(directory.path().join("upstreams.json"), runtime_options())
            .unwrap(),
    );
    let router = private_ai_gateway::http::build_router_with_admin_and_middleware(
        service.clone(),
        manager,
        None,
        Arc::new(middleware_with_keepalive(control_url, 10)),
    );
    let digest = service.workload_keyset_digest();
    let forward = tokio::spawn(
        router.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt","messages":[],"stream":true}"#))
                .unwrap(),
        ),
    );
    let first_wait = if delay == AdmissionDelay::Initial {
        &entered
    } else {
        &upstream_entered
    };
    tokio::time::timeout(Duration::from_secs(5), first_wait.notified())
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), forward)
        .await
        .expect("early response must commit while forwarding is pending")
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-aci-keyset-digest"], digest);
    assert!(!response.headers().contains_key("x-receipt-id"));
    clock.set(1_050);
    assert_eq!(service.renew_keyset_if_due(100).unwrap(), Some(1_150));
    let complete = tokio::spawn(raw_body(response));
    if delay != AdmissionDelay::Initial {
        // Failover/reverify starts only after headers cite K0. Its new verifier
        // wait then crosses K0's expiry before the next admission decision.
        upstream_release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
    }
    clock.set(admitted_at);
    release.notify_one();
    let (headers, body) = complete.await.unwrap();
    assert_eq!(headers["x-aci-keyset-digest"], digest);
    let expected_calls = usize::from(delay != AdmissionDelay::Initial);
    assert_eq!(
        forwarded.lock().unwrap().len(),
        expected_calls,
        "expired admission must not forward"
    );
    assert!(body.starts_with(": PROCESSING"), "{body}");
    assert!(body.contains("\"code\":503"), "{body}");
    assert!(body.contains("data: [DONE]"), "{body}");
    assert_eq!(
        metric_total(&service, "private_ai_gateway_receipts_issued_total"),
        0
    );
    let report = wait_for_post(&posts, |r| r["errorMessage"] == "keyset_expired").await;
    assert_eq!(report["status"], 503);
    assert_eq!(report["errorSource"], "gateway");
    assert_eq!(report["selectedRouteId"], Value::Null);
    assert_eq!(
        report["attemptIndex"],
        usize::from(delay == AdmissionDelay::Failover)
    );
    if delay == AdmissionDelay::Failover {
        let first = wait_for_post(&posts, |r| r["selectedRouteId"] == "openai:first").await;
        assert_eq!(first["status"], 502);
        assert_eq!(first["attemptIndex"], 0);
    }
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
        tee_only_domains: Vec::new(),
    };
    let report_for = |id: &str, abort: &Arc<std::sync::atomic::AtomicBool>| StreamReport {
        control: ControlClient::new(&config).unwrap(),
        request_id: id.to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        request_model: "gpt".to_string(),
        pricing: None,
        spend_mode: None,
        tenant: TenantIdentity::default(),
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        session_kind: None,
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
async fn buffered_upstream_error_text_reaches_the_caller_and_nothing_else() {
    captured_logs();
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let mw = middleware(control_url);
    let upstream_body = json!({
        "error": { "type": "invalid_request_error", "message": format!("{CANARY} is not valid") }
    });
    let service = build_service_with_upstream(400, upstream_body.to_string().into_bytes());

    let (status, _, body) =
        response_parts(mw.handle_completion(&service, chat_input()).await).await;
    assert_eq!(status, 400);
    assert!(
        body.to_string().contains(CANARY),
        "the caller is told why their request was rejected: {body}"
    );

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(400)).await;
    assert_eq!(report["errorMessage"], json!("upstream_http_error"));
    assert_canary_absent(&posts, "probe-canary-buffered");
}

#[tokio::test]
async fn streaming_upstream_error_text_stays_out_of_reports_and_logs() {
    captured_logs();
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({ "allow": true, "candidates": [{ "routeId": "openai:gpt", "format": "openai" }] }),
    )
    .await;
    let mw = middleware(control_url);
    let upstream_body = json!({ "error": { "message": format!("{CANARY} overloaded") } });
    let service = build_service_with_upstream(503, upstream_body.to_string().into_bytes());
    let mut input = chat_input();
    input.stream = true;

    let (status, _, _) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 503);

    let report = wait_for_post(&posts, |r| r["status"].as_i64() == Some(503)).await;
    assert_eq!(report["errorMessage"], json!("upstream_http_error"));
    assert_canary_absent(&posts, "probe-canary-stream");
}

#[tokio::test]
async fn in_band_stream_error_settles_with_a_class_and_none_of_its_text() {
    captured_logs();
    // A 200 stream failed by an in-band error event settles as
    // `stream_inband_error`; the event's message appears nowhere in the report.
    let (control_url, posts) = spawn_control_capturing(200, json!({})).await;
    let control = ControlClient::new(&MiddlewareConfig {
        control_url,
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: None,
        send_request_features: None,
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
        tenant: TenantIdentity::default(),
        virtual_key_id: None,
        selected_route_id: Some("openai:gpt".to_string()),
        attempt_index: 0,
        upstream_status: 200,
        prefix_hash: None,
        session_kind: None,
        started: std::time::Instant::now(),
        downstream_abort: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        settled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let events: Vec<Result<Bytes, ServiceError>> = vec![Ok(Bytes::from(format!(
        "data: {{\"error\":{{\"message\":\"{CANARY} quoted prompt text\"}}}}\n\ndata: [DONE]\n\n",
    )))];
    let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
    let mut metered = MeterStream::new(inner, report, SseProtocol::OpenaiChat);
    while metered.next().await.is_some() {}
    drop(metered);

    let report = wait_for_post(&posts, |r| r["requestId"] == json!("r-inband")).await;
    assert_eq!(report["status"], json!(502));
    assert_eq!(report["errorMessage"], json!("stream_inband_error"));
    assert_canary_absent(&posts, "probe-canary-inband");
}

struct PartialUsageUpstream {
    requests: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl UpstreamBackend for PartialUsageUpstream {
    fn name(&self) -> &str {
        "partial-usage-fixture"
    }
    fn url_origin(&self) -> Option<&str> {
        None
    }
    async fn forward(&self, _: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        unreachable!()
    }
    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&req.request.body).unwrap());
        let events = [3,7].map(|n| Ok(Bytes::from(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"x\"}}}}],\"usage\":{{\"prompt_tokens\":11,\"completion_tokens\":{n},\"total_tokens\":{}}}}}\n\n",11+n))));
        Ok(UpstreamStreamResponse {
            status_code: 200,
            headers: HashMap::from([("content-type".into(), "text/event-stream".into())]),
            body: Box::pin(
                futures_util::stream::iter(events).chain(futures_util::stream::pending()),
            ),
            served_instance_id: None,
        })
    }
}

#[tokio::test]
async fn deployment_streaming_usage_reports_latest_partial_once_on_disconnect() {
    use private_ai_gateway::aci::upstream::{ModelRoute, ModelRouterBackend, StreamingUsage};
    let (control_url, posts) = spawn_control_capturing(
        200,
        json!({"allow":true,
        "candidates":[{"routeId":"endpoint:gpt-test","format":"openai"}]}),
    )
    .await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut router = ModelRouterBackend::new("router");
    router
        .add_route(
            ModelRoute::new(
                "gpt-test",
                "actual-model",
                Arc::new(PartialUsageUpstream {
                    requests: requests.clone(),
                }),
                "endpoint:gpt-test",
            )
            .unwrap()
            .with_streaming_usage(Some(StreamingUsage::Continuous)),
        )
        .unwrap();
    let service = build_service_with_backend(Arc::new(router));
    let mut input = chat_input();
    input.stream = true;
    input.params["stream"] = json!(true);
    input.received_body = serde_json::to_vec(&input.params).unwrap().into();
    let response = middleware(control_url)
        .handle_completion(&service, input)
        .await;
    assert_eq!(response.status(), 200);
    let mut body = response.into_body().into_data_stream();
    let mut received = String::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !received.contains("\"completion_tokens\":7") {
            received.push_str(std::str::from_utf8(&body.next().await.unwrap().unwrap()).unwrap());
        }
    })
    .await
    .unwrap();
    drop(body);
    let report = wait_for_post(&posts, |r| r["status"] == json!(499)).await;
    assert_eq!(report["usage"]["completion_tokens"], 7);
    assert_eq!(report["usage"]["prompt_tokens"], 11);
    assert_eq!(report["usage"]["total_tokens"], 18);
    assert_eq!(
        requests.lock().unwrap()[0]["stream_options"],
        json!({"include_usage":true,"continuous_usage_stats":true})
    );
    assert_eq!(
        posts
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["status"] == json!(499))
            .count(),
        1
    );
}

fn assert_admitted_receipt(
    receipt: &private_ai_gateway::aci::receipt::SignedReceipt,
    original_id: &str,
) {
    let document = receipt.document_json().unwrap();
    assert_eq!(document["served_at"], 109);
    let upstream = document["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "upstream.verified")
        .unwrap();
    assert_eq!(upstream["session_id"], original_id);
}

#[tokio::test]
async fn delayed_buffered_finalization_and_retained_failover_preserve_admission() {
    use admission::{candidate, request, Attempt, Harness, RESPONSE};
    for retained in [false, true] {
        let h = Harness::new(vec![Attempt::Response {
            status: if retained { 503 } else { 200 },
            at: 120,
        }]);
        let original = h.listed().remove(0);
        h.clock.set(109);
        let mut candidates = vec![candidate("first")];
        if retained {
            candidates.push(candidate("unrouteable"));
        }
        let result = h
            .service
            .forward_chat_completion_for_middleware(
                request(vec![]),
                candidates,
                false,
                MiddlewareReceiptJournal::default(),
            )
            .await
            .unwrap();
        let MiddlewareForwardResult::Forwarded(forwarded) = result else {
            panic!("buffered response expected")
        };
        assert_eq!(forwarded.session_id.as_deref(), Some(original.session_id()));
        h.clock.set(140);
        assert_eq!(
            h.service.get_attested_session(original.session_id()),
            Some(original.clone())
        );
        let finalized = h
            .service
            .finalize_middleware_receipt(
                forwarded.receipt,
                RESPONSE.to_vec(),
                Some("application/json"),
                None,
                None,
            )
            .unwrap();
        assert_admitted_receipt(&finalized.receipt, original.session_id());
        h.clock.set(144);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_some());
        h.clock.set(145);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_none());
    }
}

#[tokio::test]
async fn streaming_journal_handoff_holds_admission_until_delayed_finalization() {
    use admission::{candidate, request, Attempt, Harness};
    for streaming_finalizer in [false, true] {
        let h = Harness::new(vec![Attempt::Response {
            status: 200,
            at: 109,
        }]);
        let original = h.listed().remove(0);
        h.clock.set(109);
        let journal = MiddlewareReceiptJournal::default();
        let result = h
            .service
            .forward_chat_completion_for_middleware(
                request(vec![]),
                vec![candidate("first")],
                true,
                journal.clone(),
            )
            .await
            .unwrap();
        let MiddlewareForwardResult::Stream(mut forwarded) = result else {
            panic!("stream expected")
        };
        h.clock.set(120);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_some());
        let mut bytes = Vec::new();
        while let Some(chunk) = forwarded.body.next().await {
            bytes.extend_from_slice(&chunk.unwrap());
        }
        drop(forwarded.body);
        h.clock.set(140);
        assert!(
            h.service
                .get_attested_session(original.session_id())
                .is_some(),
            "journal now owns the lease"
        );
        if streaming_finalizer {
            let source: ServiceResponseStream =
                Box::pin(futures_util::stream::iter([Ok(Bytes::from(bytes))]));
            let mut finalized = h
                .service
                .finalize_middleware_response_stream(
                    journal,
                    source,
                    "/v1/chat/completions",
                    Some("text/event-stream"),
                    None,
                    None,
                    None,
                )
                .unwrap();
            while let Some(chunk) = finalized.body.next().await {
                chunk.unwrap();
            }
        } else {
            let draft = journal.take().unwrap();
            h.service
                .finalize_middleware_receipt(draft, bytes, Some("text/event-stream"), None, None)
                .unwrap();
        }
        let receipt = h
            .service
            .get_receipt_by_receipt_id(&forwarded.receipt_id)
            .unwrap();
        assert_admitted_receipt(&receipt, original.session_id());
        h.clock.set(144);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_some());
        h.clock.set(145);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_none());
    }
}

#[tokio::test]
async fn abandoning_drafts_streams_and_inflight_attempts_releases_leases() {
    use admission::{candidate, request, Attempt, Harness};
    for streaming in [false, true] {
        let h = Harness::new(vec![Attempt::Response {
            status: 200,
            at: 109,
        }]);
        let original = h.listed().remove(0);
        h.clock.set(109);
        let journal = MiddlewareReceiptJournal::default();
        let result = h
            .service
            .forward_chat_completion_for_middleware(
                request(vec![]),
                vec![candidate("first")],
                streaming,
                journal.clone(),
            )
            .await
            .unwrap();
        h.clock.set(120);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_some());
        drop(result);
        drop(journal);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_none());
    }
    // Cancellation while awaiting the backend drops the attempt's admission.
    let h = Harness::new(vec![Attempt::Pending]);
    let original = h.listed().remove(0);
    h.clock.set(109);
    let mut future = Box::pin(h.service.forward_chat_completion_for_middleware(
        request(vec![]),
        vec![candidate("first")],
        false,
        MiddlewareReceiptJournal::default(),
    ));
    assert!(futures_util::poll!(&mut future).is_pending());
    h.clock.set(120);
    assert!(h
        .service
        .get_attested_session(original.session_id())
        .is_some());
    drop(future);
    assert!(h
        .service
        .get_attested_session(original.session_id())
        .is_none());
}

#[tokio::test]
async fn replacing_a_retained_response_releases_its_session_lease() {
    use admission::{candidate, request, Attempt, Harness, RESPONSE};
    let h = Harness::new(vec![
        Attempt::Response {
            status: 503,
            at: 120,
        },
        Attempt::Response {
            status: 503,
            at: 130,
        },
    ]);
    let original = h.listed().remove(0);
    h.clock.set(109);
    let result = h
        .service
        .forward_chat_completion_for_middleware(
            request(vec![]),
            vec![
                candidate("first"),
                candidate("second"),
                candidate("unrouteable"),
            ],
            false,
            MiddlewareReceiptJournal::default(),
        )
        .await
        .unwrap();
    let MiddlewareForwardResult::Forwarded(forwarded) = result else {
        panic!("buffered response expected")
    };
    let replacement_id = forwarded.session_id.clone().unwrap();
    assert_ne!(replacement_id, original.session_id());
    assert!(
        h.service
            .get_attested_session(original.session_id())
            .is_none(),
        "replaced retained owner released its lease"
    );
    assert!(h.service.get_attested_session(&replacement_id).is_some());
    h.clock.set(140);
    let finalized = h
        .service
        .finalize_middleware_receipt(
            forwarded.receipt,
            RESPONSE.to_vec(),
            Some("application/json"),
            None,
            None,
        )
        .unwrap();
    assert_eq!(finalized.receipt.document_json().unwrap()["served_at"], 120);
    h.clock.set(145);
    assert!(h.service.get_attested_session(&replacement_id).is_none());
}

#[tokio::test]
async fn dropping_the_last_journal_owner_releases_a_completed_pending_draft() {
    use admission::{candidate, request, Attempt, Harness};
    let h = Harness::new(vec![Attempt::Response {
        status: 200,
        at: 109,
    }]);
    let original = h.listed().remove(0);
    h.clock.set(109);
    let journal = MiddlewareReceiptJournal::default();
    let clone = journal.clone();
    let result = h
        .service
        .forward_chat_completion_for_middleware(
            request(vec![]),
            vec![candidate("first")],
            true,
            journal.clone(),
        )
        .await
        .unwrap();
    let MiddlewareForwardResult::Stream(mut forwarded) = result else {
        panic!("stream expected")
    };
    while let Some(chunk) = forwarded.body.next().await {
        chunk.unwrap();
    }
    drop(forwarded);
    h.clock.set(120);
    drop(journal);
    assert!(
        h.service
            .get_attested_session(original.session_id())
            .is_some(),
        "the cloned journal still owns the draft"
    );
    drop(clone);
    assert!(h
        .service
        .get_attested_session(original.session_id())
        .is_none());
}

fn images_input() -> CompletionInput {
    let params = json!({"model":"gpt-test","prompt":"a blue circle","n":1,"size":"512x512","quality":"low","user":"alice","safety_identifier":"hashed-user","style":"vivid"});
    CompletionInput {
        audio_form: None,
        upload_permit: None,
        endpoint: Endpoint::ImagesGenerations,
        endpoint_path: "/v1/images/generations",
        received_body: serde_json::to_vec(&params).unwrap().into(),
        params,
        request_id: "req-images".into(),
        ..chat_input()
    }
}

#[tokio::test]
async fn images_reject_invalid_counts_before_consult() {
    let (url, pres, _) = spawn_control_capturing_pre(json!({"allow":true,"candidates":[{"routeId":"image:model","format":"openai","supportedEndpoints":["/v1/images/generations"]}]})).await;
    let (service, requests) = build_recording_service(200, b"{}".to_vec(), "application/json");
    for n in [json!(0), json!(11), json!(1.5)] {
        let mut input = images_input();
        input.params["n"] = n;
        input.received_body = serde_json::to_vec(&input.params).unwrap().into();
        let (status, _, body) = response_parts(
            middleware(url.clone())
                .handle_completion(&service, input)
                .await,
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["code"], "invalid_parameter");
        assert_eq!(body["error"]["param"], "n");
    }
    assert!(pres.lock().unwrap().is_empty());
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn images_opt_in_consult_schema_cost_and_post_usage() {
    for (requested, returned, with_usage) in
        [(Some(2), 1, false), (Some(2), 2, true), (None, 1, false)]
    {
        let (url, pres, posts) = spawn_control_capturing_pre(json!({"allow":true,"candidates":[
            {"routeId":"plain:model","format":"openai"},
            {"routeId":"image:model","format":"openai","supportedEndpoints":["/v1/images/generations"]}],
            "pricing":{"imageCostPerImage":{"512x512|high":"99","512x512|low":"0.1"},"inputCostPerToken":"0.01","outputCostPerToken":"0.02"}})).await;
        let mut upstream = json!({"created":1,"id":"private","model":"private","data":[{"b64_json":"aW1hZ2U=","url":"https://private","revised_prompt":{"url":"https://private/image","provider":"private"},"unknown":1}],"quality":"high","background":{"url":"https://private"},"provider":"private"});
        upstream["data"] = json!(vec![upstream["data"][0].clone(); returned]);
        if with_usage {
            upstream["usage"] = json!({"input_tokens":2,"output_tokens":3,"trace":"private","input_tokens_details":{"text_tokens":2,"url":"https://private/image"},"output_tokens_details":{"image_tokens":3,"provider":"private"}});
        } else {
            upstream.as_object_mut().unwrap().remove("created");
        }
        let (service, requests) = build_recording_service(
            200,
            serde_json::to_vec(&upstream).unwrap(),
            "application/json",
        );
        let original_digest = service.workload_keyset_digest();
        assert_eq!(
            service.renew_keyset_if_due(600_000_000).unwrap(),
            Some(2_300_000_000)
        );
        assert_ne!(service.workload_keyset_digest(), original_digest);
        let mut input = images_input();
        if let Some(n) = requested {
            input.params["n"] = json!(n);
        } else {
            input.params.as_object_mut().unwrap().remove("n");
        }
        input.received_body = serde_json::to_vec(&input.params).unwrap().into();
        let (status, headers, body) =
            response_parts(middleware(url).handle_completion(&service, input).await).await;
        assert_eq!(status, 200);
        assert_eq!(
            body["data"],
            json!(vec![json!({"b64_json":"aW1hZ2U="}); returned])
        );
        assert_eq!(
            body["usage"]["cost"],
            json!(if with_usage { 0.28 } else { 0.1 })
        );
        assert!(body.get("id").is_none());
        assert!(body.get("model").is_none());
        assert!(body.get("provider").is_none());
        assert!(body["usage"].get("trace").is_none());
        assert!(body.get("background").is_none());
        if with_usage {
            assert_eq!(
                body["usage"],
                json!({"input_tokens":2,"output_tokens":3,"total_tokens":0,"input_tokens_details":{"text_tokens":2,"image_tokens":0},"output_tokens_details":{"text_tokens":0,"image_tokens":3},"cost":0.28})
            );
            assert_eq!(body["created"], 1);
        } else {
            assert_eq!(
                body["usage"],
                json!({"input_tokens":0,"output_tokens":0,"total_tokens":0,"input_tokens_details":{"text_tokens":0,"image_tokens":0},"cost":0.1})
            );
            assert!(body["created"].as_u64().unwrap() > 0);
        }
        assert!(!body.to_string().contains("https://private"));
        let receipt = service
            .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
            .unwrap();
        assert!(receipt.chat_id.is_none());
        assert_eq!(
            receipt.document_json().unwrap()["workload_keyset_digest"],
            headers["x-aci-keyset-digest"].to_str().unwrap()
        );
        assert_eq!(
            headers["x-aci-keyset-digest"],
            service.workload_keyset_digest()
        );
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].path.as_deref(), Some("/v1/images/generations"));
            assert_eq!(requests[0].body["response_format"], "b64_json");
            assert_eq!(
                requests[0].body.get("n"),
                requested.map(|n| json!(n)).as_ref()
            );
            assert!(requests[0].body.get("style").is_none());
            assert!(requests[0].body.get("user").is_none());
            assert!(requests[0].body.get("safety_identifier").is_none());
        }
        assert_eq!(
            pres.lock().unwrap()[0]["endpoint"],
            "/v1/images/generations"
        );
        assert!(pres.lock().unwrap()[0].get("request").is_none());
        let report = wait_for_post(&posts, |post| post["requestId"] == "req-images").await;
        assert_eq!(report["selectedRouteId"], "image:model");
        assert_eq!(
            report["images"],
            json!({"count":returned,"size":"512x512","quality":"low"})
        );
        assert_eq!(
            report["usage"],
            if with_usage {
                upstream["usage"].clone()
            } else {
                Value::Null
            }
        );
    }
}

#[tokio::test]
async fn images_cost_overflow_omits_display_cost_and_preserves_billable_report() {
    let pricing = json!({"imageCostPerImage":"0.1","inputCostPerToken":rust_decimal::Decimal::MAX.to_string()});
    let (url, posts) = spawn_control_capturing(200, json!({"allow":true,"candidates":[{"routeId":"image:model","format":"openai","supportedEndpoints":["/v1/images/generations"]}],"pricing":pricing})).await;
    let upstream_usage = json!({"input_tokens":2,"total_tokens":2,"cost":0});
    let (service, _) = build_recording_service(
        200,
        serde_json::to_vec(&json!({"data":[{"b64_json":"ok"}],"usage":upstream_usage})).unwrap(),
        "application/json",
    );
    let (status, headers, body) = response_parts(
        middleware(url)
            .handle_completion(&service, images_input())
            .await,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body["usage"],
        json!({"input_tokens":2,"output_tokens":0,"total_tokens":2,"input_tokens_details":{"text_tokens":0,"image_tokens":0}})
    );
    let receipt = service
        .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
        .unwrap();
    assert!(receipt.chat_id.is_none());
    let report = wait_for_post(&posts, |post| post["requestId"] == "req-images").await;
    assert_eq!(report["status"], 200);
    assert_eq!(
        report["images"],
        json!({"count":1,"size":"512x512","quality":"low"})
    );
    assert_eq!(report["usage"], upstream_usage);
    assert_eq!(report["pricing"], pricing);
}

#[tokio::test]
async fn images_no_opt_in_candidate_returns_model_not_found() {
    let (url, _) = spawn_control_capturing(
        200,
        json!({"allow":true,"candidates":[{"routeId":"plain:model","format":"openai"}]}),
    )
    .await;
    let (service, requests) = build_recording_service(200, b"{}".to_vec(), "application/json");
    let (status, _, body) = response_parts(
        middleware(url)
            .handle_completion(&service, images_input())
            .await,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["type"], "model_not_found");
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn images_malformed_2xx_is_not_billed_and_never_returns_url() {
    for (requested, upstream) in [
        (Some(1), json!({"data":[{"url":"https://private"}]})),
        (
            Some(2),
            json!({"data":[{"b64_json":"ok"},{"b64_json":"ok"},{"b64_json":"ok"}]}),
        ),
        (None, json!({"data":[{"b64_json":"ok"},{"b64_json":"ok"}]})),
    ] {
        let (url, posts) = spawn_control_capturing(200, json!({"allow":true,"candidates":[{"routeId":"image:model","format":"openai","supportedEndpoints":["/v1/images/generations"]}],"pricing":{"imageCostPerImage":"1"}})).await;
        let (service, requests) = build_recording_service(
            200,
            serde_json::to_vec(&upstream).unwrap(),
            "application/json",
        );
        let mut input = images_input();
        if let Some(n) = requested {
            input.params["n"] = json!(n);
        } else {
            input.params.as_object_mut().unwrap().remove("n");
        }
        input.received_body = serde_json::to_vec(&input.params).unwrap().into();
        let (status, headers, body) =
            response_parts(middleware(url).handle_completion(&service, input).await).await;
        assert_eq!(status, 502);
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(body.get("data").is_none());
        let receipt = service
            .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
            .unwrap();
        assert!(receipt.chat_id.is_none());
        let document = receipt.document_json().unwrap();
        let returned = document["event_log"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["type"] == "response.returned")
            .unwrap();
        assert_eq!(
            returned["body_hash"],
            private_ai_gateway::aci::digest::sha256_hex(&serde_json::to_vec(&body).unwrap())
        );
        assert!(!body.to_string().contains("https://private"));
        let report = wait_for_post(&posts, |post| post["requestId"] == "req-images").await;
        assert_eq!(report["status"], 502);
        assert!(report["usage"].is_null());
        assert!(report.get("images").is_none());
    }
}

#[tokio::test]
async fn images_failed_attempts_carry_no_billable_image_usage() {
    let (url, posts) = spawn_control_capturing(200, json!({"allow":true,"candidates":[
        {"routeId":"images:first","format":"openai","supportedEndpoints":["/v1/images/generations"]},
        {"routeId":"images:second","format":"openai","supportedEndpoints":["/v1/images/generations"]}],
        "pricing":{"imageCostPerImage":"0.1"}})).await;
    let (service, _, _) = build_sequenced_service(vec![500, 200]);
    let (status, _, body) = response_parts(
        middleware(url)
            .handle_completion(&service, images_input())
            .await,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["usage"]["cost"], json!(0.1));
    let failure = wait_for_post(&posts, |post| post["selectedRouteId"] == "images:first").await;
    assert_eq!(failure["status"], 500);
    assert!(failure.get("images").is_none());
    assert!(failure["usage"].is_null());
    let success = wait_for_post(&posts, |post| post["selectedRouteId"] == "images:second").await;
    assert_eq!(
        success["images"],
        json!({"count":1,"size":"512x512","quality":"low"})
    );
}

fn audio_input() -> CompletionInput {
    let params = json!({"model":"gpt-test","response_format":"json","prompt":"sensitive hint"});
    CompletionInput {
        endpoint: Endpoint::AudioTranscriptions,
        endpoint_path: "/v1/audio/transcriptions",
        params,
        received_body: bytes::Bytes::from_static(b"raw multipart request bytes"),
        audio_form: Some(Arc::new(private_ai_gateway::middleware::audio::AudioForm {
            file: bytes::Bytes::from_static(b"sensitive audio"),
            extension: "wav".into(),
        })),
        request_id: "req-audio".into(),
        ..chat_input()
    }
}

fn audio_http_request(provider: &Value, host: &str) -> axum::http::Request<axum::body::Body> {
    let raw = "--upload\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-test\r\n--upload\r\nContent-Disposition: form-data; name=\"file\"; filename=\"private.wav\"\r\n\r\nsensitive audio\r\n--upload--\r\n";
    axum::http::Request::builder()
        .method("POST")
        .uri("/v1/audio/transcriptions")
        .header("host", host)
        .header("content-type", "multipart/form-data; boundary=upload")
        .header("x-aci-provider", provider.to_string())
        .body(axum::body::Body::from(raw))
        .unwrap()
}

fn audio_policy_service() -> (Arc<AciService>, Arc<Mutex<Vec<RecordedRequest>>>) {
    use private_ai_gateway::aci::upstream::{ModelRoute, ModelRouterBackend};
    let requests = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingUpstream {
        requests: requests.clone(),
        status: 200,
        body: br#"{"text":"hello","duration":2.965}"#.to_vec(),
        content_type: "application/json",
    });
    let mut router = ModelRouterBackend::new("audio-policy");
    for (route, is_tee) in [("plain:model", false), ("tee:model", true)] {
        router
            .add_route(
                ModelRoute::new("gpt-test", "whisper", backend.clone(), route)
                    .unwrap()
                    .with_is_tee(Some(is_tee)),
            )
            .unwrap();
    }
    (build_service_with_backend(Arc::new(router)), requests)
}

#[tokio::test]
async fn audio_http_tee_only_host_rejects_non_tee_candidate_despite_caller_opt_out() {
    use tower::ServiceExt;
    let (url, pres, _) = spawn_control_capturing_pre(json!({"allow":true,"candidates":[{"routeId":"plain:model","format":"openai","supportedEndpoints":["/v1/audio/transcriptions"]}]})).await;
    let (service, requests) = audio_policy_service();
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        UpstreamConfigManager::load(directory.path().join("upstreams.json"), runtime_options())
            .unwrap(),
    );
    let mw = new_middleware(&MiddlewareConfig {
        control_url: url,
        tee_only_domains: vec!["tee.example".into()],
        control_token: None,
        control_timeout_ms: Some(2000),
        control_post_timeout_ms: Some(2000),
        sse_keepalive_ms: None,
        send_request_features: None,
    })
    .unwrap();
    let app = private_ai_gateway::http::build_router_with_admin_and_middleware(
        service,
        manager,
        None,
        Arc::new(mw),
    );
    let provider = json!({"aci_verified":false,"order":["plain"]});
    let response = app
        .oneshot(audio_http_request(&provider, "tee.example"))
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let _ = response_parts(response).await;
    assert!(requests.lock().unwrap().is_empty());
    let pre = &pres.lock().unwrap()[0];
    assert_eq!(pre["tee"], true);
    assert_eq!(pre["provider"], provider);
    assert_eq!(pre["endpoint"], "/v1/audio/transcriptions");
}

#[tokio::test]
async fn audio_http_header_policy_reaches_consult_enforces_pins_and_never_reaches_upstream() {
    use tower::ServiceExt;
    let (url, pres, _) = spawn_control_capturing_pre(json!({"allow":true,"candidates":[{"routeId":"tee:model","format":"openai","supportedEndpoints":["/v1/audio/transcriptions"]}]})).await;
    let (service, requests) = audio_policy_service();
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        UpstreamConfigManager::load(directory.path().join("upstreams.json"), runtime_options())
            .unwrap(),
    );
    let app = private_ai_gateway::http::build_router_with_admin_and_middleware(
        service.clone(),
        manager,
        None,
        Arc::new(middleware(url)),
    );
    let (status, headers, _) = response_parts(
        app.clone()
            .oneshot(audio_http_request(
                &json!({"aci_verified":true}),
                "gateway.example",
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    let receipt = service
        .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
        .unwrap();
    let document = receipt.document_json().unwrap();
    let session = document["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "upstream.verified")
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    for (pin, expected_status) in [(session.clone(), 200), ("ab".repeat(32), 412)] {
        let provider = json!({"order":["tee"],"aci_session_ids":[pin]});
        let (status, headers, _) = response_parts(
            app.clone()
                .oneshot(audio_http_request(&provider, "gateway.example"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, expected_status);
        assert_eq!(pres.lock().unwrap().last().unwrap()["provider"], provider);
        let receipt = service
            .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
            .unwrap();
        let document = receipt.document_json().unwrap();
        let events = document["event_log"].as_array().unwrap();
        let verified = events
            .iter()
            .find(|e| e["type"] == "upstream.verified")
            .unwrap();
        assert_eq!(verified["required"], true);
        if status == 200 {
            assert_eq!(verified["session_id"], session);
        } else {
            assert!(!events.iter().any(|e| e["type"] == "request.forwarded"));
        }
    }
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["model"], "whisper");
    for request in requests.iter() {
        assert!(!request
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("x-aci-provider")));
        assert!(request.body.get("provider").is_none());
    }
}

#[tokio::test]
async fn audio_success_reports_ceil_duration_prices_and_sanitizes_nested_output() {
    let (url,pres,posts)=spawn_control_capturing_pre(json!({"allow":true,"candidates":[{"routeId":"near:model","format":"openai","supportedEndpoints":["/v1/audio/transcriptions"]}],"pricing":{"audioCostPerSecond":"0.001","inputCostPerToken":"0.01"}})).await;
    let (service,requests)=build_recording_service(200,serde_json::to_vec(&json!({"text":"hello","duration":2.965,"usage":{"type":"tokens","input_tokens":2,"output_tokens":1,"total_tokens":3,"input_token_details":{"audio_tokens":2,"private":"secret"}},"segments":[{"text":"hello","url":"secret"}]})).unwrap(),"application/json");
    let (status, headers, body) = response_parts(
        middleware(url)
            .handle_completion(&service, audio_input())
            .await,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({"text":"hello","usage":{"type":"duration","seconds":3,"cost":0.003}})
    );
    let forwarded = {
        let calls = requests.lock().unwrap();
        assert_eq!(calls.len(), 1);
        calls[0].clone()
    };
    let (fields, form) = private_ai_gateway::middleware::audio::parse(
        forwarded.raw_body.clone(),
        forwarded.content_type.as_deref().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(fields["response_format"], "json");
    assert_eq!(form.file, "sensitive audio");
    let receipt = service
        .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
        .unwrap();
    assert!(receipt.chat_id.is_none());
    let report = wait_for_post(&posts, |p| p["requestId"] == "req-audio").await;
    assert_eq!(report["audio"], json!({"seconds":3,"raw_duration":2.965}));
    assert_eq!(report["usage"], json!({"type":"duration","seconds":3}));
    assert_eq!(report["status"], 200);
    let pre = pres.lock().unwrap()[0].clone();
    assert_eq!(pre["endpoint"], "/v1/audio/transcriptions");
    let pre = pre.to_string();
    assert!(!pre.contains("sensitive"));
    assert!(!pre.contains("prompt"));
    assert!(!pre.contains("filename"));
    assert!(!pre.contains("prefixHash"));
}

#[tokio::test]
async fn audio_invalid_duration_is_502_without_billing_or_retranscription() {
    let (url, posts) = spawn_control_capturing(200, json!({
        "allow":true,"candidates":[
            {"routeId":"near:model","format":"openai","supportedEndpoints":["/v1/audio/transcriptions"]},
            {"routeId":"near:other","format":"openai","supportedEndpoints":["/v1/audio/transcriptions"]}
        ],"pricing":{"audioCostPerSecond":"1"}
    })).await;
    let (service, requests) = build_recording_service(
        200,
        br#"{"text":"private transcript","duration":1,"usage":{"type":"duration","seconds":-1}}"#
            .to_vec(),
        "application/json",
    );
    let (status, headers, body) = response_parts(
        middleware(url)
            .handle_completion(&service, audio_input())
            .await,
    )
    .await;
    assert_eq!(status, 502);
    assert!(!body.to_string().contains("private transcript"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    let receipt = service
        .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
        .unwrap();
    assert!(receipt.chat_id.is_none());
    assert!(receipt.document_json().unwrap()["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["type"] == "request.forwarded"));
    let report = wait_for_post(&posts, |p| p["requestId"] == "req-audio").await;
    assert_eq!(report["status"], 502);
    assert!(report.get("audio").is_none());
    assert!(report["usage"].is_null());
}

#[tokio::test]
async fn audio_requires_endpoint_opt_in() {
    let (url, _) = spawn_control_capturing(
        200,
        json!({"allow":true,"candidates":[
            {"routeId":"near:model","format":"openai","supportedEndpoints":[]}
        ]}),
    )
    .await;
    let (service, requests) = build_recording_service(
        200,
        br#"{"text":"hello","duration":2.1}"#.to_vec(),
        "application/json",
    );
    let (status, _, _) = response_parts(
        middleware(url)
            .handle_completion(&service, audio_input())
            .await,
    )
    .await;
    assert_eq!(status, 404);
    assert!(requests.lock().unwrap().is_empty());
}
