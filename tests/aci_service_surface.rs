//! Service-side ACI surface coverage.
//!
//! This file deliberately excludes the relying-party verification procedure
//! from ACI §9. It covers the service behavior that an ACI aggregator should
//! expose: reports, receipts (as §7.2 envelopes), response headers, the
//! legacy compatibility surfaces, and the plaintext, ACI E2EE v2 (§6), and
//! legacy E2EE request paths.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use bytes::Bytes;
use futures_util::stream;
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    decrypt_legacy_ecdsa_with_secret_key, decrypt_with_secret_key, decrypt_x25519_with_secret_key,
    encrypt_for_public_key, encrypt_legacy_for_public_key, encrypt_x25519_for_public_key,
    legacy_ecdsa_public_key_from_secret, public_key_from_secret, x25519_public_key_hex,
    E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_X25519_AESGCM, E2EE_VERSION_V2,
};
use private_ai_gateway::aci::keys::verify_receipt_signature;
use private_ai_gateway::aci::receipt::{
    receipt_signing_input, SignedReceipt, UpstreamVerifiedEvent, VerificationResult,
};
use private_ai_gateway::aci::types::{ServiceCapabilities, TlsSpki};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore, UpstreamVerificationRequest,
    UpstreamVerifier,
};
use private_ai_gateway::http::build_router;
use serde_json::Value;
use tower::ServiceExt;
use x25519_dalek::StaticSecret as X25519SecretKey;

use common::{event_from_request, StaticKeyProvider, StubQuoter};

const CHAT_REQUEST: &[u8] =
    br#"{"model":"aci-model","messages":[{"role":"user","content":"hello"}]}"#;
const CHAT_RESPONSE: &[u8] = br#"{"id":"chat-aci-1","object":"chat.completion","choices":[]}"#;
const E2EE_CHAT_RESPONSE: &[u8] = br#"{"id":"chat-aci-1","object":"chat.completion","model":"aci-model","choices":[{"index":0,"message":{"role":"assistant","reasoning":"private-thought","content":"plain-answer"},"finish_reason":"stop"}]}"#;

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
    /// Route classification the aggregator reads: only a TEE-attesting route
    /// is a candidate for a constrained request (§5.3).
    is_tee: Option<bool>,
    response_body: Vec<u8>,
    stream_status: u16,
    stream_headers: HashMap<String, String>,
    stream_chunks: Vec<Bytes>,
    /// Transport failure emitted after the scripted chunks.
    stream_error: Option<String>,
}

impl Default for RecordingUpstream {
    fn default() -> Self {
        let mut stream_headers = HashMap::new();
        stream_headers.insert("content-type".to_string(), "text/event-stream".to_string());
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            is_tee: Some(true),
            response_body: CHAT_RESPONSE.to_vec(),
            stream_status: 200,
            stream_headers,
            stream_chunks: vec![
                Bytes::from_static(b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hel\"}\n\n"),
                Bytes::from_static(b"data: {\"id\":\"chat-stream-1\",\"delta\":\"lo\"}\n\n"),
                Bytes::from_static(b"data: [DONE]\n\n"),
            ],
            stream_error: None,
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

    fn with_stream_chunks_then_error(stream_chunks: Vec<Bytes>) -> Self {
        Self {
            stream_chunks,
            stream_error: Some("connection reset".to_string()),
            ..Self::default()
        }
    }

    fn with_content_type(mut self, content_type: &str) -> Self {
        self.stream_headers
            .insert("content-type".to_string(), content_type.to_string());
        self
    }

    fn without_content_type(mut self) -> Self {
        self.stream_headers.remove("content-type");
        self
    }
}

#[async_trait]
impl UpstreamBackend for RecordingUpstream {
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let model_id = serde_json::from_slice::<Value>(&req.body)
            .ok()
            .and_then(|body| {
                body.get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        Ok(PreparedUpstreamRequest {
            request: req,
            upstream_name: self.name().to_string(),
            url_origin: self.url_origin().map(str::to_string),
            model_id,
            route_id: None,
            is_tee: self.is_tee,
        })
    }

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
            body: {
                let ok = self.stream_chunks.clone().into_iter().map(Ok);
                let tail = self
                    .stream_error
                    .clone()
                    .map(|message| Err(UpstreamError::Transport(message)));
                Box::pin(stream::iter(ok.chain(tail)))
            },
            served_instance_id: None,
        })
    }

    // The mock stands in for a backend that enforces the verifier's channel
    // binding on its connection (the trait default fails closed).
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

/// A verifier that always fails: exercises the fail-closed refusal path.
struct AlwaysFailed;

#[async_trait]
impl UpstreamVerifier for AlwaysFailed {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            verifier_id: "surface-verifier/v1".to_string(),
            reason: Some("quote verification failed".to_string()),
            ..event_from_request(&request, VerificationResult::Failed)
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
    harness_with(upstream, Arc::new(AlwaysVerified), false)
}

fn harness_with_e2ee(upstream: RecordingUpstream) -> Harness {
    harness_with(upstream, Arc::new(AlwaysVerified), true)
}

fn harness_with(
    upstream: RecordingUpstream,
    verifier: Arc<dyn UpstreamVerifier>,
    enable_e2ee: bool,
) -> Harness {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let upstream_calls = upstream.calls();
    let mut cfg = AciServiceConfig::for_test();
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: if enable_e2ee {
            vec!["3".to_string()]
        } else {
            vec![]
        },
        serving: "aggregator".to_string(),
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
            verifier,
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
            is_tee: Some(true),
            response_body: CHAT_RESPONSE.to_vec(),
            stream_status: 400,
            stream_headers: headers,
            stream_chunks: vec![Bytes::from_static(
                br#"{"error":{"message":"Invalid request parameters","type":"invalid_request_error","code":400}}"#,
            )],
            stream_error: None,
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

/// ACI v2 request AAD: JCS of the purpose-tagged object.
fn aci_request_aad(algo: &str, model: &str, field: &str, nonce: &str, ts: u64) -> Vec<u8> {
    private_ai_gateway::aci::digest::jcs_bytes(&serde_json::json!({
        "purpose": "aci.e2ee.request.v2",
        "algo": algo,
        "model": model,
        "field": field,
        "nonce": nonce,
        "ts": ts,
    }))
    .unwrap()
}

/// ACI v2 response AAD: like the request AAD but tagged `aci.e2ee.response.v2`
/// and additionally binding the response `id`.
fn aci_response_aad(
    algo: &str,
    model: &str,
    id: &str,
    field: &str,
    nonce: &str,
    ts: u64,
) -> Vec<u8> {
    private_ai_gateway::aci::digest::jcs_bytes(&serde_json::json!({
        "purpose": "aci.e2ee.response.v2",
        "algo": algo,
        "model": model,
        "id": id,
        "field": field,
        "nonce": nonce,
        "ts": ts,
    }))
    .unwrap()
}

/// Main-suite adapter: one event from a signed receipt's document.
fn receipt_event(receipt: &SignedReceipt, event_type: &str) -> Value {
    payload_event(&receipt_payload(receipt), event_type).clone()
}

fn receipt_payload(receipt: &SignedReceipt) -> Value {
    receipt.document_json().unwrap()
}

fn payload_event<'a>(payload: &'a Value, event_type: &str) -> &'a Value {
    payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == event_type)
        .unwrap()
}

