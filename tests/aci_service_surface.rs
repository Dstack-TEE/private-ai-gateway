//! Service-side ACI surface coverage.
//!
//! This file deliberately excludes the relying-party verification procedure
//! from ACI §10. It covers the service behavior that an ACI aggregator should
//! expose. Tests for implemented surfaces run by default. Tests for specified
//! but not-yet-implemented surfaces are `#[ignore]` with a reason; they are
//! still concrete executable specs, not a prose checklist.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use bytes::Bytes;
use futures_util::stream;
use private_ai_gateway::aci::canonical::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    decrypt_legacy_ecdsa_with_secret_key, decrypt_legacy_ed25519_with_secret_key,
    decrypt_with_secret_key, ed25519_public_key_hex, encrypt_for_public_key,
    encrypt_legacy_for_public_key, legacy_ecdsa_public_key_from_secret, public_key_from_secret,
    E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519, E2EE_VERSION_V2,
};
use private_ai_gateway::aci::receipt::{
    UpstreamVerifiedEvent, VerificationResult, EVENT_TRANSPARENCY_REQUEST_MODIFIED,
    EVENT_TRANSPARENCY_RESPONSE_MODIFIED,
};
use private_ai_gateway::aci::types::{Receipt, ServiceCapabilities, TlsSpki};
use private_ai_gateway::aci::upstream::{
    UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, ChatCompletionRequest, FixedClock, GatewayRequestContext,
    InMemoryReceiptStore, ReceiptOwner, UpstreamVerificationRequest, UpstreamVerifier,
    CHAT_COMPLETIONS_PATH,
};
use private_ai_gateway::http::build_router;
use serde_json::Value;
use tower::ServiceExt;

use common::{event_from_request, verified_event, StaticKeyProvider, StubQuoter};

const CHAT_REQUEST: &[u8] =
    br#"{"model":"aci-model","messages":[{"role":"user","content":"hello"}]}"#;
const CHAT_RESPONSE: &[u8] = br#"{"id":"chat-aci-1","object":"chat.completion","choices":[]}"#;
const E2EE_CHAT_RESPONSE: &[u8] = br#"{"id":"chat-aci-1","object":"chat.completion","model":"aci-model","choices":[{"index":0,"message":{"role":"assistant","content":"plain-answer"},"finish_reason":"stop"}]}"#;
const E2EE_COMPLETION_RESPONSE: &[u8] = br#"{"id":"cmpl-aci-1","object":"text_completion","model":"aci-model","choices":[{"index":0,"text":"completion-answer","finish_reason":"stop"}]}"#;

#[derive(Clone)]
struct HttpResult {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Clone)]
struct Requester {
    app: Router,
}

impl Requester {
    async fn get(&self, uri: &str, headers: &[(&str, &str)]) -> HttpResult {
        let mut req = Request::builder().method("GET").uri(uri);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        self.call(req.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, uri: &str, body: &[u8], headers: &[(&str, &str)]) -> HttpResult {
        let mut req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        self.call(req.body(Body::from(body.to_vec())).unwrap())
            .await
    }

    async fn post_owned_headers(
        &self,
        uri: &str,
        body: &[u8],
        headers: &[(&str, String)],
    ) -> HttpResult {
        let mut req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (name, value) in headers {
            req = req.header(*name, value);
        }
        self.call(req.body(Body::from(body.to_vec())).unwrap())
            .await
    }

    async fn call(&self, req: Request<Body>) -> HttpResult {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        HttpResult {
            status,
            headers,
            body,
        }
    }
}

struct RecordingUpstream {
    calls: Arc<Mutex<Vec<UpstreamRequest>>>,
    response_body: Vec<u8>,
    stream_status: u16,
    stream_headers: HashMap<String, String>,
    stream_chunks: Vec<Bytes>,
}

impl Default for RecordingUpstream {
    fn default() -> Self {
        let mut stream_headers = HashMap::new();
        stream_headers.insert("content-type".to_string(), "text/event-stream".to_string());
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response_body: CHAT_RESPONSE.to_vec(),
            stream_status: 200,
            stream_headers,
            stream_chunks: vec![
                Bytes::from_static(b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hel\"}\n\n"),
                Bytes::from_static(b"data: {\"id\":\"chat-stream-1\",\"delta\":\"lo\"}\n\n"),
                Bytes::from_static(b"data: [DONE]\n\n"),
            ],
        }
    }
}

impl RecordingUpstream {
    fn calls(&self) -> Arc<Mutex<Vec<UpstreamRequest>>> {
        self.calls.clone()
    }

    fn with_response_body(response_body: &[u8]) -> Self {
        Self {
            response_body: response_body.to_vec(),
            ..Self::default()
        }
    }

    fn with_stream_chunks(stream_chunks: Vec<Bytes>) -> Self {
        Self {
            stream_chunks,
            ..Self::default()
        }
    }
}

#[async_trait]
impl UpstreamBackend for RecordingUpstream {
    fn name(&self) -> &str {
        "surface-upstream"
    }

    fn url_origin(&self) -> Option<&str> {
        Some("https://surface-upstream.example")
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.calls.lock().unwrap().push(req);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamResponse {
            status_code: 200,
            body: self.response_body.clone(),
            headers,
            served_instance_id: None,
        })
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.calls.lock().unwrap().push(req);
        Ok(UpstreamStreamResponse {
            status_code: self.stream_status,
            headers: self.stream_headers.clone(),
            body: Box::pin(stream::iter(self.stream_chunks.clone().into_iter().map(Ok))),
            served_instance_id: None,
        })
    }
}

struct AlwaysVerified;

#[async_trait]
impl UpstreamVerifier for AlwaysVerified {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            verifier_id: "surface-verifier/v1".to_string(),
            evidence: Some(serde_json::json!({
                "digest": format!("sha256:{}", "11".repeat(32)),
                "data": "data:application/json;base64,eyJmaXh0dXJlIjoic3VyZmFjZS1ldmlkZW5jZSJ9",
            })),
            ..event_from_request(&request, VerificationResult::Verified)
        }
    }
}

struct Harness {
    requester: Requester,
    service: Arc<AciService>,
    upstream_calls: Arc<Mutex<Vec<UpstreamRequest>>>,
}

fn harness() -> Harness {
    harness_with_upstream(RecordingUpstream::default())
}

fn harness_with_upstream(upstream: RecordingUpstream) -> Harness {
    harness_with_upstream_and_e2ee(upstream, false)
}

fn harness_with_e2ee(upstream: RecordingUpstream) -> Harness {
    harness_with_upstream_and_e2ee(upstream, true)
}

