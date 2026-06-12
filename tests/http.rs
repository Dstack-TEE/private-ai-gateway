//! End-to-end HTTP tests over the axum router.
//!
//! Uses `tower::ServiceExt::oneshot` to drive the router directly,
//! avoiding a TCP listener.

use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use private_ai_gateway::aci::keys::{verify_receipt_signature, KeyProvider};
use private_ai_gateway::aci::receipt::{
    canonical_bytes_for_signing, ChannelBinding, UpstreamVerifiedEvent, VerificationResult,
};
use private_ai_gateway::aci::types::{Receipt, ServiceCapabilities, TlsSpki};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::http::build_router;
use tower::ServiceExt;

use common::{StaticKeyProvider, StubQuoter};

const RESPONSE_BODY: &[u8] = br#"{"id":"chat-xyz","object":"chat.completion"}"#;

struct StubUpstream {
    body: Vec<u8>,
    received: Arc<Mutex<Option<Vec<u8>>>>,
}

impl StubUpstream {
    fn new(body: &[u8]) -> (Self, Arc<Mutex<Option<Vec<u8>>>>) {
        let received = Arc::new(Mutex::new(None));
        (
            StubUpstream {
                body: body.to_vec(),
                received: received.clone(),
            },
            received,
        )
    }
}

#[async_trait]
impl UpstreamBackend for StubUpstream {
    fn name(&self) -> &str {
        "stub-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("http://stub-upstream")
    }
    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        *self.received.lock().unwrap() = Some(req.body);
        let mut headers = std::collections::HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamResponse {
            status_code: 200,
            body: self.body.clone(),
            headers,
        })
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward(req.request).await
    }
}

struct TestHarness {
    service: Arc<AciService>,
    received: Arc<Mutex<Option<Vec<u8>>>>,
    receipt_keys: Vec<private_ai_gateway::aci::types::KeyedPublicKey>,
}

fn make_harness() -> TestHarness {
    make_harness_with_tls_public_keys(None)
}

fn make_harness_with_tls_public_keys(tls_public_keys: Option<Vec<TlsSpki>>) -> TestHarness {
    let keys = Arc::new(StaticKeyProvider::default());
    let receipt_keys = keys.receipt_keys();
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, received) = StubUpstream::new(RESPONSE_BODY);
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("private-ai-gateway");
    cfg.service_capabilities = ServiceCapabilities::default();
    cfg.tls_public_keys = tls_public_keys;
    let svc = AciService::new(
        keys,
        quoter,
        upstream,
        store,
        cfg,
        Arc::new(FixedClock(1_700_000_000)),
    )
    .unwrap();
    TestHarness {
        service: Arc::new(svc),
        received,
        receipt_keys,
    }
}

async fn body_bytes(b: Body) -> Vec<u8> {
    to_bytes(b, usize::MAX).await.unwrap().to_vec()
}

#[tokio::test]
async fn attestation_report_endpoint_shape() {
    let h = make_harness();
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/attestation/report?nonce=abcd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(body.get("api_version").unwrap(), "aci/1");
    assert_eq!(
        body.get("workload_id").unwrap().as_str().unwrap(),
        h.service.workload_id()
    );
    assert_eq!(
        body.get("workload_keyset_digest")
            .unwrap()
            .as_str()
            .unwrap(),
        h.service.workload_keyset_digest()
    );
    assert!(body
        .get("attestation")
        .unwrap()
        .get("report_data")
        .unwrap()
        .is_string());
    assert!(body
        .get("attestation")
        .unwrap()
        .get("keyset_endorsement")
        .unwrap()
        .get("value")
        .unwrap()
        .is_string());
    // The capability advertisement is empty by default; no E2EE
    // version is wired.
    assert_eq!(
        body.get("service_capabilities")
            .unwrap()
            .get("supported_e2ee_versions")
            .unwrap(),
        &serde_json::Value::Array(vec![])
    );
}

