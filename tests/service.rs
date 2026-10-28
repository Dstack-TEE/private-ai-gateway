//! Service composition tests: fail-closed defaults, source
//! provenance, capability default, upstream-verification semantics.

use std::sync::{Arc, Mutex};

use common::admission;
mod common;

use async_trait::async_trait;
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::types::{ServiceCapabilities, SourceProvenance};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore, ServiceError,
    UpstreamVerificationError, UpstreamVerificationRequest, UpstreamVerifier,
};
use private_ai_gateway::aggregator::session::{AttestedSession, ClaimStatus};
use private_ai_gateway::aggregator::session_store::{SessionLease, SessionStore};

use common::{failed_event, verified_event, StaticKeyProvider, StubQuoter};

/// Find one event object in a signed receipt's payload.
fn payload_event(
    receipt: &private_ai_gateway::aci::receipt::SignedReceipt,
    event_type: &str,
) -> serde_json::Value {
    receipt.document_json().unwrap()["event_log"]
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
    fn extend_retention(&self, _lease: &SessionLease, _deadline: u64) -> std::io::Result<()> {
        Err(std::io::Error::other("session store unavailable"))
    }
    fn current_or_seal(
        &self,
        _fingerprint: &str,
        _retention_until: u64,
        _now: u64,
        _seal: &mut dyn FnMut() -> std::io::Result<AttestedSession>,
    ) -> std::io::Result<SessionLease> {
        Err(std::io::Error::other("session store unavailable"))
    }

    fn get_session(&self, _session_id: &str, _now: u64) -> Option<AttestedSession> {
        None
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
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let model_id = serde_json::from_slice::<serde_json::Value>(&req.body)
            .ok()
            .and_then(|body| {
                body.get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        Ok(PreparedUpstreamRequest {
            request: req,
            upstream_name: "stub-upstream".to_string(),
            url_origin: Some("http://stub-upstream".to_string()),
            model_id,
            route_id: None,
            // A TEE-attesting route, so a constrained request can select it.
            is_tee: Some(true),
        })
    }

    fn name(&self) -> &str {
        "stub-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("http://stub-upstream")
    }
    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        *self.received.lock().unwrap() = Some(req.body.to_vec());
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
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let mut response = self.forward(req.request).await?;
        if event.provider_type.as_deref() == Some("chutes") {
            if let Some(ChannelBinding::E2eePublicKeySha256 { key_id, .. }) =
                event.channel_bindings.first()
            {
                response.served_instance_id = key_id.clone();
            }
        }
        Ok(response)
    }
}

fn make_service_raw(body: &[u8]) -> (AciService, ReceivedBody) {
    make_service_raw_with_verifier(body, None)
}

fn make_service_raw_with_verifier(
    body: &[u8],
    verifier: Option<Arc<dyn UpstreamVerifier>>,
) -> (AciService, ReceivedBody) {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, received) = StubUpstream::new(body);
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test();
    // Do not advertise unwired E2EE.
    cfg.service_capabilities = ServiceCapabilities {
        supported_multipart_constraint_endpoints: Vec::new(),
        supported_e2ee_versions: vec![],
        serving: "aggregator".to_string(),
        ..Default::default()
    };
    let svc = match verifier {
        Some(verifier) => AciService::new_with_upstream_verifier(
            keys,
            quoter,
            upstream,
            verifier,
            store,
            cfg,
            Arc::new(FixedClock(1_700_000_000)),
        ),
        None => AciService::new(
            keys,
            quoter,
            upstream,
            store,
            cfg,
            Arc::new(FixedClock(1_700_000_000)),
        ),
    }
    .unwrap();
    (svc, received)
}

fn make_service(body: &[u8]) -> (Arc<AciService>, ReceivedBody) {
    let (svc, received) = make_service_raw(body);
    (Arc::new(svc), received)
}

#[tokio::test]
async fn required_with_no_verifier_fails_closed_before_forwarding() {
    let (svc, received) = make_service(br#"{"id":"x"}"#);
    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, true, None)
        .await
        .unwrap_err();
    match err {
        ServiceError::UpstreamVerification(kind) => {
            let reason = kind.to_string();
            assert!(reason.contains("no verifier result"), "{reason:?}");
        }
        other => panic!("expected UpstreamVerification, got {other:?}"),
    }
    assert!(received.lock().unwrap().is_none());
}

