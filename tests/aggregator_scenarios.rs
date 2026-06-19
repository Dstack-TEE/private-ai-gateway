//! End-to-end ACI aggregator scenarios with a mock requester, mock upstream,
//! and mock upstream verifier.
//!
//! This file is the executable conformance sketch for the aggregator slice:
//! it drives the public HTTP router where possible and drops to `AciService`
//! only for behavior that is not yet surfaced as an HTTP feature, such as
//! request rewriting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod common;

use async_trait::async_trait;
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt as _;
use private_ai_gateway::aci::canonical::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    decrypt_with_secret_key, encrypt_for_public_key, public_key_from_secret, E2EE_VERSION_V2,
};
use private_ai_gateway::aci::identity;
use private_ai_gateway::aci::keys::{
    verify_keyset_endorsement, verify_receipt_signature, KeyProvider,
};
use private_ai_gateway::aci::receipt::{
    canonical_bytes_for_signing, UpstreamVerifiedEvent, VerificationResult,
    EVENT_MIDDLEWARE_FORWARDED, EVENT_REQUEST_FORWARDED, EVENT_REQUEST_RECEIVED,
    EVENT_RESPONSE_RECEIVED, EVENT_RESPONSE_RETURNED, EVENT_ROUTE_SELECTED,
    EVENT_TRANSPARENCY_REQUEST_MODIFIED, EVENT_TRANSPARENCY_RESPONSE_MODIFIED,
    EVENT_UPSTREAM_VERIFIED,
};
use private_ai_gateway::aci::types::{KeyedPublicKey, Receipt, ServiceCapabilities};
use private_ai_gateway::aci::upstream::{
    ModelRoute, ModelRouterBackend, UpstreamBackend, UpstreamError, UpstreamRequest,
    UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore, UpstreamVerificationRequest,
    UpstreamVerifier,
};
use private_ai_gateway::http::{
    build_internal_backend_router, build_router, build_router_with_uds_middleware,
    serve_unix_listener, GatewayRequestStore, MiddlewareReceiptJournal, StoredGatewayRequest,
};
use rand::RngCore;
use serde_json::Value;
use tokio::sync::Notify;
use tower::ServiceExt;

use common::{event_from_request, verified_event, StaticKeyProvider, StubQuoter};

const CHAT_REQUEST: &[u8] =
    br#"{"model":"gpt-test","messages":[{"role":"user","content":"hello"}],"temperature":0}"#;
const CHAT_RESPONSE: &[u8] =
    br#"{"id":"chat-mock-1","object":"chat.completion","model":"mock-model","choices":[{"index":0,"message":{"role":"assistant","content":"world"},"finish_reason":"stop"}]}"#;

#[derive(Debug, Clone)]
struct UpstreamCall {
    body: Vec<u8>,
    headers: HashMap<String, String>,
    path: Option<String>,
}

struct MockUpstream {
    name: String,
    origin: String,
    response: Mutex<UpstreamResponse>,
    models_response: Mutex<UpstreamResponse>,
    calls: Arc<Mutex<Vec<UpstreamCall>>>,
}

impl MockUpstream {
    fn new(status_code: u16, body: &[u8]) -> (Self, Arc<Mutex<Vec<UpstreamCall>>>) {
        Self::named(
            "mock-upstream",
            "https://mock-upstream.example",
            status_code,
            body,
        )
    }

    fn named(
        name: &str,
        origin: &str,
        status_code: u16,
        body: &[u8],
    ) -> (Self, Arc<Mutex<Vec<UpstreamCall>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let models_body = br#"{"object":"list","data":[{"id":"mock-model","object":"model","owned_by":"mock-upstream"}]}"#;
        (
            Self {
                name: name.to_string(),
                origin: origin.to_string(),
                response: Mutex::new(UpstreamResponse {
                    status_code,
                    body: body.to_vec(),
                    headers,
                }),
                models_response: Mutex::new(UpstreamResponse {
                    status_code: 200,
                    body: models_body.to_vec(),
                    headers: HashMap::from([(
                        "content-type".to_string(),
                        "application/json".to_string(),
                    )]),
                }),
                calls: calls.clone(),
            },
            calls,
        )
    }
}

#[async_trait]
impl UpstreamBackend for MockUpstream {
    fn name(&self) -> &str {
        &self.name
    }

    fn url_origin(&self) -> Option<&str> {
        Some(&self.origin)
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.calls.lock().unwrap().push(UpstreamCall {
            body: req.body,
            headers: req.headers,
            path: req.path,
        });
        Ok(self.response.lock().unwrap().clone())
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Ok(self.models_response.lock().unwrap().clone())
    }
}

struct ScriptedVerifier {
    result: VerificationResult,
    reason: Option<String>,
    evidence: Option<serde_json::Value>,
    calls: Arc<Mutex<Vec<UpstreamVerificationRequest>>>,
}

impl ScriptedVerifier {
    fn verified() -> (Self, Arc<Mutex<Vec<UpstreamVerificationRequest>>>) {
        Self::new(VerificationResult::Verified, None)
    }

    fn failed(reason: &str) -> (Self, Arc<Mutex<Vec<UpstreamVerificationRequest>>>) {
        Self::new(VerificationResult::Failed, Some(reason.to_string()))
    }

    fn new(
        result: VerificationResult,
        reason: Option<String>,
    ) -> (Self, Arc<Mutex<Vec<UpstreamVerificationRequest>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                result,
                reason,
                evidence: Some(serde_json::json!({
                    "digest": format!("sha256:{}", "ab".repeat(32)),
                    "data": "data:application/json;base64,eyJmaXh0dXJlIjoidXBzdHJlYW0tMSJ9",
                })),
                calls: calls.clone(),
            },
            calls,
        )
    }
}

#[async_trait]
impl UpstreamVerifier for ScriptedVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.calls.lock().unwrap().push(request.clone());
        UpstreamVerifiedEvent {
            verifier_id: "mock-verifier/v1".to_string(),
            reason: self.reason.clone(),
            evidence: self.evidence.clone(),
            ..event_from_request(&request, self.result)
        }
    }
}

#[derive(Clone)]
struct MockRequester {
    app: Router,
}

struct HttpResult {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct MiddlewareCall {
    request_id: Option<String>,
    user_model: Option<String>,
    authorization: Option<String>,
    tenant_header: Option<String>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct FixtureMiddlewareState {
    backend_socket: PathBuf,
    target_route_id: String,
    response_body_override: Option<Vec<u8>>,
    calls: Arc<Mutex<Vec<MiddlewareCall>>>,
}

impl MockRequester {
    fn new(app: Router) -> Self {
        Self { app }
    }

    async fn get(&self, uri: &str) -> HttpResult {
        self.call(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post_chat(&self, body: &[u8], headers: &[(&str, &str)]) -> HttpResult {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        self.call(builder.body(Body::from(body.to_vec())).unwrap())
            .await
    }

    async fn call(&self, request: Request<Body>) -> HttpResult {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX)
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

async fn fixture_middleware_handler(
    State(state): State<FixtureMiddlewareState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let request_id = headers
        .get("x-private-ai-gateway-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let user_model = headers
        .get("x-private-ai-gateway-user-model")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let tenant_header = headers
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    state.calls.lock().unwrap().push(MiddlewareCall {
        request_id: request_id.clone(),
        user_model,
        authorization,
        tenant_header,
        body: body.to_vec(),
    });
    let Some(request_id) = request_id else {
        return (
            StatusCode::BAD_REQUEST,
            HeaderMap::new(),
            b"missing request id".to_vec(),
        );
    };
    let resp = reqwest::Client::builder()
        .unix_socket(state.backend_socket.clone())
        .build()
        .unwrap()
        .post("http://private-ai-gateway/internal/forward")
        .header("x-private-ai-gateway-request-id", request_id)
        .header("x-private-ai-gateway-targets", state.target_route_id)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
    let mut out_headers = HeaderMap::new();
    for name in [
        "content-type",
        "x-receipt-id",
        "x-e2ee-applied",
        "x-e2ee-version",
        "x-e2ee-algo",
    ] {
        if let Some(value) = resp.headers().get(name) {
            out_headers.insert(name, value.clone());
        }
    }
    let backend_body = resp.bytes().await.unwrap().to_vec();
    let body = state.response_body_override.clone().unwrap_or(backend_body);
    (status, out_headers, body)
}

async fn fixture_middleware_models() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"object":"list","data":[{"id":"tenant-facing-model","object":"model","owned_by":"fixture-middleware"}]}"#,
    )
}

async fn fixture_middleware_rejects_with_forged_trust_headers() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert("x-receipt-id", HeaderValue::from_static("fake-receipt"));
    headers.insert("x-e2ee-applied", HeaderValue::from_static("true"));
    headers.insert("x-e2ee-version", HeaderValue::from_static("2"));
    headers.insert("x-aci-identity", HeaderValue::from_static("sha256:forged"));
    headers.insert(
        "x-private-ai-gateway-request-id",
        HeaderValue::from_static("forged"),
    );
    (
        StatusCode::BAD_REQUEST,
        headers,
        br#"{"error":{"message":"middleware rejected","type":"middleware_error","code":null,"param":null}}"#.to_vec(),
    )
}

async fn fixture_middleware_generated_chat() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        br#"{"id":"chat-middleware-generated","object":"chat.completion","model":"tenant-facing-model","choices":[{"index":0,"message":{"role":"assistant","content":"middleware-only"},"finish_reason":"stop"}]}"#.to_vec(),
    )
}