fn legacy_model_public_key(h: &Harness, signing_algo: &str) -> String {
    h.service
        .legacy_e2ee_keys()
        .iter()
        .find(|key| key.algo == signing_algo)
        .unwrap()
        .public_key_hex
        .clone()
}

/// A valid ACI v2 nonce (64 lowercase hex chars, §7.5) derived from a label, so
/// each test uses a distinct, readable value without hardcoding 64-char hex.
fn hex_nonce(label: &str) -> String {
    let mut out = String::with_capacity(64);
    let bytes = label.as_bytes();
    for i in 0..32 {
        out.push_str(&format!("{:02x}", bytes.get(i).copied().unwrap_or(0)));
    }
    out
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
    let aad = aci_request_aad(
        &model_key.algo,
        model,
        "messages.0.content",
        nonce,
        timestamp,
    );
    let encrypted_content =
        encrypt_for_public_key(&model_key.public_key_hex, b"hello", &aad).unwrap();
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

/// Build an X25519-suite E2EE chat request: the client encrypts whole message
/// content to the keyset's X25519 service key (§7.1 RECOMMENDED suite). Suite
/// selection is by the `algo` of the matched `X-Model-Pub-Key` entry (§7.4).
fn e2ee_x25519_chat_request(
    h: &Harness,
    client_secret: &X25519SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = h
        .service
        .keyset()
        .e2ee_public_keys
        .iter()
        .find(|k| k.algo == E2EE_ALGO_X25519_AESGCM)
        .expect("x25519 e2ee key is published")
        .clone();
    let aad = aci_request_aad(
        &model_key.algo,
        model,
        "messages.0.content",
        nonce,
        timestamp,
    );
    let encrypted_content =
        encrypt_x25519_for_public_key(&model_key.public_key_hex, b"hello", &aad).unwrap();
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{"role": "user", "content": encrypted_content}],
    });
    let headers = vec![
        ("x-client-pub-key", x25519_public_key_hex(client_secret)),
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
    let aad = aci_request_aad(&model_key.algo, model, "prompt", nonce, timestamp);
    let encrypted_prompt =
        encrypt_for_public_key(&model_key.public_key_hex, b"hello", &aad).unwrap();
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

/// ACI v2 response AAD bound to the request model `aci-model`, for the given
/// response id and full field path (spec §7.2, §7.3).
fn e2ee_response_aad(h: &Harness, nonce: &str, response_id: &str, field: &str) -> Vec<u8> {
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    aci_response_aad(
        &model_key.algo,
        "aci-model",
        response_id,
        field,
        nonce,
        1_700_000_000,
    )
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

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aci_attestation_report_binds_nonce_and_serves_exact_keyset_bytes() {
    let h = harness();
    let resp = h
        .requester
        .get("/v1/aci/attestation?nonce=9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0", &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    let report = json_body(&resp);
    assert_eq!(report["api_version"], "aci/1");
    // No legacy compatibility fields on the canonical report.
    assert!(report.get("signing_address").is_none());
    assert!(report.get("all_attestations").is_none());
    assert!(report.get("workload_id").is_none());

    // The served keyset object's JCS form hashes to the restated digest (§4.1).
    let keyset_value = &report["attestation"]["workload_keyset"];
    assert!(keyset_value.is_object());
    assert_eq!(
        report["workload_keyset_digest"].as_str().unwrap(),
        sha256_hex(&private_ai_gateway::aci::digest::jcs_bytes(keyset_value).unwrap())
    );
    assert_eq!(*keyset_value, h.service.keyset_value());

    // report_data = sha256 of the §3.2 statement for the supplied nonce.
    let statement = private_ai_gateway::aci::identity::attestation_statement(
        h.service.workload_keyset_digest(),
        Some("9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0"),
    )
    .unwrap();
    assert_eq!(
        report["attestation"]["report_data"].as_str().unwrap(),
        hex::encode(private_ai_gateway::aci::identity::report_data(&statement))
    );
}

#[tokio::test]
async fn aci_attestation_report_rejects_invalid_nonce_with_400() {
    let h = harness();
    // A nonce is exactly 64 lowercase hex chars (§3.2): wrong length,
    // uppercase, non-hex, and escaping-relevant characters are all 400.
    let short = "a".repeat(63);
    let long = "a".repeat(65);
    let uppercase = format!("A{}", "a".repeat(63));
    let nonhex = format!("g{}", "a".repeat(63));
    for bad in [
        short.as_str(),
        long.as_str(),
        uppercase.as_str(),
        nonhex.as_str(),
        "with%20space",
        "quote%22here",
    ] {
        let resp = h
            .requester
            .get(&format!("/v1/aci/attestation?nonce={bad}"), &[])
            .await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST, "nonce {bad}");
    }
    let exact = "a".repeat(64);
    let resp = h
        .requester
        .get(&format!("/v1/aci/attestation?nonce={exact}"), &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
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

// ---------------------------------------------------------------------------
// Receipts and response headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plaintext_chat_response_headers_and_receipt_binding_are_covered() {
    let h = harness();
    let resp = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(resp.body, CHAT_RESPONSE);
    assert_eq!(header(&resp.headers, "x-aci-version"), "aci/1");
    assert!(resp.headers.get("x-aci-identity").is_none());
    assert!(resp.headers.get("x-e2ee-algo").is_none());
    assert!(resp.headers.get("x-e2ee-version").is_none());
    assert_eq!(
        header(&resp.headers, "x-aci-keyset-digest"),
        h.service.workload_keyset_digest()
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "false");

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-aci-1"));
    let payload = receipt_payload(&receipt);
    assert_eq!(payload["api_version"], "aci/1");
    assert_eq!(
        payload["workload_keyset_digest"].as_str().unwrap(),
        h.service.workload_keyset_digest()
    );
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(CHAT_RESPONSE)
    );
    // The verified event is slim (§7.5): a session citation, no inline detail.
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["result"], "verified");
    let session_id = verified["session_id"].as_str().unwrap();
    assert_eq!(session_id.len(), 64, "session id is bare 64-hex (§8)");
    assert!(session_id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(verified.get("channel_bindings").is_none());
    assert!(verified.get("claims").is_none());
    assert!(verified.get("evidence").is_none());
}

#[tokio::test]
async fn aci_receipt_endpoint_serves_signed_document() {
    let h = harness();
    let chat = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    let receipt_id = header(&chat.headers, "x-receipt-id");

    let resp = h
        .requester
        .get(&format!("/v1/aci/receipts/{receipt_id}"), &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    // §7.2: one readable document; the signature covers JCS(document minus
    // `signature`) under the attested key that `key_id` names.
    let document = json_body(&resp);
    assert_eq!(document["api_version"], "aci/1");
    assert!(document["event_log"].is_array());
    let signature = hex::decode(document["signature"].as_str().unwrap()).unwrap();
    let key_id = document["key_id"].as_str().unwrap();
    let receipt_key = h
        .service
        .keyset()
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == key_id)
        .expect("receipt key_id resolves in the attested keyset");
    assert!(verify_receipt_signature(
        receipt_key,
        &receipt_signing_input(&document).unwrap(),
        &signature
    ));
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
    // A wrong credential reads as a missing receipt (§7.6): no existence
    // oracle for other tenants' receipts.
    assert_eq!(wrong_requester.status, StatusCode::NOT_FOUND);

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
async fn empty_tool_calls_are_stripped_and_visible_as_differing_hashes() {
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
    let payload = receipt_payload(&receipt);
    assert_ne!(
        payload_event(&payload, "request.received")["body_hash"],
        payload_event(&payload, "request.forwarded")["body_hash"]
    );
}

// ---------------------------------------------------------------------------
// Fail-closed refusal (§7.5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failed_upstream_verification_returns_503_with_refusal_receipt() {
    let h = harness_with(RecordingUpstream::default(), Arc::new(AlwaysFailed), false);
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            CONSTRAINED_REQUEST,
            &[("authorization", "Bearer requester-a")],
        )
        .await;
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_type(&resp), "upstream_verification_failed");
    // The prompt was not forwarded (§1.2).
    assert!(h.upstream_calls.lock().unwrap().is_empty());

    // The error carries X-Receipt-Id (§5.2) and the refusal receipt hashes the
    // exact error body served, with the §7.5 failed form and no forwarding.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(CONSTRAINED_REQUEST)
    );
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["result"], "failed");
    assert_eq!(verified["required"], true);
    assert!(verified["reason"]
        .as_str()
        .unwrap()
        .contains("quote verification failed"));
    assert!(verified.get("session_id").is_none());
    assert!(payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["type"] != "request.forwarded"));

    // The refusal receipt is credential-bound like any other (§7.6).
    let unauthenticated = h
        .requester
        .get(&format!("/v1/aci/receipts/{receipt_id}"), &[])
        .await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);
    let original = h
        .requester
        .get(
            &format!("/v1/aci/receipts/{receipt_id}"),
            &[("authorization", "Bearer requester-a")],
        )
        .await;
    assert_eq!(original.status, StatusCode::OK);
}