#[tokio::test]
async fn verification_opt_out_forwards_and_records_failed_event() {
    let (svc, received) = make_service(br#"{"id":"chat-xyz"}"#);
    let result = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, false, None)
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
async fn verifier_event_result_verified_emits_upstream_verified() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz"}"#);
    let event = UpstreamVerifiedEvent {
        url_origin: Some("http://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        ..verified_event("stub-upstream", "x")
    };
    let result = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, false, Some(event))
        .await
        .unwrap();
    let uv = payload_event(&result.receipt, "upstream.verified");
    assert_eq!(uv["result"], "verified");
    // The event is slim (§7.5): verification detail lives in the cited session.
    assert!(uv.get("verifier_id").is_none());
    let sid = uv["session_id"].as_str().unwrap();
    assert_eq!(sid.len(), 64, "session id is bare 64-hex (§8)");
    assert!(sid.bytes().all(|b| b.is_ascii_hexdigit()));
}

/// A distinct, complete evidence bundle per verification round.
fn round_evidence(round: &str) -> serde_json::Value {
    let evidence = private_ai_gateway::aggregator::session::EvidenceRef::from_bytes(
        "application/json",
        format!(r#"{{"round":"{round}"}}"#).as_bytes(),
    );
    serde_json::json!({ "digest": evidence.digest, "data": evidence.data_uri })
}

#[tokio::test]
async fn verified_upstream_binding_creates_attested_session() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#);
    let event = UpstreamVerifiedEvent {
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        evidence: Some(serde_json::json!({
            "digest": "sha256:c66545694666be261c5babe518913c3536f26b1dd34c5f14de71dd7ad1968c1a",
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
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, false, Some(event))
        .await
        .unwrap();
    let uv = payload_event(&result.receipt, "upstream.verified");
    let session_id = uv["session_id"]
        .as_str()
        .expect("verified binding should produce a session id");
    let session = svc
        .get_attested_session(session_id)
        .expect("session audit record should be queryable");
    // The id is the hash of the exact served bytes (§8), never stored inside.
    assert_eq!(session.session_id(), session_id);
    assert_eq!(
        session_id,
        hex::encode(private_ai_gateway::aci::digest::sha256_raw(session.bytes()))
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

    // The receipt event stays slim (§7.5): no inline claims or evidence — the
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
        Some("sha256:c66545694666be261c5babe518913c3536f26b1dd34c5f14de71dd7ad1968c1a")
    );
}

/// Only a complete reviewed Chutes profile may reuse its establishing snapshot
/// across nonce-bound evidence rounds. Legacy output remains evidence-sensitive.
#[tokio::test]
async fn chutes_instance_session_is_stable_across_evidence_rounds() {
    use base64::Engine as _;
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#);
    let profiles: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/session_material.json")).unwrap();
    let own = &profiles["chutes"];
    let claims = serde_json::json!({
        "session_material_version": "chutes/1", "trust_boundary": "model_instance",
        "evidence_scope": "model_instance", "chute_id": "chute", "canonical_model_id": "model",
        "instance_tdx_reports": {"instance-1": own["tdx_report"]},
        "instance_tdx_report_sha256": {"instance-1": own["tdx_report_sha256"]},
        "instance_advisory_ids": {"instance-1": own["advisory_ids"]},
        "instance_measurements": {"instance-1": own["measurement"]},
        "instance_tcb_statuses": {"instance-1": own["tcb_status"]},
        "instance_gpu": {"instance-1": {"gpu_verified": false, "gpu_evidence_present": false,
            "gpu_evidence_nonce_matched": null, "gpu_arch": null}},
    });
    let chutes_event = |round: &str| UpstreamVerifiedEvent {
        provider_type: Some("chutes".to_string()),
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "private-ai-verifier/chutes/v1".to_string(),
        provider_claims: Some(claims.clone()),
        // §8.2: the digest is over the decoded evidence bytes.
        evidence: Some(serde_json::json!({
            "digest": private_ai_gateway::aci::digest::sha256_hex(
                &base64::engine::general_purpose::STANDARD.decode(round).unwrap(),
            ),
            "data": format!("data:application/json;base64,{}", round),
        })),
        channel_bindings: vec![ChannelBinding::E2eePublicKeySha256 {
            provider: "chutes".to_string(),
            key_id: Some("instance-1".to_string()),
            algorithm: "chutes-ml-kem-768".to_string(),
            public_key_sha256: "aa".repeat(32),
        }],
        ..verified_event("stub-upstream", "x")
    };
    let session_for = |result: &private_ai_gateway::aggregator::service::ForwardResult| {
        payload_event(&result.receipt, "upstream.verified")["session_id"]
            .as_str()
            .expect("verified Chutes binding should produce a session id")
            .to_string()
    };

    let first = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            false,
            Some(chutes_event("YWJj")),
        )
        .await
        .unwrap();
    let second = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            false,
            Some(chutes_event("ZGVm")),
        )
        .await
        .unwrap();

    let session_id = session_for(&first);
    assert_eq!(
        session_for(&second),
        session_id,
        "a new evidence round must resolve to the existing instance session"
    );
    let session = svc
        .get_attested_session(&session_id)
        .expect("Chutes session should be queryable");
    // §8.2: the record carries the establishing round's evidence (digest +
    // data) so a relying party can deep-audit it, while the session id stays
    // stable across nonce-bound evidence rounds.
    let evidence = &session.document().evidence;
    assert_eq!(
        evidence.digest.as_deref(),
        Some(private_ai_gateway::aci::digest::sha256_hex(b"abc").as_str(),),
        "the first round's evidence digest must persist in the document"
    );
    assert_eq!(
        evidence.data_uri.as_deref(),
        Some("data:application/json;base64,YWJj"),
        "the first round's evidence bytes must persist in the document"
    );
    assert!(
        evidence.digest_matches_data(),
        "persisted evidence must satisfy the §8.2 digest check"
    );
}

#[tokio::test]
async fn verified_upstream_binding_fails_without_persisted_session() {
    let (svc, received) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);
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
        aci_identity: None,
        verification_round: None,
    };

    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, false, Some(event))
        .await
        .expect_err("receipt must not cite a session that was not persisted");

    assert!(matches!(err, ServiceError::SessionStore(_)));
    assert!(
        received.lock().unwrap().is_none(),
        "persistence must precede forwarding"
    );
}