#[derive(Clone)]
struct StreamingMiddlewareState {
    release_second: Arc<Notify>,
}

async fn fixture_middleware_streaming_handler(
    State(state): State<StreamingMiddlewareState>,
) -> impl IntoResponse {
    let stream = futures_util::stream::unfold(0u8, move |step| {
        let state = state.clone();
        async move {
            match step {
                0 => Some((
                    Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"data: {\"id\":\"middleware-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"}}]}\n\n",
                    )),
                    1,
                )),
                1 => {
                    state.release_second.notified().await;
                    Some((
                        Ok::<_, std::convert::Infallible>(Bytes::from_static(
                            b"data: [DONE]\n\n",
                        )),
                        2,
                    ))
                }
                _ => None,
            }
        }
    });
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
}

fn test_socket_path(name: &str) -> PathBuf {
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    let file = format!(
        "pag-{name}-{}-{}.sock",
        std::process::id(),
        hex::encode(bytes)
    );
    // Prefer the standard temp dir (honours $TMPDIR), but a Unix socket path
    // must fit in `sockaddr_un.sun_path` (~104 bytes on macOS, ~108 on Linux).
    // macOS `$TMPDIR` (/var/folders/...) can overflow it, so fall back to the
    // short `/tmp` only when the temp-dir path would be too long.
    let candidate = std::env::temp_dir().join(&file);
    if candidate.as_os_str().len() < 100 {
        candidate
    } else {
        PathBuf::from("/tmp").join(file)
    }
}

async fn serve_router_uds(name: &str, app: Router) -> PathBuf {
    let path = test_socket_path(name);
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        serve_unix_listener(listener, app).await.unwrap();
    });
    path
}

struct Harness {
    service: Arc<AciService>,
    requester: MockRequester,
    upstream_calls: Arc<Mutex<Vec<UpstreamCall>>>,
    receipt_keys: Vec<KeyedPublicKey>,
}

fn make_harness(verifier: ScriptedVerifier) -> Harness {
    make_harness_with_upstream(verifier, 200, CHAT_RESPONSE)
}

fn make_harness_with_upstream(
    verifier: ScriptedVerifier,
    upstream_status: u16,
    upstream_body: &[u8],
) -> Harness {
    let keys = Arc::new(StaticKeyProvider::default());
    let receipt_keys = keys.receipt_keys();
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, upstream_calls) = MockUpstream::new(upstream_status, upstream_body);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("private-ai-gateway");
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: vec![],
    };
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            quoter,
            Arc::new(upstream),
            Arc::new(verifier),
            store,
            cfg,
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let requester = MockRequester::new(build_router(service.clone()));
    Harness {
        service,
        requester,
        upstream_calls,
        receipt_keys,
    }
}

fn json_body(result: &HttpResult) -> Value {
    serde_json::from_slice(&result.body).unwrap()
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).unwrap().to_str().unwrap()
}

fn event<'a>(receipt: &'a Receipt, event_type: &str) -> &'a Value {
    &receipt
        .event_log
        .iter()
        .find(|e| e.event_type == event_type)
        .unwrap()
        .fields
}

fn assert_valid_receipt_signature(receipt: &Receipt, receipt_key: &KeyedPublicKey) {
    let canonical_bytes = canonical_bytes_for_signing(receipt).unwrap();
    let signature = hex::decode(&receipt.signature.value_hex).unwrap();
    assert!(verify_receipt_signature(
        receipt_key,
        &canonical_bytes,
        &signature
    ));
}

#[tokio::test]
async fn report_establishes_identity_keyset_endorsement_and_nonce_binding() {
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);

    // The legacy endpoint binds the old dstack-vllm-proxy report_data layout:
    // signing_address(20) ‖ zeros(12) ‖ nonce(32), with a 32-byte hex nonce
    // placed verbatim.
    let nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
    let response = h
        .requester
        .get(&format!("/v1/attestation/report?nonce={nonce}"))
        .await;
    assert_eq!(response.status, StatusCode::OK);
    let body = json_body(&response);

    assert_eq!(body["api_version"], "aci/1");
    assert_eq!(body["workload_id"], h.service.workload_id());
    assert_eq!(
        body["workload_keyset_digest"],
        h.service.workload_keyset_digest()
    );

    let signing_address = body["signing_address"]
        .as_str()
        .unwrap()
        .trim_start_matches("0x")
        .to_string();
    let report_data_hex = body["attestation"]["report_data"].as_str().unwrap();
    assert_eq!(&report_data_hex[0..40], signing_address);
    assert_eq!(&report_data_hex[40..64], &"00".repeat(12));
    assert_eq!(&report_data_hex[64..128], nonce);

    let endorsement_payload = identity::keyset_endorsement_payload(h.service.keyset()).unwrap();
    let endorsement_sig = hex::decode(
        body["attestation"]["keyset_endorsement"]["value"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(verify_keyset_endorsement(
        &h.service.keyset().workload_identity.public_key,
        &endorsement_payload,
        &endorsement_sig
    ));

    let quote = hex::decode(body["attestation"]["evidence"]["quote"].as_str().unwrap()).unwrap();
    let report_data = hex::decode(report_data_hex).unwrap();
    assert!(
        quote.ends_with(&report_data),
        "stub quote should carry the exact report_data bytes"
    );
}

#[tokio::test]
async fn relying_party_can_verify_report_chat_receipt_chain() {
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);

    let report = h
        .service
        .attestation_report(Some("rp nonce".to_string()))
        .await
        .unwrap();
    let endorsement_payload = identity::keyset_endorsement_payload(h.service.keyset()).unwrap();
    let endorsement_sig = hex::decode(&report.attestation.keyset_endorsement.value_hex).unwrap();
    assert!(verify_keyset_endorsement(
        &report
            .attestation
            .workload_keyset
            .workload_identity
            .public_key,
        &endorsement_payload,
        &endorsement_sig
    ));
    let statement =
        identity::attestation_statement(h.service.keyset(), Some("rp nonce".to_string())).unwrap();
    assert_eq!(
        report.attestation.report_data_hex,
        hex::encode(identity::report_data(&statement).unwrap())
    );

    let response = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(response.status, StatusCode::OK);
    let receipt_id = header_str(&response.headers, "x-receipt-id").to_string();
    let receipt = h
        .service
        .get_receipt_by_receipt_id(&receipt_id)
        .expect("receipt should be retained");

    assert_eq!(receipt.workload_id, report.workload_id);
    assert_eq!(
        receipt.workload_keyset_digest,
        report.workload_keyset_digest
    );
    assert_eq!(
        event(&receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["wire_hash"],
        sha256_hex(CHAT_RESPONSE)
    );

    let canonical_bytes = canonical_bytes_for_signing(&receipt).unwrap();
    let signature = hex::decode(&receipt.signature.value_hex).unwrap();
    let receipt_key = report
        .attestation
        .workload_keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == receipt.signature.key_id)
        .expect("receipt key must be in attested keyset");
    assert!(verify_receipt_signature(
        receipt_key,
        &canonical_bytes,
        &signature
    ));
}

