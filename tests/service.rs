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
use private_ai_gateway::aggregator::session::{AttestedSession, ClaimStatus};
use private_ai_gateway::aggregator::session_store::SessionStore;
use private_ai_gateway::aggregator::upstream_config::UpstreamSessionSink;

use common::{failed_event, verified_event, StaticKeyProvider, StubQuoter};

type ReceivedBody = Arc<Mutex<Option<Vec<u8>>>>;

struct StubUpstream {
    body: Vec<u8>,
    received: ReceivedBody,
}

struct FailingSessionStore;

impl SessionStore for FailingSessionStore {
    fn put_session(&self, _session: AttestedSession, _ts: u64) -> std::io::Result<u64> {
        Err(std::io::Error::other("session store unavailable"))
    }

    fn get_session(&self, _session_id: &str, _now: u64) -> Option<AttestedSession> {
        None
    }

    fn list_sessions(&self, _provider: Option<&str>, _now: u64) -> Vec<AttestedSession> {
        Vec::new()
    }
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

fn make_service_raw(body: &[u8], upstream_required_default: bool) -> (AciService, ReceivedBody) {
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
    (svc, received)
}

fn make_service(body: &[u8], upstream_required_default: bool) -> (Arc<AciService>, ReceivedBody) {
    let (svc, received) = make_service_raw(body, upstream_required_default);
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
        url_origin: Some("http://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        ..verified_event("stub-upstream", "x")
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
        provider_claims: Some(serde_json::json!({
            "release": "fixture",
            "verified_claims": ["source-verified"]
        })),
        ..verified_event("stub-upstream", "x")
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
    assert_eq!(session.api_version, "aci/1");
    assert_eq!(session.provider, "stub-upstream");
    assert_eq!(session.endpoint.as_deref(), Some("https://stub-upstream"));
    assert_eq!(session.verifier_id, "stub-verifier-1");
    // provider_claims are folded verbatim into claims.extra; typed claims beyond
    // tee_attested stay Unknown until a per-provider mapping populates them.
    assert_eq!(
        session.claims.extra.get("release").and_then(|v| v.as_str()),
        Some("fixture")
    );
    assert_eq!(session.channel_binding.len(), 1);
    let binding = serde_json::to_value(&session.channel_binding[0]).unwrap();
    assert_eq!(binding["type"], "tls_spki_sha256");
    assert_eq!(
        binding["spki_sha256"],
        serde_json::Value::String("aa".repeat(32))
    );

    // Shallow audit: the receipt's upstream.verified carries the typed claim
    // verdicts inline. A verified result asserts tee_attested (verifier-derived).
    let receipt_claims = uv
        .fields
        .get("claims")
        .expect("upstream.verified must carry typed claim verdicts");
    assert_eq!(receipt_claims["tee_attested"]["status"], "asserted");
    assert_eq!(receipt_claims["tee_attested"]["source"], "verifier_derived");
    assert_eq!(receipt_claims["tcb_up_to_date"]["status"], "unknown");

    // Deep audit: the persisted session carries the same verdicts plus evidence.
    let session_claims = serde_json::to_value(&session.claims).unwrap();
    assert_eq!(session_claims["tee_attested"]["status"], "asserted");
    assert_eq!(
        session.evidence.digest.as_deref(),
        Some(format!("sha256:{}", "11".repeat(32)).as_str())
    );
}

#[tokio::test]
async fn verified_upstream_binding_fails_without_persisted_session() {
    let (svc, received) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#, true);
    let svc = svc.with_session_store(Arc::new(FailingSessionStore));
    let event = UpstreamVerifiedEvent {
        upstream_name: "stub-upstream".to_string(),
        provider: None,
        model_id: "x".to_string(),
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: None,
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://stub-upstream".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
        provider_claims: None,
    };

    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, None, Some(event))
        .await
        .expect_err("receipt must not cite a session that was not persisted");

    assert!(matches!(err, ServiceError::SessionStore(_)));
    assert!(received.lock().unwrap().is_some());
}

#[tokio::test]
async fn session_is_per_tee_channel_not_per_model() {
    // Two requests to the SAME TEE channel (same upstream / endpoint / binding /
    // evidence) routed to different models must collapse to ONE session: a
    // session attests the verified channel, not the model. (A router-based
    // upstream serving N models therefore yields 1 session, not N.) The model
    // served is recorded on the receipt, never on the session.
    let (svc, _) = make_service(br#"{"id":"chat-xyz"}"#, true);
    let event = |model: &str| UpstreamVerifiedEvent {
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://stub-upstream".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
        ..verified_event("stub-upstream", model)
    };
    let session_id_of = |result: &private_ai_gateway::aggregator::service::ForwardResult| {
        result
            .receipt
            .event_log
            .iter()
            .find(|e| e.event_type == "upstream.verified")
            .and_then(|e| e.fields.get("session_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    let r1 = svc
        .forward_chat_completion(
            br#"{"model":"model-a","messages":[]}"#,
            None,
            None,
            Some(event("model-a")),
        )
        .await
        .unwrap();
    let r2 = svc
        .forward_chat_completion(
            br#"{"model":"model-b","messages":[]}"#,
            None,
            None,
            Some(event("model-b")),
        )
        .await
        .unwrap();

    assert_eq!(
        session_id_of(&r1),
        session_id_of(&r2),
        "same TEE channel, different models -> one session"
    );
    // And only one session is stored for the channel.
    assert_eq!(svc.list_attested_sessions(Some("stub-upstream")).len(), 1);
}

#[tokio::test]
async fn attested_session_id_changes_when_verification_material_changes() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#, true);
    let make_event = |digest_byte: &str| UpstreamVerifiedEvent {
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        evidence: Some(serde_json::json!({
            "digest": format!("sha256:{}", digest_byte.repeat(32)),
            "data": "data:application/json;base64,eyJmaXh0dXJlIjoic3R1Yi11cHN0cmVhbS1hdHRlc3RhdGlvbiJ9",
        })),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://stub-upstream".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
        provider_claims: Some(serde_json::json!({
            "release": "fixture",
        })),
        ..verified_event("stub-upstream", "x")
    };

    let first = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            None,
            Some(make_event("11")),
        )
        .await
        .unwrap();
    let second = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            None,
            Some(make_event("22")),
        )
        .await
        .unwrap();

    let first_session_id = first
        .receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "upstream.verified")
        .and_then(|e| e.fields.get("session_id"))
        .and_then(|v| v.as_str())
        .expect("first verified binding should produce a session id");
    let second_session_id = second
        .receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "upstream.verified")
        .and_then(|e| e.fields.get("session_id"))
        .and_then(|v| v.as_str())
        .expect("second verified binding should produce a session id");