fn harness_with_upstream_and_e2ee(upstream: RecordingUpstream, enable_e2ee: bool) -> Harness {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let upstream_calls = upstream.calls();
    let mut cfg = AciServiceConfig::for_test("private-ai-gateway");
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: if enable_e2ee {
            vec![E2EE_VERSION_V2.to_string()]
        } else {
            vec![]
        },
    };
    // Configured TLS SPKI for the keyset, instead of the test provider default.
    cfg.tls_public_keys = Some(vec![TlsSpki {
        domain: None,
        spki_sha256_hex: "configured-spki-sha256-hex".to_string(),
    }]);
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            quoter,
            Arc::new(upstream),
            Arc::new(AlwaysVerified),
            Arc::new(InMemoryReceiptStore::default()),
            cfg,
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    Harness {
        requester: Requester {
            app: build_router(service.clone()),
        },
        service,
        upstream_calls,
    }
}

fn harness_with_streaming_upstream_error() -> Harness {
    let mut headers = HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("connection".to_string(), "keep-alive".to_string());
    headers.insert("transfer-encoding".to_string(), "chunked".to_string());
    headers.insert("content-length".to_string(), "999".to_string());
    headers.insert("x-upstream-error".to_string(), "true".to_string());
    harness_with_upstream(
        RecordingUpstream {
            calls: Arc::new(Mutex::new(Vec::new())),
            response_body: CHAT_RESPONSE.to_vec(),
            stream_status: 400,
            stream_headers: headers,
            stream_chunks: vec![Bytes::from_static(
                br#"{"error":{"message":"Invalid request parameters","type":"invalid_request_error","code":400}}"#,
            )],
        },
    )
}

fn json_body(resp: &HttpResult) -> Value {
    serde_json::from_slice(&resp.body).unwrap()
}

fn error_type(resp: &HttpResult) -> String {
    json_body(resp)["error"]["type"]
        .as_str()
        .unwrap()
        .to_string()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).unwrap().to_str().unwrap()
}

fn receipt_event<'a>(receipt: &'a Receipt, event_type: &str) -> &'a Value {
    &receipt
        .event_log
        .iter()
        .find(|event| event.event_type == event_type)
        .unwrap()
        .fields
}

fn e2ee_request(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    e2ee_chat_request(h, client_secret, nonce, false)
}

fn e2ee_stream_request(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    e2ee_chat_request(h, client_secret, nonce, true)
}