#[tokio::test]
async fn models_endpoint_is_openai_compatible_and_not_a_trust_surface() {
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);

    let response = h.requester.get("/v1/models").await;
    assert_eq!(response.status, StatusCode::OK);
    let body = String::from_utf8(response.body).unwrap();
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["object"], "list");
    assert_eq!(json["data"][0]["id"], "mock-model");
    for forbidden in [
        "canonical_id",
        "attestation_provider",
        "e2ee_supported_versions",
    ] {
        assert!(
            !body.contains(forbidden),
            "/v1/models must not expose ACI trust metadata field {forbidden}"
        );
    }
}

#[tokio::test]
async fn model_router_rewrites_before_verification_forwarding_and_receipt_hashing() {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream_a, calls_a) = MockUpstream::named(
        "upstream-a",
        "https://upstream-a.example",
        200,
        br#"{"id":"chat-route-a","object":"chat.completion","model":"upstream-a-model","choices":[{"index":0,"message":{"role":"assistant","content":"a"},"finish_reason":"stop"}]}"#,
    );
    let (upstream_b, calls_b) = MockUpstream::named(
        "upstream-b",
        "https://upstream-b.example",
        200,
        br#"{"id":"chat-route-b","object":"chat.completion","model":"upstream-b-model","choices":[{"index":0,"message":{"role":"assistant","content":"b"},"finish_reason":"stop"}]}"#,
    );
    let upstream_a: Arc<dyn UpstreamBackend> = Arc::new(upstream_a);
    let upstream_b: Arc<dyn UpstreamBackend> = Arc::new(upstream_b);
    let mut router = ModelRouterBackend::new("model-router");
    router
        .add_route(
            ModelRoute::new(
                "public-a",
                "upstream-a-model",
                upstream_a,
                "upstream-a:public-a",
            )
            .unwrap(),
        )
        .unwrap();
    router
        .add_route(
            ModelRoute::new(
                "public-b",
                "upstream-b-model",
                upstream_b,
                "upstream-b:public-b",
            )
            .unwrap(),
        )
        .unwrap();

    let (verifier, verifier_calls) = ScriptedVerifier::verified();
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("private-ai-gateway");
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: vec![E2EE_VERSION_V2.to_string()],
    };
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            quoter,
            Arc::new(router),
            Arc::new(verifier),
            store,
            cfg,
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );

    let received = br#"{"model":"public-a","messages":[]}"#;
    let result = service
        .forward_chat_completion(received, None, Some(true), None)
        .await
        .unwrap();

    let forwarded_body = {
        let calls_a = calls_a.lock().unwrap();
        assert_eq!(calls_a.len(), 1);
        assert_eq!(calls_a[0].path.as_deref(), Some("/v1/chat/completions"));
        let forwarded: Value = serde_json::from_slice(&calls_a[0].body).unwrap();
        assert_eq!(forwarded["model"], "upstream-a-model");
        assert_eq!(forwarded["messages"].as_array().unwrap().len(), 0);
        calls_a[0].body.clone()
    };
    assert!(calls_b.lock().unwrap().is_empty());

    {
        let verifier_calls = verifier_calls.lock().unwrap();
        assert_eq!(verifier_calls.len(), 1);
        assert_eq!(verifier_calls[0].upstream_name, "upstream-a");
        assert_eq!(
            verifier_calls[0].url_origin.as_deref(),
            Some("https://upstream-a.example")
        );
        assert_eq!(verifier_calls[0].model_id, "upstream-a-model");
        assert_eq!(
            verifier_calls[0].forwarded_body_hash,
            sha256_hex(&forwarded_body)
        );
    }

    assert_eq!(
        event(&result.receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(received)
    );
    assert_eq!(
        event(&result.receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
        sha256_hex(&forwarded_body)
    );
    assert_eq!(
        event(&result.receipt, EVENT_UPSTREAM_VERIFIED)["model_id"],
        "upstream-a-model"
    );
    assert!(result
        .receipt
        .event_log
        .iter()
        .any(|event| event.event_type == EVENT_TRANSPARENCY_REQUEST_MODIFIED));

    let requester = MockRequester::new(build_router(service.clone()));
    let models = requester.get("/v1/models").await;
    assert_eq!(models.status, StatusCode::OK);
    let models = json_body(&models);
    let ids = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["public-a", "public-b"]);
    let models_text = serde_json::to_string(&models).unwrap();
    assert!(!models_text.contains("upstream-a"));
    assert!(!models_text.contains("upstream-b"));
    assert!(!models_text.contains("upstream-a-model"));
    assert!(!models_text.contains("upstream-b-model"));

    let forged = requester
        .call(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-private-ai-gateway-request-id", "req-attacker")
                .header("x-private-ai-gateway-targets", "upstream-b:public-b")
                .body(Body::from(received.to_vec()))
                .unwrap(),
        )
        .await;
    assert_eq!(forged.status, StatusCode::OK);
    assert_eq!(
        calls_a.lock().unwrap().len(),
        2,
        "public frontend must ignore caller-supplied internal target headers"
    );
    assert!(calls_b.lock().unwrap().is_empty());

    let selected = br#"{"model":"tenant-facing-model","messages":[]}"#;
    let request_store = GatewayRequestStore::default();
    let backend = MockRequester::new(build_internal_backend_router(
        service.clone(),
        request_store.clone(),
    ));
    let selected_journal = MiddlewareReceiptJournal::default();
    request_store.insert(
        "req-test-target-route".to_string(),
        StoredGatewayRequest {
            endpoint_path: "/v1/chat/completions",
            received_body: selected.to_vec(),
            upstream_required: true,
            requester: None,
            e2ee: None,
            user_model: Some("tenant-facing-model".to_string()),
            receipt_journal: selected_journal.clone(),
        },
    );
    let selected_response = backend
        .call(
            Request::builder()
                .method("POST")
                .uri("/internal/forward")
                .header("x-private-ai-gateway-request-id", "req-test-target-route")
                .header("x-private-ai-gateway-targets", "upstream-b:public-b")
                .body(Body::from(selected.to_vec()))
                .unwrap(),
        )
        .await;
    assert_eq!(selected_response.status, StatusCode::OK);
    {
        let calls_b = calls_b.lock().unwrap();
        assert_eq!(calls_b.len(), 1);
        let forwarded: Value = serde_json::from_slice(&calls_b[0].body).unwrap();
        assert_eq!(forwarded["model"], "upstream-b-model");
    }
    let receipt_id = header_str(&selected_response.headers, "x-receipt-id").to_string();
    assert!(
        service.get_receipt_by_receipt_id(&receipt_id).is_none(),
        "internal backend must not finalize a middleware receipt before frontend observes the final response"
    );
    let selected_draft = selected_journal
        .take()
        .expect("internal backend must append a receipt draft");
    let selected_finalized = service
        .finalize_middleware_receipt(
            selected_draft,
            &selected_response.body,
            Some(header_str(&selected_response.headers, "content-type")),
            None,
            None,
        )
        .unwrap();
    let selected_receipt = selected_finalized.receipt;
    assert_eq!(selected_receipt.receipt_id, receipt_id);
    assert_eq!(
        event(&selected_receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(selected)
    );
    assert_eq!(
        event(&selected_receipt, EVENT_UPSTREAM_VERIFIED)["model_id"],
        "upstream-b-model"
    );
    assert_eq!(
        event(&selected_receipt, EVENT_MIDDLEWARE_FORWARDED)["body_hash"],
        sha256_hex(selected)
    );
    assert_eq!(
        event(&selected_receipt, EVENT_ROUTE_SELECTED)["target_route_id"],
        "upstream-b:public-b"
    );

    request_store.insert(
        "req-test-bad-route".to_string(),
        StoredGatewayRequest {
            endpoint_path: "/v1/chat/completions",
            received_body: selected.to_vec(),
            upstream_required: true,
            requester: None,
            e2ee: None,
            user_model: Some("tenant-facing-model".to_string()),
            receipt_journal: MiddlewareReceiptJournal::default(),
        },
    );
    let bad_target = backend
        .call(
            Request::builder()
                .method("POST")
                .uri("/internal/forward")
                .header("x-private-ai-gateway-request-id", "req-test-bad-route")
                .header("x-private-ai-gateway-targets", "missing:route")
                .body(Body::from(selected.to_vec()))
                .unwrap(),
        )
        .await;
    assert_eq!(bad_target.status, StatusCode::BAD_REQUEST);

    let expiring_store = GatewayRequestStore::new(Duration::from_millis(1));
    let expiring_backend = MockRequester::new(build_internal_backend_router(
        service.clone(),
        expiring_store.clone(),
    ));
    expiring_store.insert(
        "req-test-expired".to_string(),
        StoredGatewayRequest {
            endpoint_path: "/v1/chat/completions",
            received_body: selected.to_vec(),
            upstream_required: true,
            requester: None,
            e2ee: None,
            user_model: Some("tenant-facing-model".to_string()),
            receipt_journal: MiddlewareReceiptJournal::default(),
        },
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    let expired = expiring_backend
        .call(
            Request::builder()
                .method("POST")
                .uri("/internal/forward")
                .header("x-private-ai-gateway-request-id", "req-test-expired")
                .header("x-private-ai-gateway-targets", "upstream-b:public-b")
                .body(Body::from(selected.to_vec()))
                .unwrap(),
        )
        .await;
    assert_eq!(expired.status, StatusCode::BAD_REQUEST);

    let uds_store = GatewayRequestStore::default();
    let backend_socket = serve_router_uds(
        "backend",
        build_internal_backend_router(service.clone(), uds_store.clone()),
    )
    .await;
    let middleware_calls = Arc::new(Mutex::new(Vec::new()));
    let middleware_socket = serve_router_uds(
        "middleware",
        Router::new()
            .route("/v1/chat/completions", post(fixture_middleware_handler))
            .route("/v1/models", axum::routing::get(fixture_middleware_models))
            .with_state(FixtureMiddlewareState {
                backend_socket: backend_socket.clone(),
                target_route_id: "upstream-b:public-b".to_string(),
                response_body_override: None,
                calls: middleware_calls.clone(),
            }),
    )
    .await;
    let public_with_middleware = MockRequester::new(build_router_with_uds_middleware(
        service.clone(),
        uds_store.clone(),
        middleware_socket,
    ));
    let middleware_models = public_with_middleware.get("/v1/models").await;
    assert_eq!(middleware_models.status, StatusCode::OK);
    let middleware_models = json_body(&middleware_models);
    assert_eq!(middleware_models["data"][0]["id"], "tenant-facing-model");
    let middleware_response = public_with_middleware
        .post_chat(
            selected,
            &[
                ("authorization", "Bearer tenant-token"),
                ("x-tenant-id", "tenant-a"),
            ],
        )
        .await;
    assert_eq!(middleware_response.status, StatusCode::OK);
    {
        let calls = middleware_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0]
            .request_id
            .as_deref()
            .is_some_and(|id| id.starts_with("req_")));
        assert_eq!(calls[0].user_model.as_deref(), Some("tenant-facing-model"));
        assert_eq!(
            calls[0].authorization.as_deref(),
            Some("Bearer tenant-token")
        );
        assert_eq!(calls[0].tenant_header.as_deref(), Some("tenant-a"));
        assert_eq!(calls[0].body, selected);
    }
    {
        let calls_b = calls_b.lock().unwrap();
        assert_eq!(calls_b.len(), 2);
        let forwarded: Value = serde_json::from_slice(&calls_b[1].body).unwrap();
        assert_eq!(forwarded["model"], "upstream-b-model");
    }
    let receipt_id = header_str(&middleware_response.headers, "x-receipt-id");
    let receipt = service
        .get_receipt_by_receipt_id(receipt_id)
        .expect("middleware response must persist a receipt");
    assert_eq!(
        event(&receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(selected)
    );
    assert_eq!(
        event(&receipt, EVENT_UPSTREAM_VERIFIED)["model_id"],
        "upstream-b-model"
    );
    assert_eq!(
        event(&receipt, EVENT_MIDDLEWARE_FORWARDED)["body_hash"],
        sha256_hex(selected)
    );
    assert_eq!(
        event(&receipt, EVENT_ROUTE_SELECTED)["target_route_id"],
        "upstream-b:public-b"
    );
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RECEIVED)["cleartext_hash"],
        event(&receipt, EVENT_RESPONSE_RETURNED)["cleartext_hash"],
    );

    let rejecting_middleware_socket = serve_router_uds(
        "middleware-rejecting",
        Router::new().route(
            "/v1/chat/completions",
            post(fixture_middleware_rejects_with_forged_trust_headers),
        ),
    )
    .await;
    let public_with_rejecting_middleware = MockRequester::new(build_router_with_uds_middleware(
        service.clone(),
        uds_store.clone(),
        rejecting_middleware_socket,
    ));
    let rejected_response = public_with_rejecting_middleware
        .post_chat(selected, &[])
        .await;
    assert_eq!(rejected_response.status, StatusCode::BAD_REQUEST);
    assert!(rejected_response.headers.get("x-receipt-id").is_none());
    assert!(rejected_response.headers.get("x-e2ee-applied").is_none());
    assert!(rejected_response
        .headers
        .get("x-private-ai-gateway-request-id")
        .is_none());
    assert_eq!(
        header_str(&rejected_response.headers, "x-aci-identity"),
        service.workload_id()
    );

    let release_second = Arc::new(Notify::new());
    let streaming_middleware_socket = serve_router_uds(
        "middleware-streaming",
        Router::new()
            .route(
                "/v1/chat/completions",
                post(fixture_middleware_streaming_handler),
            )
            .with_state(StreamingMiddlewareState {
                release_second: release_second.clone(),
            }),
    )
    .await;
    let public_streaming = build_router_with_uds_middleware(
        service.clone(),
        uds_store.clone(),
        streaming_middleware_socket,
    );
    let streaming_request = br#"{"model":"tenant-facing-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let streaming_response = tokio::time::timeout(
        Duration::from_millis(200),
        public_streaming.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(streaming_request.to_vec()))
                .unwrap(),
        ),
    )
    .await
    .expect("frontend must return middleware streaming response before the stream finishes")
    .unwrap();
    assert_eq!(streaming_response.status(), StatusCode::OK);
    assert!(streaming_response.headers().get("x-receipt-id").is_none());
    assert_eq!(
        header_str(streaming_response.headers(), "content-type"),
        "text/event-stream"
    );
    let mut streaming_body = streaming_response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_millis(200), streaming_body.next())
        .await
        .expect("first middleware SSE chunk should be available immediately")
        .expect("stream should yield first chunk")
        .unwrap();
    assert_eq!(
        first,
        Bytes::from_static(
            b"data: {\"id\":\"middleware-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"}}]}\n\n"
        )
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), streaming_body.next())
            .await
            .is_err(),
        "frontend must not synthesize the second chunk before middleware sends it"
    );
    release_second.notify_waiters();
    let second = tokio::time::timeout(Duration::from_millis(200), streaming_body.next())
        .await
        .expect("second middleware SSE chunk should be released")
        .expect("stream should yield second chunk")
        .unwrap();
    assert_eq!(second, Bytes::from_static(b"data: [DONE]\n\n"));
    assert!(streaming_body.next().await.is_none());

    let provider_b_body = br#"{"id":"chat-route-b","object":"chat.completion","model":"upstream-b-model","choices":[{"index":0,"message":{"role":"assistant","content":"b"},"finish_reason":"stop"}]}"#;
    let mutated_body = br#"{"id":"chat-route-b","object":"chat.completion","model":"upstream-b-model","choices":[{"index":0,"message":{"role":"assistant","content":"middleware changed"},"finish_reason":"stop"}]}"#;
    let mutating_middleware_socket = serve_router_uds(
        "middleware-mutating",
        Router::new()
            .route("/v1/chat/completions", post(fixture_middleware_handler))
            .with_state(FixtureMiddlewareState {
                backend_socket: backend_socket.clone(),
                target_route_id: "upstream-b:public-b".to_string(),
                response_body_override: Some(mutated_body.to_vec()),
                calls: Arc::new(Mutex::new(Vec::new())),
            }),
    )
    .await;
    let public_with_mutating_middleware = MockRequester::new(build_router_with_uds_middleware(
        service.clone(),
        uds_store.clone(),
        mutating_middleware_socket,
    ));
    let mutated_response = public_with_mutating_middleware
        .post_chat(selected, &[])
        .await;
    assert_eq!(mutated_response.status, StatusCode::OK);
    assert_eq!(mutated_response.body, mutated_body);
    let mutated_receipt_id = header_str(&mutated_response.headers, "x-receipt-id");
    let mutated_receipt = service
        .get_receipt_by_receipt_id(mutated_receipt_id)
        .expect("frontend must finalize receipt after middleware response mutation");
    assert_eq!(
        event(&mutated_receipt, EVENT_RESPONSE_RECEIVED)["cleartext_hash"],
        sha256_hex(provider_b_body)
    );
    assert_eq!(
        event(&mutated_receipt, EVENT_RESPONSE_RETURNED)["cleartext_hash"],
        sha256_hex(mutated_body)
    );
    assert!(mutated_receipt
        .event_log
        .iter()
        .any(|event| event.event_type == EVENT_TRANSPARENCY_RESPONSE_MODIFIED));

    let client_secret = k256::SecretKey::from_slice(&[0x67; 32]).unwrap();
    let nonce = "nonce-middleware-route";
    let timestamp = 1_700_000_000u64;
    let model_key = &service.keyset().e2ee_public_keys[0];
    let request_aad = format!(
        "v2|req|algo={}|model=tenant-facing-model|m=0|c=-|n={nonce}|ts={timestamp}",
        model_key.algo
    );
    let encrypted_content =
        encrypt_for_public_key(&model_key.public_key_hex, b"hello", request_aad.as_bytes())
            .unwrap();
    let encrypted_body = serde_json::to_vec(&serde_json::json!({
        "model": "tenant-facing-model",
        "messages": [{"role": "user", "content": encrypted_content}],
    }))
    .unwrap();
    let e2ee_response = public_with_middleware
        .call(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-client-pub-key", public_key_from_secret(&client_secret))
                .header("x-model-pub-key", model_key.public_key_hex.clone())
                .header("x-e2ee-version", E2EE_VERSION_V2)
                .header("x-e2ee-nonce", nonce)
                .header("x-e2ee-timestamp", timestamp.to_string())
                .body(Body::from(encrypted_body))
                .unwrap(),
        )
        .await;
    assert_eq!(
        e2ee_response.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&e2ee_response.body)
    );
    assert_eq!(header_str(&e2ee_response.headers, "x-e2ee-applied"), "true");
    {
        let calls = middleware_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let body: Value = serde_json::from_slice(&calls[1].body).unwrap();
        assert_eq!(body["model"], "tenant-facing-model");
        assert_eq!(body["messages"][0]["content"], "hello");
    }
    {
        let calls_b = calls_b.lock().unwrap();
        assert_eq!(calls_b.len(), 4);
        let forwarded: Value = serde_json::from_slice(&calls_b[3].body).unwrap();
        assert_eq!(forwarded["model"], "upstream-b-model");
        assert_eq!(forwarded["messages"][0]["content"], "hello");
    }
    let encrypted_response = json_body(&e2ee_response);
    let encrypted_answer = encrypted_response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    assert_ne!(encrypted_answer, "b");
    let response_aad = format!(
        "v2|resp|algo={}|model=tenant-facing-model|id=chat-route-b|choice=0|field=content|n={nonce}|ts={timestamp}",
        model_key.algo
    );
    let decrypted =
        decrypt_with_secret_key(&client_secret, encrypted_answer, response_aad.as_bytes()).unwrap();
    assert_eq!(decrypted, b"b");
    let upstream_model_aad =
        response_aad.replace("model=tenant-facing-model", "model=upstream-b-model");
    assert!(decrypt_with_secret_key(
        &client_secret,
        encrypted_answer,
        upstream_model_aad.as_bytes()
    )
    .is_err());
    let e2ee_receipt_id = header_str(&e2ee_response.headers, "x-receipt-id");
    let e2ee_receipt = service
        .get_receipt_by_receipt_id(e2ee_receipt_id)
        .expect("middleware E2EE response must persist a receipt");
    let middleware_forwarded_hash = {
        let calls = middleware_calls.lock().unwrap();
        sha256_hex(&calls[1].body)
    };
    assert_eq!(
        event(&e2ee_receipt, EVENT_MIDDLEWARE_FORWARDED)["body_hash"],
        middleware_forwarded_hash
    );
    assert_eq!(
        event(&e2ee_receipt, EVENT_ROUTE_SELECTED)["target_route_id"],
        "upstream-b:public-b"
    );

    let generated_middleware_socket = serve_router_uds(
        "middleware-generated",
        Router::new().route(
            "/v1/chat/completions",
            post(fixture_middleware_generated_chat),
        ),
    )
    .await;
    let public_with_generated_middleware = MockRequester::new(build_router_with_uds_middleware(
        service.clone(),
        uds_store.clone(),
        generated_middleware_socket,
    ));
    let generated_nonce = "nonce-middleware-generated";
    let generated_timestamp = 1_700_000_010u64;
    let generated_request_aad = format!(
        "v2|req|algo={}|model=tenant-facing-model|m=0|c=-|n={generated_nonce}|ts={generated_timestamp}",
        model_key.algo
    );
    let generated_encrypted_content = encrypt_for_public_key(
        &model_key.public_key_hex,
        b"hello",
        generated_request_aad.as_bytes(),
    )
    .unwrap();
    let generated_encrypted_body = serde_json::to_vec(&serde_json::json!({
        "model": "tenant-facing-model",
        "messages": [{"role": "user", "content": generated_encrypted_content}],
    }))
    .unwrap();
    let generated_response = public_with_generated_middleware
        .call(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-client-pub-key", public_key_from_secret(&client_secret))
                .header("x-model-pub-key", model_key.public_key_hex.clone())
                .header("x-e2ee-version", E2EE_VERSION_V2)
                .header("x-e2ee-nonce", generated_nonce)
                .header("x-e2ee-timestamp", generated_timestamp.to_string())
                .body(Body::from(generated_encrypted_body))
                .unwrap(),
        )
        .await;
    assert_eq!(generated_response.status, StatusCode::OK);
    assert!(generated_response.headers.get("x-receipt-id").is_none());
    assert_eq!(
        header_str(&generated_response.headers, "x-e2ee-applied"),
        "true"
    );
    let generated_encrypted_response = json_body(&generated_response);
    let generated_encrypted_answer = generated_encrypted_response["choices"][0]["message"]
        ["content"]
        .as_str()
        .unwrap();
    assert_ne!(generated_encrypted_answer, "middleware-only");
    let generated_response_aad = format!(
        "v2|resp|algo={}|model=tenant-facing-model|id=chat-middleware-generated|choice=0|field=content|n={generated_nonce}|ts={generated_timestamp}",
        model_key.algo
    );
    let generated_decrypted = decrypt_with_secret_key(
        &client_secret,
        generated_encrypted_answer,
        generated_response_aad.as_bytes(),
    )
    .unwrap();
    assert_eq!(generated_decrypted, b"middleware-only");
}