    assert_ne!(first_session_id, second_session_id);
    let first_session = svc
        .get_attested_session(first_session_id)
        .expect("first session should remain queryable");
    let second_session = svc
        .get_attested_session(second_session_id)
        .expect("second session should remain queryable");
    let first_digest = format!("sha256:{}", "11".repeat(32));
    let second_digest = format!("sha256:{}", "22".repeat(32));
    assert_eq!(
        first_session.evidence.digest.as_deref(),
        Some(first_digest.as_str())
    );
    assert_eq!(
        second_session.evidence.digest.as_deref(),
        Some(second_digest.as_str())
    );
}

#[tokio::test]
async fn verifier_event_failed_with_required_fails_before_forwarding() {
    let (svc, received) = make_service(br#"{"id":"chat-xyz"}"#, true);
    let event = UpstreamVerifiedEvent {
        verifier_id: "stub-verifier-1".to_string(),
        reason: Some("quote did not match expected app-id".to_string()),
        ..failed_event("stub-upstream", "x")
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
fn service_init_accepts_unknown_source_provenance() {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test("x");
    cfg.source_provenance = SourceProvenance::default();
    AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0))).unwrap();
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

#[tokio::test]
async fn background_verification_writes_inspectable_session_into_the_store() {
    let (service, _) = make_service(b"{}", true);
    let event = UpstreamVerifiedEvent {
        provider: Some("tinfoil".to_string()),
        url_origin: Some("https://preflight-upstream".to_string()),
        verifier_id: "preflight-verifier/v1".to_string(),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://preflight-upstream".to_string(),
            spki_sha256: "bb".repeat(32),
        }],
        provider_claims: Some(serde_json::json!({ "tcb_status": "UpToDate" })),
        ..verified_event("preflight-upstream", "preflight-model")
    };

    // Nothing verified yet — nothing to inspect.
    assert!(service.list_attested_sessions(None).is_empty());

    // The background verification writes the session through the sink — pure
    // attestation, no client request and no body. The preflight API then reads
    // this same store.
    service.record_session(&event);

    let listed = service.list_attested_sessions(Some("preflight-upstream"));
    assert_eq!(listed.len(), 1);
    let session = &listed[0];
    assert_eq!(session.provider, "preflight-upstream");
    assert_eq!(
        session.endpoint.as_deref(),
        Some("https://preflight-upstream")
    );
    // Typed claims are populated from the provider mapping (tinfoil + UpToDate).
    assert_eq!(session.claims.tee_attested.status, ClaimStatus::Asserted);
    assert_eq!(session.claims.tcb_up_to_date.status, ClaimStatus::Asserted);
    // Resolvable by its content-addressed id too.
    assert!(service.get_attested_session(&session.session_id).is_some());

    // Re-verifying the unchanged endpoint is idempotent: content-addressing
    // means refresh writes the same record and never churns the store (and a
    // later completion path references this same session rather than copying).
    let id = session.session_id.clone();
    service.record_session(&event);
    let after = service.list_attested_sessions(Some("preflight-upstream"));
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].session_id, id);
}