#[tokio::test]
async fn verified_result_with_no_channel_binding_is_refused() {
    struct VerifiedWithoutBinding;
    #[async_trait]
    impl UpstreamVerifier for VerifiedWithoutBinding {
        async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
            UpstreamVerifiedEvent {
                verifier_id: "surface-verifier/v1".to_string(),
                channel_bindings: Vec::new(),
                ..event_from_request(&request, VerificationResult::Verified)
            }
        }
    }
    let h = harness_with(
        RecordingUpstream::default(),
        Arc::new(VerifiedWithoutBinding),
        false,
    );
    let resp = h
        .requester
        .post("/v1/chat/completions", CONSTRAINED_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_type(&resp), "upstream_verification_failed");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["result"], "failed");
    assert!(verified["reason"]
        .as_str()
        .unwrap()
        .contains("no enforceable channel binding"));
}

// ---------------------------------------------------------------------------
// E2EE gating and the legacy mode
// ---------------------------------------------------------------------------

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
    let nonce = hex_nonce("nonce-1");
    let nonce = nonce.as_str();
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
    let response_aad = e2ee_response_aad(&h, nonce, "chat-aci-1", "choices.0.message.content");
    let decrypted_response =
        decrypt_with_secret_key(&client_secret, encrypted_content, &response_aad).unwrap();
    assert_eq!(decrypted_response, b"plain-answer");

    let encrypted_reasoning = encrypted_response["choices"][0]["message"]["reasoning"]
        .as_str()
        .unwrap();
    let reasoning_aad = e2ee_response_aad(&h, nonce, "chat-aci-1", "choices.0.message.reasoning");
    let decrypted_reasoning =
        decrypt_with_secret_key(&client_secret, encrypted_reasoning, &reasoning_aad).unwrap();
    assert_eq!(decrypted_reasoning, b"private-thought");

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(
        receipt_event(&receipt, "request.received")["body_hash"],
        sha256_hex(&upstream_body)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["body_hash"],
        sha256_hex(&resp.body),
        "§7.4: the receipt covers the encrypted wire bytes"
    );
    assert_ne!(
        receipt_event(&receipt, "response.returned")["body_hash"],
        sha256_hex(E2EE_CHAT_RESPONSE),
        "the wire bytes differ from the cleartext the E2EE layer produced"
    );
}

#[tokio::test]
async fn e2ee_v2_x25519_suite_selected_by_model_key_round_trips() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = X25519SecretKey::from([0x71u8; 32]);
    let nonce = hex_nonce("nonce-x25519");
    let nonce = nonce.as_str();
    let (encrypted_body, headers) = e2ee_x25519_chat_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    // The response header follows the suite selected by X-Model-Pub-Key (§7.4).
    assert_eq!(
        header(&resp.headers, "x-e2ee-algo"),
        E2EE_ALGO_X25519_AESGCM
    );

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&upstream_body).unwrap()["messages"][0]["content"],
        "hello"
    );

    // The service encrypts the response back under X25519 to the client key.
    let encrypted_response = json_body(&resp);
    let encrypted_content = encrypted_response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    let response_aad = aci_response_aad(
        E2EE_ALGO_X25519_AESGCM,
        "aci-model",
        "chat-aci-1",
        "choices.0.message.content",
        nonce,
        1_700_000_000,
    );
    let decrypted =
        decrypt_x25519_with_secret_key(&client_secret, encrypted_content, &response_aad).unwrap();
    assert_eq!(decrypted, b"plain-answer");
}

#[tokio::test]
async fn e2ee_v2_model_key_absent_from_keyset_is_rejected() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = X25519SecretKey::from([0x72u8; 32]);
    let nonce = hex_nonce("nonce-x25519-stranger");
    let nonce = nonce.as_str();
    let (encrypted_body, mut headers) = e2ee_x25519_chat_request(&h, &client_secret, nonce);
    // Well-formed X25519 key that is not one of the attested service keys.
    let stranger = x25519_public_key_hex(&X25519SecretKey::from([0x99u8; 32]));
    for header in headers.iter_mut() {
        if header.0 == "x-model-pub-key" {
            header.1 = stranger.clone();
        }
    }

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_model_key_mismatch");
}

#[tokio::test]
async fn e2ee_v2_response_aad_uses_request_model_not_upstream_response_model() {
    let upstream_response = br#"{"id":"chat-aci-1","object":"chat.completion","model":"private-upstream-model","choices":[{"index":0,"message":{"role":"assistant","content":"plain-answer"},"finish_reason":"stop"}]}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(upstream_response));
    let client_secret = k256::SecretKey::from_slice(&[0x56; 32]).unwrap();
    let nonce = hex_nonce("nonce-request-model-aad");
    let nonce = nonce.as_str();
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
    let request_model_aad = e2ee_response_aad(&h, nonce, "chat-aci-1", "choices.0.message.content");
    let decrypted_response =
        decrypt_with_secret_key(&client_secret, encrypted_content, &request_model_aad).unwrap();
    assert_eq!(decrypted_response, b"plain-answer");

    // The response AAD binds the request model, so the upstream response model
    // must not decrypt it.
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let wrong_aad = aci_response_aad(
        &model_key.algo,
        "private-upstream-model",
        "chat-aci-1",
        "choices.0.message.content",
        nonce,
        1_700_000_000,
    );
    assert!(decrypt_with_secret_key(&client_secret, encrypted_content, &wrong_aad).is_err());
}