#[tokio::test]
async fn metrics_endpoint_exposes_aggregator_prometheus_text() {
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);

    let chat = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(chat.status, StatusCode::OK);
    assert_eq!(h.upstream_calls.lock().unwrap().len(), 1);

    let response = h.requester.get("/v1/metrics").await;
    assert_eq!(response.status, StatusCode::OK);
    assert!(header_str(&response.headers, "content-type").starts_with("text/plain; version=0.0.4"));
    assert_eq!(
        h.upstream_calls.lock().unwrap().len(),
        1,
        "/v1/metrics must not contact or expose the upstream"
    );
    let body = String::from_utf8(response.body).unwrap();
    assert!(!body.contains("vllm_requests_total"));
    assert!(body.contains("# HELP private_ai_gateway_requests_total"));
    assert!(
        body.contains(
            "private_ai_gateway_requests_total{e2ee=\"false\",endpoint=\"/v1/chat/completions\",mode=\"buffered\"} 1"
        ),
        "{body}"
    );
    assert!(
        body.contains(
            "private_ai_gateway_upstream_verifications_total{required=\"true\",result=\"verified\"} 1"
        ),
        "{body}"
    );
    assert!(
        body.contains(
            "private_ai_gateway_upstream_responses_total{endpoint=\"/v1/chat/completions\",mode=\"buffered\",model_id=\"mock-model\",status_class=\"2xx\"} 1"
        ),
        "{body}"
    );
    assert!(
        body.contains(
            "private_ai_gateway_receipts_issued_total{endpoint=\"/v1/chat/completions\",mode=\"buffered\",model_id=\"mock-model\"} 1"
        ),
        "{body}"
    );
}

