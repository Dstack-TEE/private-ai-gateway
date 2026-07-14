//! Service composition tests: fail-closed defaults, source
//! provenance, capability default, upstream-verification semantics.

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

/// Find one event object in a signed receipt's payload.
fn payload_event(
    receipt: &private_ai_gateway::aci::receipt::SignedReceipt,
    event_type: &str,
) -> serde_json::Value {
    receipt.payload_json().unwrap()["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == event_type)
        .unwrap_or_else(|| panic!("receipt must carry {event_type}"))
        .clone()
}

type ReceivedBody = Arc<Mutex<Option<Vec<u8>>>>;

struct StubUpstream {
    body: Vec<u8>,
    received: ReceivedBody,
}

struct FailingSessionStore;

impl SessionStore for FailingSessionStore {
    fn put_session(
        &self,
        _fingerprint: &str,
        _session: AttestedSession,
        _retention_until: u64,
        _now: u64,
    ) -> std::io::Result<u64> {
        Err(std::io::Error::other("session store unavailable"))
    }

    fn get_session(&self, _session_id: &str, _now: u64) -> Option<AttestedSession> {
        None
    }

    fn current_session(
        &self,
        _fingerprint: &str,
        _retention_until: u64,
        _now: u64,
    ) -> Option<AttestedSession> {
        // Always a miss, so the caller falls through to the failing `put_session`.
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
            served_instance_id: None,
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
    let mut cfg = AciServiceConfig::for_test();
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
    match err {
        ServiceError::UpstreamVerification(UpstreamVerificationError { reason, event }) => {
            assert!(reason.contains("no upstream verifier"), "{reason:?}");
            assert!(event.required);
        }
        other => panic!("expected UpstreamVerification, got {other:?}"),
    }
    assert!(received.lock().unwrap().is_none());
}

#[tokio::test]
async fn verification_opt_out_forwards_and_records_failed_event() {
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
    let uv = payload_event(&result.receipt, "upstream.verified");
    assert_eq!(uv["result"], "failed");
    assert_eq!(uv["required"], false);
    let reason = uv["reason"].as_str().unwrap();
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
    let received = payload_event(&result.receipt, "request.received");
    let actual = received["body_hash"].as_str().unwrap();
    // Hash of the empty body: an attacker pre-computes this and
    // would supply it via X-Request-Hash. The service must NEVER
    // surface that value.
    let attacker_hash = private_ai_gateway::aci::digest::sha256_hex(b"");
    assert_ne!(actual, attacker_hash);

    let expected = private_ai_gateway::aci::digest::sha256_hex(body);
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
    let uv = payload_event(&result.receipt, "upstream.verified");
    assert_eq!(uv["result"], "verified");
    // The event is slim (§8.5): verification detail lives in the cited session.
    assert!(uv.get("verifier_id").is_none());
    assert!(uv["session_id"].as_str().unwrap().starts_with("sha256:"));
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
    let uv = payload_event(&result.receipt, "upstream.verified");
    let session_id = uv["session_id"]
        .as_str()
        .expect("verified binding should produce a session id");
    let session = svc
        .get_attested_session(session_id)
        .expect("session audit record should be queryable");
    // The id is the hash of the exact served bytes (§9), never stored inside.
    assert_eq!(session.session_id(), session_id);
    assert_eq!(
        session_id,
        format!(
            "sha256:{}",
            hex::encode(private_ai_gateway::aci::digest::sha256_raw(session.bytes()))
        )
    );
    let document = session.document();
    assert_eq!(document.api_version, "aci/1");
    assert_eq!(document.upstream_name, "stub-upstream");
    assert_eq!(document.endpoint.as_deref(), Some("https://stub-upstream"));
    assert_eq!(document.verifier_id, "stub-verifier-1");
    // provider_claims are folded verbatim into claims.extra; typed claims beyond
    // tee_attested stay Unknown until a per-provider mapping populates them.
    assert_eq!(
        document
            .claims
            .extra
            .get("release")
            .and_then(|v| v.as_str()),
        Some("fixture")
    );
    assert_eq!(document.channel_binding.len(), 1);
    let binding = serde_json::to_value(&document.channel_binding[0]).unwrap();
    assert_eq!(binding["type"], "tls_spki_sha256");
    assert_eq!(
        binding["spki_sha256"],
        serde_json::Value::String("aa".repeat(32))
    );

    // The receipt event stays slim (§8.5): no inline claims or evidence — the
    // content-addressed session carries every verification detail.
    assert!(uv.get("claims").is_none());
    assert!(uv.get("evidence").is_none());
    assert!(uv.get("channel_bindings").is_none());

    // Deep audit: the persisted session carries the verdicts plus evidence.
    let session_claims = serde_json::to_value(&document.claims).unwrap();
    assert_eq!(session_claims["tee_attested"]["status"], "asserted");
    assert_eq!(session_claims["tee_attested"]["source"], "verifier_derived");
    assert_eq!(session_claims["tcb_up_to_date"]["status"], "unknown");
    assert_eq!(
        document.evidence.digest.as_deref(),
        Some(format!("sha256:{}", "11".repeat(32)).as_str())
    );
}

#[tokio::test]
async fn verified_upstream_binding_fails_without_persisted_session() {
    let (svc, received) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#, true);
    let svc = svc.with_session_store(Arc::new(FailingSessionStore));
    let event = UpstreamVerifiedEvent {
        upstream_name: "stub-upstream".to_string(),
        provider_type: None,
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
        payload_event(&result.receipt, "upstream.verified")["session_id"]
            .as_str()
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

    let first_event = payload_event(&first.receipt, "upstream.verified");
    let first_session_id = first_event["session_id"]
        .as_str()
        .expect("first verified binding should produce a session id");
    let second_event = payload_event(&second.receipt, "upstream.verified");
    let second_session_id = second_event["session_id"]
        .as_str()
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
        first_session.document().evidence.digest.as_deref(),
        Some(first_digest.as_str())
    );
    assert_eq!(
        second_session.document().evidence.digest.as_deref(),
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
        ServiceError::UpstreamVerification(UpstreamVerificationError { reason, event }) => {
            assert!(reason.contains("quote did not match"));
            assert_eq!(event.upstream_name, "stub-upstream");
        }
        other => panic!("expected UpstreamVerification, got {other:?}"),
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
    let mut cfg = AciServiceConfig::for_test();
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
        let mut cfg = AciServiceConfig::for_test();
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
    let mut cfg = AciServiceConfig::for_test();
    cfg.source_provenance = SourceProvenance {
        repo_url: None,
        repo_commit: None,
        image_digest: Some(format!("sha256:{}", "ab".repeat(32))),
        image_provenance: None,
    };
    AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0))).unwrap();
}