#[tokio::test]
async fn e2ee_v2_decrypts_multimodal_image_and_audio_parts() {
    // Per-part decryption of `image_url.url` and `input_audio.data`
    // alongside a text part (spec §7.2).
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = k256::SecretKey::from_slice(&[0x64; 32]).unwrap();
    let nonce = hex_nonce("nonce-multimodal");
    let nonce = nonce.as_str();
    let timestamp = 1_700_000_000u64;
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let algo = model_key.algo.clone();
    let pub_key = model_key.public_key_hex.clone();

    let enc = |field: &str, plaintext: &[u8]| {
        let aad = aci_request_aad(&algo, "aci-model", field, nonce, timestamp);
        encrypt_for_public_key(&pub_key, plaintext, &aad).unwrap()
    };
    let text_ct = enc("messages.0.content.0.text", b"look at this");
    let image_ct = enc(
        "messages.0.content.1.image_url.url",
        b"data:image/png;base64,iVBORw0KGgo=",
    );
    let audio_ct = enc("messages.0.content.2.input_audio.data", b"UklGRAAAAABXQVZF");
    let body = serde_json::json!({
        "model": "aci-model",
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": text_ct},
            {"type": "image_url", "image_url": {"url": image_ct}},
            {"type": "input_audio", "input_audio": {"data": audio_ct, "format": "wav"}},
        ]}],
    });
    let headers = vec![
        ("x-client-pub-key", public_key_from_secret(&client_secret)),
        ("x-model-pub-key", pub_key.clone()),
        ("x-e2ee-version", E2EE_VERSION_V2.to_string()),
        ("x-e2ee-nonce", nonce.to_string()),
        ("x-e2ee-timestamp", timestamp.to_string()),
    ];

    let resp = h
        .requester
        .post_owned_headers(
            "/v1/chat/completions",
            &serde_json::to_vec(&body).unwrap(),
            &headers,
        )
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    let upstream: Value = serde_json::from_slice(&upstream_body).unwrap();
    let content = &upstream["messages"][0]["content"];
    assert_eq!(content[0]["text"], "look at this");
    assert_eq!(
        content[1]["image_url"]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
    assert_eq!(content[2]["input_audio"]["data"], "UklGRAAAAABXQVZF");
    // Non-encrypted sibling members pass through untouched.
    assert_eq!(content[2]["input_audio"]["format"], "wav");
}

#[tokio::test]
async fn e2ee_v2_response_encrypts_message_audio_data() {
    // Buffered chat responses encrypt `choices.{i}.message.audio.data` (§7.2).
    let audio_response = br#"{"id":"chat-aci-1","object":"chat.completion","model":"aci-model","choices":[{"index":0,"message":{"role":"assistant","audio":{"id":"audio-1","data":"QUJDMTIzYXVkaW8="}},"finish_reason":"stop"}]}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(audio_response));
    let client_secret = k256::SecretKey::from_slice(&[0x65; 32]).unwrap();
    let nonce = hex_nonce("nonce-response-audio");
    let nonce = nonce.as_str();
    let (encrypted_body, headers) = e2ee_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");

    let encrypted_response = json_body(&resp);
    let encrypted_audio = encrypted_response["choices"][0]["message"]["audio"]["data"]
        .as_str()
        .expect("message.audio.data must be an encrypted hex string");
    assert_ne!(encrypted_audio, "QUJDMTIzYXVkaW8=");
    let audio_aad = e2ee_response_aad(&h, nonce, "chat-aci-1", "choices.0.message.audio.data");
    let decrypted = decrypt_with_secret_key(&client_secret, encrypted_audio, &audio_aad).unwrap();
    assert_eq!(decrypted, b"QUJDMTIzYXVkaW8=");
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
    let encrypted_reasoning = encrypted_response["choices"][0]["message"]["reasoning"]
        .as_str()
        .unwrap();
    let decrypted_reasoning =
        decrypt_legacy_ecdsa_with_secret_key(&client_secret, encrypted_reasoning, None).unwrap();
    assert_eq!(decrypted_reasoning, b"private-thought");
}

#[tokio::test]
async fn legacy_signing_algo_with_nonce_is_rejected_as_removed_v2() {
    // The AAD-bound legacy variant (LegacyV2) is removed: a legacy X-Signing-Algo
    // request that carries a nonce/timestamp is rejected, rather than silently
    // decrypted without AAD. Such clients must drop X-Signing-Algo and use the
    // ACI path instead.
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = k256::SecretKey::from_slice(&[0x62; 32]).unwrap();
    let (body, mut headers) = legacy_ecdsa_request(&h, &client_secret);
    headers.push(("x-e2ee-nonce", "any-nonce".to_string()));
    headers.push(("x-e2ee-timestamp", "1700000000".to_string()));

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_version");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
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
    let (_body, mut headers) = e2ee_request(&h, &client_secret, &hex_nonce("nonce-version"));
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
    let (_body, mut headers) = e2ee_request(&h, &client_secret, &hex_nonce("nonce-timestamp"));
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
    let (body, headers) = e2ee_request(&h, &client_secret, &hex_nonce("nonce-replay"));
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
    // JCS AAD needs no escaping, so `model` is rejected only when it is absent
    // or not a string (spec §7.3), never for its contents.
    let invalid = br#"{"model":123,"messages":[]}"#;
    let client_pub = public_key_from_secret(&client_secret);
    let nonce = hex_nonce("payload-model");
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            invalid,
            &[
                ("x-client-pub-key", &client_pub),
                ("x-model-pub-key", &model_key.public_key_hex),
                ("x-e2ee-version", "2"),
                ("x-e2ee-nonce", &nonce),
                ("x-e2ee-timestamp", "1700000000"),
            ],
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_payload_model");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v2_malformed_nonce_is_rejected_before_upstream() {
    // §7.5: the nonce must be exactly 64 lowercase hex characters.
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = k256::SecretKey::from_slice(&[0x60; 32]).unwrap();
    let (_body, mut headers) = e2ee_request(&h, &client_secret, &hex_nonce("valid-nonce"));
    headers
        .iter_mut()
        .find(|(name, _)| *name == "x-e2ee-nonce")
        .unwrap()
        .1 = "not-64-hex".to_string();
    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", CHAT_REQUEST, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_nonce");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

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
    let payload = receipt_payload(&receipt);
    // The wire hash covers the raw in-order stream including framing (§7.4).
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(&resp.body)
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

// ---------------------------------------------------------------------------
// Keyset / TLS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plaintext_https_keyset_publishes_configured_tls_spki() {
    let h = harness();
    let tls_keys = &h.service.keyset().tls_public_keys;
    assert_eq!(tls_keys.len(), 1);
    assert_eq!(tls_keys[0].domain, None);
    assert_eq!(tls_keys[0].spki_sha256_hex, "configured-spki-sha256-hex");
}

// ---------------------------------------------------------------------------
// legacy signature endpoint
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// /v1/completions and /v1/embeddings surfaces (plaintext)
// ---------------------------------------------------------------------------

