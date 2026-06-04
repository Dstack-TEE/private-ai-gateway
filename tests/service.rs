//! Service composition tests: fail-closed defaults, source
//! provenance, capability default, X-Upstream-Verification semantics.

use std::sync::{Arc, Mutex};

mod common;

use async_trait::async_trait;
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::types::{ServiceCapabilities, SourceProvenance};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore, ServiceError,
    UpstreamVerificationError,
};

use common::{StaticKeyProvider, StubQuoter};

type ReceivedBody = Arc<Mutex<Option<Vec<u8>>>>;

struct StubUpstream {
    body: Vec<u8>,
    received: ReceivedBody,
}

impl StubUpstream {
    fn new(body: &[u8]) -> (Self, ReceivedBody) {
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
        Ok(UpstreamResponse {
            status_code: 200,
            body: self.body.clone(),
            headers: Default::default(),
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

fn make_service(body: &[u8], upstream_required_default: bool) -> (Arc<AciService>, ReceivedBody) {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, received) = StubUpstream::new(body);
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("private-ai-gateway");
    cfg.upstream_required_default = upstream_required_default;
    // Do not advertise unwired E2EE.
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: vec![],
        body_retention_seconds: 0,
    };
    let svc = AciService::new(
        keys,
        quoter,
        upstream,
        store,
        cfg,
        Arc::new(FixedClock(1_700_000_000)),
    )
    .unwrap();
    (Arc::new(svc), received)
}

#[tokio::test]
async fn default_required_with_no_verifier_fails_closed_before_forwarding() {
    let (svc, received) = make_service(br#"{"id":"x"}"#, true);
    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ServiceError::UpstreamVerification(UpstreamVerificationError::NoVerifierResult)
    ));
    assert!(received.lock().unwrap().is_none());
}

#[tokio::test]
async fn x_upstream_verification_none_forwards_and_records_failed_event() {
    let (svc, received) = make_service(br#"{"id":"chat-xyz"}"#, true);
    let result = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, Some(false), None)
        .await
        .unwrap();
    assert_eq!(result.upstream_status, 200);
    assert!(received.lock().unwrap().is_some());

    // Aggregator receipts always carry upstream.verified. The opt-out
    // path records a synthesised failed event so a downstream
    // verifier sees the actual state.
    let uv = result
        .receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "upstream.verified")
        .expect("opt-out must still record upstream.verified");
    assert_eq!(uv.fields.get("result").unwrap().as_str().unwrap(), "failed");
    assert!(!uv.fields.get("required").unwrap().as_bool().unwrap());
    assert_eq!(
        uv.fields.get("verifier_id").unwrap().as_str().unwrap(),
        "none"
    );
    let reason = uv.fields.get("reason").unwrap().as_str().unwrap();
    assert!(
        reason.contains("no upstream verifier"),
        "reason should explain why result is failed, got {reason:?}"
    );
}

#[tokio::test]
async fn x_request_hash_header_value_does_not_enter_request_received_hash() {
    // The service computes request.received from the bytes axum
    // observed. The body source is the function argument; this test
    // simulates a malicious "trusted" X-Request-Hash value by
    // hashing an *empty* body and confirming the body_hash field
    // records the hash of the actual bytes the service received.
    let (svc, _) = make_service(br#"{"id":"chat-xyz"}"#, true);
    let body = br#"{"model":"x","messages":[]}"#;
    let result = svc
        .forward_chat_completion(body, None, Some(false), None)
        .await
        .unwrap();
    let received = result
        .receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "request.received")
        .unwrap();
    let actual = received.fields.get("body_hash").unwrap().as_str().unwrap();
    // Hash of the empty body: an attacker pre-computes this and
    // would supply it via X-Request-Hash. The service must NEVER
    // surface that value.
    let attacker_hash = private_ai_gateway::aci::canonical::sha256_hex(b"");
    assert_ne!(actual, attacker_hash);

    let expected = private_ai_gateway::aci::canonical::sha256_hex(body);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn verifier_event_result_verified_emits_upstream_verified() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz"}"#, true);
    let event = UpstreamVerifiedEvent {
        vendor: "stub-upstream".to_string(),
        model_id: "x".to_string(),
        url_origin: Some("http://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: None,
        channel_bindings: Vec::new(),
        provider_claims: None,
    };
    let result = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, None, Some(event))
        .await
        .unwrap();
    let uv = result
        .receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "upstream.verified")
        .expect("must emit upstream.verified");
    assert_eq!(
        uv.fields.get("result").unwrap().as_str().unwrap(),
        "verified"
    );
    assert_eq!(
        uv.fields.get("verifier_id").unwrap().as_str().unwrap(),
        "stub-verifier-1"
    );
}