fn e2ee_chat_request(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
    stream: bool,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let aad = format!(
        "v2|req|algo={}|model={model}|m=0|c=-|n={nonce}|ts={timestamp}",
        model_key.algo
    );
    let encrypted_content =
        encrypt_for_public_key(&model_key.public_key_hex, b"hello", aad.as_bytes()).unwrap();
    let body = serde_json::json!({
        "model": model,
        "stream": stream,
        "messages": [{"role": "user", "content": encrypted_content}],
    });
    let headers = vec![
        ("x-client-pub-key", public_key_from_secret(client_secret)),
        ("x-model-pub-key", model_key.public_key_hex.clone()),
        ("x-e2ee-version", E2EE_VERSION_V2.to_string()),
        ("x-e2ee-nonce", nonce.to_string()),
        ("x-e2ee-timestamp", timestamp.to_string()),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

fn e2ee_completion_request(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    e2ee_completion_request_with_stream(h, client_secret, nonce, false)
}

fn e2ee_completion_stream_request(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    e2ee_completion_request_with_stream(h, client_secret, nonce, true)
}

fn e2ee_completion_request_with_stream(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
    stream: bool,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let aad = format!(
        "v2|req|algo={}|model={model}|field=prompt|n={nonce}|ts={timestamp}",
        model_key.algo
    );
    let encrypted_prompt =
        encrypt_for_public_key(&model_key.public_key_hex, b"hello", aad.as_bytes()).unwrap();
    let body = serde_json::json!({
        "model": model,
        "prompt": encrypted_prompt,
        "stream": stream,
    });
    let headers = vec![
        ("x-client-pub-key", public_key_from_secret(client_secret)),
        ("x-model-pub-key", model_key.public_key_hex.clone()),
        ("x-e2ee-version", E2EE_VERSION_V2.to_string()),
        ("x-e2ee-nonce", nonce.to_string()),
        ("x-e2ee-timestamp", timestamp.to_string()),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

fn e2ee_response_aad(h: &Harness, nonce: &str, choice_index: u64, field_name: &str) -> String {
    e2ee_response_aad_with_id(h, nonce, "chat-aci-1", choice_index, field_name)
}

fn e2ee_response_aad_with_id(
    h: &Harness,
    nonce: &str,
    response_id: &str,
    choice_index: u64,
    field_name: &str,
) -> String {
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    format!(
        "v2|resp|algo={}|model=aci-model|id={response_id}|choice={choice_index}|field={field_name}|n={nonce}|ts=1700000000",
        model_key.algo
    )
}

fn legacy_model_public_key(h: &Harness, signing_algo: &str) -> String {
    h.service
        .keyset()
        .e2ee_public_keys
        .iter()
        .find(|key| key.algo == signing_algo)
        .unwrap()
        .public_key_hex
        .clone()
}

fn legacy_ecdsa_request(
    h: &Harness,
    client_secret: &k256::SecretKey,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model_key = legacy_model_public_key(h, E2EE_ALGO_LEGACY_ECDSA);
    let encrypted_content =
        encrypt_legacy_for_public_key(E2EE_ALGO_LEGACY_ECDSA, &model_key, b"hello", None).unwrap();
    let body = serde_json::json!({
        "model": "aci-model",
        "messages": [{"role": "user", "content": encrypted_content}],
    });
    let headers = vec![
        ("x-signing-algo", E2EE_ALGO_LEGACY_ECDSA.to_string()),
        (
            "x-client-pub-key",
            legacy_ecdsa_public_key_from_secret(client_secret),
        ),
        ("x-model-pub-key", model_key),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

fn legacy_ed25519_v2_request(
    h: &Harness,
    client_secret: &ed25519_dalek::SigningKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = legacy_model_public_key(h, E2EE_ALGO_LEGACY_ED25519);
    let aad = format!("v2|req|algo=ed25519|model={model}|m=0|c=-|n={nonce}|ts={timestamp}");
    let encrypted_content = encrypt_legacy_for_public_key(
        E2EE_ALGO_LEGACY_ED25519,
        &model_key,
        b"hello",
        Some(aad.as_bytes()),
    )
    .unwrap();
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": encrypted_content}],
    });
    let headers = vec![
        ("x-signing-algo", E2EE_ALGO_LEGACY_ED25519.to_string()),
        ("x-client-pub-key", ed25519_public_key_hex(client_secret)),
        ("x-model-pub-key", model_key),
        ("x-e2ee-nonce", nonce.to_string()),
        ("x-e2ee-timestamp", timestamp.to_string()),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

fn sse_json_events(body: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(body).unwrap();
    text.split("\n\n")
        .filter_map(|event| {
            let data = event
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                None
            } else {
                Some(serde_json::from_str::<Value>(&data).unwrap())
            }
        })
        .collect()
}

#[tokio::test]
async fn attestation_report_compat_query_params_are_service_scoped_noops() {
    let h = harness();
    let baseline = h.requester.get("/v1/attestation/report?nonce=n", &[]).await;
    let compat = h
        .requester
        .get(
            "/v1/attestation/report?nonce=n&model=gpt-a&signing_public_key=abc&signing_address=0xabc&signing_algo=ecdsa",
            &[],
        )
        .await;
    assert_eq!(baseline.status, StatusCode::OK);
    assert_eq!(compat.status, StatusCode::OK);
    let baseline = json_body(&baseline);
    let compat = json_body(&compat);
    assert_eq!(baseline["workload_id"], compat["workload_id"]);
    assert_eq!(
        baseline["workload_keyset_digest"],
        compat["workload_keyset_digest"]
    );
    assert_eq!(
        baseline["attestation"]["report_data"],
        compat["attestation"]["report_data"]
    );
    assert_eq!(baseline["signing_algo"], "ecdsa");
    assert_eq!(
        baseline["all_attestations"][0]["signing_public_key"],
        baseline["signing_public_key"]
    );
    assert_eq!(
        baseline["all_attestations"][0]["workload_id"],
        baseline["workload_id"]
    );

    let ed = h
        .requester
        .get("/v1/attestation/report?signing_algo=ed25519", &[])
        .await;
    assert_eq!(ed.status, StatusCode::OK);
    let ed = json_body(&ed);
    assert_eq!(ed["signing_algo"], "ed25519");
    assert_eq!(ed["signing_public_key"].as_str().unwrap().len(), 64);
    assert_eq!(ed["signing_address"], ed["signing_public_key"]);
}

#[tokio::test]
async fn plaintext_chat_response_headers_and_receipt_binding_are_covered() {
    let h = harness();
    let resp = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(resp.body, CHAT_RESPONSE);
    assert_eq!(header(&resp.headers, "x-aci-version"), "aci/1");
    assert_eq!(
        header(&resp.headers, "x-aci-identity"),
        h.service.workload_id()
    );
    assert_eq!(
        header(&resp.headers, "x-aci-keyset-digest"),
        h.service.workload_keyset_digest()
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "false");

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-aci-1"));
    assert_eq!(
        receipt_event(&receipt, "request.received")["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(CHAT_RESPONSE)
    );
}

#[tokio::test]
async fn receipt_lookup_by_chat_id_returns_signature_wrapper() {
    let h = harness();
    let chat = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(chat.status, StatusCode::OK);

    let receipt = h.requester.get("/v1/signature/chat-aci-1", &[]).await;
    assert_eq!(receipt.status, StatusCode::OK);
    let receipt_body = json_body(&receipt);
    assert_eq!(receipt_body["api_version"], "aci/1");
    assert_eq!(receipt_body["receipt"]["chat_id"], "chat-aci-1");
    assert!(receipt_body["signature"].is_string());
}

#[tokio::test]
async fn receipt_lookup_requires_authenticated_original_requester() {
    let h = harness();
    let chat = h
        .requester
        .post(
            "/v1/chat/completions",
            CHAT_REQUEST,
            &[("authorization", "Bearer requester-a")],
        )
        .await;
    assert_eq!(chat.status, StatusCode::OK);
    let unauthenticated = h.requester.get("/v1/signature/chat-aci-1", &[]).await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);

    let wrong_requester = h
        .requester
        .get(
            "/v1/signature/chat-aci-1",
            &[("authorization", "Bearer requester-b")],
        )
        .await;
    assert_eq!(wrong_requester.status, StatusCode::FORBIDDEN);

    let original = h
        .requester
        .get(
            "/v1/signature/chat-aci-1",
            &[("authorization", "Bearer requester-a")],
        )
        .await;
    assert_eq!(original.status, StatusCode::OK);
}

#[tokio::test]
async fn request_rewrite_is_recorded_by_hash_without_retaining_the_body() {
    let h = harness();
    let original = br#"{"model":"public","messages":[]}"#;
    let forwarded = br#"{"model":"private-upstream","messages":[]}"#;

    let result = h
        .service
        .forward_chat_completion_request(ChatCompletionRequest {
            context: GatewayRequestContext::default(),
            endpoint_path: CHAT_COMPLETIONS_PATH,
            received_body: original,
            forwarded_body: Some(forwarded.to_vec()),
            upstream_required: Some(true),
            upstream_verification_event: Some(UpstreamVerifiedEvent {
                url_origin: Some("https://surface-upstream.example".to_string()),
                verifier_id: "surface-verifier/v1".to_string(),
                evidence: Some(serde_json::json!({
                    "digest": format!("sha256:{}", "11".repeat(32)),
                    "data": "data:application/json;base64,eyJmaXh0dXJlIjoic3VyZmFjZS1ldmlkZW5jZSJ9",
                })),
                ..verified_event("surface-upstream", "private-upstream")
            }),
            requester: Some(ReceiptOwner::from_bearer("requester-a")),
            e2ee: None,
            first_byte_deadline: None,
        })
        .await
        .unwrap();
    assert_eq!(
        receipt_event(&result.receipt, "request.received")["body_hash"],
        sha256_hex(original)
    );
    assert_eq!(
        receipt_event(&result.receipt, "request.forwarded")["body_hash"],
        sha256_hex(forwarded)
    );
    assert_eq!(
        receipt_event(&result.receipt, EVENT_TRANSPARENCY_REQUEST_MODIFIED),
        &serde_json::json!({})
    );
    // The rewrite is committed by hash + the transparency event; the gateway
    // never stores the post-rewrite body, so there is no body endpoint to read.
    assert_ne!(sha256_hex(original), sha256_hex(forwarded));
}

#[tokio::test]
async fn empty_tool_calls_are_stripped_and_logged_as_request_modified() {
    let h = harness();
    let request = br#"{"model":"aci-model","messages":[{"role":"assistant","content":"","tool_calls":[]},{"role":"user","content":"hello"}]}"#;

    let resp = h.requester.post("/v1/chat/completions", request, &[]).await;
    assert_eq!(resp.status, StatusCode::OK);

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    let upstream_json = serde_json::from_slice::<Value>(&upstream_body).unwrap();
    assert!(upstream_json["messages"][0].get("tool_calls").is_none());
    assert_eq!(upstream_json["messages"][0]["content"], "");

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(
        receipt_event(&receipt, EVENT_TRANSPARENCY_REQUEST_MODIFIED),
        &serde_json::json!({})
    );
    assert_ne!(
        receipt_event(&receipt, "request.received")["body_hash"],
        receipt_event(&receipt, "request.forwarded")["body_hash"]
    );
}

#[tokio::test]
async fn e2ee_headers_are_rejected_when_service_advertises_no_e2ee_support() {
    let h = harness();
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            CHAT_REQUEST,
            &[("x-e2ee-version", "2")],
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_version");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v2_success_sets_e2ee_headers_and_receipt_hashes_cleartext_and_wire_separately() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = k256::SecretKey::from_slice(&[0x55; 32]).unwrap();
    let nonce = "nonce-1";
    let (encrypted_body, headers) = e2ee_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "2");
    assert_eq!(
        header(&resp.headers, "x-e2ee-algo"),
        h.service.keyset().e2ee_public_keys[0].algo
    );

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&upstream_body).unwrap()["messages"][0]["content"],
        "hello"
    );
    assert_ne!(resp.body, E2EE_CHAT_RESPONSE);
    let encrypted_response = json_body(&resp);
    let encrypted_content = encrypted_response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    let response_aad = e2ee_response_aad(&h, nonce, 0, "content");
    let decrypted_response =
        decrypt_with_secret_key(&client_secret, encrypted_content, response_aad.as_bytes())
            .unwrap();
    assert_eq!(decrypted_response, b"plain-answer");

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(
        receipt_event(&receipt, "request.received")["body_hash"],
        sha256_hex(&upstream_body)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["cleartext_hash"],
        sha256_hex(E2EE_CHAT_RESPONSE)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(&resp.body)
    );
    assert_ne!(
        receipt_event(&receipt, "response.returned")["cleartext_hash"],
        receipt_event(&receipt, "response.returned")["wire_hash"]
    );
    assert_eq!(
        receipt_event(&receipt, EVENT_TRANSPARENCY_RESPONSE_MODIFIED),
        &serde_json::json!({})
    );
}

#[tokio::test]
async fn e2ee_v2_response_aad_uses_request_model_not_upstream_response_model() {
    let upstream_response = br#"{"id":"chat-aci-1","object":"chat.completion","model":"private-upstream-model","choices":[{"index":0,"message":{"role":"assistant","content":"plain-answer"},"finish_reason":"stop"}]}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(upstream_response));
    let client_secret = k256::SecretKey::from_slice(&[0x56; 32]).unwrap();
    let nonce = "nonce-request-model-aad";
    let (encrypted_body, headers) = e2ee_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);

    let encrypted_response = json_body(&resp);
    let encrypted_content = encrypted_response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    let request_model_aad = e2ee_response_aad(&h, nonce, 0, "content");
    let decrypted_response = decrypt_with_secret_key(
        &client_secret,
        encrypted_content,
        request_model_aad.as_bytes(),
    )
    .unwrap();
    assert_eq!(decrypted_response, b"plain-answer");

    let wrong_aad = request_model_aad.replace("model=aci-model", "model=private-upstream-model");
    assert!(
        decrypt_with_secret_key(&client_secret, encrypted_content, wrong_aad.as_bytes()).is_err()
    );
}

#[tokio::test]
async fn legacy_ecdsa_e2ee_v1_matches_vllm_proxy_no_aad_shape() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = k256::SecretKey::from_slice(&[0x61; 32]).unwrap();
    let (encrypted_body, headers) = legacy_ecdsa_request(&h, &client_secret);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "1");
    assert_eq!(header(&resp.headers, "x-e2ee-algo"), "ecdsa");

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&upstream_body).unwrap()["messages"][0]["content"],
        "hello"
    );
    let encrypted_response = json_body(&resp);
    let encrypted_content = encrypted_response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    let decrypted_response =
        decrypt_legacy_ecdsa_with_secret_key(&client_secret, encrypted_content, None).unwrap();
    assert_eq!(decrypted_response, b"plain-answer");
}