const EMBEDDINGS_REQUEST: &[u8] = br#"{"model":"aci-model","input":"the quick brown fox"}"#;
const EMBEDDINGS_PLAIN_RESPONSE: &[u8] =
    br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.5,-0.25]}],"model":"aci-model","usage":{"prompt_tokens":3,"total_tokens":3}}"#;

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
    let payload = receipt_payload(&receipt);
    assert_eq!(payload["endpoint"], "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-aci-1"));
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(request)
    );
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
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

    let receipt = h.service.get_receipt_by_receipt_id(&receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    assert_eq!(payload["endpoint"], "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-stream-1"));
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(expected_body)
    );

    let receipt_response = h.requester.get("/v1/signature/chat-stream-1", &[]).await;
    assert_eq!(receipt_response.status, StatusCode::OK);
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
    let payload = receipt_payload(&receipt);
    assert_eq!(payload["endpoint"], "/v1/embeddings");
    // OpenAI embeddings responses carry no `id`; the gateway leaves
    // the receipt chat_id empty for those.
    assert!(receipt.chat_id.is_none());
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(EMBEDDINGS_REQUEST)
    );
    assert_eq!(
        payload_event(&payload, "request.forwarded")["body_hash"],
        sha256_hex(EMBEDDINGS_REQUEST)
    );
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
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
    let payload = body["receipt"].clone();
    assert_eq!(
        payload["receipt_id"].as_str().unwrap(),
        receipt_id,
        "receipt lookup by receipt_id must return the same receipt"
    );
    assert!(
        payload["chat_id"].is_null(),
        "embeddings receipts have no chat_id"
    );
    assert_eq!(payload["endpoint"], "/v1/embeddings");

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
async fn embeddings_endpoint_supports_legacy_v1_e2ee() {
    let e2ee_embeddings_response: &[u8] = br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.5,-0.25,1.0]}],"model":"aci-model","usage":{"prompt_tokens":5,"total_tokens":5}}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(
        e2ee_embeddings_response,
    ));
    let client_secret = k256::SecretKey::from_slice(&[0x73; 32]).unwrap();
    let model_key = legacy_model_public_key(&h, E2EE_ALGO_LEGACY_ECDSA);
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
            legacy_ecdsa_public_key_from_secret(&client_secret),
        ),
        ("x-model-pub-key", model_key),
    ];

    let resp = h
        .requester
        .post_owned_headers(
            "/v1/embeddings",
            &serde_json::to_vec(&body).unwrap(),
            &headers,
        )
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

// ---------------------------------------------------------------------------
// Serving constraints (§5.3)
// ---------------------------------------------------------------------------

/// `harness_with`, but the service default is unverified serving — the
/// tighten-only surface a client constraint must override.
fn harness_serving_optional(verifier: Arc<dyn UpstreamVerifier>) -> Harness {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let upstream = RecordingUpstream::default();
    let upstream_calls = upstream.calls();
    let cfg = AciServiceConfig::for_test();
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            quoter,
            Arc::new(upstream),
            verifier,
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

const CONSTRAINED_REQUEST: &[u8] = br#"{"model":"aci-model","messages":[{"role":"user","content":"hi"}],"provider":{"aci_verified":true}}"#;

#[tokio::test]
async fn provider_member_is_stripped_and_recorded_as_the_rewrite() {
    let h = harness();
    let resp = h
        .requester
        .post("/v1/chat/completions", CONSTRAINED_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);

    // The whole member is consumed and stripped before forwarding (§5.3).
    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    let upstream_json = serde_json::from_slice::<Value>(&upstream_body).unwrap();
    assert!(upstream_json.get("provider").is_none());

    // The strip appears as the §7.4 rewrite; received covers the client bytes.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(CONSTRAINED_REQUEST)
    );
    assert_ne!(
        payload_event(&payload, "request.received")["body_hash"],
        payload_event(&payload, "request.forwarded")["body_hash"]
    );
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["required"], true);
    assert_eq!(verified["result"], "verified");
}

#[tokio::test]
async fn unknown_aci_member_is_rejected_never_ignored() {
    let h = harness();
    let body = br#"{"model":"aci-model","messages":[],"provider":{"aci_bogus":true}}"#;
    let resp = h.requester.post("/v1/chat/completions", body, &[]).await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "invalid_request_error");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_aci_session_ids_list_is_rejected() {
    let h = harness();
    let body = br#"{"model":"aci-model","messages":[],"provider":{"aci_session_ids":[]}}"#;
    let resp = h.requester.post("/v1/chat/completions", body, &[]).await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "invalid_request_error");
}

