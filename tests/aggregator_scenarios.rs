//! End-to-end ACI aggregator scenarios with a mock requester, mock upstream,
//! and mock upstream verifier.
//!
//! This file is the executable conformance sketch for the aggregator slice:
//! it drives the public HTTP router where possible and drops to `AciService`
//! only for behavior that is not yet surfaced as an HTTP feature, such as
//! request rewriting.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::identity;
use private_ai_gateway::aci::keys::{verify_receipt_signature, KeyProvider};
use private_ai_gateway::aci::receipt::{
    SignedReceipt, UpstreamVerifiedEvent, VerificationResult, EVENT_REQUEST_FORWARDED,
    EVENT_REQUEST_RECEIVED, EVENT_RESPONSE_RETURNED, EVENT_UPSTREAM_VERIFIED,
};
use private_ai_gateway::aci::types::{KeyedPublicKey, ServiceCapabilities};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore, UpstreamVerificationRequest,
    UpstreamVerifier,
};
use private_ai_gateway::http::build_router;
use serde_json::Value;
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
    #[allow(dead_code)]
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
                    served_instance_id: None,
                }),
                models_response: Mutex::new(UpstreamResponse {
                    status_code: 200,
                    body: models_body.to_vec(),
                    headers: HashMap::from([(
                        "content-type".to_string(),
                        "application/json".to_string(),
                    )]),
                    served_instance_id: None,
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

    // The mock stands in for a backend that enforces the verifier's channel
    // binding on its connection (the trait default fails closed).
    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(req).await
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

struct Harness {
    service: Arc<AciService>,
    requester: MockRequester,
    upstream_calls: Arc<Mutex<Vec<UpstreamCall>>>,
    receipt_keys: Vec<KeyedPublicKey>,
}

fn make_harness(verifier: ScriptedVerifier) -> Harness {
    make_harness_full(verifier, 200, CHAT_RESPONSE, true)
}

/// Service policy does not require verification: forward and record failed.
fn make_harness_not_required(verifier: ScriptedVerifier) -> Harness {
    make_harness_full(verifier, 200, CHAT_RESPONSE, false)
}

fn make_harness_with_upstream(
    verifier: ScriptedVerifier,
    upstream_status: u16,
    upstream_body: &[u8],
) -> Harness {
    make_harness_full(verifier, upstream_status, upstream_body, true)
}

fn make_harness_full(
    verifier: ScriptedVerifier,
    upstream_status: u16,
    upstream_body: &[u8],
    upstream_required_default: bool,
) -> Harness {
    let keys = Arc::new(StaticKeyProvider::default());
    let receipt_keys = keys.receipt_keys();
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, upstream_calls) = MockUpstream::new(upstream_status, upstream_body);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test();
    cfg.upstream_required_default = upstream_required_default;
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

fn event(receipt: &SignedReceipt, event_type: &str) -> Value {
    receipt.payload_json().unwrap()["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == event_type)
        .unwrap_or_else(|| panic!("receipt must carry {event_type}"))
        .clone()
}

fn assert_valid_receipt_signature(receipt: &SignedReceipt, receipt_key: &KeyedPublicKey) {
    let signature = hex::decode(&receipt.signature_hex).unwrap();
    assert!(verify_receipt_signature(
        receipt_key,
        &receipt.payload,
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
        .attestation_report(Some("rp-nonce".to_string()))
        .await
        .unwrap();
    // Recompute the §10.1 binding chain from the served bytes.
    let keyset_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &report.attestation.workload_keyset_b64,
    )
    .unwrap();
    assert_eq!(
        identity::workload_keyset_digest(&keyset_bytes),
        report.workload_keyset_digest
    );
    let statement =
        identity::attestation_statement(&report.workload_keyset_digest, Some("rp-nonce")).unwrap();
    assert_eq!(
        report.attestation.report_data_hex,
        hex::encode(identity::report_data(&statement))
    );

    let response = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(response.status, StatusCode::OK);
    let receipt_id = header_str(&response.headers, "x-receipt-id").to_string();
    let receipt = h
        .service
        .get_receipt_by_receipt_id(&receipt_id)
        .expect("receipt should be retained");

    let payload = receipt.payload_json().unwrap();
    assert_eq!(
        payload["workload_keyset_digest"],
        report.workload_keyset_digest
    );
    assert_eq!(
        event(&receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["body_hash"],
        sha256_hex(CHAT_RESPONSE)
    );

    // Resolve the signing key in the keyset decoded from the served bytes.
    let keyset: private_ai_gateway::aci::types::WorkloadKeyset =
        serde_json::from_slice(&keyset_bytes).unwrap();
    let signature = hex::decode(&receipt.signature_hex).unwrap();
    let receipt_key = keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == receipt.key_id)
        .expect("receipt key must be in attested keyset");
    assert!(verify_receipt_signature(
        receipt_key,
        &receipt.payload,
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
    assert!(response.headers.get("x-aci-identity").is_none());
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
    let payload = receipt.payload_json().unwrap();
    assert_eq!(
        payload["workload_keyset_digest"],
        h.service.workload_keyset_digest()
    );
    assert_eq!(payload["endpoint"], "/v1/chat/completions");
    assert_eq!(payload["method"], "POST");
    assert_eq!(payload["served_at"], 1_700_000_000);

    let event_types: Vec<&str> = payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        event_types,
        vec![
            EVENT_REQUEST_RECEIVED,
            EVENT_REQUEST_FORWARDED,
            EVENT_UPSTREAM_VERIFIED,
            EVENT_RESPONSE_RETURNED,
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
        event(&receipt, EVENT_UPSTREAM_VERIFIED)["model_id"],
        "mock-model"
    );
    assert!(event(&receipt, EVENT_UPSTREAM_VERIFIED)["session_id"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["body_hash"],
        sha256_hex(CHAT_RESPONSE)
    );
    assert_valid_receipt_signature(&receipt, &h.receipt_keys[0]);

    let receipt_response = h.requester.get("/v1/signature/chat-mock-1").await;
    assert_eq!(receipt_response.status, StatusCode::OK);
    let receipt_json = json_body(&receipt_response);
    assert_eq!(
        receipt_json["text"].as_str().unwrap().matches(':').count(),
        1
    );
    assert!(receipt_json["signature"].is_string());
    assert!(receipt_json["receipt"]["payload_b64"].is_string());
}

#[tokio::test]
async fn required_upstream_verification_failure_blocks_before_forwarding() {
    let (verifier, verifier_calls) = ScriptedVerifier::failed("quote app-id mismatch");
    let h = make_harness(verifier);

    let response = h.requester.post_chat(CHAT_REQUEST, &[]).await;
    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
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

    // The refusal is receipt-backed (§8.5): the receipt records the failed
    // event and the exact error body served, with nothing forwarded.
    let receipt_id = header_str(&response.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let uv = event(&receipt, EVENT_UPSTREAM_VERIFIED);
    assert_eq!(uv["result"], "failed");
    assert_eq!(uv["required"], true);
    assert_eq!(uv["reason"], "quote app-id mismatch");
    assert_eq!(
        event(&receipt, EVENT_RESPONSE_RETURNED)["body_hash"],
        sha256_hex(&response.body)
    );
    assert_valid_receipt_signature(&receipt, &h.receipt_keys[0]);
}

#[tokio::test]
async fn optional_verification_is_best_effort_and_receipt_records_failed_not_required() {
    let (verifier, _verifier_calls) = ScriptedVerifier::failed("cached evidence stale");
    let h = make_harness_not_required(verifier);

    let response = h.requester.post_chat(CHAT_REQUEST, &[]).await;
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
        header_str(&response.headers, "x-aci-keyset-digest"),
        h.service.workload_keyset_digest()
    );

    let receipt_id = header_str(&response.headers, "x-receipt-id");
    let receipt = h.service.get_receipt_by_receipt_id(receipt_id).unwrap();
    let received = event(&receipt, EVENT_REQUEST_RECEIVED);
    let actual = received["body_hash"].as_str().unwrap();
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
        AciServiceConfig::for_test(),
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
    let payload = result.receipt.payload_json().unwrap();
    let event_types: Vec<&str> = payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        event_types,
        vec![
            EVENT_REQUEST_RECEIVED,
            EVENT_REQUEST_FORWARDED,
            EVENT_UPSTREAM_VERIFIED,
            EVENT_RESPONSE_RETURNED,
        ]
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
    let payload: Value = serde_json::from_slice(
        &base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            json_body(&by_chat)["receipt"]["payload_b64"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(payload["chat_id"], "chat-mock-1");
    assert_eq!(payload["receipt_id"], receipt_id);

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
        event(&receipt, EVENT_RESPONSE_RETURNED)["body_hash"],
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