#[tokio::test]
async fn legacy_ed25519_e2ee_v2_uses_x25519_and_response_model_aad() {
    let upstream_response = br#"{"id":"chat-aci-1","object":"chat.completion","model":"private-upstream-model","choices":[{"index":0,"message":{"role":"assistant","content":"plain-answer"},"finish_reason":"stop"}]}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(upstream_response));
    let client_secret = ed25519_dalek::SigningKey::from_bytes(&[0x62; 32]);
    let nonce = "legacy-ed25519-nonce";
    let (encrypted_body, headers) = legacy_ed25519_v2_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "2");
    assert_eq!(header(&resp.headers, "x-e2ee-algo"), "ed25519");

    let encrypted_response = json_body(&resp);
    let encrypted_content = encrypted_response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    let legacy_response_aad = "v2|resp|algo=ed25519|model=private-upstream-model|id=chat-aci-1|choice=0|field=content|n=legacy-ed25519-nonce|ts=1700000000";
    let decrypted_response = decrypt_legacy_ed25519_with_secret_key(
        &client_secret,
        encrypted_content,
        Some(legacy_response_aad.as_bytes()),
    )
    .unwrap();
    assert_eq!(decrypted_response, b"plain-answer");

    let request_model_aad =
        legacy_response_aad.replace("model=private-upstream-model", "model=aci-model");
    assert!(decrypt_legacy_ed25519_with_secret_key(
        &client_secret,
        encrypted_content,
        Some(request_model_aad.as_bytes()),
    )
    .is_err());
}

#[tokio::test]
async fn e2ee_v2_missing_headers_are_rejected_before_upstream() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            CHAT_REQUEST,
            &[("x-e2ee-version", "2")],
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_header_missing");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v2_invalid_version_is_rejected_before_upstream() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = k256::SecretKey::from_slice(&[0x56; 32]).unwrap();
    let (_body, mut headers) = e2ee_request(&h, &client_secret, "nonce-version");
    headers
        .iter_mut()
        .find(|(name, _)| *name == "x-e2ee-version")
        .unwrap()
        .1 = "3".to_string();
    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", CHAT_REQUEST, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_version");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v2_invalid_timestamp_is_rejected_before_upstream() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = k256::SecretKey::from_slice(&[0x57; 32]).unwrap();
    let (_body, mut headers) = e2ee_request(&h, &client_secret, "nonce-timestamp");
    headers
        .iter_mut()
        .find(|(name, _)| *name == "x-e2ee-timestamp")
        .unwrap()
        .1 = "1".to_string();
    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", CHAT_REQUEST, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_timestamp");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v2_replayed_nonce_tuple_is_rejected() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = k256::SecretKey::from_slice(&[0x58; 32]).unwrap();
    let (body, headers) = e2ee_request(&h, &client_secret, "nonce-replay");
    let first = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(first.status, StatusCode::OK);

    let second = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(second.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&second), "e2ee_replay_detected");
}