#[tokio::test]
async fn pinned_current_session_serves_and_is_cited() {
    let h = harness();
    // Learn the channel's current session id from an unconstrained request.
    let first = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(first.status, StatusCode::OK);
    let receipt_id = header(&first.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let session_id = payload_event(&receipt_payload(&receipt), "upstream.verified")["session_id"]
        .as_str()
        .expect("verified receipt cites a session")
        .to_string();

    let body = format!(
        r#"{{"model":"aci-model","messages":[],"provider":{{"aci_session_ids":["{session_id}"]}}}}"#
    );
    let resp = h
        .requester
        .post("/v1/chat/completions", body.as_bytes(), &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let verified = payload_event(&receipt_payload(&receipt), "upstream.verified").clone();
    assert_eq!(verified["session_id"], Value::String(session_id));
}

#[tokio::test]
async fn unmatched_pinned_sessions_refuse_with_session_not_accepted() {
    let h = harness();
    let bogus = "ab".repeat(32);
    let body = format!(
        r#"{{"model":"aci-model","messages":[],"provider":{{"aci_session_ids":["{bogus}"]}}}}"#
    );
    let resp = h
        .requester
        .post("/v1/chat/completions", body.as_bytes(), &[])
        .await;
    assert_eq!(resp.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(error_type(&resp), "session_not_accepted");
    // The prompt was not forwarded (§5.3).
    assert!(h.upstream_calls.lock().unwrap().is_empty());

    // The refusal is recorded like any other (§7.5): failed form, no session,
    // response hash over the exact error body served.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["result"], "failed");
    assert!(verified.get("session_id").is_none());
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn aci_verified_makes_an_unconstrained_request_fail_closed() {
    // Unconstrained: the upstream is still verified and the outcome recorded,
    // but a failure does not refuse (§1.2).
    let h = harness_serving_optional(Arc::new(AlwaysFailed));
    let resp = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let verified = payload_event(&receipt_payload(&receipt), "upstream.verified").clone();
    assert_eq!(verified["required"], false);
    assert_eq!(verified["result"], "failed");

    // The same request with aci_verified tightens to fail-closed (§5.3).
    let resp = h
        .requester
        .post("/v1/chat/completions", CONSTRAINED_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_type(&resp), "upstream_verification_failed");
}

// --- SSE stream-repair suite (restored from main: c643fb5, 3ea864e, c26878d) ---

#[tokio::test]
async fn a_stream_cut_mid_event_is_not_given_an_error() {
    let h = harness_with_upstream(RecordingUpstream::with_stream_chunks(vec![
        Bytes::from_static(b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hel\"}\n\n"),
        Bytes::from_static(b"data: {\"id\":\"chat-stream-1\","),
    ]));
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
            &[],
        )
        .await;

    assert_eq!(
        resp.body,
        b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hel\"}\n\ndata: {\"id\":\"chat-stream-1\","
    );
}

// The streaming entry point treats a response with no declared media type as an
// event stream, so the missing chat terminator is still supplied on a clean end.
#[tokio::test]
async fn a_stream_without_a_content_type_is_still_observed() {
    let h = harness_with_upstream(
        RecordingUpstream::with_stream_chunks(vec![Bytes::from_static(
            b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hi\"}\n\n",
        )])
        .without_content_type(),
    );
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
            &[],
        )
        .await;
    let body = String::from_utf8(resp.body.clone()).unwrap();
    assert!(!body.contains("\"error\""), "{body}");
    assert!(
        body.ends_with("data: [DONE]\n\n"),
        "the terminator is supplied: {body}"
    );
}

// An error-shaped finish reason without `[DONE]`, then a clean end, must not be
// stamped with a success terminator: the finalizer detects the error (as the
// meter does) and appends nothing, so a signed receipt never frames an upstream
// error as a completed response.
#[tokio::test]
async fn an_error_finish_reason_is_not_completed_with_done() {
    let h = harness_with_upstream(RecordingUpstream::with_stream_chunks(vec![
        Bytes::from_static(b"data: {\"choices\":[{\"finish_reason\":\"upstream_error\"}]}\n\n"),
    ]));
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
            &[],
        )
        .await;

    let body = String::from_utf8(resp.body.clone()).unwrap();
    assert!(
        !body.contains("[DONE]"),
        "no success terminator over an error: {body}"
    );
    assert!(
        body.ends_with("\"finish_reason\":\"upstream_error\"}]}\n\n"),
        "nothing appended: {body}"
    );
}

// An OpenAI chat upstream that closes normally without ever sending `[DONE]`
// has finished, not truncated: `[DONE]` is a fixed marker the gateway can
// supply. It completes the framing rather than injecting an error, and because
// that happens inside the finalizer the receipt commits to exactly the bytes
// the client received — appended terminator included.
#[tokio::test]
async fn a_chat_stream_missing_done_is_completed_inside_the_receipt() {
    let h = harness_with_upstream(RecordingUpstream::with_stream_chunks(vec![
        Bytes::from_static(b"data: {\"id\":\"chat-stream-1\",\"delta\":\"hel\"}\n\n"),
        Bytes::from_static(b"data: {\"id\":\"chat-stream-1\",\"delta\":\"lo\"}\n\n"),
    ]));
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
            &[],
        )
        .await;

    assert_eq!(resp.status, StatusCode::OK);
    let body = String::from_utf8(resp.body.clone()).unwrap();
    assert!(
        !body.contains("\"error\""),
        "a clean close is not an error: {body}"
    );
    assert!(
        body.ends_with("data: [DONE]\n\n"),
        "the missing terminator is supplied: {body}"
    );

    // §7.4 `response.returned` covers the bytes actually served, so the
    // appended terminator is inside the signature — not just what the upstream
    // sent. A clean stream is signed.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(
        payload_event(&receipt_payload(&receipt), "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
}

// An Anthropic stream that closes cleanly without `message_stop` has finished,
// so it is completed and signed — but the gateway appends nothing: fabricating
// a `message_stop` would assert an end the upstream never sent. The client gets
// exactly what the upstream sent, then EOF, as litellm's Anthropic passthrough
// also does.
#[tokio::test]
async fn an_anthropic_stream_missing_message_stop_is_completed_without_a_tail() {
    let h = harness_with_upstream(RecordingUpstream::with_stream_chunks(vec![
        Bytes::from_static(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n",
        ),
    ]));
    let resp = h
        .requester
        .post(
            "/v1/messages",
            br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
            &[],
        )
        .await;

    let body = String::from_utf8(resp.body.clone()).unwrap();
    assert!(
        !body.contains("event: error"),
        "a clean close is not an error: {body}"
    );
    assert!(
        !body.contains("message_stop"),
        "no terminal is fabricated: {body}"
    );
    assert!(
        body.ends_with("data: {\"type\":\"content_block_delta\"}\n\n"),
        "nothing appended: {body}"
    );
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(
        payload_event(&receipt_payload(&receipt), "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
}

// A Responses stream that closes without `response.completed` also completes,
// but its terminal carries the final response object and cannot be fabricated,
// so nothing is appended — no error, and the receipt covers the bytes as sent.
#[tokio::test]
async fn a_responses_stream_missing_its_terminal_is_completed_without_a_tail() {
    let h = harness_with_upstream(RecordingUpstream::with_stream_chunks(vec![
        Bytes::from_static(
            b"data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"delta\":\"hi\"}\n\n",
        ),
    ]));
    let resp = h
        .requester
        .post(
            "/v1/responses",
            br#"{"model":"aci-model","stream":true,"input":"hi"}"#,
            &[],
        )
        .await;

    let body = String::from_utf8(resp.body.clone()).unwrap();
    assert!(
        !body.contains("\"error\""),
        "a clean close is not an error: {body}"
    );
    assert!(
        body.ends_with("\"delta\":\"hi\"}\n\n"),
        "nothing appended (the terminal cannot be fabricated): {body}"
    );
    let receipt_id = header(&resp.headers, "x-receipt-id");
    assert!(
        h.service.get_receipt_by_receipt_id(receipt_id).is_some(),
        "a clean end is signed"
    );
}

// A stream that ended mid-event is left alone: supplying the delimiter the
// upstream never sent would dispatch a fragment the client would otherwise
// discard, and it would fail on that instead of reading the error.
// A transport failure reaches the finalizer, which holds the protocol state
// that decides whether the client can still be told. It appends the error only
// when the stream stopped on a complete event without its terminal; a stream
// that already ended, or stopped mid-event, is left as it is.
#[tokio::test]
async fn a_transport_failure_is_told_to_the_client_only_when_it_is_safe() {
    struct Case {
        name: &'static str,
        path: &'static str,
        request: &'static [u8],
        chunks: Vec<Bytes>,
        expect_error: bool,
        /// Substring the appended error must contain, when one is expected.
        expect_in_error: &'static str,
    }
    let chat =
        br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let responses = br#"{"model":"aci-model","stream":true,"input":"hi"}"#;
    let cases = vec![
        Case {
            name: "already terminated, then the connection drops",
            path: "/v1/chat/completions",
            request: chat,
            chunks: vec![
                Bytes::from_static(b"data: {\"id\":\"c\",\"delta\":\"hi\"}\n\n"),
                Bytes::from_static(b"data: [DONE]\n\n"),
            ],
            expect_error: false,
            expect_in_error: "",
        },
        Case {
            name: "content with no completion reason, then the connection drops",
            path: "/v1/chat/completions",
            request: chat,
            chunks: vec![Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            )],
            expect_error: true,
            expect_in_error: "data: [DONE]",
        },
        Case {
            // A finish reason does not rescue a broken connection: the transport
            // failed, so the client is told, whatever content-level signal came
            // before it.
            name: "a finish reason, then the connection drops",
            path: "/v1/chat/completions",
            request: chat,
            chunks: vec![Bytes::from_static(
                b"data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
            )],
            expect_error: true,
            expect_in_error: "data: [DONE]",
        },
        Case {
            name: "mid-event, then the connection drops",
            path: "/v1/chat/completions",
            request: chat,
            chunks: vec![Bytes::from_static(b"data: {\"id\":\"c\",")],
            expect_error: false,
            expect_in_error: "",
        },
        Case {
            // The error continues the event numbering rather than restarting
            // it, which is why the finalizer builds it: only there is the last
            // sequence known.
            name: "responses stream drops after sequence 4",
            path: "/v1/responses",
            request: responses,
            chunks: vec![Bytes::from_static(
                b"data: {\"type\":\"response.output_text.delta\",\"sequence_number\":4}\n\n",
            )],
            expect_error: true,
            expect_in_error: "\"sequence_number\":5",
        },
    ];
    for case in cases {
        let h = harness_with_upstream(RecordingUpstream::with_stream_chunks_then_error(
            case.chunks.clone(),
        ));
        let resp = h.requester.post(case.path, case.request, &[]).await;
        let body = String::from_utf8(resp.body.clone()).unwrap();
        assert_eq!(
            body.contains("\"error\""),
            case.expect_error,
            "{}: {body:?}",
            case.name
        );
        assert!(
            body.contains(case.expect_in_error),
            "{}: expected {:?} in {body:?}",
            case.name,
            case.expect_in_error
        );
        // A failed stream is signed too (§7.4): the receipt attests the
        // exact wire bytes — including any synthesized error tail — which is
        // what §6.5's truncation check relies on.
        let receipt_id = header(&resp.headers, "x-receipt-id");
        let receipt = h
            .service
            .get_receipt_by_receipt_id(receipt_id)
            .unwrap_or_else(|| panic!("{}: a broken stream still gets its receipt", case.name));
        assert_eq!(
            payload_event(&receipt_payload(&receipt), "response.returned")["body_hash"],
            sha256_hex(&resp.body),
            "{}: the receipt covers the bytes actually served",
            case.name
        );
    }
}

// The empty-tool-calls normalization is live code driving `forwarded_body`;
// the receipt commits to the rewrite as the §7.4 hash difference.
#[tokio::test]
async fn empty_tool_calls_are_stripped_and_recorded_as_the_rewrite() {
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
    let payload = receipt_payload(&receipt);
    assert_ne!(
        payload_event(&payload, "request.received")["body_hash"],
        payload_event(&payload, "request.forwarded")["body_hash"],
        "the strip is the §7.4 rewrite"
    );
}

// §1.2: a route not classed as attested is not a candidate for a constrained
// request — refused before the verifier ever runs, nothing forwarded.
#[tokio::test]
async fn aci_constraint_refuses_a_route_not_classed_as_attested() {
    let h = harness_with_upstream(RecordingUpstream {
        is_tee: Some(false),
        ..RecordingUpstream::default()
    });
    let body = br#"{"model":"aci-model","messages":[],"provider":{"aci_verified":true}}"#;

    let resp = h.requester.post("/v1/chat/completions", body, &[]).await;
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_type(&resp), "upstream_verification_failed");
    assert!(
        h.upstream_calls.lock().unwrap().is_empty(),
        "a non-TEE route must never receive a constrained prompt"
    );
}

// §5.3: a direct service satisfies `aci_verified: true` by construction —
// served, no `upstream.verified` event, no session; a pinned list still
// refuses (§5.3 "a direct service has no sessions").
#[tokio::test]
async fn direct_service_satisfies_aci_verified_by_construction() {
    let mut cfg = AciServiceConfig::for_test();
    cfg.service_capabilities.serving = "direct".to_string();
    let upstream = RecordingUpstream::default();
    let calls = upstream.calls();
    let service = Arc::new(
        AciService::new(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(upstream),
            Arc::new(InMemoryReceiptStore::default()),
            cfg,
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let h = Harness {
        requester: Requester {
            app: build_router(service.clone()),
        },
        service,
        upstream_calls: calls,
    };

    let body = br#"{"model":"aci-model","messages":[],"provider":{"aci_verified":true}}"#;
    let resp = h.requester.post("/v1/chat/completions", body, &[]).await;
    assert_eq!(resp.status, StatusCode::OK, "{:?}", resp.body);
    assert_eq!(h.upstream_calls.lock().unwrap().len(), 1);
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    assert!(
        !payload["event_log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "upstream.verified"),
        "a direct service's receipts carry no upstream.verified event (§4.1)"
    );

    let pinned = format!(
        r#"{{"model":"aci-model","messages":[],"provider":{{"aci_session_ids":["{}"]}}}}"#,
        "ab".repeat(32)
    );
    let resp = h
        .requester
        .post("/v1/chat/completions", pinned.as_bytes(), &[])
        .await;
    assert_eq!(resp.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(error_type(&resp), "session_not_accepted");
}

// --- restored from main: E2EE coverage on the other prompt endpoints ---

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

const E2EE_COMPLETION_RESPONSE: &[u8] = br#"{"id":"cmpl-aci-1","object":"text_completion","model":"aci-model","choices":[{"index":0,"text":"completion-answer","finish_reason":"stop"}]}"#;

const E2EE_EMBEDDINGS_RESPONSE: &[u8] =
    br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.5,-0.25,1.0]}],"model":"aci-model","usage":{"prompt_tokens":5,"total_tokens":5}}"#;

fn e2ee_embeddings_request_string(
    h: &Harness,
    client_secret: &k256::SecretKey,
    nonce: &str,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model = "aci-model";
    let timestamp = 1_700_000_000u64;
    let model_key = &h.service.keyset().e2ee_public_keys[0];
    let aad = aci_request_aad(&model_key.algo, model, "input", nonce, timestamp);
    let encrypted_input =
        encrypt_for_public_key(&model_key.public_key_hex, b"hello", &aad).unwrap();
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
    let aad_0 = aci_request_aad(&model_key.algo, model, "input.0", nonce, timestamp);
    let aad_1 = aci_request_aad(&model_key.algo, model, "input.1", nonce, timestamp);
    let enc_0 = encrypt_for_public_key(&model_key.public_key_hex, b"first", &aad_0).unwrap();
    let enc_1 = encrypt_for_public_key(&model_key.public_key_hex, b"second", &aad_1).unwrap();
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

// E2EE stream encryption only knows how to encrypt SSE event payloads. An
// upstream that answered `application/json` on a streaming request cannot be
// encrypted, so the request is rejected rather than returned in the clear or
// bound to a receipt — the same rule the middleware path enforces.
#[tokio::test]
async fn e2ee_streaming_rejects_a_non_sse_upstream_without_a_receipt() {
    let h = harness_with_e2ee(
        RecordingUpstream::with_stream_chunks(vec![Bytes::from_static(
            br#"{"id":"chat-1","choices":[{"message":{"content":"hi"}}]}"#,
        )])
        .with_content_type("application/json"),
    );
    let client_secret = k256::SecretKey::from_slice(&[0x5b; 32]).unwrap();
    let nonce = hex_nonce("nonce-json");
    let (encrypted_body, headers) = e2ee_stream_request(&h, &client_secret, nonce.as_str());

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;

    assert_ne!(
        resp.status,
        StatusCode::OK,
        "the combination must be rejected"
    );
    assert!(
        resp.headers.get("x-receipt-id").is_none(),
        "a rejected E2EE response is not bound to a receipt"
    );
}

#[tokio::test]
async fn e2ee_v2_encrypts_streamed_reasoning_field() {
    let frame = format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": "chat-reasoning-stream-1",
            "object": "chat.completion.chunk",
            "model": "aci-model",
            "choices": [{
                "index": 7,
                "delta": {"reasoning": "private streamed thought"},
            }],
        })
    )
    .into_bytes();
    let stream_chunks = vec![Bytes::from(frame), Bytes::from_static(b"data: [DONE]\n\n")];
    let h = harness_with_e2ee(RecordingUpstream::with_stream_chunks(stream_chunks));
    let client_secret = k256::SecretKey::from_slice(&[0x67; 32]).unwrap();
    let nonce = hex_nonce("nonce-reasoning-stream");
    let nonce = nonce.as_str();
    let (encrypted_body, headers) = e2ee_stream_request(&h, &client_secret, nonce);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);

    let events = sse_json_events(&resp.body);
    assert_eq!(events.len(), 1);
    let encrypted_reasoning = events[0]["choices"][0]["delta"]["reasoning"]
        .as_str()
        .unwrap();
    let response_aad = e2ee_response_aad(
        &h,
        nonce,
        "chat-reasoning-stream-1",
        "choices.7.delta.reasoning",
    );
    let decrypted_reasoning =
        decrypt_with_secret_key(&client_secret, encrypted_reasoning, &response_aad).unwrap();
    assert_eq!(decrypted_reasoning, b"private streamed thought");
}