#[tokio::test]
async fn attestation_report_selects_domain_tls_binding_from_host_header() {
    let h = make_harness_with_tls_public_keys(Some(vec![
        TlsSpki {
            domain: Some("api.example.com".to_string()),
            spki_sha256_hex: "aa".repeat(32),
        },
        TlsSpki {
            domain: Some("chat.example.com".to_string()),
            spki_sha256_hex: "bb".repeat(32),
        },
    ]));
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/attestation/report?nonce=abcd")
                .header("host", "Api.Example.COM:443")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    let tls_keys = body["attestation"]["workload_keyset"]["tls_public_keys"]
        .as_array()
        .unwrap();
    assert_eq!(tls_keys.len(), 2);
    assert_eq!(tls_keys[0]["domain"], "api.example.com");
    assert_eq!(tls_keys[0]["spki_sha256"], "aa".repeat(32));
    assert_eq!(
        body["attestation"]["evidence"]["downstream_tls_binding"],
        serde_json::json!({
            "domain": "api.example.com",
            "spki_sha256": "aa".repeat(32),
        })
    );
}

#[tokio::test]
async fn attestation_report_nonce_null_when_absent() {
    let h = make_harness();
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/attestation/report")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();

    // Re-derive report_data with nonce=None and confirm match.
    let stmt =
        private_ai_gateway::aci::identity::attestation_statement(h.service.keyset(), None).unwrap();
    let expected_hex = hex::encode(private_ai_gateway::aci::identity::report_data(&stmt).unwrap());
    assert_eq!(
        body.get("attestation")
            .unwrap()
            .get("report_data")
            .unwrap()
            .as_str()
            .unwrap(),
        expected_hex
    );
}

#[tokio::test]
async fn chat_default_required_fails_closed_without_verifier() {
    let h = make_harness();
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(br#"{"model":"x","messages":[]}"#.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(
        body.get("error")
            .unwrap()
            .get("type")
            .unwrap()
            .as_str()
            .unwrap(),
        "upstream_verification_failed"
    );
    // Upstream MUST NOT have been called.
    assert!(h.received.lock().unwrap().is_none());
}

#[tokio::test]
async fn chat_opt_out_forwards_and_signs_receipt_with_failed_event() {
    let h = make_harness();
    let app = build_router(h.service.clone());

    let request_bytes = br#"{"model":"x","messages":[]}"#.to_vec();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-upstream-verification", "none")
                .body(Body::from(request_bytes.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let receipt_id = resp
        .headers()
        .get("x-receipt-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        resp.headers()
            .get("x-aci-identity")
            .unwrap()
            .to_str()
            .unwrap(),
        h.service.workload_id()
    );

    let receipt: Receipt = h
        .service
        .get_receipt_by_receipt_id(&receipt_id)
        .expect("receipt should be retained");

    // Aggregator receipt: upstream.verified must be present even in
    // the opt-out path, recorded as failed/no-verifier.
    let uv = receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "upstream.verified")
        .expect("upstream.verified must be emitted on opt-out");
    assert_eq!(uv.fields.get("result").unwrap().as_str().unwrap(), "failed");
    assert!(!uv.fields.get("required").unwrap().as_bool().unwrap());

    // request.received body_hash matches the bytes the launcher
    // received.
    let received = receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "request.received")
        .unwrap();
    let expected = private_ai_gateway::aci::canonical::sha256_hex(&request_bytes);
    assert_eq!(
        received.fields.get("body_hash").unwrap().as_str().unwrap(),
        expected
    );

    // Signature verifies under the keyset receipt key.
    let canonical_bytes = canonical_bytes_for_signing(&receipt).unwrap();
    let sig = hex::decode(&receipt.signature.value_hex).unwrap();
    assert!(verify_receipt_signature(
        &h.receipt_keys[0],
        &canonical_bytes,
        &sig
    ));
}

