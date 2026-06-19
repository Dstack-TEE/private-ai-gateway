//! End-to-end HTTP tests over the axum router.
//!
//! Uses `tower::ServiceExt::oneshot` to drive the router directly,
//! avoiding a TCP listener.

use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    extract::{RawQuery, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use private_ai_gateway::aci::keys::{verify_receipt_signature, KeyProvider};
use private_ai_gateway::aci::receipt::{
    canonical_bytes_for_signing, ChannelBinding, UpstreamVerifiedEvent,
};
use private_ai_gateway::aci::types::{Receipt, ServiceCapabilities, TlsSpki};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::aggregator::upstream_config::{
    UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::http::{build_router, build_router_with_admin};
use serde_json::Value;
use tower::ServiceExt;

use common::{verified_event, StaticKeyProvider, StubQuoter};

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
    let nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/attestation/report?nonce={nonce}"))
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

    // Legacy dstack-vllm-proxy compatibility fields are injected at the top
    // level (and mirrored into each all_attestations entry).
    let quote = body["attestation"]["evidence"]["quote"].as_str().unwrap();
    assert_eq!(body["intel_quote"].as_str().unwrap(), quote);
    assert_eq!(body["request_nonce"].as_str().unwrap(), nonce);
    let nvidia_payload: serde_json::Value =
        serde_json::from_str(body["nvidia_payload"].as_str().unwrap()).unwrap();
    assert_eq!(nvidia_payload["nonce"], nonce);
    assert_eq!(nvidia_payload["arch"], "HOPPER");
    // No model / no upstream configured → empty GPU evidence placeholder.
    assert_eq!(nvidia_payload["evidence_list"], serde_json::json!([]));
    let first = &body["all_attestations"][0];
    assert_eq!(first["intel_quote"].as_str().unwrap(), quote);
    assert_eq!(first["request_nonce"].as_str().unwrap(), nonce);

    // The quote binds the legacy report_data layout the old verifier parses:
    // signing_address(20) ‖ zeros(12) ‖ nonce(32). With a 32-byte hex nonce the
    // trailing field is the raw nonce bytes.
    let signing_address = body["signing_address"]
        .as_str()
        .unwrap()
        .trim_start_matches("0x");
    let report_data = body["attestation"]["evidence"]["quote_report_data"]
        .as_str()
        .unwrap();
    assert_eq!(&report_data[0..40], signing_address);
    assert_eq!(&report_data[40..64], &"00".repeat(12));
    assert_eq!(&report_data[64..128], nonce);
}

#[tokio::test]
async fn attestation_report_ed25519_binds_pubkey_identity() {
    // v1 + ed25519: report_data identity is the 32-byte ed25519 public key
    // (not a 20-byte address), then the raw nonce.
    let h = make_harness();
    let app = build_router(h.service.clone());
    let nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/attestation/report?nonce={nonce}&signing_algo=ed25519"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(body["signing_algo"], "ed25519");
    let signing_address = body["signing_address"].as_str().unwrap();
    let report_data = body["attestation"]["evidence"]["quote_report_data"]
        .as_str()
        .unwrap();
    assert_eq!(&report_data[0..64], signing_address);
    assert_eq!(&report_data[64..128], nonce);
}

#[tokio::test]
async fn attestation_report_v2_binds_sha256_of_address_and_tls_spki() {
    // v2: report_data identity is SHA256(signing_key ‖ TLS SPKI fingerprint).
    let spki = "aa".repeat(32);
    let h = make_harness_with_tls_public_keys(Some(vec![TlsSpki {
        domain: Some("api.example.com".to_string()),
        spki_sha256_hex: spki.clone(),
    }]));
    let app = build_router(h.service.clone());
    let nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/attestation/report?nonce={nonce}&signing_algo=ecdsa&version=2"
                ))
                .header("host", "api.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    let signing_address = body["signing_address"]
        .as_str()
        .unwrap()
        .trim_start_matches("0x");
    let report_data = body["attestation"]["evidence"]["quote_report_data"]
        .as_str()
        .unwrap();
    let mut preimage = hex::decode(signing_address).unwrap();
    preimage.extend(hex::decode(&spki).unwrap());
    let expected = private_ai_gateway::aci::canonical::sha256_hex(&preimage);
    let expected = expected.trim_start_matches("sha256:");
    assert_eq!(&report_data[0..64], expected);
    assert_eq!(&report_data[64..128], nonce);
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
async fn attestation_report_rejects_unconfigured_domain_tls_binding_host() {
    let h = make_harness_with_tls_public_keys(Some(vec![TlsSpki {
        domain: Some("api.example.com".to_string()),
        spki_sha256_hex: "aa".repeat(32),
    }]));
    let app = build_router(h.service.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/attestation/report?nonce=abcd")
                .header("host", "other.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    assert_eq!(body["error"]["type"], "not_found");
}

#[tokio::test]
async fn attestation_report_nonce_null_when_absent() {
    let h = make_harness();
    let app = build_router(h.service.clone());
    // The canonical ACI endpoint binds the statement digest as report_data; the
    // legacy endpoint instead binds the old signing_address ‖ nonce layout.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/aci/attestation")
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
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        evidence: Some(serde_json::json!({
            "digest": format!("sha256:{}", "11".repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoic3R1Yi11cHN0cmVhbS1hdHRlc3RhdGlvbiJ9",
        })),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://stub-upstream".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
        ..verified_event("stub-upstream", "x")
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
    assert_eq!(body["api_version"], "aci/1");
    assert_eq!(body["session_id"], session_id);
    assert_eq!(body["provider"], "stub-upstream");
    assert_eq!(body["endpoint"], "https://stub-upstream");
    assert_eq!(body["verifier_id"], "stub-verifier-1");
    assert_eq!(body["channel_binding"][0]["type"], "tls_spki_sha256");
    // The by-id record serves the full evidence bundle, including the data-URI.
    assert!(body["evidence"]["data"].is_string());

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
    assert_eq!(body["api_version"], "aci/1");
    assert_eq!(body["sessions"][0]["session_id"], session_id);
    // The broad list keeps the integrity digest but strips the evidence bytes;
    // fetch a single session by id for the full bundle (see above).
    assert!(body["sessions"][0]["evidence"]["digest"].is_string());
    assert!(body["sessions"][0]["evidence"]["data"].is_null());

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

// ---- Model-scoped attestation report (upstream GPU evidence) ----

fn upstream_runtime_options() -> UpstreamRuntimeOptions {
    UpstreamRuntimeOptions {
        verifier_mode: UpstreamVerifierMode::Preverified,
        accepted_workload_ids: vec![],
        accepted_image_digests: vec![],
        accepted_dstack_kms_root_public_keys: vec![],
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 30,
        verifier_request_timeout_seconds: 30,
    }
}

fn setup_with_config(config_json: &str) -> (Arc<AciService>, Router) {
    // Unique per call: a coarse system clock can hand concurrent tests the same
    // nanos, so an atomic counter guarantees distinct temp paths.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "pag-http-upstreams-{}-{}.json",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    std::fs::write(&path, config_json).unwrap();
    let manager = Arc::new(UpstreamConfigManager::load(&path, upstream_runtime_options()).unwrap());
    let keys = Arc::new(StaticKeyProvider::default());
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            Arc::new(StubQuoter::default()),
            manager.backend(),
            manager.verifier(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router_with_admin(service.clone(), manager, None);
    (service, app)
}

/// Records the query string and `Authorization` header the stub received.
type CapturedRequest = Arc<Mutex<Option<(String, Option<String>)>>>;

#[derive(Clone)]
struct PhalaStubState {
    captured: CapturedRequest,
    nvidia_payload: Option<String>,
}

async fn phala_attest_handler(
    State(s): State<PhalaStubState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    *s.captured.lock().unwrap() = Some((query.unwrap_or_default(), auth));
    match s.nvidia_payload {
        Some(payload) => Json(serde_json::json!({ "nvidia_payload": payload })).into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
    }
}

async fn serve_phala_stub(nvidia_payload: Option<String>) -> (String, CapturedRequest) {
    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/v1/attestation/report", get(phala_attest_handler))
        .with_state(PhalaStubState {
            captured: captured.clone(),
            nvidia_payload,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

const TEST_NONCE: &str = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";

#[tokio::test]
async fn attestation_report_merges_upstream_nvidia_payload() {
    let nvidia_payload =
        r#"{"nonce":"x","evidence_list":[{"certificate":"c","evidence":"e"}],"arch":"HOPPER"}"#
            .to_string();
    let (base, captured) = serve_phala_stub(Some(nvidia_payload.clone())).await;
    let config = format!(
        r#"[{{"name":"phala-a","provider":"phala-direct","base_url":"{base}","models":{{"phala/gemma":"gemma-up"}},"bearer_token":"tok"}}]"#
    );
    let (_svc, app) = setup_with_config(&config);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/attestation/report?model=phala/gemma&nonce={TEST_NONCE}&signing_algo=ecdsa"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();

    // The upstream's real nvidia_payload is merged at the top level and mirrored.
    assert_eq!(body["nvidia_payload"].as_str().unwrap(), nvidia_payload);
    assert_eq!(
        body["all_attestations"][0]["nvidia_payload"]
            .as_str()
            .unwrap(),
        nvidia_payload
    );
    // The gateway's own report fields are untouched.
    let quote = body["attestation"]["evidence"]["quote"].as_str().unwrap();
    assert_eq!(body["intel_quote"].as_str().unwrap(), quote);
    assert!(body["signing_address"].is_string());

    // The upstream was queried with the client nonce, version=2, and the bearer.
    let (query, auth) = captured.lock().unwrap().clone().unwrap();
    assert!(
        query.contains(&format!("nonce={TEST_NONCE}")),
        "query: {query}"
    );
    assert!(query.contains("version=2"), "query: {query}");
    assert_eq!(auth.as_deref(), Some("Bearer tok"));
}

#[tokio::test]
async fn attestation_report_generates_nonce_and_merges_when_client_omits_nonce() {
    // No client nonce: the gateway generates one (like dstack-vllm-proxy) and
    // still fetches/merges the upstream GPU evidence, rather than returning an
    // empty placeholder.
    let nvidia_payload =
        r#"{"nonce":"x","evidence_list":[{"certificate":"c","evidence":"e"}],"arch":"HOPPER"}"#
            .to_string();
    let (base, captured) = serve_phala_stub(Some(nvidia_payload.clone())).await;
    let config = format!(
        r#"[{{"name":"phala-a","provider":"phala-direct","base_url":"{base}","models":{{"phala/gemma":"gemma-up"}},"bearer_token":"tok"}}]"#
    );
    let (_svc, app) = setup_with_config(&config);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/attestation/report?model=phala/gemma")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();

    // Real evidence merged (not the empty placeholder).
    assert_eq!(body["nvidia_payload"].as_str().unwrap(), nvidia_payload);
    // A 32-byte hex nonce was generated and echoed.
    let request_nonce = body["request_nonce"].as_str().unwrap();
    assert_eq!(request_nonce.len(), 64);
    assert!(request_nonce.chars().all(|c| c.is_ascii_hexdigit()));
    // The upstream was queried with that generated nonce.
    let (query, _auth) = captured.lock().unwrap().clone().unwrap();
    assert!(
        query.contains(&format!("nonce={request_nonce}")),
        "query: {query}"
    );
}

#[tokio::test]
async fn attestation_report_degrades_to_empty_nvidia_on_upstream_error() {
    let (base, _captured) = serve_phala_stub(None).await; // upstream returns 500
    let config = format!(
        r#"[{{"name":"phala-a","provider":"phala-direct","base_url":"{base}","models":{{"phala/gemma":"gemma-up"}},"bearer_token":"tok"}}]"#
    );
    let (_svc, app) = setup_with_config(&config);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/attestation/report?model=phala/gemma&nonce={TEST_NONCE}&signing_algo=ecdsa"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();
    let nvidia: Value = serde_json::from_str(body["nvidia_payload"].as_str().unwrap()).unwrap();
    assert_eq!(nvidia["evidence_list"], serde_json::json!([]));
    assert_eq!(nvidia["nonce"], TEST_NONCE);
}

async fn chutes_instances_handler() -> Json<Value> {
    Json(serde_json::json!({
        "instances": [{"instance_id": "inst-1", "e2e_pubkey": "pk-1", "nonces": ["n1"]}],
        "nonce_expires_in": 60,
    }))
}

async fn chutes_evidence_handler() -> Json<Value> {
    Json(serde_json::json!({
        "evidence": [{
            "instance_id": "inst-1",
            "quote": "cXVvdGUtYjY0",
            "gpu_evidence": [{"arch": "HOPPER"}],
        }]
    }))
}

async fn serve_chutes_stub() -> String {
    let app = Router::new()
        .route("/e2e/instances/:chute_id", get(chutes_instances_handler))
        .route("/chutes/:chute_id/evidence", get(chutes_evidence_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn attestation_report_chutes_multi_instance_shape() {
    let base = serve_chutes_stub().await;
    let config = format!(
        r#"[{{"name":"chutes-a","provider":"chutes","base_url":"{base}","models":{{"chutes/m":"m-up"}},"bearer_token":"tok","chutes_e2ee_api_base":"{base}","chutes_chute_ids":{{"m-up":"00000000-0000-0000-0000-0000000c1234"}}}}]"#
    );
    let (_svc, app) = setup_with_config(&config);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/attestation/report?model=chutes/m")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(resp.into_body()).await).unwrap();

    assert_eq!(body["attestation_type"], "chutes");
    let nonce = body["nonce"].as_str().unwrap();
    let entries = body["all_attestations"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["instance_id"], "inst-1");
    assert_eq!(entries[0]["e2e_pubkey"], "pk-1");
    assert_eq!(entries[0]["nonce"].as_str().unwrap(), nonce);
    assert_eq!(entries[0]["intel_quote"], "cXVvdGUtYjY0");
    assert!(entries[0]["gpu_evidence"].is_array());
}
