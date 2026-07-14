//! Service-side ACI surface coverage.
//!
//! This file deliberately excludes the relying-party verification procedure
//! from ACI §10. It covers the service behavior that an ACI aggregator should
//! expose: reports, receipts (as §8.2 envelopes), response headers, the §13
//! legacy compatibility surfaces, and the plaintext, ACI E2EE v3 (§7), and
//! legacy E2EE request paths.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use futures_util::stream;
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    decrypt_legacy_ecdsa_with_secret_key, encrypt_legacy_for_public_key,
    legacy_ecdsa_public_key_from_secret, seal_v3, unseal_v3, x25519_public_key_from_hex,
    x25519_public_key_hex, x25519_secret_key_from_bytes, E2EE_ALGO_LEGACY_ECDSA,
    E2EE_ALGO_X25519_AESGCM, E2EE_CONTEXT_REQUEST, E2EE_CONTEXT_RESPONSE,
};
use private_ai_gateway::aci::keys::verify_receipt_signature;
use private_ai_gateway::aci::receipt::{SignedReceipt, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::types::{ServiceCapabilities, TlsSpki};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
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

fn receipt_payload(receipt: &SignedReceipt) -> Value {
    receipt.payload_json().unwrap()
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

/// The attested §7.1 X25519 service key a v3 client encrypts to.
fn v3_model_public_key(h: &Harness) -> String {
    h.service
        .keyset()
        .e2ee_public_keys
        .iter()
        .find(|key| key.algo == E2EE_ALGO_X25519_AESGCM)
        .unwrap()
        .public_key_hex
        .clone()
}

/// Seal `plaintext` per §7.2 and return the envelope body plus the three
/// E2EE headers (§6.1).
fn v3_sealed_request(
    h: &Harness,
    model: &str,
    plaintext: &[u8],
    client_secret: &x25519_dalek::StaticSecret,
) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let model_key = v3_model_public_key(h);
    let recipient = x25519_public_key_from_hex(&model_key).unwrap();
    let client_key = x25519_public_key_hex(client_secret);
    let ctx = E2EE_CONTEXT_REQUEST;
    let sealed = seal_v3(&recipient, ctx, model, Some(&client_key), plaintext).unwrap();
    let body = serde_json::json!({ "model": model, "sealed_b64": BASE64.encode(sealed) });
    let headers = vec![
        ("x-e2ee-version", "3".to_string()),
        ("x-client-pub-key", client_key),
        ("x-model-pub-key", model_key),
    ];
    (serde_json::to_vec(&body).unwrap(), headers)
}

/// Unseal one §7.3 response envelope (a buffered body or one SSE data payload).
fn v3_unseal_response(
    client_secret: &x25519_dalek::StaticSecret,
    model: &str,
    wire: &[u8],
) -> Vec<u8> {
    let envelope: Value = serde_json::from_slice(wire).unwrap();
    let object = envelope.as_object().unwrap();
    assert_eq!(
        object.keys().collect::<Vec<_>>(),
        vec!["sealed_b64"],
        "§7.3 response envelope carries only sealed_b64"
    );
    let sealed = BASE64
        .decode(object["sealed_b64"].as_str().unwrap())
        .unwrap();
    unseal_v3(client_secret, E2EE_CONTEXT_RESPONSE, model, None, &sealed).unwrap()
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
        .get("/v1/aci/attestation?nonce=fresh-nonce_1", &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    let report = json_body(&resp);
    assert_eq!(report["api_version"], "aci/1");
    // No legacy compatibility fields on the canonical report.
    assert!(report.get("signing_address").is_none());
    assert!(report.get("all_attestations").is_none());
    assert!(report.get("workload_id").is_none());

    // The decoded keyset bytes hash to the restated digest (§5.1).
    let keyset_bytes = BASE64
        .decode(
            report["attestation"]["workload_keyset_b64"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        report["workload_keyset_digest"].as_str().unwrap(),
        sha256_hex(&keyset_bytes)
    );
    assert_eq!(keyset_bytes, h.service.keyset_bytes());

    // report_data = sha256 of the §4.2 statement for the supplied nonce.
    let statement = private_ai_gateway::aci::identity::attestation_statement(
        h.service.workload_keyset_digest(),
        Some("fresh-nonce_1"),
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
    for bad in ["with%20space", "quote%22here", "plus%2Bplus"] {
        let resp = h
            .requester
            .get(&format!("/v1/aci/attestation?nonce={bad}"), &[])
            .await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST, "nonce {bad}");
    }
    // 128 chars is the limit; 129 is rejected.
    let long_ok = "a".repeat(128);
    let resp = h
        .requester
        .get(&format!("/v1/aci/attestation?nonce={long_ok}"), &[])
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    let too_long = "a".repeat(129);
    let resp = h
        .requester
        .get(&format!("/v1/aci/attestation?nonce={too_long}"), &[])
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
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
    // The verified event is slim (§8.5): a session citation, no inline detail.
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["result"], "verified");
    assert!(verified["session_id"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert!(verified.get("channel_bindings").is_none());
    assert!(verified.get("claims").is_none());
    assert!(verified.get("evidence").is_none());
}

#[tokio::test]
async fn aci_receipt_endpoint_serves_signed_bytes_envelope() {
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
    let envelope = json_body(&resp);
    let payload_bytes = BASE64
        .decode(envelope["payload_b64"].as_str().unwrap())
        .unwrap();
    let signature = hex::decode(envelope["signature"].as_str().unwrap()).unwrap();
    let key_id = envelope["key_id"].as_str().unwrap();
    let receipt_key = h
        .service
        .keyset()
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == key_id)
        .expect("envelope key_id resolves in the attested keyset");
    assert_eq!(envelope["algo"].as_str().unwrap(), receipt_key.algo);
    assert!(verify_receipt_signature(
        receipt_key,
        &payload_bytes,
        &signature
    ));
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
    assert!(receipt_body["signature"].is_string());
    // The embedded receipt is the §8.2 envelope.
    assert!(receipt_body["receipt"]["payload_b64"].is_string());
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
        })
        .await
        .unwrap();
    // A rewrite IS the hash pair differing (§8.4); nothing else records it and
    // the gateway never stores the post-rewrite body.
    let payload = receipt_payload(&result.receipt);
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(original)
    );
    assert_eq!(
        payload_event(&payload, "request.forwarded")["body_hash"],
        sha256_hex(forwarded)
    );
    assert_ne!(sha256_hex(original), sha256_hex(forwarded));
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
// Fail-closed refusal (§8.5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failed_upstream_verification_returns_502_with_refusal_receipt() {
    let h = harness_with(RecordingUpstream::default(), Arc::new(AlwaysFailed), false);
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            CHAT_REQUEST,
            &[("authorization", "Bearer requester-a")],
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_type(&resp), "upstream_verification_failed");
    // The prompt was not forwarded (§1.2).
    assert!(h.upstream_calls.lock().unwrap().is_empty());

    // The error carries X-Receipt-Id (§6.2) and the refusal receipt hashes the
    // exact error body served, with the §8.5 failed form and no forwarding.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["result"], "failed");
    assert_eq!(verified["required"], true);
    assert_eq!(verified["reason"], "quote verification failed");
    assert!(verified.get("session_id").is_none());
    assert!(payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["type"] != "request.forwarded"));

    // The refusal receipt is credential-bound like any other (§8.6).
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
        .post("/v1/chat/completions", CHAT_REQUEST, &[])
        .await;
    assert_eq!(resp.status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_type(&resp), "upstream_verification_failed");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    let verified = payload_event(&payload, "upstream.verified");
    assert_eq!(verified["result"], "failed");
    assert_eq!(verified["reason"], "no enforceable channel binding");
}

