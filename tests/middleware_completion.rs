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
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::StreamExt;
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    seal_v3, unseal_v3, x25519_public_key_from_hex, x25519_public_key_hex,
    x25519_secret_key_from_bytes, E2EE_ALGO_X25519_AESGCM, E2EE_CONTEXT_REQUEST,
    E2EE_CONTEXT_RESPONSE,
};
use private_ai_gateway::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::upstream::{
    UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, E2eeRequestParts, FixedClock, InMemoryReceiptStore,
    ServiceResponseStream, UpstreamVerificationRequest, UpstreamVerifier,
};
use private_ai_gateway::aggregator::upstream_config::{
    UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::middleware::control::ControlClient;
use private_ai_gateway::middleware::errors::Surface;
use private_ai_gateway::middleware::request_transform::Endpoint;
use private_ai_gateway::middleware::sse::{MeterStream, StreamReport};
use private_ai_gateway::middleware::{CompletionInput, Middleware, MiddlewareConfig};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use common::{event_from_request, StaticKeyProvider, StubQuoter};

// A mock upstream that returns a fixed response for any forward, and fixed
// SSE chunks (when given) for any streaming forward.
struct MockUpstream {
    status: u16,
    body: Vec<u8>,
    stream_chunks: Vec<Bytes>,
}

#[async_trait]
impl UpstreamBackend for MockUpstream {
    fn name(&self) -> &str {
        "mock-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://mock-upstream.example")
    }
    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<private_ai_gateway::aci::upstream::UpstreamStreamResponse, UpstreamError> {
        if self.stream_chunks.is_empty() {
            // Fall back to the buffered adapter, matching the trait default.
            let response = self.forward(req).await?;
            return Ok(private_ai_gateway::aci::upstream::UpstreamStreamResponse {
                status_code: response.status_code,
                headers: response.headers,
                body: Box::pin(futures_util::stream::once(async move {
                    Ok(Bytes::from(response.body))
                })),
                served_instance_id: None,
            });
        }
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        Ok(private_ai_gateway::aci::upstream::UpstreamStreamResponse {
            status_code: self.status,
            headers,
            body: Box::pin(futures_util::stream::iter(
                self.stream_chunks.clone().into_iter().map(Ok),
            )),
            served_instance_id: None,
        })
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

    // The mock stands in for a backend that enforces the verifier's channel
    // binding on its connection (the trait default fails closed).
    async fn forward_verified_prepared(
        &self,
        req: private_ai_gateway::aci::upstream::PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(req).await
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: private_ai_gateway::aci::upstream::PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<private_ai_gateway::aci::upstream::UpstreamStreamResponse, UpstreamError> {
        self.forward_stream_prepared(req).await
    }
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

fn build_service_failing_verify() -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(MockUpstream {
                status: 200,
                body: b"{}".to_vec(),
                stream_chunks: Vec::new(),
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
    build_service_with_mock_upstream(MockUpstream {
        status,
        body,
        stream_chunks: Vec::new(),
    })
}

fn build_service_with_mock_upstream(upstream: MockUpstream) -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(upstream),
            Arc::new(OkVerifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
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
        request_id: "req-1".to_string(),
        user_model: Some("gpt-test".to_string()),
        stream: false,
    }
}

/// Seal `plaintext` per §7.2 and terminate it through the service's own E2EE
/// front door, returning the input the HTTP handler would hand the middleware.
fn e2ee_v3_input(
    service: &AciService,
    client_secret: &x25519_dalek::StaticSecret,
    model: &str,
    plaintext: &[u8],
    stream: bool,
) -> CompletionInput {
    let model_key = service
        .keyset()
        .e2ee_public_keys
        .iter()
        .find(|key| key.algo == E2EE_ALGO_X25519_AESGCM)
        .unwrap()
        .public_key_hex
        .clone();
    let recipient = x25519_public_key_from_hex(&model_key).unwrap();
    let client_public_key = x25519_public_key_hex(client_secret);
    let ctx = E2EE_CONTEXT_REQUEST;
    let sealed = seal_v3(&recipient, ctx, model, Some(&client_public_key), plaintext).unwrap();
    let envelope =
        serde_json::to_vec(&json!({ "model": model, "sealed_b64": BASE64.encode(sealed) }))
            .unwrap();
    let prepared = service
        .prepare_e2ee_request(
            E2eeRequestParts {
                signing_algo: None,
                client_public_key: Some(&client_public_key),
                model_public_key: Some(&model_key),
                version: Some("3"),
            },
            &envelope,
            "/v1/chat/completions",
        )
        .unwrap();
    assert_eq!(prepared.decrypted_body, plaintext);

    let mut input = chat_input();
    input.upstream_required = false;
    input.params = serde_json::from_slice(&prepared.decrypted_body).unwrap();
    input.received_body = prepared.decrypted_body;
    input.e2ee = Some(prepared.context);
    input.user_model = Some(model.to_string());
    input.stream = stream;
    input
}

/// Unseal one §7.3 response envelope (a buffered body or one SSE data payload).
fn e2ee_v3_unseal(client_secret: &x25519_dalek::StaticSecret, model: &str, wire: &[u8]) -> Vec<u8> {
    let envelope: Value = serde_json::from_slice(wire).unwrap();
    let sealed = BASE64
        .decode(envelope["sealed_b64"].as_str().unwrap())
        .unwrap();
    unseal_v3(client_secret, E2EE_CONTEXT_RESPONSE, model, None, &sealed).unwrap()
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
async fn e2ee_v3_buffered_middleware_seals_wire_body_and_receipt_hashes_it() {
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

    let client_secret = x25519_secret_key_from_bytes(&[0x7b; 32]).unwrap();
    let plaintext = br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#;
    let input = e2ee_v3_input(&service, &client_secret, "gpt-test", plaintext, false);

    let response = mw.handle_completion(&service, input).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let wire = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&wire));
    assert_eq!(headers.get("x-e2ee-applied").unwrap(), "true");

    // The wire body is one sealed §7.3 envelope over the final client body.
    let client_body: Value =
        serde_json::from_slice(&e2ee_v3_unseal(&client_secret, "gpt-test", &wire)).unwrap();
    assert_eq!(client_body["id"], json!("chat-1"));

    // §8.4: request.received hashes the unsealed original bytes;
    // response.returned hashes the sealed wire bytes.
    let receipt_id = headers.get("x-receipt-id").unwrap().to_str().unwrap();
    let receipt = service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt.payload_json().unwrap();
    let event = |event_type: &str| {
        payload["event_log"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["type"] == event_type)
            .unwrap()
            .clone()
    };
    assert_eq!(payload["model"], "gpt-test");
    assert_eq!(
        event("request.received")["body_hash"],
        sha256_hex(plaintext)
    );
    assert_eq!(event("response.returned")["body_hash"], sha256_hex(&wire));
}

#[tokio::test]
async fn e2ee_v3_streaming_middleware_seals_each_event_and_finalizes_receipt() {
    let control_url = spawn_control(
        200,
        json!({
            "allow": true,
            "candidates": [{ "routeId": "openai:gpt-test", "format": "openai" }]
        }),
    )
    .await;
    let mw = middleware(control_url);
    let service = build_service_with_mock_upstream(MockUpstream {
        status: 200,
        body: Vec::new(),
        stream_chunks: vec![
            Bytes::from_static(
                b"data: {\"id\":\"chat-s1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            ),
            Bytes::from_static(b"data: [DONE]\n\n"),
        ],
    });

    let client_secret = x25519_secret_key_from_bytes(&[0x7c; 32]).unwrap();
    let plaintext =
        br#"{"model":"gpt-test","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let input = e2ee_v3_input(&service, &client_secret, "gpt-test", plaintext, true);

    let response = mw.handle_completion(&service, input).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let wire = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&wire));
    assert_eq!(headers.get("x-e2ee-applied").unwrap(), "true");
    assert_eq!(headers.get("content-type").unwrap(), "text/event-stream");

    // Each event's data payload is sealed; [DONE] stays plaintext (§7.3).
    let text = String::from_utf8(wire.to_vec()).unwrap();
    let events: Vec<&str> = text
        .split("\n\n")
        .filter(|event| !event.is_empty())
        .collect();
    assert_eq!(events.len(), 2, "{text}");
    let payload = events[0].strip_prefix("data: ").unwrap();
    let unsealed: Value = serde_json::from_slice(&e2ee_v3_unseal(
        &client_secret,
        "gpt-test",
        payload.as_bytes(),
    ))
    .unwrap();
    assert_eq!(unsealed["id"], json!("chat-s1"));
    assert_eq!(events[1], "data: [DONE]");

    // The streaming receipt is finalized over the sealed wire stream (§8.4).
    let receipt_id = headers.get("x-receipt-id").unwrap().to_str().unwrap();
    let receipt = service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt.payload_json().unwrap();
    let returned = payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == "response.returned")
        .unwrap();
    assert_eq!(returned["body_hash"], sha256_hex(&wire));
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
    let metered = MeterStream::new(inner, report);
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

    let (status, headers, body) = response_parts(mw.handle_completion(&service, input).await).await;
    assert_eq!(status, 502, "spec §11: upstream_verification_failed is 502");
    assert_eq!(body["error"]["type"], json!("upstream_verification_failed"));
    // The refusal is receipt-backed (§8.5).
    assert!(headers.get("x-receipt-id").is_some());

    let report = wait_for_post(&posts, |r| r["errorSource"] == json!("upstream")).await;
    assert_eq!(report["status"].as_i64(), Some(502));
    assert_eq!(report["selectedRouteId"], Value::Null);
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