#[tokio::test]
async fn verified_upstream_request_returns_aci_headers_and_signed_receipt() {
    let (verifier, verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);

    let response = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, CHAT_RESPONSE);
    assert_eq!(header_str(&response.headers, "x-aci-version"), "aci/1");
    assert_eq!(
        header_str(&response.headers, "x-aci-identity"),
        h.service.workload_id()
    );
    assert_eq!(
        header_str(&response.headers, "x-aci-keyset-digest"),
        h.service.workload_keyset_digest()
    );
    assert_eq!(header_str(&response.headers, "x-e2ee-applied"), "false");
    let receipt_id = header_str(&response.headers, "x-receipt-id").to_string();

    {
        let calls = h.upstream_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].body, CHAT_REQUEST);
        assert!(calls[0].headers.is_empty());
    }

    {
        let verifier_calls = verifier_calls.lock().unwrap();
        assert_eq!(verifier_calls.len(), 1);
        assert_eq!(verifier_calls[0].model_id, "gpt-test");
        assert!(verifier_calls[0].required);
        assert_eq!(
            verifier_calls[0].forwarded_body_hash,
            sha256_hex(CHAT_REQUEST)
        );
    }

    let receipt = h
        .service
        .get_receipt_by_receipt_id(&receipt_id)
        .expect("receipt should be retained");
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-mock-1"));
    assert_eq!(receipt.workload_id, h.service.workload_id());
    assert_eq!(
        receipt.workload_keyset_digest,
        h.service.workload_keyset_digest()
    );
    assert_eq!(receipt.endpoint, "/v1/chat/completions");
    assert_eq!(receipt.method, "POST");
    assert_eq!(receipt.served_at, 1_700_000_000);

    let event_types: Vec<_> = receipt
        .event_log
        .iter()
        .map(|e| (e.seq, e.event_type.as_str()))
        .collect();
    assert_eq!(
        event_types,
        vec![
            (0, EVENT_REQUEST_RECEIVED),
            (1, EVENT_REQUEST_FORWARDED),
            (2, EVENT_UPSTREAM_VERIFIED),
            (3, EVENT_RESPONSE_RETURNED),
        ]
    );
    assert_eq!(
        event(&receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        event(&receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        event(&receipt, EVENT_UPSTREAM_VERIFIED)["result"],
        "verified"
    );
    assert_eq!(event(&receipt, EVENT_UPSTREAM_VERIFIED)["required"], true);
    assert_eq!(
        event(&receipt, EVENT_UPSTREAM_VERIFIED)["verifier_id"],
        "mock-verifier/v1"
    );
    assert_eq!(
        event(&receipt, EVENT_UPSTREAM_VERIFIED)["model_id"],
        "mock-model"
    );
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["cleartext_hash"],
        sha256_hex(CHAT_RESPONSE)
    );
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["wire_hash"],
        sha256_hex(CHAT_RESPONSE)
    );
    assert_valid_receipt_signature(&receipt, &h.receipt_keys[0]);

    let receipt_response = h.requester.get("/v1/signature/chat-mock-1").await;
    assert_eq!(receipt_response.status, StatusCode::OK);
    let receipt_json = json_body(&receipt_response);
    assert_eq!(receipt_json["receipt"]["receipt_id"], receipt_id);
    assert_eq!(
        receipt_json["text"].as_str().unwrap().matches(':').count(),
        1
    );
    assert!(receipt_json["signature"].is_string());
    assert_eq!(
        receipt_json["receipt"]["event_log"][2]["type"],
        EVENT_UPSTREAM_VERIFIED
    );
}