#[tokio::test]
async fn chat_x_request_hash_is_ignored() {
    let h = make_harness();
    let app = build_router(h.service.clone());

    let request_bytes = br#"{"model":"x","messages":[]}"#.to_vec();
    let attacker_hash = format!("sha256:{}", "00".repeat(32));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-upstream-verification", "none")
                .header("x-request-hash", attacker_hash.clone())
                .body(Body::from(request_bytes.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let receipt_id = resp
        .headers()
        .get("x-receipt-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let receipt = h.service.get_receipt_by_receipt_id(&receipt_id).unwrap();
    let received = receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "request.received")
        .unwrap();
    let actual = received.fields.get("body_hash").unwrap().as_str().unwrap();
    assert_ne!(
        actual, attacker_hash,
        "the launcher MUST NOT use client-supplied X-Request-Hash"
    );
    let expected = private_ai_gateway::aci::canonical::sha256_hex(&request_bytes);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn attested_session_lookup_returns_audit_record() {
    let h = make_harness();
    let event = UpstreamVerifiedEvent {
        vendor: "stub-upstream".to_string(),
        provider: None,
        model_id: "x".to_string(),
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: Some(serde_json::json!({
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoic3R1Yi11cHN0cmVhbS1hdHRlc3RhdGlvbiJ9",
        })),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://stub-upstream".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
        provider_claims: None,
    };
    let result = h
        .service
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, None, Some(event))
        .await
        .unwrap();
    let session_id = result
        .receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "upstream.verified")
        .and_then(|e| e.fields.get("session_id"))
        .and_then(|v| v.as_str())
        .expect("receipt should reference session")
        .to_string();

    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/aci/sessions/{session_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(body["api_version"], "aci.session.v1");
    assert_eq!(body["session_id"], session_id);
    assert_eq!(body["provider"], "stub-upstream");
    assert_eq!(body["endpoint"], "https://stub-upstream");
    assert_eq!(body["verifier_id"], "stub-verifier-1");
    assert_eq!(body["channel_binding"][0]["type"], "tls_spki_sha256");

    // Canonical receipt endpoint returns the bare signed receipt (no legacy
    // signature wrapper), addressable by the gateway receipt_id.
    let receipt_id = result.receipt.receipt_id.clone();
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/aci/receipts/{receipt_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(body["receipt_id"], receipt_id);
    assert_eq!(body["api_version"], "aci/1"); // signed ACI artifact keeps aci/1
    assert!(body.get("event_log").is_some());
    assert!(
        body.get("signature").is_some() && body.get("text").is_none(),
        "canonical receipt is bare, not the legacy signature wrapper"
    );

    // Sessions list, filtered by provider.
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/aci/sessions?provider=stub-upstream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(body["api_version"], "aci.session_list.v1");
    assert_eq!(body["sessions"][0]["session_id"], session_id);

    // Legacy alias still returns the dstack-vllm-proxy signature wrapper.
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/signature/{receipt_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert!(
        body.get("signing_address").is_some() && body.get("receipt").is_some(),
        "legacy alias keeps the signature-wrapper shape"
    );
}

#[tokio::test]
async fn chat_invalid_json_returns_400_before_forwarding() {
    let h = make_harness();
    let app = build_router(h.service.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-upstream-verification", "none")
                .body(Body::from("not json".as_bytes().to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(h.received.lock().unwrap().is_none());
}

#[tokio::test]
async fn chat_invalid_x_upstream_verification_header_rejected() {
    let h = make_harness();
    let app = build_router(h.service.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-upstream-verification", "maybe")
                .body(Body::from(br#"{"model":"x","messages":[]}"#.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(h.received.lock().unwrap().is_none());
}

#[tokio::test]
async fn receipt_lookup_by_chat_id() {
    let h = make_harness();
    let app = build_router(h.service.clone());

    // Issue a chat completion (opt-out).
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-upstream-verification", "none")
                .body(Body::from(br#"{"model":"x","messages":[]}"#.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/signature/chat-xyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(
        body.get("receipt")
            .unwrap()
            .get("chat_id")
            .unwrap()
            .as_str()
            .unwrap(),
        "chat-xyz"
    );
    assert_eq!(body.get("api_version").unwrap(), "aci/1");
    assert!(body.get("signature").unwrap().is_string());
}

#[tokio::test]
async fn receipt_lookup_unknown_chat_id_returns_404() {
    let h = make_harness();
    let app = build_router(h.service.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/signature/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(
        body.get("error")
            .unwrap()
            .get("type")
            .unwrap()
            .as_str()
            .unwrap(),
        "not_found"
    );
}