#[tokio::test]
async fn verified_upstream_binding_creates_attested_session() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#, true);
    let event = UpstreamVerifiedEvent {
        vendor: "stub-upstream".to_string(),
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
        provider_claims: Some(serde_json::json!({
            "release": "fixture",
            "verified_claims": ["source-verified"]
        })),
    };

    let result = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, None, Some(event))
        .await
        .unwrap();
    let uv = result
        .receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "upstream.verified")
        .expect("must emit upstream.verified");
    let session_id = uv
        .fields
        .get("session_id")
        .and_then(|v| v.as_str())
        .expect("verified binding should produce a session id");
    let session = svc
        .get_attested_session(session_id)
        .expect("session audit record should be queryable");
    assert_eq!(session.session_id, session_id);
    assert_eq!(session.direction, "upstream");
    assert_eq!(session.upstream.provider, "stub-upstream");
    assert_eq!(session.upstream.model_id.as_deref(), Some("x"));
    assert_eq!(
        session.upstream.endpoint_origin.as_deref(),
        Some("https://stub-upstream")
    );
    assert_eq!(session.verification.verifier_id, "stub-verifier-1");
    assert_eq!(
        session.verification.verified_claims,
        vec![
            "encrypted-session-verified".to_string(),
            "source-verified".to_string()
        ]
    );
    assert_eq!(session.session_binding.len(), 1);
    assert_eq!(
        session.session_binding[0]["spki_sha256"],
        serde_json::Value::String("aa".repeat(32))
    );
}

#[tokio::test]
async fn verifier_event_failed_with_required_fails_before_forwarding() {
    let (svc, received) = make_service(br#"{"id":"chat-xyz"}"#, true);
    let event = UpstreamVerifiedEvent {
        vendor: "stub-upstream".to_string(),
        model_id: "x".to_string(),
        url_origin: None,
        verifier_id: "stub-verifier-1".to_string(),
        result: VerificationResult::Failed,
        required: true,
        reason: Some("quote did not match expected app-id".to_string()),
        evidence: None,
        channel_bindings: Vec::new(),
        provider_claims: None,
    };
    let err = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            Some(true),
            Some(event),
        )
        .await
        .unwrap_err();
    match err {
        ServiceError::UpstreamVerification(UpstreamVerificationError::VerifierFailed(reason)) => {
            assert!(reason.contains("quote did not match"));
        }
        other => panic!("expected VerifierFailed, got {other:?}"),
    }
    assert!(received.lock().unwrap().is_none());
}

#[test]
fn service_init_rejects_empty_source_provenance() {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("x");
    cfg.source_provenance = SourceProvenance::default();
    let err = AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0)))
        .err()
        .expect("must fail");
    assert!(matches!(err, ServiceError::InvalidSourceProvenance));
}

#[test]
fn service_init_rejects_partial_repo_provenance() {
    for sp in [
        SourceProvenance {
            repo_url: Some("https://github.com/x/y".to_string()),
            repo_commit: None,
            image_digest: None,
            image_provenance: None,
        },
        SourceProvenance {
            repo_url: None,
            repo_commit: Some("deadbeef".to_string()),
            image_digest: None,
            image_provenance: None,
        },
    ] {
        let keys = Arc::new(StaticKeyProvider::default());
        let quoter = Arc::new(StubQuoter::default());
        let (upstream, _) = StubUpstream::new(b"{}");
        let upstream = Arc::new(upstream);
        let store = Arc::new(InMemoryReceiptStore::default());
        let mut cfg = AciServiceConfig::for_test("x");
        cfg.source_provenance = sp;
        let err = AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0)))
            .err()
            .expect("must fail");
        assert!(matches!(err, ServiceError::InvalidSourceProvenance));
    }
}

#[test]
fn service_init_accepts_image_digest_only_provenance() {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("x");
    cfg.source_provenance = SourceProvenance {
        repo_url: None,
        repo_commit: None,
        image_digest: Some(format!("sha256:{}", "ab".repeat(32))),
        image_provenance: None,
    };
    AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0))).unwrap();
}

#[test]
fn service_refuses_test_keys_in_production_mode() {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("x");
    cfg.allow_test_keys = false;
    let err = AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0)))
        .err()
        .expect("must fail");
    assert!(matches!(err, ServiceError::TestKeysInProduction));
}

#[tokio::test]
async fn attestation_report_does_not_advertise_unwired_e2ee_by_default() {
    let (svc, _) = make_service(b"{}", true);
    let report = svc.attestation_report(None).await.unwrap();
    assert!(report
        .service_capabilities
        .supported_e2ee_versions
        .is_empty());
}