#[tokio::test]
async fn required_upstream_verification_failure_blocks_before_forwarding() {
    let (verifier, verifier_calls) = ScriptedVerifier::failed("quote app-id mismatch");
    let h = make_harness(verifier);

    let response = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers.get("x-receipt-id").is_none());
    assert_eq!(
        json_body(&response)["error"]["type"],
        "upstream_verification_failed"
    );
    assert!(json_body(&response)["error"]["message"]
        .as_str()
        .unwrap()
        .contains("quote app-id mismatch"));
    assert!(h.upstream_calls.lock().unwrap().is_empty());
    assert_eq!(verifier_calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn explicit_none_is_best_effort_and_receipt_records_failed_not_required() {
    let (verifier, _verifier_calls) = ScriptedVerifier::failed("cached evidence stale");
    let h = make_harness(verifier);

    let response = h
        .requester
        .post_chat(CHAT_REQUEST, &[("x-upstream-verification", "none")])
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(h.upstream_calls.lock().unwrap().len(), 1);

    let receipt_id = header_str(&response.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let uv = event(&receipt, EVENT_UPSTREAM_VERIFIED);
    assert_eq!(uv["result"], "failed");
    assert_eq!(uv["required"], false);
    assert_eq!(uv["reason"], "cached evidence stale");
}

#[tokio::test]
async fn client_supplied_hashes_and_aci_headers_do_not_override_service_observations() {
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);
    let forged_hash = format!("sha256:{}", "00".repeat(32));

    let response = h
        .requester
        .post_chat(
            CHAT_REQUEST,
            &[
                ("x-request-hash", &forged_hash),
                ("x-aci-identity", "sha256:forged"),
                ("x-aci-keyset-digest", "sha256:forged"),
            ],
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        header_str(&response.headers, "x-aci-identity"),
        h.service.workload_id()
    );
    assert_eq!(
        header_str(&response.headers, "x-aci-keyset-digest"),
        h.service.workload_keyset_digest()
    );

    let receipt_id = header_str(&response.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let actual = event(&receipt, EVENT_REQUEST_RECEIVED)["body_hash"]
        .as_str()
        .unwrap();
    assert_eq!(actual, sha256_hex(CHAT_REQUEST));
    assert_ne!(actual, forged_hash);
}

#[tokio::test]
async fn request_rewrite_receipt_distinguishes_received_and_forwarded_bytes() {
    let keys = Arc::new(StaticKeyProvider::default());
    let receipt_keys = keys.receipt_keys();
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, upstream_calls) = MockUpstream::new(200, CHAT_RESPONSE);
    let store = Arc::new(InMemoryReceiptStore::default());
    let service = AciService::new(
        keys,
        quoter,
        Arc::new(upstream),
        store,
        AciServiceConfig::for_test("private-ai-gateway"),
        Arc::new(FixedClock(1_700_000_000)),
    )
    .unwrap();

    let received = br#"{"model":"public-name","messages":[]}"#;
    let forwarded = br#"{"model":"private-upstream-name","messages":[]}"#;
    let verifier_event = UpstreamVerifiedEvent {
        url_origin: Some("https://mock-upstream.example".to_string()),
        verifier_id: "mock-verifier/v1".to_string(),
        evidence: Some(serde_json::json!({
            "digest": format!("sha256:{}", "cd".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoicHJpdmF0ZS11cHN0cmVhbS1uYW1lIn0=",
        })),
        ..verified_event("mock-upstream", "private-upstream-name")
    };

    let result = service
        .forward_chat_completion(
            received,
            Some(forwarded.to_vec()),
            Some(true),
            Some(verifier_event),
        )
        .await
        .unwrap();
    assert_eq!(upstream_calls.lock().unwrap()[0].body, forwarded);
    assert_eq!(
        event(&result.receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(received)
    );
    assert_eq!(
        event(&result.receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
        sha256_hex(forwarded)
    );
    let event_types: Vec<_> = result
        .receipt
        .event_log
        .iter()
        .map(|e| (e.seq, e.event_type.as_str()))
        .collect();
    assert_eq!(
        event_types,
        vec![
            (0, EVENT_REQUEST_RECEIVED),
            (1, EVENT_REQUEST_FORWARDED),
            (2, EVENT_TRANSPARENCY_REQUEST_MODIFIED),
            (3, EVENT_UPSTREAM_VERIFIED),
            (4, EVENT_RESPONSE_RETURNED),
        ]
    );
    assert_eq!(
        event(&result.receipt, EVENT_TRANSPARENCY_REQUEST_MODIFIED),
        &serde_json::json!({})
    );
    assert_ne!(
        event(&result.receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        event(&result.receipt, EVENT_REQUEST_FORWARDED)["body_hash"]
    );
    assert_valid_receipt_signature(&result.receipt, &receipt_keys[0]);
}

#[tokio::test]
async fn receipt_path_errors_follow_aci_shape() {
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);
    let chat = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(chat.status, StatusCode::OK);
    let receipt_id = header_str(&chat.headers, "x-receipt-id").to_string();

    let by_chat = h.requester.get("/v1/signature/chat-mock-1").await;
    assert_eq!(by_chat.status, StatusCode::OK);
    assert_eq!(json_body(&by_chat)["receipt"]["chat_id"], "chat-mock-1");
    assert_eq!(json_body(&by_chat)["receipt"]["receipt_id"], receipt_id);

    let unknown = h.requester.get("/v1/signature/missing").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(json_body(&unknown)["error"]["type"], "not_found");
}

#[tokio::test]
async fn invalid_request_inputs_are_rejected_before_verifier_or_upstream() {
    let (verifier, verifier_calls) = ScriptedVerifier::verified();
    let h = make_harness(verifier);

    let invalid_json = h.requester.post_chat(b"not-json", &[]).await;
    assert_eq!(invalid_json.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(&invalid_json)["error"]["type"],
        "invalid_request_error"
    );
    assert!(h.upstream_calls.lock().unwrap().is_empty());
    assert!(verifier_calls.lock().unwrap().is_empty());

    let invalid_header = h
        .requester
        .post_chat(CHAT_REQUEST, &[("x-upstream-verification", "maybe")])
        .await;
    assert_eq!(invalid_header.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(&invalid_header)["error"]["type"],
        "invalid_request_error"
    );
    assert!(h.upstream_calls.lock().unwrap().is_empty());
    assert!(verifier_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn non_2xx_upstream_response_is_still_bound_to_a_receipt() {
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let upstream_body = br#"{"error":{"message":"rate limited","type":"rate_limit"}}"#;
    let h = make_harness_with_upstream(verifier, 429, upstream_body);

    let response = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.body, upstream_body);
    assert_eq!(h.upstream_calls.lock().unwrap().len(), 1);

    let receipt_id = header_str(&response.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["cleartext_hash"],
        sha256_hex(upstream_body)
    );
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["wire_hash"],
        sha256_hex(upstream_body)
    );
    assert_valid_receipt_signature(&receipt, &h.receipt_keys[0]);
}

#[test]
fn future_aci_surfaces_not_covered_by_this_runnable_suite() {
    let missing = [
        "provider-specific upstream verifiers for real provider evidence",
        "TLS SPKI observation and enforcement by verifier/local proxy",
        "persistent receipt store for receipts and retained bodies",
        "real upstream verifier integrations for Chutes, Tinfoil, Phala, and others",
    ];
    assert_eq!(missing.len(), 4);
    assert!(missing.iter().all(|s| !s.is_empty()));
}

// ---------------------------------------------------------------------------
// Request-level failover: candidate loop, route attribution, receipt events.
// ---------------------------------------------------------------------------

fn router_service(router: ModelRouterBackend, verifier: ScriptedVerifier) -> Arc<AciService> {
    AciService::new_with_upstream_verifier(
        Arc::new(StaticKeyProvider::default()),
        Arc::new(StubQuoter::default()),
        Arc::new(router),
        Arc::new(verifier),
        Arc::new(InMemoryReceiptStore::default()),
        AciServiceConfig::for_test("private-ai-gateway"),
        Arc::new(FixedClock(1_700_000_000)),
    )
    .map(Arc::new)
    .unwrap()
}

async fn run_internal_forward(
    service: &Arc<AciService>,
    targets: &str,
    received: &[u8],
    upstream_required: bool,
) -> (HttpResult, MiddlewareReceiptJournal) {
    let request_store = GatewayRequestStore::default();
    let backend = MockRequester::new(build_internal_backend_router(
        service.clone(),
        request_store.clone(),
    ));
    let journal = MiddlewareReceiptJournal::default();
    request_store.insert(
        "req-failover".to_string(),
        StoredGatewayRequest {
            endpoint_path: "/v1/chat/completions",
            received_body: received.to_vec(),
            upstream_required,
            requester: None,
            e2ee: None,
            user_model: Some("public".to_string()),
            receipt_journal: journal.clone(),
        },
    );
    let response = backend
        .call(
            Request::builder()
                .method("POST")
                .uri("/internal/forward")
                .header("x-private-ai-gateway-request-id", "req-failover")
                .header("x-private-ai-gateway-targets", targets)
                .body(Body::from(received.to_vec()))
                .unwrap(),
        )
        .await;
    (response, journal)
}

type UpstreamCalls = Arc<Mutex<Vec<UpstreamCall>>>;

fn two_route_router(
    status_a: u16,
    body_a: &[u8],
    status_b: u16,
    body_b: &[u8],
) -> (ModelRouterBackend, UpstreamCalls, UpstreamCalls) {
    let (upstream_a, calls_a) =
        MockUpstream::named("upstream-a", "https://upstream-a.example", status_a, body_a);
    let (upstream_b, calls_b) =
        MockUpstream::named("upstream-b", "https://upstream-b.example", status_b, body_b);
    let upstream_a: Arc<dyn UpstreamBackend> = Arc::new(upstream_a);
    let upstream_b: Arc<dyn UpstreamBackend> = Arc::new(upstream_b);
    let mut router = ModelRouterBackend::new("model-router");
    router
        .add_route(
            ModelRoute::new(
                "public-a",
                "upstream-a-model",
                upstream_a,
                "upstream-a:public-a",
            )
            .unwrap(),
        )
        .unwrap();
    router
        .add_route(
            ModelRoute::new(
                "public-b",
                "upstream-b-model",
                upstream_b,
                "upstream-b:public-b",
            )
            .unwrap(),
        )
        .unwrap();
    (router, calls_a, calls_b)
}

#[tokio::test]
async fn failover_advances_to_next_candidate_on_provider_error() {
    let body_b = br#"{"id":"chat-b","object":"chat.completion","model":"upstream-b-model","choices":[{"index":0,"message":{"role":"assistant","content":"b"},"finish_reason":"stop"}]}"#;
    let (router, calls_a, calls_b) =
        two_route_router(503, br#"{"error":"unavailable"}"#, 200, body_b);
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let service = router_service(router, verifier);

    let received = br#"{"model":"public-a","messages":[]}"#;
    let (response, journal) = run_internal_forward(
        &service,
        "upstream-a:public-a,upstream-b:public-b",
        received,
        true,
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(calls_a.lock().unwrap().len(), 1, "r1 attempted once");
    assert_eq!(calls_b.lock().unwrap().len(), 1, "r2 served the request");
    assert_eq!(
        header_str(&response.headers, "x-private-ai-gateway-selected-route"),
        "upstream-b:public-b"
    );
    assert_eq!(
        header_str(&response.headers, "x-private-ai-gateway-attempts"),
        "2"
    );

    let draft = journal.take().expect("receipt draft");
    let finalized = service
        .finalize_middleware_receipt(
            draft,
            &response.body,
            Some(header_str(&response.headers, "content-type")),
            None,
            None,
        )
        .unwrap();
    // Failover is not in the receipt; the receipt attests only the served
    // (selected) route. The attempt count is surfaced via the header above.
    let receipt = finalized.receipt;
    assert_eq!(
        event(&receipt, EVENT_ROUTE_SELECTED)["target_route_id"],
        "upstream-b:public-b"
    );
}

#[tokio::test]
async fn failover_all_unknown_routes_returns_routing_error() {
    let (router, calls_a, _calls_b) =
        two_route_router(200, br#"{"id":"a"}"#, 200, br#"{"id":"b"}"#);
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let service = router_service(router, verifier);

    let received = br#"{"model":"public-a","messages":[]}"#;
    let (response, _journal) =
        run_internal_forward(&service, "missing-a:x,missing-b:y", received, true).await;

    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["type"], "model_routing_error");
    assert!(calls_a.lock().unwrap().is_empty());
}

#[tokio::test]
async fn non_tee_route_forwards_unattested_without_verifier() {
    // A non-TEE route forwards even with no verifier and no attestation
    // (never fail-closed). The receipt records this honestly on
    // `upstream.verified`: verifier_id "none", evidence null.
    let (upstream, calls) = MockUpstream::named(
        "openai",
        "https://api.openai.example",
        200,
        br#"{"id":"c","object":"chat.completion","model":"gpt-x","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
    );
    let upstream: Arc<dyn UpstreamBackend> = Arc::new(upstream);
    let mut router = ModelRouterBackend::new("model-router");
    router
        .add_route(
            ModelRoute::new("public", "gpt-x", upstream, "openai:public")
                .unwrap()
                .with_is_tee(Some(false)),
        )
        .unwrap();
    // Service WITHOUT a verifier: mirrors a real non-TEE upstream.
    let service = Arc::new(
        AciService::new(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(router),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );

    let received = br#"{"model":"public","messages":[]}"#;
    // `upstream_required: true` from the frontend must NOT fail-close a
    // route the backend classified as non-TEE.
    let (response, journal) = run_internal_forward(&service, "openai:public", received, true).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(calls.lock().unwrap().len(), 1);

    let draft = journal.take().expect("receipt draft");
    let finalized = service
        .finalize_middleware_receipt(
            draft,
            &response.body,
            Some(header_str(&response.headers, "content-type")),
            None,
            None,
        )
        .unwrap();
    let receipt = finalized.receipt;
    // Non-TEE is signalled by the existing upstream.verified fields:
    // no verifier ran and there is no attestation evidence.
    let verified = event(&receipt, EVENT_UPSTREAM_VERIFIED);
    assert_eq!(verified["verifier_id"], "none");
    assert!(verified
        .get("evidence")
        .map(|v| v.is_null())
        .unwrap_or(true));
    // The route is still bound and recorded as selected.
    assert_eq!(
        event(&receipt, EVENT_ROUTE_SELECTED)["target_route_id"],
        "openai:public"
    );
}

/// Verifier that fails verification for one named upstream and verifies
/// all others. Exercises per-candidate TEE fail-closed + failover.
struct KeyedVerifier {
    fail_for: String,
}

#[async_trait]
impl UpstreamVerifier for KeyedVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let verified = request.upstream_name != self.fail_for;
        let result = if verified {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        };
        UpstreamVerifiedEvent {
            verifier_id: "keyed-verifier/v1".to_string(),
            reason: (!verified).then(|| "verification failed".to_string()),
            ..event_from_request(&request, result)
        }
    }
}

fn keyed_verifier_service(router: ModelRouterBackend, fail_for: &str) -> Arc<AciService> {
    AciService::new_with_upstream_verifier(
        Arc::new(StaticKeyProvider::default()),
        Arc::new(StubQuoter::default()),
        Arc::new(router),
        Arc::new(KeyedVerifier {
            fail_for: fail_for.to_string(),
        }),
        Arc::new(InMemoryReceiptStore::default()),
        AciServiceConfig::for_test("private-ai-gateway"),
        Arc::new(FixedClock(1_700_000_000)),
    )
    .map(Arc::new)
    .unwrap()
}

#[tokio::test]
async fn failover_advances_when_first_tee_candidate_fails_verification() {
    let body_b = br#"{"id":"chat-b","object":"chat.completion","model":"upstream-b-model","choices":[{"index":0,"message":{"role":"assistant","content":"b"},"finish_reason":"stop"}]}"#;
    let (router, calls_a, calls_b) = two_route_router(200, br#"{"id":"a"}"#, 200, body_b);
    // upstream-a fails verification (fail-closed); upstream-b verifies.
    let service = keyed_verifier_service(router, "upstream-a");

    let received = br#"{"model":"public-a","messages":[]}"#;
    let (response, journal) = run_internal_forward(
        &service,
        "upstream-a:public-a,upstream-b:public-b",
        received,
        true,
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert!(
        calls_a.lock().unwrap().is_empty(),
        "r1 must never be forwarded after fail-closed verification"
    );
    assert_eq!(calls_b.lock().unwrap().len(), 1);
    assert_eq!(
        header_str(&response.headers, "x-private-ai-gateway-selected-route"),
        "upstream-b:public-b"
    );

    let draft = journal.take().expect("receipt draft");
    let receipt = service
        .finalize_middleware_receipt(
            draft,
            &response.body,
            Some(header_str(&response.headers, "content-type")),
            None,
            None,
        )
        .unwrap()
        .receipt;
    // The receipt attests only the served route; the failed first attempt
    // is not recorded in it (it surfaces via the attempts header → request_logs).
    assert_eq!(
        event(&receipt, EVENT_ROUTE_SELECTED)["target_route_id"],
        "upstream-b:public-b"
    );
}

#[tokio::test]
async fn failover_all_tee_routes_fail_verification_returns_503() {
    // Fail verification for every upstream (fail-closed for all).
    let (router, calls_a, calls_b) = two_route_router(200, br#"{"id":"a"}"#, 200, br#"{"id":"b"}"#);
    let (verifier, _calls) = ScriptedVerifier::failed("attestation rejected");
    let service = router_service(router, verifier);

    let received = br#"{"model":"public-a","messages":[]}"#;
    let (response, _journal) = run_internal_forward(
        &service,
        "upstream-a:public-a,upstream-b:public-b",
        received,
        true,
    )
    .await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["type"], "upstream_verification_failed");
    assert!(calls_a.lock().unwrap().is_empty());
    assert!(calls_b.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failover_envelope_form_splits_candidates_and_advances() {
    // Mixed-format envelope: each candidate carries its own body.
    let body_b = br#"{"id":"chat-b","object":"chat.completion","model":"upstream-b-model","choices":[{"index":0,"message":{"role":"assistant","content":"b"},"finish_reason":"stop"}]}"#;
    let (router, calls_a, calls_b) =
        two_route_router(503, br#"{"error":"unavailable"}"#, 200, body_b);
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let service = router_service(router, verifier);

    let envelope = br#"{"candidates":[
        {"target":"upstream-a:public-a","body":{"model":"public-a","messages":[]}},
        {"target":"upstream-b:public-b","body":{"model":"public-b","messages":[]}}
    ]}"#;

    let request_store = GatewayRequestStore::default();
    let backend = MockRequester::new(build_internal_backend_router(
        service.clone(),
        request_store.clone(),
    ));
    let journal = MiddlewareReceiptJournal::default();
    request_store.insert(
        "req-envelope".to_string(),
        StoredGatewayRequest {
            endpoint_path: "/v1/chat/completions",
            received_body: br#"{"model":"public","messages":[]}"#.to_vec(),
            upstream_required: true,
            requester: None,
            e2ee: None,
            user_model: Some("public".to_string()),
            receipt_journal: journal.clone(),
        },
    );
    // No -targets header: the envelope is authoritative.
    let response = backend
        .call(
            Request::builder()
                .method("POST")
                .uri("/internal/forward")
                .header("x-private-ai-gateway-request-id", "req-envelope")
                .body(Body::from(envelope.to_vec()))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(calls_a.lock().unwrap().len(), 1);
    assert_eq!(calls_b.lock().unwrap().len(), 1);
    // Candidate B's own body was forwarded (model rewritten to upstream-b-model).
    let forwarded_b: Value = serde_json::from_slice(&calls_b.lock().unwrap()[0].body).unwrap();
    assert_eq!(forwarded_b["model"], "upstream-b-model");
    assert_eq!(
        header_str(&response.headers, "x-private-ai-gateway-selected-route"),
        "upstream-b:public-b"
    );

    let receipt = service
        .finalize_middleware_receipt(
            journal.take().unwrap(),
            &response.body,
            Some(header_str(&response.headers, "content-type")),
            None,
            None,
        )
        .unwrap()
        .receipt;
    assert_eq!(
        event(&receipt, EVENT_ROUTE_SELECTED)["target_route_id"],
        "upstream-b:public-b"
    );
}

#[tokio::test]
async fn failover_envelope_rejects_mismatched_stream_flags() {
    let (router, _calls_a, _calls_b) =
        two_route_router(200, br#"{"id":"a"}"#, 200, br#"{"id":"b"}"#);
    let (verifier, _verifier_calls) = ScriptedVerifier::verified();
    let service = router_service(router, verifier);

    let envelope = br#"{"candidates":[
        {"target":"upstream-a:public-a","body":{"model":"public-a","messages":[],"stream":false}},
        {"target":"upstream-b:public-b","body":{"model":"public-b","messages":[],"stream":true}}
    ]}"#;

    let request_store = GatewayRequestStore::default();
    let backend = MockRequester::new(build_internal_backend_router(
        service.clone(),
        request_store.clone(),
    ));
    request_store.insert(
        "req-bad-stream".to_string(),
        StoredGatewayRequest {
            endpoint_path: "/v1/chat/completions",
            received_body: br#"{"model":"public","messages":[]}"#.to_vec(),
            upstream_required: true,
            requester: None,
            e2ee: None,
            user_model: Some("public".to_string()),
            receipt_journal: MiddlewareReceiptJournal::default(),
        },
    );
    let response = backend
        .call(
            Request::builder()
                .method("POST")
                .uri("/internal/forward")
                .header("x-private-ai-gateway-request-id", "req-bad-stream")
                .body(Body::from(envelope.to_vec()))
                .unwrap(),
        )
        .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
}