#[tokio::test]
async fn session_is_per_tee_channel_not_per_model() {
    // Two requests to the SAME TEE channel (same upstream / endpoint / binding /
    // evidence) routed to different models must collapse to ONE session: a
    // session attests the verified channel, not the model. (A router-based
    // upstream serving N models therefore yields 1 session, not N.) The model
    // served is recorded on the receipt, never on the session.
    let (svc, _) = make_service(br#"{"id":"chat-xyz"}"#);
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
            false,
            Some(event("model-a")),
        )
        .await
        .unwrap();
    let r2 = svc
        .forward_chat_completion(
            br#"{"model":"model-b","messages":[]}"#,
            None,
            false,
            Some(event("model-b")),
        )
        .await
        .unwrap();

    assert_eq!(
        session_id_of(&r1),
        session_id_of(&r2),
        "same TEE channel, different models -> one session"
    );
    assert!(svc
        .get_attested_session(&session_id_of(&r1).unwrap())
        .is_some());
}

#[tokio::test]
async fn attested_session_id_changes_when_verification_material_changes() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#);
    let make_event = |digest_byte: &str| UpstreamVerifiedEvent {
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        evidence: Some(round_evidence(digest_byte)),
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
            false,
            Some(make_event("11")),
        )
        .await
        .unwrap();
    let second = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            false,
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
    let first_digest = round_evidence("11")["digest"].as_str().unwrap().to_string();
    let second_digest = round_evidence("22")["digest"].as_str().unwrap().to_string();
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
    let (svc, received) = make_service(br#"{"id":"chat-xyz"}"#);
    let event = UpstreamVerifiedEvent {
        verifier_id: "stub-verifier-1".to_string(),
        reason: Some("quote did not match expected app-id".to_string()),
        ..failed_event("stub-upstream", "x")
    };
    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, true, Some(event))
        .await
        .unwrap_err();
    match err {
        ServiceError::UpstreamVerification(kind) => {
            assert!(kind.to_string().contains("quote did not match"), "{kind}");
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

/// A key provider whose keyset violates §3.1: no E2EE entries, and (for the
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
fn service_init_requires_a_recognized_e2ee_key_in_the_keyset() {
    let keys = Arc::new(MisshapenKeyProvider {
        inner: StaticKeyProvider::default(),
        e2ee_keys: Vec::new(),
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
    assert!(err.to_string().contains("e2ee_public_keys"), "{err}");
}

#[test]
fn service_init_accepts_secp256k1_as_the_only_e2ee_v2_suite() {
    use private_ai_gateway::aci::e2ee::E2EE_ALGO_SECP256K1_AESGCM;
    use private_ai_gateway::aci::keys::KeyProvider as _;

    let inner = StaticKeyProvider::default();
    let e2ee_keys = inner
        .e2ee_keys()
        .into_iter()
        .filter(|key| key.algo == E2EE_ALGO_SECP256K1_AESGCM)
        .collect();
    let keys = Arc::new(MisshapenKeyProvider { inner, e2ee_keys });
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, _) = StubUpstream::new(b"{}");
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test();
    cfg.service_capabilities = ServiceCapabilities {
        supported_multipart_constraint_endpoints: Vec::new(),
        supported_e2ee_versions: vec!["2".to_string()],
        serving: "aggregator".to_string(),
        ..Default::default()
    };

    AciService::new(
        keys,
        quoter,
        Arc::new(upstream),
        store,
        cfg,
        Arc::new(FixedClock(0)),
    )
    .expect("secp256k1 is a supported E2EE v2 suite");
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
async fn listing_seals_an_inspectable_cached_session_on_demand() {
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

    let (service, _) = make_service_raw_with_verifier(
        b"{}",
        Some(Arc::new(
            private_ai_gateway::aci::verifier::StaticUpstreamVerifier::new(event.clone()),
        )),
    );
    let requests = [UpstreamVerificationRequest {
        upstream_name: event.upstream_name.clone(),
        url_origin: event.url_origin.clone(),
        model_id: event.model_id.clone(),
        forwarded_body_hash: private_ai_gateway::aci::digest::sha256_hex(b""),
        required: true,
    }];
    let listed = service.list_current_sessions(&requests).unwrap();
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

    // Listing unchanged material reuses the sealed document; forwarding can
    // reference the same session afterward.
    let id = session.session_id().to_string();
    let after = service.list_current_sessions(&requests).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].session_id(), id);
}

// --- restored from main (fail-closed gate coverage) ---

#[tokio::test]
async fn aci_required_with_no_verifier_fails_closed_before_forwarding() {
    let (svc, received) = make_service(br#"{"id":"x"}"#);
    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, true, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ServiceError::UpstreamVerification(UpstreamVerificationError::NoVerifierResult)
    ));
    assert!(received.lock().unwrap().is_none());
}

#[tokio::test]
async fn verifier_event_failed_for_aci_request_fails_before_forwarding() {
    let (svc, received) = make_service(br#"{"id":"chat-xyz"}"#);
    let event = UpstreamVerifiedEvent {
        verifier_id: "stub-verifier-1".to_string(),
        reason: Some("quote did not match expected app-id".to_string()),
        ..failed_event("stub-upstream", "x")
    };
    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, true, Some(event))
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

#[tokio::test]
async fn unconstrained_request_forwards_and_records_failed_event() {
    let (svc, received) = make_service(br#"{"id":"chat-xyz"}"#);
    let result = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, false, None)
        .await
        .unwrap();
    assert_eq!(result.upstream_status, 200);
    assert!(received.lock().unwrap().is_some());

    // Aggregator receipts always carry upstream.verified. An unconstrained
    // request records a synthesized failed event so a downstream verifier sees
    // the actual state.
    // §7.5's failed form is slim: the outcome and why, no verifier detail.
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
async fn session_listing_propagates_persistence_failure() {
    let event = verified_event("stub-upstream", "model");
    let (service, _) = make_service_raw_with_verifier(
        b"{}",
        Some(Arc::new(
            private_ai_gateway::aci::verifier::StaticUpstreamVerifier::new(event),
        )),
    );
    let service = service.with_session_store(Arc::new(FailingSessionStore));
    let config = common::session_config(serde_json::json!([{
        "name": "stub-upstream", "base_url": "http://stub-upstream",
        "provider": "phala-direct",
        "models": {"model": "model"},
    }]));
    let router = private_ai_gateway::http::build_router_with_admin(Arc::new(service), config, None);
    use tower::ServiceExt;
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/aci/sessions")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!String::from_utf8(body.to_vec())
        .unwrap()
        .contains("session store unavailable"));
}

#[tokio::test]
async fn buffered_admission_survives_expiry_with_and_without_pins() {
    use admission::{request, Attempt, Harness};
    for pinned in [true, false] {
        let h = Harness::new(vec![Attempt::Response {
            status: 200,
            at: 120,
        }]);
        let original = h.listed().remove(0);
        assert_eq!(original.document().expires_at, 110);
        h.clock.set(109);
        let ids = if pinned {
            vec![original.session_id().to_string()]
        } else {
            vec![]
        };
        let result = h
            .service
            .forward_chat_completion_request(request(ids))
            .await
            .unwrap();
        assert_eq!(result.receipt.document_json().unwrap()["served_at"], 109);
        assert_eq!(
            payload_event(&result.receipt, "upstream.verified")["session_id"],
            original.session_id()
        );
        // Publication at 120, not admission at 109, determines the deadline 125.
        h.clock.set(124);
        assert_eq!(
            h.service.get_attested_session(original.session_id()),
            Some(original.clone())
        );
        assert!(h
            .service
            .get_receipt_by_receipt_id(&result.receipt.receipt_id)
            .is_some());
        h.clock.set(125);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_none());
        assert!(h
            .service
            .get_receipt_by_receipt_id(&result.receipt.receipt_id)
            .is_none());
    }
}

#[tokio::test]
async fn single_forward_stream_holds_admission_until_publication_or_cancellation() {
    use admission::{request, Attempt, Harness};
    use futures_util::StreamExt;
    use private_ai_gateway::aggregator::service::StreamingForwardResult;
    for cancel in [false, true] {
        let h = Harness::new(vec![Attempt::Response {
            status: 200,
            at: 109,
        }]);
        let original = h.listed().remove(0);
        h.clock.set(109);
        let StreamingForwardResult::Stream(mut stream) = h
            .service
            .forward_chat_completion_stream_request(request(vec![]))
            .await
            .unwrap()
        else {
            panic!("stream expected")
        };
        h.clock.set(120);
        assert!(h
            .service
            .get_attested_session(original.session_id())
            .is_some());
        let receipt_id = stream.receipt_id.clone();
        if cancel {
            drop(stream);
            assert!(h
                .service
                .get_attested_session(original.session_id())
                .is_none());
            assert!(h.service.get_receipt_by_receipt_id(&receipt_id).is_none());
        } else {
            while let Some(chunk) = stream.body.next().await {
                chunk.unwrap();
            }
            let receipt = h.service.get_receipt_by_receipt_id(&receipt_id).unwrap();
            assert_eq!(receipt.document_json().unwrap()["served_at"], 109);
            assert_eq!(
                payload_event(&receipt, "upstream.verified")["session_id"],
                original.session_id()
            );
            // The completed stream object must no longer hold a lease.
            h.clock.set(125);
            assert!(h
                .service
                .get_attested_session(original.session_id())
                .is_none());
        }
    }
}

#[tokio::test]
async fn admission_overflow_and_persistence_failure_never_forward() {
    use admission::{request, Attempt, Harness};
    let mut h = Harness::new(vec![Attempt::Response {
        status: 200,
        at: 100,
    }]);
    h.clock.set(u64::MAX - 5);
    assert!(matches!(
        h.service
            .forward_chat_completion_request(request(vec![]))
            .await,
        Err(ServiceError::SessionStore(_))
    ));
    assert!(h.backend.events.lock().unwrap().is_empty());
    h.clock.set(100);
    h.service = h.service.with_session_store(Arc::new(FailingSessionStore));
    assert!(matches!(
        h.service
            .forward_chat_completion_request(request(vec![]))
            .await,
        Err(ServiceError::SessionStore(_))
    ));
    assert!(h.backend.events.lock().unwrap().is_empty());
}

#[test]
fn constructor_rejects_zero_and_overflowing_session_ttl() {
    for (ttl, now) in [(0, 100), (10, u64::MAX - 5)] {
        let mut config = AciServiceConfig::for_test();
        config.attested_session_ttl_seconds = ttl;
        let (backend, _) = StubUpstream::new(b"{}");
        let result = AciService::new(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(InMemoryReceiptStore::default()),
            config,
            Arc::new(FixedClock(now)),
        );
        assert!(matches!(
            result,
            Err(ServiceError::InvalidAttestedSessionTtl)
        ));
    }
    assert_eq!(
        AciServiceConfig::for_test().attested_session_ttl_seconds,
        3600
    );
}

#[tokio::test]
async fn multiple_tls_bindings_enforce_the_complete_cited_group_in_canonical_order() {
    use admission::{request, Attempt, Harness};
    let h = Harness::new(vec![
        Attempt::Response {
            status: 200,
            at: 100,
        },
        Attempt::Response {
            status: 200,
            at: 100,
        },
    ]);
    let low = ChannelBinding::TlsSpkiSha256 {
        origin: "https://test".to_string(),
        spki_sha256: "11".repeat(32),
    };
    let high = ChannelBinding::TlsSpkiSha256 {
        origin: "https://test".to_string(),
        spki_sha256: "ff".repeat(32),
    };
    h.verifier.0.lock().unwrap().channel_bindings = vec![high.clone(), low.clone()];
    let listed = h.listed();
    let result = h
        .service
        .forward_chat_completion_request(request(vec![]))
        .await
        .unwrap();
    let session_id = payload_event(&result.receipt, "upstream.verified")["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        h.service
            .get_attested_session(&session_id)
            .unwrap()
            .document()
            .channel_binding,
        vec![low.clone(), high.clone()]
    );
    assert_eq!(
        h.backend.events.lock().unwrap()[0].channel_bindings,
        vec![low.clone(), high.clone()]
    );
    assert_eq!(listed.len(), 1);
    let pinned = &listed[0];
    h.service
        .forward_chat_completion_request(request(vec![pinned.session_id().to_string()]))
        .await
        .unwrap();
    assert_eq!(
        h.backend.events.lock().unwrap()[1].channel_bindings,
        vec![low, high]
    );
}

#[tokio::test]
async fn chutes_response_must_identify_an_admitted_instance_and_releases_siblings() {
    use admission::{request, Attempt, Harness};
    for served in [Some("b"), Some("unknown"), None] {
        let h = Harness::new(vec![Attempt::Response {
            status: 200,
            at: 120,
        }]);
        {
            let mut event = h.verifier.0.lock().unwrap();
            event.provider_type = Some("chutes".to_string());
            event.channel_bindings = ["a", "b"]
                .into_iter()
                .map(|id| ChannelBinding::E2eePublicKeySha256 {
                    provider: "chutes".to_string(),
                    key_id: Some(id.to_string()),
                    algorithm: "chutes-ml-kem-768".to_string(),
                    public_key_sha256: "aa".repeat(32),
                })
                .collect();
        }
        *h.backend.instance.lock().unwrap() = served.map(str::to_string);
        let listed = h.listed();
        h.clock.set(109);
        let result = h
            .service
            .forward_chat_completion_request(request(vec![]))
            .await;
        assert_eq!(
            h.backend.events.lock().unwrap().len(),
            1,
            "response invariant fails after forwarding"
        );
        if served == Some("b") {
            let result = result.unwrap();
            let id = payload_event(&result.receipt, "upstream.verified")["session_id"]
                .as_str()
                .unwrap()
                .to_string();
            let chosen = h.service.get_attested_session(&id).unwrap();
            assert!(
                matches!(&chosen.document().channel_binding[0], ChannelBinding::E2eePublicKeySha256 { key_id: Some(key), .. } if key == "b")
            );
            let sibling = listed.iter().find(|s| s.session_id() != id).unwrap();
            assert!(h
                .service
                .get_attested_session(sibling.session_id())
                .is_none());
        } else {
            assert!(
                matches!(result, Err(ServiceError::AdmissionInvariant(_))),
                "must not manufacture a pre-forward refusal"
            );
            for session in listed {
                assert!(h
                    .service
                    .get_attested_session(session.session_id())
                    .is_none());
            }
        }
    }
}

#[tokio::test]
async fn ttl_changes_affect_only_new_documents_and_receipt_retention_is_independent() {
    use admission::Harness;
    use private_ai_gateway::aggregator::session_store::InMemorySessionStore;
    let mut h = Harness::new(vec![]);
    let sessions = Arc::new(InMemorySessionStore::default());
    h.service = h.service.with_session_store(sessions.clone());
    let original = h.listed().remove(0);
    let mut config = AciServiceConfig::for_test();
    config.attested_session_ttl_seconds = 50;
    config.receipt_ttl_seconds = 200;
    h.clock.set(105);
    h.service = AciService::new_with_upstream_verifier(
        Arc::new(StaticKeyProvider::default()),
        Arc::new(StubQuoter::default()),
        h.backend.clone(),
        h.verifier.clone(),
        Arc::new(InMemoryReceiptStore::default()),
        config,
        h.clock.clone(),
    )
    .unwrap()
    .with_session_store(sessions);
    assert_eq!(
        h.listed(),
        vec![original.clone()],
        "new TTL must not reseal a valid document"
    );
    h.clock.set(110);
    let replacement = h.listed().remove(0);
    assert_ne!(replacement.session_id(), original.session_id());
    assert_eq!(replacement.document().expires_at, 160);
    // The longer initial receipt retention survives independently of validity.
    h.clock.set(309);
    assert_eq!(
        h.service.get_attested_session(replacement.session_id()),
        Some(replacement.clone())
    );
    h.clock.set(310);
    assert!(h
        .service
        .get_attested_session(replacement.session_id())
        .is_none());
}

#[tokio::test]
async fn admission_uses_post_acquisition_time_for_a_concurrently_established_pin() {
    use admission::{request, Attempt, Harness, TestClock};
    use private_ai_gateway::aggregator::session_store::InMemorySessionStore;

    struct ResumeAfterAcquire {
        inner: Arc<InMemorySessionStore>,
        clock: Arc<TestClock>,
    }
    impl SessionStore for ResumeAfterAcquire {
        fn get_session(&self, id: &str, now: u64) -> Option<AttestedSession> {
            self.inner.get_session(id, now)
        }
        fn current_or_seal(
            &self,
            fingerprint: &str,
            retention_until: u64,
            now: u64,
            seal: &mut dyn FnMut() -> std::io::Result<AttestedSession>,
        ) -> std::io::Result<SessionLease> {
            let lease = self
                .inner
                .current_or_seal(fingerprint, retention_until, now, seal)?;
            self.clock.set(101);
            Ok(lease)
        }
        fn extend_retention(&self, lease: &SessionLease, deadline: u64) -> std::io::Result<()> {
            self.inner.extend_retention(lease, deadline)
        }
    }

    let mut h = Harness::new(vec![Attempt::Response {
        status: 200,
        at: 102,
    }]);
    let inner = Arc::new(InMemorySessionStore::default());
    h.service = h.service.with_session_store(inner.clone());
    h.clock.set(101);
    let established = h.listed().remove(0);
    // Resume an older lookup after another caller has established the channel.
    h.clock.set(100);
    h.service = h.service.with_session_store(Arc::new(ResumeAfterAcquire {
        inner,
        clock: h.clock.clone(),
    }));
    let response = h
        .service
        .forward_chat_completion_request(request(vec![established.session_id().to_string()]))
        .await
        .unwrap();
    assert_eq!(
        payload_event(&response.receipt, "upstream.verified")["session_id"],
        established.session_id()
    );
    assert_eq!(response.receipt.document_json().unwrap()["served_at"], 101);
}

struct SettableClock(std::sync::atomic::AtomicU64);

impl private_ai_gateway::aggregator::service::Clock for SettableClock {
    fn now_secs(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tokio::test]
async fn keyset_renews_at_half_life_with_the_same_keys() {
    const LIFETIME: u64 = 100;
    let clock = Arc::new(SettableClock(1_000.into()));
    let (upstream, _) = StubUpstream::new(b"{}");
    let mut cfg = AciServiceConfig::for_test();
    cfg.keyset_not_after = 1_000 + LIFETIME;
    let service = AciService::new(
        Arc::new(StaticKeyProvider::default()),
        Arc::new(StubQuoter::default()),
        Arc::new(upstream),
        Arc::new(InMemoryReceiptStore::default()),
        cfg,
        clock.clone(),
    )
    .unwrap();
    let original = service.keyset();
    let original_digest = service.workload_keyset_digest();

    clock.0.store(1_049, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(service.renew_keyset_if_due(LIFETIME).unwrap(), None);
    assert_eq!(service.workload_keyset_digest(), original_digest);

    clock.0.store(1_050, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(service.renew_keyset_if_due(LIFETIME).unwrap(), Some(1_150));
    let renewed = service.keyset();
    assert_eq!(renewed.not_after, 1_150);
    assert_eq!(renewed.receipt_signing_keys, original.receipt_signing_keys);
    assert_eq!(renewed.e2ee_public_keys, original.e2ee_public_keys);
    assert_eq!(renewed.tls_public_keys, original.tls_public_keys);
    assert_ne!(service.workload_keyset_digest(), original_digest);

    // The report binds and serves the renewed keyset.
    let report = service.attestation_report(None).await.unwrap();
    assert_eq!(
        report.workload_keyset_digest,
        service.workload_keyset_digest()
    );
    assert_eq!(report.attestation.workload_keyset, service.keyset_value());
}

/// Renewal before upstream headers and again during body delivery must not
/// change the identity captured at HTTP admission.
#[tokio::test]
async fn streaming_exchange_keeps_its_admitted_keyset_across_renewals() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use private_ai_gateway::aci::upstream::UpstreamStreamResponse;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;
    use tower::ServiceExt;

    struct PausedUpstream {
        first: AtomicBool,
        entered: Arc<Notify>,
        headers: Arc<Notify>,
        body: Arc<Notify>,
    }

    #[async_trait]
    impl UpstreamBackend for PausedUpstream {
        fn name(&self) -> &str {
            "paused"
        }

        fn url_origin(&self) -> Option<&str> {
            None
        }

        async fn forward(&self, _: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
            Ok(UpstreamResponse {
                status_code: 200,
                ..Default::default()
            })
        }

        async fn forward_stream(
            &self,
            _: UpstreamRequest,
        ) -> Result<UpstreamStreamResponse, UpstreamError> {
            let paused = self.first.swap(false, Ordering::SeqCst);
            if paused {
                self.entered.notify_one();
                self.headers.notified().await;
            }
            let release = self.body.clone();
            let body = async_stream::stream! {
                yield Ok(bytes::Bytes::from_static(b"data: first\n\n"));
                if paused {
                    release.notified().await;
                }
                yield Ok(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
            };
            Ok(UpstreamStreamResponse {
                status_code: 200,
                headers: [("content-type".to_string(), "text/event-stream".to_string())].into(),
                body: Box::pin(body),
                served_instance_id: None,
            })
        }
    }

    let entered = Arc::new(Notify::new());
    let headers = Arc::new(Notify::new());
    let body = Arc::new(Notify::new());
    let clock = Arc::new(SettableClock(1_000.into()));
    let mut config = AciServiceConfig::for_test();
    config.keyset_not_after = 1_100;
    config.service_capabilities.serving = "direct".to_string();
    let service = Arc::new(
        AciService::new(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(PausedUpstream {
                first: AtomicBool::new(true),
                entered: entered.clone(),
                headers: headers.clone(),
                body: body.clone(),
            }),
            Arc::new(InMemoryReceiptStore::default()),
            config,
            clock.clone(),
        )
        .expect("test service builds"),
    );
    let router = private_ai_gateway::http::build_router(service.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"model":"test","messages":[],"stream":true}"#,
            ))
            .expect("request builds")
    };
    let old_digest = service.workload_keyset_digest();
    let forward = tokio::spawn(router.clone().oneshot(request()));
    entered.notified().await;
    // Let the response headers be produced before renewing. The receipt keeps
    // the same serving snapshot while the body remains in flight.
    headers.notify_one();
    let response = forward
        .await
        .expect("forward task completes")
        .expect("router responds");
    assert_eq!(response.headers()["x-aci-keyset-digest"], old_digest);
    clock.0.store(1_050, Ordering::SeqCst);
    assert_eq!(
        service.renew_keyset_if_due(100).expect("renewal succeeds"),
        Some(1_150)
    );
    let receipt_id = response.headers()["x-receipt-id"]
        .to_str()
        .expect("receipt header is text")
        .to_string();
    let mut stream = response.into_body();
    let _first = stream
        .frame()
        .await
        .expect("first frame exists")
        .expect("frame succeeds");
    clock.0.store(1_100, Ordering::SeqCst);
    assert_eq!(
        service.renew_keyset_if_due(100).expect("renewal succeeds"),
        Some(1_200)
    );
    body.notify_one();
    let _remaining = stream.collect().await.expect("stream completes");
    let receipt = service
        .get_receipt_by_receipt_id(&receipt_id)
        .expect("receipt is stored");
    assert_eq!(
        receipt.document_json().expect("receipt parses")["workload_keyset_digest"],
        old_digest
    );

    let response = router
        .oneshot(request())
        .await
        .expect("next request succeeds");
    let new_digest = service.workload_keyset_digest();
    assert_ne!(new_digest, old_digest);
    assert_eq!(response.headers()["x-aci-keyset-digest"], new_digest);
    let receipt_id = response.headers()["x-receipt-id"]
        .to_str()
        .expect("receipt header is text")
        .to_string();
    let _body = response
        .into_body()
        .collect()
        .await
        .expect("next stream completes");
    let receipt = service
        .get_receipt_by_receipt_id(&receipt_id)
        .expect("next receipt is stored");
    assert_eq!(
        receipt.document_json().expect("receipt parses")["workload_keyset_digest"],
        new_digest
    );
}

#[tokio::test]
async fn buffered_receipt_uses_keyset_current_when_response_is_served() {
    assert_buffered_receipt_uses_current_keyset(true).await;
}

#[tokio::test]
async fn aggregator_receipt_uses_renewed_keyset_and_admitted_attempt_time() {
    assert_buffered_receipt_uses_current_keyset(false).await;
}

async fn assert_buffered_receipt_uses_current_keyset(direct: bool) {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use std::sync::atomic::Ordering;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    struct PausedUpstream {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl UpstreamBackend for PausedUpstream {
        fn name(&self) -> &str {
            "paused-buffered"
        }

        fn url_origin(&self) -> Option<&str> {
            None
        }

        async fn forward(&self, _: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(UpstreamResponse {
                status_code: 200,
                body: br#"{"id":"buffered"}"#.to_vec(),
                ..Default::default()
            })
        }
    }

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let clock = Arc::new(SettableClock(1_000.into()));
    let mut config = AciServiceConfig::for_test();
    config.keyset_not_after = 1_100;
    config.service_capabilities.serving = if direct { "direct" } else { "aggregator" }.to_string();
    let service = Arc::new(
        AciService::new(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(PausedUpstream {
                entered: entered.clone(),
                release: release.clone(),
            }),
            Arc::new(InMemoryReceiptStore::default()),
            config,
            clock.clone(),
        )
        .expect("test service builds"),
    );
    let router = private_ai_gateway::http::build_router(service.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model":"test","messages":[]}"#))
        .expect("request builds");
    let old_digest = service.workload_keyset_digest();
    let response_task = tokio::spawn(router.oneshot(request));
    entered.notified().await;

    clock.0.store(1_050, Ordering::SeqCst);
    service
        .renew_keyset_if_due(100)
        .expect("renewal succeeds")
        .expect("renewal is due");
    let renewed_digest = service.workload_keyset_digest();
    clock.0.store(1_101, Ordering::SeqCst);
    release.notify_one();

    let response = response_task
        .await
        .expect("request task completes")
        .expect("router responds");
    assert_eq!(response.headers()["x-aci-keyset-digest"], renewed_digest);
    assert_ne!(renewed_digest, old_digest);
    let receipt_id = response.headers()["x-receipt-id"]
        .to_str()
        .expect("receipt header is text")
        .to_string();
    response
        .into_body()
        .collect()
        .await
        .expect("body completes");
    let receipt = service
        .get_receipt_by_receipt_id(&receipt_id)
        .expect("receipt is stored");
    let document = receipt.document_json().expect("receipt parses");
    assert_eq!(document["workload_keyset_digest"], renewed_digest);
    assert_eq!(document["served_at"], 1_000);
}