#[tokio::test]
async fn e2ee_v2_invalid_payload_model_is_rejected_before_upstream() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = k256::SecretKey::from_slice(&[0x59; 32]).unwrap();
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let invalid = br#"{"model":"bad|model","messages":[]}"#;
    let client_pub = public_key_from_secret(&client_secret);
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            invalid,
            &[
                ("x-client-pub-key", &client_pub),
                ("x-model-pub-key", &model_key.public_key_hex),
                ("x-e2ee-version", "2"),
                ("x-e2ee-nonce", "nonce-1"),
                ("x-e2ee-timestamp", "1700000000"),
            ],
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_payload_model");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn streaming_chat_completion_hashes_complete_ordered_stream() {
    let h = harness();
    let streaming_request =
        br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let resp = h
        .requester
        .post("/v1/chat/completions", streaming_request, &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "content-type"), "text/event-stream");
    assert_eq!(header(&resp.headers, "x-accel-buffering"), "no");
    assert_eq!(header(&resp.headers, "cache-control"), "no-cache");
    assert_eq!(
        resp.body,
        b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hel\"}\n\ndata: {\"id\":\"chat-stream-1\",\"delta\":\"lo\"}\n\ndata: [DONE]\n\n"
    );
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn e2ee_v2_streaming_chat_encrypts_sse_events_and_hashes_cleartext_and_wire() {
    let frame1 = format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": "chat-stream-1",
            "object": "chat.completion.chunk",
            "model": "private-upstream-model",
            "choices": [{"index": 0, "delta": {"content": "hel"}}],
        })
    )
    .into_bytes();
    let frame2 = format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": "chat-stream-1",
            "object": "chat.completion.chunk",
            "model": "private-upstream-model",
            "choices": [{"index": 0, "delta": {"reasoning_content": "think"}}],
        })
    )
    .into_bytes();
    let frame3 = format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": "chat-stream-1",
            "object": "chat.completion.chunk",
            "model": "private-upstream-model",
            "choices": [{"index": 0, "delta": {"content": ""}}],
        })
    )
    .into_bytes();
    let frame4 = b"data: [DONE]\n\n".to_vec();
    let split = frame1.len() / 2;
    let stream_chunks = vec![
        Bytes::copy_from_slice(&frame1[..split]),
        Bytes::copy_from_slice(&frame1[split..]),
        Bytes::from(frame2),
        Bytes::from(frame3),
        Bytes::from(frame4),
    ];
    let cleartext_stream = stream_chunks
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect::<Vec<_>>();
    let h = harness_with_e2ee(RecordingUpstream::with_stream_chunks(stream_chunks));
    let client_secret = k256::SecretKey::from_slice(&[0x5b; 32]).unwrap();
    let nonce = "nonce-stream";
    let (encrypted_body, headers) = e2ee_stream_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "2");
    assert_eq!(header(&resp.headers, "content-type"), "text/event-stream");
    assert_ne!(resp.body, cleartext_stream);
    assert!(std::str::from_utf8(&resp.body)
        .unwrap()
        .contains("data: [DONE]"));

    let events = sse_json_events(&resp.body);
    assert_eq!(events.len(), 3);
    let encrypted_content = events[0]["choices"][0]["delta"]["content"]
        .as_str()
        .unwrap();
    assert_ne!(encrypted_content, "hel");
    let content_aad = e2ee_response_aad_with_id(&h, nonce, "chat-stream-1", 0, "content");
    let decrypted_content =
        decrypt_with_secret_key(&client_secret, encrypted_content, content_aad.as_bytes()).unwrap();
    assert_eq!(decrypted_content, b"hel");

    let encrypted_reasoning = events[1]["choices"][0]["delta"]["reasoning_content"]
        .as_str()
        .unwrap();
    let reasoning_aad =
        e2ee_response_aad_with_id(&h, nonce, "chat-stream-1", 0, "reasoning_content");
    let decrypted_reasoning = decrypt_with_secret_key(
        &client_secret,
        encrypted_reasoning,
        reasoning_aad.as_bytes(),
    )
    .unwrap();
    assert_eq!(decrypted_reasoning, b"think");
    assert!(events[2]["choices"][0]["delta"].get("content").is_none());

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&upstream_body).unwrap()["messages"][0]["content"],
        "hello"
    );
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-stream-1"));
    assert_eq!(
        receipt_event(&receipt, "response.returned")["cleartext_hash"],
        sha256_hex(&cleartext_stream)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(&resp.body)
    );
    assert_ne!(
        receipt_event(&receipt, "response.returned")["cleartext_hash"],
        receipt_event(&receipt, "response.returned")["wire_hash"]
    );
    assert_eq!(
        receipt_event(&receipt, EVENT_TRANSPARENCY_RESPONSE_MODIFIED),
        &serde_json::json!({})
    );

    let metrics = h.requester.get("/v1/metrics", &[]).await;
    assert_eq!(metrics.status, StatusCode::OK);
    let metrics_body = String::from_utf8(metrics.body).unwrap();
    assert!(
        metrics_body.contains("model_id=\"private-upstream-model\""),
        "{metrics_body}"
    );
}

#[tokio::test]
async fn streaming_chat_completion_upstream_error_is_returned_without_sse_or_receipt() {
    let h = harness_with_streaming_upstream_error();
    let streaming_request =
        br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let resp = h
        .requester
        .post("/v1/chat/completions", streaming_request, &[])
        .await;

    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(header(&resp.headers, "content-type"), "application/json");
    assert_eq!(header(&resp.headers, "x-upstream-error"), "true");
    assert!(resp.headers.get("x-receipt-id").is_none());
    assert!(resp.headers.get("x-e2ee-applied").is_none());
    assert!(resp.headers.get("x-accel-buffering").is_none());
    assert!(resp.headers.get("cache-control").is_none());
    assert!(resp.headers.get("connection").is_none());
    assert!(resp.headers.get("transfer-encoding").is_none());
    assert_ne!(resp.headers.get("content-length").unwrap(), "999");

    let response_data = json_body(&resp);
    assert_eq!(
        response_data["error"]["message"],
        "Invalid request parameters"
    );
    assert_eq!(response_data["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn plaintext_https_keyset_publishes_configured_tls_spki() {
    let h = harness();
    let report = h.service.attestation_report(None).await.unwrap();
    let tls_keys = report.attestation.workload_keyset.tls_public_keys;
    assert_eq!(tls_keys.len(), 1);
    assert_eq!(tls_keys[0].domain, None);
    assert_eq!(tls_keys[0].spki_sha256_hex, "configured-spki-sha256-hex");
}

#[tokio::test]
#[ignore = "covered by tests/dstack_live.rs when a real dstack socket is available"]
async fn replica_stable_identity_uses_kms_released_or_derived_keys() {
    let h1 = harness();
    let h2 = harness();
    assert_eq!(h1.service.workload_id(), h2.service.workload_id());
    assert_eq!(
        h1.service.workload_keyset_digest(),
        h2.service.workload_keyset_digest()
    );
}

#[tokio::test]
async fn legacy_signature_endpoint_returns_vllm_proxy_shape() {
    let h = harness();
    let chat = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(chat.status, StatusCode::OK);

    let sig = h.requester.get("/v1/signature/chat-aci-1", &[]).await;
    assert_eq!(sig.status, StatusCode::OK);
    let body = json_body(&sig);
    assert_eq!(
        body["text"],
        format!(
            "{}:{}",
            sha256_hex(CHAT_REQUEST).trim_start_matches("sha256:"),
            sha256_hex(CHAT_RESPONSE).trim_start_matches("sha256:")
        )
    );
    assert!(body["signature"].as_str().unwrap().starts_with("0x"));
    assert!(body["signing_address"].as_str().unwrap().starts_with("0x"));
    assert_eq!(body["signing_algo"], "ecdsa");
    assert_eq!(body["receipt"]["chat_id"], "chat-aci-1");

    let ed = h
        .requester
        .get("/v1/signature/chat-aci-1?signing_algo=ed25519", &[])
        .await;
    assert_eq!(ed.status, StatusCode::OK);
    let body = json_body(&ed);
    assert_eq!(body["signing_algo"], "ed25519");
    assert_eq!(body["signing_address"].as_str().unwrap().len(), 64);
    assert_eq!(body["signature"].as_str().unwrap().len(), 128);

    let invalid = h
        .requester
        .get("/v1/signature/chat-aci-1?signing_algo=invalid-algo", &[])
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&invalid), "invalid_signing_algo");
}