// ---------------------------------------------------------------------------
// E2EE gating and the §13 legacy mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2ee_headers_are_rejected_when_service_advertises_no_e2ee_support() {
    let h = harness();
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            CHAT_REQUEST,
            &[("x-e2ee-version", "3")],
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_version");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v2_is_reserved_historical_and_rejected() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let resp = h
        .requester
        .post(
            "/v1/chat/completions",
            CHAT_REQUEST,
            &[
                ("x-e2ee-version", "2"),
                ("x-client-pub-key", &"aa".repeat(32)),
                ("x-model-pub-key", &"bb".repeat(32)),
            ],
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_version");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// ACI E2EE v3 (§7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2ee_v3_buffered_roundtrip_seals_whole_bodies_and_hashes_both_sides() {
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = x25519_secret_key_from_bytes(&[0x71; 32]).unwrap();
    let (body, headers) = v3_sealed_request(&h, "aci-model", CHAT_REQUEST, &client_secret);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(header(&resp.headers, "x-aci-version"), "aci/1");

    // The service unseals to the client's exact original bytes and forwards
    // those (§7.2).
    assert_eq!(h.upstream_calls.lock().unwrap()[0].body, CHAT_REQUEST);

    // The response is one sealed unit to X-Client-Pub-Key, bound to the
    // envelope model (§7.3).
    assert_eq!(
        v3_unseal_response(&client_secret, "aci-model", &resp.body),
        E2EE_CHAT_RESPONSE
    );

    // §8.4: request.received hashes the unsealed original client bytes;
    // response.returned hashes the sealed envelope bytes on the wire.
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    assert_eq!(payload["model"], "aci-model");
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        payload_event(&payload, "request.forwarded")["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn e2ee_v3_receipt_records_the_envelope_model() {
    // §7.2 binds the envelope `model` into the AAD and §8.3 records it as the
    // receipt model — even when the sealed original bytes name another model.
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(E2EE_CHAT_RESPONSE));
    let client_secret = x25519_secret_key_from_bytes(&[0x72; 32]).unwrap();
    let (body, headers) = v3_sealed_request(&h, "envelope-model", CHAT_REQUEST, &client_secret);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(
        v3_unseal_response(&client_secret, "envelope-model", &resp.body),
        E2EE_CHAT_RESPONSE
    );

    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(receipt_payload(&receipt)["model"], "envelope-model");
}