#[tokio::test]
async fn completions_endpoint_supports_e2ee_as_optional_add_on() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(
        E2EE_COMPLETION_RESPONSE,
    ));
    let client_secret = k256::SecretKey::from_slice(&[0x5a; 32]).unwrap();
    let nonce = hex_nonce("nonce-completion");
    let nonce = nonce.as_str();
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
    let response_aad = e2ee_response_aad(&h, nonce, "cmpl-aci-1", "choices.0.text");
    let decrypted_response =
        decrypt_with_secret_key(&client_secret, encrypted_text, &response_aad).unwrap();
    assert_eq!(decrypted_response, b"completion-answer");

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt_payload(&receipt)["endpoint"], "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("cmpl-aci-1"));
    assert_eq!(
        receipt_event(&receipt, "request.received")["body_hash"],
        sha256_hex(&upstream_body)
    );
    assert_eq!(
        receipt_event(&receipt, "response.returned")["body_hash"],
        sha256_hex(&resp.body),
        "§7.4: the receipt covers the wire bytes"
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
    let nonce = hex_nonce("nonce-completion-stream");
    let nonce = nonce.as_str();
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
    let aad = e2ee_response_aad(&h, nonce, "cmpl-stream-1", "choices.0.text");
    let decrypted_text = decrypt_with_secret_key(&client_secret, encrypted_text, &aad).unwrap();
    assert_eq!(decrypted_text, b"completion-stream");

    let upstream_body = h.upstream_calls.lock().unwrap()[0].body.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&upstream_body).unwrap()["prompt"],
        "hello"
    );
    let receipt_id = header(&resp.headers, "x-receipt-id").to_string();
    let receipt = h.service.get_receipt_by_receipt_id(&receipt_id).unwrap();
    assert_eq!(receipt_payload(&receipt)["endpoint"], "/v1/completions");
    assert_eq!(receipt.chat_id.as_deref(), Some("cmpl-stream-1"));
    assert_eq!(
        receipt_event(&receipt, "response.returned")["body_hash"],
        sha256_hex(&resp.body),
        "§7.4: the receipt covers the raw ordered wire bytes"
    );
    // the cleartext differs from the encrypted wire — the layer did seal
    assert_ne!(sha256_hex(&cleartext_stream), sha256_hex(&resp.body));
}