#[tokio::test]
async fn completions_endpoint_supports_e2ee_as_optional_add_on() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(
        E2EE_COMPLETION_RESPONSE,
    ));
    let client_secret = k256::SecretKey::from_slice(&[0x5a; 32]).unwrap();
    let nonce = "nonce-completion";
    let (encrypted_body, headers) = e2ee_completion_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "2");

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&upstream_body).unwrap()["prompt"],
        "hello"
    );
    assert_ne!(resp.body, E2EE_COMPLETION_RESPONSE);

    let encrypted_response = json_body(&resp);
    let encrypted_text = encrypted_response["choices"][0]["text"].as_str().unwrap();
    let response_aad = "v2|resp|algo=secp256k1-aes-256-gcm-hkdf-sha256|model=aci-model|id=cmpl-aci-1|choice=0|field=text|n=nonce-completion|ts=1700000000";
    let decrypted_response =
        decrypt_with_secret_key(&client_secret, encrypted_text, response_aad.as_bytes()).unwrap();
    assert_eq!(decrypted_response, b"completion-answer");

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt.endpoint, "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("cmpl-aci-1"));
    assert_eq!(
        receipt_event(&receipt, "request.received")["body_hash"],
        sha256_hex(&upstream_body)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["cleartext_hash"],
        sha256_hex(E2EE_COMPLETION_RESPONSE)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn completions_endpoint_streaming_supports_e2ee_as_optional_add_on() {
    let frame = format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": "cmpl-stream-1",
            "object": "text_completion",
            "model": "private-upstream-model",
            "choices": [{"index": 0, "text": "completion-stream"}],
        })
    )
    .into_bytes();
    let done = b"data: [DONE]\n\n".to_vec();
    let stream_chunks = vec![Bytes::from(frame), Bytes::from(done)];
    let cleartext_stream = stream_chunks
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect::<Vec<_>>();
    let h = harness_with_e2ee(RecordingUpstream::with_stream_chunks(stream_chunks));
    let client_secret = k256::SecretKey::from_slice(&[0x5c; 32]).unwrap();
    let nonce = "nonce-completion-stream";
    let (encrypted_body, headers) = e2ee_completion_stream_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "content-type"), "text/event-stream");
    assert_ne!(resp.body, cleartext_stream);

    let events = sse_json_events(&resp.body);
    assert_eq!(events.len(), 1);
    let encrypted_text = events[0]["choices"][0]["text"].as_str().unwrap();
    let aad = "v2|resp|algo=secp256k1-aes-256-gcm-hkdf-sha256|model=aci-model|id=cmpl-stream-1|choice=0|field=text|n=nonce-completion-stream|ts=1700000000";
    let decrypted_text =
        decrypt_with_secret_key(&client_secret, encrypted_text, aad.as_bytes()).unwrap();
    assert_eq!(decrypted_text, b"completion-stream");

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&upstream_body).unwrap()["prompt"],
        "hello"
    );
    let receipt_id = header(&resp.headers, "x-receipt-id").to_string();
    let receipt = h.service.get_receipt_by_receipt_id(&receipt_id).unwrap();
    assert_eq!(receipt.endpoint, "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("cmpl-stream-1"));
    assert_eq!(
        receipt_event(&receipt, "response.returned")["cleartext_hash"],
        sha256_hex(&cleartext_stream)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn completions_endpoint_forwards_non_stream_and_issues_aci_receipt() {
    let h = harness();
    let request = br#"{"model":"aci-model","prompt":"hello","stream":false}"#;

    let resp = h.requester.post("/v1/completions", request, &[]).await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(resp.body, CHAT_RESPONSE);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "false");
    let receipt_id = header(&resp.headers, "x-receipt-id");

    {
        let calls = h.upstream_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path.as_deref(), Some("/v1/completions"));
        assert_eq!(calls[0].body, request);
    }

    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt.endpoint, "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-aci-1"));
    assert_eq!(
        receipt_event(&receipt, "request.received")["body_hash"],
        sha256_hex(request)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(CHAT_RESPONSE)
    );
}

#[tokio::test]
async fn completions_endpoint_streams_and_hashes_complete_response() {
    let h = harness();
    let request = br#"{"model":"aci-model","prompt":"hello","stream":true}"#;

    let resp = h.requester.post("/v1/completions", request, &[]).await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-accel-buffering"), "no");
    assert_eq!(header(&resp.headers, "cache-control"), "no-cache");
    let receipt_id = header(&resp.headers, "x-receipt-id").to_string();
    let expected_body =
        b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hel\"}\n\ndata: {\"id\":\"chat-stream-1\",\"delta\":\"lo\"}\n\ndata: [DONE]\n\n";
    assert_eq!(resp.body, expected_body);

    {
        let calls = h.upstream_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path.as_deref(), Some("/v1/completions"));
        assert_eq!(calls[0].body, request);
    }

    let receipt = h.service.get_receipt_by_receipt_id(&receipt_id).unwrap();
    assert_eq!(receipt.endpoint, "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-stream-1"));
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(expected_body)
    );

    let receipt_response = h.requester.get("/v1/signature/chat-stream-1", &[]).await;
    assert_eq!(receipt_response.status, StatusCode::OK);
    assert_eq!(
        json_body(&receipt_response)["receipt"]["receipt_id"],
        receipt_id
    );
}

// ---------------------------------------------------------------------------
// /v1/embeddings surface
// ---------------------------------------------------------------------------