#[tokio::test]
async fn e2ee_v3_streaming_seals_each_event_and_leaves_done_plaintext() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = x25519_secret_key_from_bytes(&[0x73; 32]).unwrap();
    let streaming_request =
        br#"{"model":"aci-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let (body, headers) = v3_sealed_request(&h, "aci-model", streaming_request, &client_secret);

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "content-type"), "text/event-stream");
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");

    // SSE framing stays plaintext; each event's data payload is one sealed
    // unit; the [DONE] sentinel stays plaintext (§7.3).
    let wire = String::from_utf8(resp.body.clone()).unwrap();
    let events: Vec<&str> = wire
        .split("\n\n")
        .filter(|event| !event.is_empty())
        .collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2], "data: [DONE]");
    let expected = [
        br#"{"id":"chat-stream-1","delta":"hel"}"#.as_slice(),
        br#"{"id":"chat-stream-1","delta":"lo"}"#.as_slice(),
    ];
    for (event, expected) in events[..2].iter().zip(expected) {
        let payload = event.strip_prefix("data: ").unwrap();
        assert_eq!(
            v3_unseal_response(&client_secret, "aci-model", payload.as_bytes()),
            expected
        );
    }

    // The wire hash covers the sealed in-order stream including framing (§8.4).
    let receipt_id = header(&resp.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let payload = receipt_payload(&receipt);
    assert_eq!(
        payload_event(&payload, "request.received")["body_hash"],
        sha256_hex(streaming_request)
    );
    assert_eq!(
        payload_event(&payload, "response.returned")["body_hash"],
        sha256_hex(&resp.body)
    );
}

#[tokio::test]
async fn e2ee_v3_embeddings_buffered_roundtrip() {
    let embeddings_response = br#"{"object":"list","model":"aci-model","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2]}]}"#;
    let h = harness_with_e2ee(RecordingUpstream::with_response_body(embeddings_response));
    let client_secret = x25519_secret_key_from_bytes(&[0x74; 32]).unwrap();
    let plaintext = br#"{"model":"aci-model","input":"hello"}"#;
    let (body, headers) = v3_sealed_request(&h, "aci-model", plaintext, &client_secret);

    let resp = h
        .requester
        .post_owned_headers("/v1/embeddings", &body, &headers)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(header(&resp.headers, "x-e2ee-applied"), "true");
    assert_eq!(h.upstream_calls.lock().unwrap()[0].body, plaintext);
    assert_eq!(
        v3_unseal_response(&client_secret, "aci-model", &resp.body),
        embeddings_response
    );
}