#[tokio::test]
async fn embeddings_endpoint_supports_aci_v2_e2ee_with_string_input() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(
        E2EE_EMBEDDINGS_RESPONSE,
    ));
    let client_secret = k256::SecretKey::from_slice(&[0x71; 32]).unwrap();
    let nonce = hex_nonce("nonce-embed-string");
    let nonce = nonce.as_str();
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
    let response_aad = e2ee_response_aad(&h, nonce, "", "data.0.embedding");
    let decrypted =
        decrypt_with_secret_key(&client_secret, encrypted_embedding, &response_aad).unwrap();
    let decoded: Value = serde_json::from_slice(&decrypted).unwrap();
    assert_eq!(decoded, serde_json::json!([0.5, -0.25, 1.0]));

    // Receipt records cleartext + wire hashes of the response, like chat.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt_payload(&receipt)["endpoint"], "/v1/embeddings");
    assert_eq!(
        receipt_event(&receipt, "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn embeddings_endpoint_supports_aci_v2_e2ee_with_array_input() {
    let response_with_two = br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[1.0]},{"object":"embedding","index":1,"embedding":[2.0]}],"model":"aci-model","usage":{"prompt_tokens":4,"total_tokens":4}}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(response_with_two));
    let client_secret = k256::SecretKey::from_slice(&[0x72; 32]).unwrap();
    let nonce = hex_nonce("nonce-embed-array");
    let nonce = nonce.as_str();
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

    let encrypted_response = json_body(&resp);
    for (idx, expected) in [(0u64, [1.0]), (1u64, [2.0])] {
        let ciphertext = encrypted_response["data"][idx as usize]["embedding"]
            .as_str()
            .unwrap();
        let response_aad = e2ee_response_aad(&h, nonce, "", &format!("data.{idx}.embedding"));
        let plaintext = decrypt_with_secret_key(&client_secret, ciphertext, &response_aad).unwrap();
        let decoded: Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(decoded, serde_json::json!(expected));
    }
}

// The inherited wrapper embeds the receipt document, which still carries the
// upstream `chat_id` a legacy client looks the exchange up by.
#[tokio::test]
async fn legacy_signature_wrapper_still_embeds_the_chat_id() {
    let h = harness();
    let chat = h
        .requester
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(chat.status, StatusCode::OK);

    let sig = h.requester.get("/v1/signature/chat-aci-1", &[]).await;
    assert_eq!(sig.status, StatusCode::OK);
    assert_eq!(json_body(&sig)["receipt"]["chat_id"], "chat-aci-1");
}

// `enable_e2ee` gates ACI E2EE (§6) only. The inherited dstack-vllm-proxy
// path predates it and stays available, so existing clients keep working on a
// deployment that has not turned the ACI scheme on.
#[tokio::test]
async fn legacy_v1_e2ee_still_works_when_aci_e2ee_is_off() {
    let h = harness(); // supported_e2ee_versions is empty here
    assert!(h.service.supported_e2ee_versions().is_empty());

    let client_secret = k256::SecretKey::from_slice(&[0x62; 32]).unwrap();
    let (encrypted_body, headers) = legacy_ecdsa_request(&h, &client_secret);
    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &encrypted_body, &headers)
        .await;

    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-e2ee-version"), "1");
}