const EMBEDDINGS_REQUEST: &[u8] = br#"{"model":"aci-model","input":"the quick brown fox"}"#;
const E2EE_EMBEDDINGS_RESPONSE: &[u8] =
    br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.5,-0.25,1.0]}],"model":"aci-model","usage":{"prompt_tokens":5,"total_tokens":5}}"#;
const EMBEDDINGS_PLAIN_RESPONSE: &[u8] =
    br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.5,-0.25]}],"model":"aci-model","usage":{"prompt_tokens":3,"total_tokens":3}}"#;

fn e2ee_embeddings_request_string(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let aad = format!(
        "v2|req|algo={}|model={model}|field=input|n={nonce}|ts={timestamp}",
        model_key.algo
    );
    let encrypted_input =
        encrypt_for_public_key(&model_key.public_key_hex, b"hello", aad.as_bytes()).unwrap();
    let body = serde_json::json!({
        "model": model,
        "input": encrypted_input,
    });
    let headers = vec![
        ("x-client-pub-key", public_key_from_secret(client_secret)),
        ("x-model-pub-key", model_key.public_key_hex.clone()),
        ("x-e2ee-version", E2EE_VERSION_V2.to_string()),
        ("x-e2ee-nonce", nonce.to_string()),
        ("x-e2ee-timestamp", timestamp.to_string()),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

fn e2ee_embeddings_request_array(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let aad_0 = format!(
        "v2|req|algo={}|model={model}|field=input.0|n={nonce}|ts={timestamp}",
        model_key.algo
    );
    let aad_1 = format!(
        "v2|req|algo={}|model={model}|field=input.1|n={nonce}|ts={timestamp}",
        model_key.algo
    );
    let enc_0 =
        encrypt_for_public_key(&model_key.public_key_hex, b"first", aad_0.as_bytes()).unwrap();
    let enc_1 =
        encrypt_for_public_key(&model_key.public_key_hex, b"second", aad_1.as_bytes()).unwrap();
    let body = serde_json::json!({
        "model": model,
        "input": [enc_0, enc_1],
    });
    let headers = vec![
        ("x-client-pub-key", public_key_from_secret(client_secret)),
        ("x-model-pub-key", model_key.public_key_hex.clone()),
        ("x-e2ee-version", E2EE_VERSION_V2.to_string()),
        ("x-e2ee-nonce", nonce.to_string()),
        ("x-e2ee-timestamp", timestamp.to_string()),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

fn legacy_ecdsa_embeddings_request(
    h: &Harness,
    client_secret: &k256::SecretKey,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model_key = legacy_model_public_key(h, E2EE_ALGO_LEGACY_ECDSA);
    let encrypted_input =
        encrypt_legacy_for_public_key(E2EE_ALGO_LEGACY_ECDSA, &model_key, b"hello", None).unwrap();
    let body = serde_json::json!({
        "model": "aci-model",
        "input": encrypted_input,
    });
    let headers = vec![
        ("x-signing-algo", E2EE_ALGO_LEGACY_ECDSA.to_string()),
        (
            "x-client-pub-key",
            legacy_ecdsa_public_key_from_secret(client_secret),
        ),
        ("x-model-pub-key", model_key),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

fn legacy_ed25519_v2_embeddings_request(
    h: &Harness,
    client_secret: &ed25519_dalek::SigningKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = legacy_model_public_key(h, E2EE_ALGO_LEGACY_ED25519);
    let aad = format!("v2|req|algo=ed25519|model={model}|field=input|n={nonce}|ts={timestamp}");
    let encrypted_input = encrypt_legacy_for_public_key(
        E2EE_ALGO_LEGACY_ED25519,
        &model_key,
        b"hello",
        Some(aad.as_bytes()),
    )
    .unwrap();
    let body = serde_json::json!({
        "model": model,
        "input": encrypted_input,
    });
    let headers = vec![
        ("x-signing-algo", E2EE_ALGO_LEGACY_ED25519.to_string()),
        ("x-client-pub-key", ed25519_public_key_hex(client_secret)),
        ("x-model-pub-key", model_key),
        ("x-e2ee-nonce", nonce.to_string()),
        ("x-e2ee-timestamp", timestamp.to_string()),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

#[tokio::test]
async fn embeddings_endpoint_forwards_non_stream_and_issues_aci_receipt() {
    let h = harness_with_upstream(RecordingUpstream::with_response_body(
        EMBEDDINGS_PLAIN_RESPONSE,
    ));

    let resp = h
        .requester
        .post("/v1/embeddings", EMBEDDINGS_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(resp.body, EMBEDDINGS_PLAIN_RESPONSE);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "false");
    let receipt_id = header(&resp.headers, "x-receipt-id");

    {
        let calls = h.upstream_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path.as_deref(), Some("/v1/embeddings"));
        assert_eq!(calls[0].body, EMBEDDINGS_REQUEST);
    }

    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt.endpoint, "/v1/embeddings");
    // OpenAI embeddings responses carry no `id`; the gateway leaves
    // the receipt chat_id empty for those.
    assert!(receipt.chat_id.is_none());
    assert_eq!(
        receipt_event(&receipt, "request.received")["body_hash"],
        sha256_hex(EMBEDDINGS_REQUEST)
    );
    assert_eq!(
        receipt_event(&receipt, "request.forwarded")["body_hash"],
        sha256_hex(EMBEDDINGS_REQUEST)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(EMBEDDINGS_PLAIN_RESPONSE)
    );
}

#[tokio::test]
async fn embeddings_receipt_is_retrievable_by_receipt_id_over_http() {
    // Embeddings responses have no `id`, so the `/v1/signature/{id}`
    // route must fall back to receipt_id lookup or callers have no way
    // to retrieve the receipt issued via the `x-receipt-id` header.
    let h = harness_with_upstream(RecordingUpstream::with_response_body(
        EMBEDDINGS_PLAIN_RESPONSE,
    ));

    let resp = h
        .requester
        .post("/v1/embeddings", EMBEDDINGS_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    let receipt_id = header(&resp.headers, "x-receipt-id").to_string();

    let fetched = h
        .requester
        .get(&format!("/v1/signature/{receipt_id}"), &[])
        .await;
    assert_eq!(fetched.status, StatusCode::OK);
    let body = json_body(&fetched);
    assert_eq!(
        body["receipt"]["receipt_id"].as_str().unwrap(),
        receipt_id,
        "receipt lookup by receipt_id must return the same receipt"
    );
    assert!(
        body["receipt"]["chat_id"].is_null(),
        "embeddings receipts have no chat_id"
    );
    assert_eq!(body["receipt"]["endpoint"], "/v1/embeddings");

    let unknown = h.requester.get("/v1/signature/rcpt-deadbeef", &[]).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn embeddings_endpoint_forces_buffered_even_when_client_sets_stream_true() {
    let h = harness_with_upstream(RecordingUpstream::with_response_body(
        EMBEDDINGS_PLAIN_RESPONSE,
    ));
    let request = br#"{"model":"aci-model","input":"hi","stream":true}"#;

    let resp = h.requester.post("/v1/embeddings", request, &[]).await;
    assert_eq!(resp.status, StatusCode::OK);
    // Buffered JSON, never SSE.
    let content_type = header(&resp.headers, "content-type");
    assert!(
        content_type.starts_with("application/json"),
        "expected JSON, got {content_type}"
    );
    assert_eq!(resp.body, EMBEDDINGS_PLAIN_RESPONSE);
    // The cache/x-accel headers stay off on the buffered path.
    assert!(resp.headers.get("x-accel-buffering").is_none());
    let calls = h.upstream_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].path.as_deref(), Some("/v1/embeddings"));
}

#[tokio::test]
async fn embeddings_endpoint_supports_aci_v2_e2ee_with_string_input() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(
        E2EE_EMBEDDINGS_RESPONSE,
    ));
    let client_secret = k256::SecretKey::from_slice(&[0x71; 32]).unwrap();
    let nonce = "nonce-embed-string";
    let (encrypted_body, headers) = e2ee_embeddings_request_string(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/embeddings", &encrypted_body, &headers)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "2");

    // The forwarded body must be cleartext "hello".
    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    let upstream_json: Value = serde_json::from_slice(&upstream_body).unwrap();
    assert_eq!(upstream_json["input"], "hello");
    assert_eq!(upstream_json["model"], "aci-model");

    // The response embedding must be encrypted under the request model AAD.
    assert_ne!(resp.body, E2EE_EMBEDDINGS_RESPONSE);
    let encrypted_response = json_body(&resp);
    let encrypted_embedding = encrypted_response["data"][0]["embedding"]
        .as_str()
        .expect("data[0].embedding must be encrypted hex string after E2EE");
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let response_aad = format!(
        "v2|resp|algo={}|model=aci-model|id=|data=0|field=embedding|n={nonce}|ts=1700000000",
        model_key.algo
    );
    let decrypted =
        decrypt_with_secret_key(&client_secret, encrypted_embedding, response_aad.as_bytes())
            .unwrap();
    let decoded: Value = serde_json::from_slice(&decrypted).unwrap();
    assert_eq!(decoded, serde_json::json!([0.5, -0.25, 1.0]));

    // Receipt records cleartext + wire hashes of the response, like chat.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt.endpoint, "/v1/embeddings");
    assert_eq!(
        receipt_event(&receipt, "response.returned")["cleartext_hash"],
        sha256_hex(E2EE_EMBEDDINGS_RESPONSE)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["wire_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn embeddings_endpoint_supports_aci_v2_e2ee_with_array_input() {
    let response_with_two = br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[1.0]},{"object":"embedding","index":1,"embedding":[2.0]}],"model":"aci-model","usage":{"prompt_tokens":4,"total_tokens":4}}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(response_with_two));
    let client_secret = k256::SecretKey::from_slice(&[0x72; 32]).unwrap();
    let nonce = "nonce-embed-array";
    let (encrypted_body, headers) = e2ee_embeddings_request_array(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/embeddings", &encrypted_body, &headers)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    let upstream_json: Value = serde_json::from_slice(&upstream_body).unwrap();
    assert_eq!(upstream_json["input"][0], "first");
    assert_eq!(upstream_json["input"][1], "second");

    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let encrypted_response = json_body(&resp);
    for (idx, expected) in [(0u64, [1.0]), (1u64, [2.0])] {
        let ciphertext = encrypted_response["data"][idx as usize]["embedding"]
            .as_str()
            .unwrap();
        let response_aad = format!(
            "v2|resp|algo={}|model=aci-model|id=|data={idx}|field=embedding|n={nonce}|ts=1700000000",
            model_key.algo
        );
        let plaintext =
            decrypt_with_secret_key(&client_secret, ciphertext, response_aad.as_bytes()).unwrap();
        let decoded: Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(decoded, serde_json::json!(expected));
    }
}

#[tokio::test]
async fn embeddings_endpoint_supports_legacy_v1_e2ee() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(
        E2EE_EMBEDDINGS_RESPONSE,
    ));
    let client_secret = k256::SecretKey::from_slice(&[0x73; 32]).unwrap();
    let (encrypted_body, headers) = legacy_ecdsa_embeddings_request(&h, &client_secret);

    let resp = h
        .requester
        .post_owned_headers("/v1/embeddings", &encrypted_body, &headers)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "1");
    assert_eq!(header(&resp.headers, "x-e2ee-algo"), "ecdsa");

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    let upstream_json: Value = serde_json::from_slice(&upstream_body).unwrap();
    assert_eq!(upstream_json["input"], "hello");

    let encrypted_response = json_body(&resp);
    let encrypted_embedding = encrypted_response["data"][0]["embedding"].as_str().unwrap();
    let decrypted =
        decrypt_legacy_ecdsa_with_secret_key(&client_secret, encrypted_embedding, None).unwrap();
    let decoded: Value = serde_json::from_slice(&decrypted).unwrap();
    assert_eq!(decoded, serde_json::json!([0.5, -0.25, 1.0]));
}

#[tokio::test]
async fn embeddings_endpoint_supports_legacy_ed25519_v2_e2ee() {
    let upstream_response = br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[7.5]}],"model":"private-upstream-model","usage":{"prompt_tokens":1,"total_tokens":1}}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(upstream_response));
    let client_secret = ed25519_dalek::SigningKey::from_bytes(&[0x74; 32]);
    let nonce = "legacy-ed25519-embed-nonce";
    let (encrypted_body, headers) = legacy_ed25519_v2_embeddings_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/embeddings", &encrypted_body, &headers)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "2");
    assert_eq!(header(&resp.headers, "x-e2ee-algo"), "ed25519");

    let encrypted_response = json_body(&resp);
    let encrypted_embedding = encrypted_response["data"][0]["embedding"].as_str().unwrap();
    // Legacy v2 binds the response model (not the request model) into AAD.
    let response_aad =
        "v2|resp|algo=ed25519|model=private-upstream-model|id=|data=0|field=embedding|n=legacy-ed25519-embed-nonce|ts=1700000000";
    let decrypted = decrypt_legacy_ed25519_with_secret_key(
        &client_secret,
        encrypted_embedding,
        Some(response_aad.as_bytes()),
    )
    .unwrap();
    let decoded: Value = serde_json::from_slice(&decrypted).unwrap();
    assert_eq!(decoded, serde_json::json!([7.5]));

    // Request-model AAD must NOT decrypt under legacy v2.
    let wrong_aad = response_aad.replace("model=private-upstream-model", "model=aci-model");
    assert!(decrypt_legacy_ed25519_with_secret_key(
        &client_secret,
        encrypted_embedding,
        Some(wrong_aad.as_bytes()),
    )
    .is_err());
}