#[tokio::test]
async fn e2ee_v3_partial_headers_are_rejected() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_key = x25519_public_key_hex(&x25519_secret_key_from_bytes(&[0x75; 32]).unwrap());
    let model_key = v3_model_public_key(&h);
    let partial_sets: &[&[(&str, &str)]] = &[
        &[("x-e2ee-version", "3")],
        &[("x-e2ee-version", "3"), ("x-client-pub-key", &client_key)],
        &[
            ("x-client-pub-key", &client_key),
            ("x-model-pub-key", &model_key),
        ],
    ];
    for headers in partial_sets {
        let resp = h
            .requester
            .post("/v1/chat/completions", CHAT_REQUEST, headers)
            .await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{headers:?}");
        assert_eq!(error_type(&resp), "e2ee_header_missing", "{headers:?}");
    }
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v3_public_keys_must_parse_as_32_hex_bytes() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_key = x25519_public_key_hex(&x25519_secret_key_from_bytes(&[0x76; 32]).unwrap());
    let model_key = v3_model_public_key(&h);
    let prefixed_model_key = format!("0x{model_key}");
    let bad_key_sets: &[&[(&str, &str)]] = &[
        // Non-hex client key.
        &[
            ("x-e2ee-version", "3"),
            ("x-client-pub-key", "not-hex-at-all"),
            ("x-model-pub-key", &model_key),
        ],
        // Truncated (16-byte) client key.
        &[
            ("x-e2ee-version", "3"),
            ("x-client-pub-key", "aabbccddeeff00112233445566778899"),
            ("x-model-pub-key", &model_key),
        ],
        // Non-hex model key.
        &[
            ("x-e2ee-version", "3"),
            ("x-client-pub-key", &client_key),
            ("x-model-pub-key", "zz"),
        ],
        // §3: hex with no 0x prefix — the prefixed attested key is rejected.
        &[
            ("x-e2ee-version", "3"),
            ("x-client-pub-key", &client_key),
            ("x-model-pub-key", &prefixed_model_key),
        ],
    ];
    for headers in bad_key_sets {
        let resp = h
            .requester
            .post("/v1/chat/completions", CHAT_REQUEST, headers)
            .await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{headers:?}");
        assert_eq!(error_type(&resp), "e2ee_invalid_public_key", "{headers:?}");
    }
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v3_unattested_model_key_is_rejected() {
    // A well-formed X25519 key that is not in the attested keyset: the client
    // must prove it encrypted to a key it could have verified (§7.4). The
    // comparison is verbatim against the attested `public_key` string, so a
    // re-cased variant of the attested key is a mismatch too.
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_key = x25519_public_key_hex(&x25519_secret_key_from_bytes(&[0x77; 32]).unwrap());
    let uppercased_model_key = v3_model_public_key(&h).to_uppercase();
    for model_key in [client_key.as_str(), uppercased_model_key.as_str()] {
        let resp = h
            .requester
            .post(
                "/v1/chat/completions",
                CHAT_REQUEST,
                &[
                    ("x-e2ee-version", "3"),
                    ("x-client-pub-key", &client_key),
                    ("x-model-pub-key", model_key),
                ],
            )
            .await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_type(&resp), "e2ee_model_key_mismatch");
    }
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v3_malformed_envelopes_are_rejected() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = x25519_secret_key_from_bytes(&[0x78; 32]).unwrap();
    let (_, headers) = v3_sealed_request(&h, "aci-model", CHAT_REQUEST, &client_secret);
    let bad_bodies: &[&[u8]] = &[
        // Not JSON.
        b"not json",
        // No model.
        br#"{"sealed_b64":"AAAA"}"#,
        // Non-string model.
        br#"{"model":42,"sealed_b64":"AAAA"}"#,
        // No sealed_b64.
        br#"{"model":"aci-model"}"#,
        // sealed_b64 is not base64.
        br#"{"model":"aci-model","sealed_b64":"%%%"}"#,
        // Decodes, but shorter than ephemeral key + nonce + tag.
        br#"{"model":"aci-model","sealed_b64":"AAAAAAAAAAAAAA=="}"#,
    ];
    for body in bad_bodies {
        let resp = h
            .requester
            .post_owned_headers("/v1/chat/completions", body, &headers)
            .await;
        assert_eq!(
            resp.status,
            StatusCode::BAD_REQUEST,
            "{}",
            String::from_utf8_lossy(body)
        );
        assert_eq!(
            error_type(&resp),
            "e2ee_decryption_failed",
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v3_aead_failures_are_rejected() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = x25519_secret_key_from_bytes(&[0x79; 32]).unwrap();

    // Tampered ciphertext fails authentication.
    let (body, headers) = v3_sealed_request(&h, "aci-model", CHAT_REQUEST, &client_secret);
    let mut envelope: Value = serde_json::from_slice(&body).unwrap();
    let mut sealed = BASE64
        .decode(envelope["sealed_b64"].as_str().unwrap())
        .unwrap();
    *sealed.last_mut().unwrap() ^= 0x01;
    envelope["sealed_b64"] = Value::String(BASE64.encode(sealed));
    let resp = h
        .requester
        .post_owned_headers(
            "/v1/chat/completions",
            &serde_json::to_vec(&envelope).unwrap(),
            &headers,
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_decryption_failed");

    // A sealed body replayed under a different envelope model fails: the
    // envelope model is bound into the AAD (§7.2).
    let (body, headers) = v3_sealed_request(&h, "aci-model", CHAT_REQUEST, &client_secret);
    let mut envelope: Value = serde_json::from_slice(&body).unwrap();
    envelope["model"] = Value::String("other-model".to_string());
    let resp = h
        .requester
        .post_owned_headers(
            "/v1/chat/completions",
            &serde_json::to_vec(&envelope).unwrap(),
            &headers,
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_decryption_failed");

    // The confidentiality attack E2EE stops: an intermediary replays the sealed
    // request but swaps X-Client-Pub-Key (headers[1]) to reseal the response to
    // itself. That key is bound into the request AAD (§7.2), so the unseal fails.
    let (body, mut headers) = v3_sealed_request(&h, "aci-model", CHAT_REQUEST, &client_secret);
    headers[1].1 = x25519_public_key_hex(&x25519_secret_key_from_bytes(&[0x7b; 32]).unwrap());
    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_decryption_failed");

    assert!(h.upstream_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn e2ee_v3_headers_on_an_unsupported_endpoint_are_rejected() {
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = x25519_secret_key_from_bytes(&[0x7a; 32]).unwrap();
    let (body, headers) = v3_sealed_request(&h, "aci-model", CHAT_REQUEST, &client_secret);
    let resp = h
        .requester
        .post_owned_headers("/v1/responses", &body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_unsupported_endpoint");
    assert!(h.upstream_calls.lock().unwrap().is_empty());
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
    assert!(resp.headers.get("x-e2ee-algo").is_none());

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
async fn legacy_signing_algo_with_e2ee_version_is_rejected() {
    // Only the no-AAD legacy v1 mode survives (§13): a legacy X-Signing-Algo
    // request asking for a versioned scheme must drop X-Signing-Algo and use
    // ACI v3 instead.
    let h = harness_with_e2ee(RecordingUpstream::default());
    let client_secret = k256::SecretKey::from_slice(&[0x62; 32]).unwrap();
    let (body, mut headers) = legacy_ecdsa_request(&h, &client_secret);
    headers.push(("x-e2ee-version", "2".to_string()));

    let resp = h
        .requester
        .post_owned_headers("/v1/chat/completions", &body, &headers)
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_type(&resp), "e2ee_invalid_version");
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
    // The wire hash covers the raw in-order stream including framing (§8.4).
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
// §13 legacy signature endpoint
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
    let payload: Value = serde_json::from_slice(
        &BASE64
            .decode(body["receipt"]["payload_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
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