/// A key provider whose keyset violates §4.1: no E2EE entries, and (for the
/// role-separation case) the receipt key republished as an E2EE key.
struct MisshapenKeyProvider {
    inner: StaticKeyProvider,
    e2ee_keys: Vec<private_ai_gateway::aci::types::KeyedPublicKey>,
}

impl private_ai_gateway::aci::keys::KeyProvider for MisshapenKeyProvider {
    fn receipt_keys(&self) -> Vec<private_ai_gateway::aci::types::KeyedPublicKey> {
        self.inner.receipt_keys()
    }
    fn sign_receipt(
        &self,
        key_id: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, private_ai_gateway::aci::keys::KeyError> {
        self.inner.sign_receipt(key_id, payload)
    }
    fn e2ee_keys(&self) -> Vec<private_ai_gateway::aci::types::KeyedPublicKey> {
        self.e2ee_keys.clone()
    }
    fn tls_spkis(&self) -> Vec<private_ai_gateway::aci::types::TlsSpki> {
        self.inner.tls_spkis()
    }
    fn is_test_only(&self) -> bool {
        true
    }
}

#[test]
fn service_init_requires_an_e2ee_key_when_e2ee_is_advertised() {
    let keys = Arc::new(MisshapenKeyProvider {
        inner: StaticKeyProvider::default(),
        e2ee_keys: Vec::new(),
    });
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test();
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: vec!["3".to_string()],
    };
    let err = AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0)))
        .err()
        .expect("must fail");
    assert!(matches!(err, ServiceError::Keyset(_)), "{err:?}");
    assert!(err.to_string().contains("e2ee_public_keys"), "{err}");
}

#[test]
fn service_init_rejects_a_receipt_key_reused_as_e2ee_key() {
    use private_ai_gateway::aci::keys::KeyProvider as _;
    let inner = StaticKeyProvider::default();
    let mut reused = inner.receipt_keys().remove(0);
    reused.algo = "x25519-aes-256-gcm-hkdf-sha256".to_string();
    let keys = Arc::new(MisshapenKeyProvider {
        inner,
        e2ee_keys: vec![reused],
    });
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let cfg = AciServiceConfig::for_test();
    let err = AciService::new(keys, quoter, upstream, store, cfg, Arc::new(FixedClock(0)))
        .err()
        .expect("must fail");
    assert!(matches!(err, ServiceError::Keyset(_)), "{err:?}");
    assert!(err.to_string().contains("distinct"), "{err}");
}

#[test]
fn service_refuses_test_keys_in_production_mode() {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test();
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
        provider_type: Some("tinfoil".to_string()),
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
    let document = session.document();
    assert_eq!(document.upstream_name, "preflight-upstream");
    assert_eq!(
        document.endpoint.as_deref(),
        Some("https://preflight-upstream")
    );
    // Typed claims are populated from the provider mapping (tinfoil + UpToDate).
    assert_eq!(document.claims.tee_attested.status, ClaimStatus::Asserted);
    assert_eq!(document.claims.tcb_up_to_date.status, ClaimStatus::Asserted);
    // Resolvable by its content-addressed id too.
    assert!(service.get_attested_session(session.session_id()).is_some());

    // Re-verifying the unchanged channel is idempotent: the store's channel
    // dedup returns the live session instead of sealing a new document (and a
    // later completion path references this same session rather than copying).
    let id = session.session_id().to_string();
    service.record_session(&event);
    let after = service.list_attested_sessions(Some("preflight-upstream"));
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].session_id(), id);
}
