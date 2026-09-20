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
    AciService, AciServiceConfig, Clock, FixedClock, InMemoryReceiptStore, ServiceError,
    UpstreamVerificationError,
};
use private_ai_gateway::aggregator::session::{AttestedSession, ClaimStatus, EvidenceRef};
use private_ai_gateway::aggregator::session_store::{JsonlSessionStore, SessionStore};
use private_ai_gateway::aggregator::upstream_config::UpstreamSessionSink;

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

fn make_service_raw(body: &[u8]) -> (AciService, ReceivedBody) {
    make_service_with_clock(body, Arc::new(FixedClock(1_700_000_000)))
}

fn make_service_with_clock(body: &[u8], clock: Arc<dyn Clock>) -> (AciService, ReceivedBody) {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let (upstream, received) = StubUpstream::new(body);
    let upstream = Arc::new(upstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    let mut cfg = AciServiceConfig::for_test();
    // Do not advertise unwired E2EE.
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: vec![],
        serving: "aggregator".to_string(),
    };
    let svc = AciService::new(keys, quoter, upstream, store, cfg, clock).unwrap();
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

#[tokio::test]
async fn verified_upstream_binding_creates_attested_session() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#);
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
        Some(format!("sha256:{}", "11".repeat(32)).as_str())
    );
}

/// Chutes verifies its whole fleet under one nonce, so the raw evidence bundle
/// changes every round even when an instance's own material does not. The
/// bundle therefore stays out of the dedup fingerprint — sealing it in would
/// mint a fresh session per instance per round, growing the store without
/// bound — but the establishing round's evidence must persist in the
/// document: an empty `evidence` breaks §8.2 deep audit, and relying parties
/// (§9.2 check 2) reject the record, which made every Chutes-routed model
/// fail receipt verification.
#[tokio::test]
async fn chutes_instance_session_is_stable_across_evidence_rounds() {
    use base64::Engine as _;
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#);
    let chutes_event = |round: &str| UpstreamVerifiedEvent {
        provider_type: Some("chutes".to_string()),
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "private-ai-verifier/chutes/v1".to_string(),
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

// --- Chutes per-instance session regression suite -------------------------
//
// The tests below pin the store-level contract the stability test above can
// only imply: dedup must bound what hits the disk, survive restarts, stay
// per-instance under fleet churn, renew on validity lapse, and upgrade a
// pre-evidence record once a bundle becomes available.

/// A §8.2 evidence bundle over `bytes`: the digest of the decoded bytes plus
/// the bytes as a data URI.
fn chutes_evidence(bytes: &[u8]) -> serde_json::Value {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    serde_json::json!({
        "digest": private_ai_gateway::aci::digest::sha256_hex(bytes),
        "data": format!("data:application/json;base64,{b64}"),
    })
}

/// The Chutes instance a per-instance session attests.
fn chutes_instance_id_of(session: &AttestedSession) -> String {
    match session
        .document()
        .channel_binding
        .first()
        .expect("a chutes session carries its instance binding")
    {
        ChannelBinding::E2eePublicKeySha256 {
            key_id: Some(id), ..
        } => id.clone(),
        other => panic!("expected a chutes e2ee binding, got {other:?}"),
    }
}

/// A per-test JSONL session log under the temp dir, with any stale leftovers
/// from an earlier run (log, advisory-lock file, compaction temp) removed.
fn chutes_log_path(label: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("pr213-chutes-{label}-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("jsonl.lock"));
    let _ = std::fs::remove_file(path.with_extension("jsonl.tmp"));
    path
}

fn count_log_lines(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// The session id a verified Chutes round's receipt cites.
fn chutes_session_of(result: &private_ai_gateway::aggregator::service::ForwardResult) -> String {
    payload_event(&result.receipt, "upstream.verified")["session_id"]
        .as_str()
        .expect("verified Chutes binding should produce a session id")
        .to_string()
}

/// One verified Chutes round for `instance-1` carrying `evidence` (or none).
fn chutes_instance_event(evidence: Option<serde_json::Value>) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        provider_type: Some("chutes".to_string()),
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "private-ai-verifier/chutes/v1".to_string(),
        evidence,
        channel_bindings: vec![ChannelBinding::E2eePublicKeySha256 {
            provider: "chutes".to_string(),
            key_id: Some("instance-1".to_string()),
            algorithm: "chutes-ml-kem-768".to_string(),
            public_key_sha256: "aa".repeat(32),
        }],
        ..verified_event("stub-upstream", "x")
    }
}

/// Forward one verified Chutes round and return the session id its receipt
/// cites.
async fn chutes_forward(svc: &AciService, event: UpstreamVerifiedEvent) -> String {
    chutes_session_of(
        &svc.forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, false, Some(event))
            .await
            .unwrap(),
    )
}

/// Upgrade regression (PR #213 review): a per-instance session sealed with
/// NO evidence — replayed from a log written before evidence was persisted,
/// or left by an earlier verified round that genuinely supplied none — shares
/// its fingerprint with the evidence-bearing sessions that must replace it.
/// The fingerprint hit must not shadow the new bundle: the channel reseals
/// exactly once, the superseded record stays resolvable by id for the
/// receipts that already cite it, and every later round — before and after
/// another restart — dedupes without appending to the log.
#[tokio::test]
async fn chutes_instance_session_reseals_once_when_evidence_becomes_available() {
    let path = chutes_log_path("upgrade");

    // A pre-evidence log: one per-instance record with empty evidence, exactly
    // the shape the earlier binary persisted for Chutes.
    let old_id = {
        let (svc, _) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);
        let svc = svc.with_session_store(Arc::new(
            JsonlSessionStore::open(&path, 1_700_000_000).unwrap(),
        ));
        let result = svc
            .forward_chat_completion(
                br#"{"model":"x","messages":[]}"#,
                None,
                false,
                Some(chutes_instance_event(None)),
            )
            .await
            .unwrap();
        let old_id = chutes_session_of(&result);
        let old = svc
            .get_attested_session(&old_id)
            .expect("the pre-evidence record must be queryable");
        assert!(
            old.document().evidence.is_empty(),
            "the seeded record must carry no evidence"
        );
        old_id
    };
    assert_eq!(count_log_lines(&path), 1);

    // Restarted on the same log, a round supplying a valid bundle must not
    // keep citing the empty-evidence record: it fails every §9.2 deep audit.
    let (svc, _) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);
    let svc = svc.with_session_store(Arc::new(
        JsonlSessionStore::open(&path, 1_700_000_000).unwrap(),
    ));
    let result = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            false,
            Some(chutes_instance_event(Some(chutes_evidence(b"abc")))),
        )
        .await
        .unwrap();
    let new_id = chutes_session_of(&result);
    assert_ne!(
        new_id, old_id,
        "a replayed empty-evidence session must not shadow a round with a valid bundle"
    );
    let session = svc
        .get_attested_session(&new_id)
        .expect("the resealed record must be queryable");
    assert_eq!(
        session.document().evidence,
        EvidenceRef::from_value(&chutes_evidence(b"abc")),
        "the resealed record carries the round that supplied the evidence"
    );
    assert!(session.document().evidence.is_verifiable_bundle());
    assert!(
        svc.get_attested_session(&old_id).is_some(),
        "the superseded record stays resolvable by id, so receipts citing it \
         still resolve it — their §9.2 evidence check against that record still \
         fails; the reseal does not retro-repair them"
    );

    // One reseal per fingerprint, not one per round: later evidence rounds
    // dedupe onto the resealed session without growing the log.
    assert_eq!(count_log_lines(&path), 2);
    let bytes_after_reseal = std::fs::metadata(&path).unwrap().len();
    for round in 0..8 {
        let round = format!("round-{round}");
        svc.record_session(&chutes_instance_event(Some(chutes_evidence(
            round.as_bytes(),
        ))));
    }
    assert_eq!(
        count_log_lines(&path),
        2,
        "evidence rounds after the reseal must not append"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), bytes_after_reseal);

    // Replay after another restart keeps the evidence-bearing session current.
    drop(svc);
    let (svc, _) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);
    let svc = svc.with_session_store(Arc::new(
        JsonlSessionStore::open(&path, 1_700_000_000).unwrap(),
    ));
    let result = svc
        .forward_chat_completion(
            br#"{"model":"x","messages":[]}"#,
            None,
            false,
            Some(chutes_instance_event(Some(chutes_evidence(
                b"post-restart",
            )))),
        )
        .await
        .unwrap();
    assert_eq!(
        chutes_session_of(&result),
        new_id,
        "replay must resolve the fingerprint to the evidence-bearing record"
    );
    assert_eq!(
        count_log_lines(&path),
        2,
        "a replayed evidence round must not append"
    );

    drop(svc);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("jsonl.lock"));
    let _ = std::fs::remove_file(path.with_extension("jsonl.tmp"));
}

/// Same-run evidence recovery: a verified round that supplies no evidence
/// seals an empty-evidence session, and the next round that does supply a
/// valid bundle must reseal in place — without a restart — so the recovery
/// converges immediately instead of pinning the channel to a record that
/// fails every §9.2 deep audit for the rest of the validity window.
#[tokio::test]
async fn chutes_instance_session_reseals_when_evidence_recovers_within_a_run() {
    let (svc, _) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);

    // A round with no evidence cites the empty-evidence session it sealed.
    let empty_id = chutes_forward(&svc, chutes_instance_event(None)).await;
    assert!(svc
        .get_attested_session(&empty_id)
        .unwrap()
        .document()
        .evidence
        .is_empty());

    // The next round's bundle reseals the channel in place.
    let sealed_id =
        chutes_forward(&svc, chutes_instance_event(Some(chutes_evidence(b"abc")))).await;
    assert_ne!(
        sealed_id, empty_id,
        "a recovering evidence round must not cite the empty-evidence session"
    );
    assert_eq!(
        svc.get_attested_session(&sealed_id)
            .unwrap()
            .document()
            .evidence,
        EvidenceRef::from_value(&chutes_evidence(b"abc"))
    );

    // And later rounds dedupe onto the resealed session, empty or not.
    assert_eq!(
        chutes_forward(&svc, chutes_instance_event(Some(chutes_evidence(b"def")))).await,
        sealed_id
    );
    assert_eq!(
        chutes_forward(&svc, chutes_instance_event(None)).await,
        sealed_id
    );
}

/// Partial or invalid evidence must not lock the channel out of a later
/// complete bundle, and must not mint a non-auditable record in place of the
/// current one either. Only a full §8.2 bundle (a digest plus decodable data
/// hashing to it) is worth resealing for; a digest without data, data without
/// a digest, a non-`data:` URI, or bytes that do not hash to the digest
/// dedupes onto whatever is current, and a complete round upgrades it — even
/// after any number of bad rounds.
#[tokio::test]
async fn chutes_instance_session_is_not_locked_by_partial_or_invalid_evidence() {
    let (svc, _) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);
    let digest_of_abc = private_ai_gateway::aci::digest::sha256_hex(b"abc");
    // digest without data
    let digest_only = serde_json::json!({ "digest": digest_of_abc });
    // data without digest
    let data_only = serde_json::json!({ "data": "data:application/json;base64,YWJj" });
    // complete shape, but the data ("xyz") does not hash to the digest
    let bad_hash = serde_json::json!({
        "digest": digest_of_abc,
        "data": "data:application/json;base64,eHl6",
    });
    // a digest plus a URI we cannot decode as evidence data
    let foreign_uri = serde_json::json!({
        "digest": digest_of_abc,
        "data": "https://attest.example/evidence/abc",
    });
    let bad_rounds = [digest_only, data_only, bad_hash, foreign_uri];

    // An empty start.
    let empty_id = chutes_forward(&svc, chutes_instance_event(None)).await;

    // Partial/invalid rounds do not upgrade: deduping onto the existing
    // record beats minting a new record that cannot pass a §9.2 audit.
    for bad in &bad_rounds {
        assert_eq!(
            chutes_forward(&svc, chutes_instance_event(Some(bad.clone()))).await,
            empty_id,
            "an incomplete or invalid bundle must not reseal the channel"
        );
    }

    // A complete bundle upgrades — even after all those bad rounds.
    let sealed_id =
        chutes_forward(&svc, chutes_instance_event(Some(chutes_evidence(b"abc")))).await;
    assert_ne!(
        sealed_id, empty_id,
        "a complete bundle must upgrade a record that bad rounds left behind"
    );
    assert_eq!(
        svc.get_attested_session(&sealed_id)
            .unwrap()
            .document()
            .evidence,
        EvidenceRef::from_value(&chutes_evidence(b"abc"))
    );

    // Later complete rounds dedupe, and partial/invalid rounds no longer
    // disturb the established session.
    assert_eq!(
        chutes_forward(&svc, chutes_instance_event(Some(chutes_evidence(b"def")))).await,
        sealed_id
    );
    for bad in &bad_rounds {
        assert_eq!(
            chutes_forward(&svc, chutes_instance_event(Some(bad.clone()))).await,
            sealed_id,
            "a bad round after a complete bundle must dedupe, not reseal"
        );
    }
}

/// Bounded growth: a fleet of instances verified over many nonce-bound
/// evidence rounds appends one record per instance per validity window —
/// never one per round — keeps each instance's establishing evidence, stays
/// per-instance under sibling, GPU, binding, and fleet churn, and replays
/// after a restart without resealing anybody.
#[tokio::test]
async fn chutes_fleet_log_stays_bounded_across_instances_and_evidence_rounds() {
    let path = chutes_log_path("fleet");
    let binding = |id: &str, material: &str| ChannelBinding::E2eePublicKeySha256 {
        provider: "chutes".to_string(),
        key_id: Some(id.to_string()),
        algorithm: "chutes-ml-kem-768".to_string(),
        public_key_sha256: material.repeat(32),
    };
    /// The knobs one fleet verification round can turn. Every field that is
    /// an instance's OWN material (its TCB status, GPU outcome, binding key)
    /// must rebuild only that instance; the fleet-aggregate GPU flag varies
    /// every round and must never leak into a per-instance fingerprint.
    struct FleetRound {
        evidence: serde_json::Value,
        instance_2_tcb: &'static str,
        instance_1_gpu_verified: bool,
        instance_2_material: &'static str,
        fleet_gpu_verified: bool,
        with_third_instance: bool,
    }
    let fleet_event = |f: &FleetRound| UpstreamVerifiedEvent {
        provider_type: Some("chutes".to_string()),
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "private-ai-verifier/chutes/v1".to_string(),
        evidence: Some(f.evidence.clone()),
        channel_bindings: {
            let mut bindings = vec![
                binding("instance-1", "aa"),
                binding("instance-2", f.instance_2_material),
            ];
            if f.with_third_instance {
                bindings.push(binding("instance-3", "cc"));
            }
            bindings
        },
        provider_claims: Some(serde_json::json!({
            "trust_boundary": "model_instance",
            "chute_id": "chute-x",
            "gpu_verified": f.fleet_gpu_verified,
            "verified_instance_ids": ["instance-1", "instance-2", "instance-3"],
            "instance_tcb_statuses": {
                "instance-1": "UpToDate",
                "instance-2": f.instance_2_tcb,
                "instance-3": "UpToDate"
            },
            "instance_measurements": {
                "instance-1": "profile-x",
                "instance-2": "profile-y",
                "instance-3": "profile-z"
            },
            "instance_gpu": {
                "instance-1": { "gpu_verified": f.instance_1_gpu_verified, "gpu_arch": "hopper" },
                "instance-2": { "gpu_verified": false },
                "instance-3": { "gpu_verified": true, "gpu_arch": "blackwell" }
            }
        })),
        ..verified_event("stub-upstream", "x")
    };
    let record = |svc: &AciService, f: &FleetRound| svc.record_session(&fleet_event(f));
    // An instance's CURRENT session is the one sealed by its latest rebuild:
    // the listed record carrying the evidence of the round that sealed it.
    let current_id_with_evidence =
        |svc: &AciService, instance: &str, evidence: &serde_json::Value| -> Option<String> {
            svc.list_attested_sessions(Some("stub-upstream"))
                .into_iter()
                .find(|s| {
                    chutes_instance_id_of(s) == instance
                        && s.document().evidence == EvidenceRef::from_value(evidence)
                })
                .map(|s| s.session_id().to_string())
        };

    let (svc, _) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);
    let svc = svc.with_session_store(Arc::new(
        JsonlSessionStore::open(&path, 1_700_000_000).unwrap(),
    ));

    // Round 1 establishes one session per instance, sealed with that round's
    // fleet evidence bundle.
    let round_1 = chutes_evidence(b"round-1");
    record(
        &svc,
        &FleetRound {
            evidence: round_1.clone(),
            instance_2_tcb: "UpToDate",
            instance_1_gpu_verified: true,
            instance_2_material: "aa",
            fleet_gpu_verified: true,
            with_third_instance: false,
        },
    );
    assert_eq!(
        count_log_lines(&path),
        2,
        "one record per instance, not per binding set or evidence bundle"
    );
    let bytes_after_round_1 = std::fs::metadata(&path).unwrap().len();
    let id_1 = current_id_with_evidence(&svc, "instance-1", &round_1)
        .expect("instance-1 is sealed by round 1");
    let id_2 = current_id_with_evidence(&svc, "instance-2", &round_1)
        .expect("instance-2 is sealed by round 1");

    // 128 more nonce-bound rounds with the fleet-aggregate GPU flag
    // alternating every round: same sessions, no bytes appended.
    for round in 2..=129u32 {
        record(
            &svc,
            &FleetRound {
                evidence: chutes_evidence(format!("round-{round}").as_bytes()),
                instance_2_tcb: "UpToDate",
                instance_1_gpu_verified: true,
                instance_2_material: "aa",
                fleet_gpu_verified: round % 2 == 0,
                with_third_instance: false,
            },
        );
    }
    assert_eq!(
        count_log_lines(&path),
        2,
        "evidence rounds and fleet-aggregate churn must not append"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), bytes_after_round_1);
    assert_eq!(
        current_id_with_evidence(&svc, "instance-1", &round_1).as_deref(),
        Some(id_1.as_str()),
        "128 evidence rounds must not rebuild instance-1"
    );
    assert_eq!(
        current_id_with_evidence(&svc, "instance-2", &round_1).as_deref(),
        Some(id_2.as_str()),
        "128 evidence rounds must not rebuild instance-2"
    );

    // A sibling's own TCB change rebuilds only that sibling.
    let tcb_round = chutes_evidence(b"round-tcb");
    record(
        &svc,
        &FleetRound {
            evidence: tcb_round.clone(),
            instance_2_tcb: "OutOfDate",
            instance_1_gpu_verified: true,
            instance_2_material: "aa",
            fleet_gpu_verified: false,
            with_third_instance: false,
        },
    );
    assert_eq!(count_log_lines(&path), 3, "exactly one rebuild is appended");
    assert_eq!(
        current_id_with_evidence(&svc, "instance-1", &round_1).as_deref(),
        Some(id_1.as_str()),
        "a sibling's TCB change must not rebuild instance-1"
    );
    let id_2b = current_id_with_evidence(&svc, "instance-2", &tcb_round)
        .expect("instance-2 rebuilds on its own TCB change");
    assert_ne!(id_2b, id_2);

    // That instance's own GPU outcome dropping out rebuilds only it.
    let gpu_round = chutes_evidence(b"round-gpu");
    record(
        &svc,
        &FleetRound {
            evidence: gpu_round.clone(),
            instance_2_tcb: "OutOfDate",
            instance_1_gpu_verified: false,
            instance_2_material: "aa",
            fleet_gpu_verified: true,
            with_third_instance: false,
        },
    );
    assert_eq!(count_log_lines(&path), 4, "exactly one rebuild is appended");
    assert_eq!(
        current_id_with_evidence(&svc, "instance-2", &tcb_round).as_deref(),
        Some(id_2b.as_str()),
        "a sibling's GPU change must not rebuild instance-2"
    );
    let id_1b = current_id_with_evidence(&svc, "instance-1", &gpu_round)
        .expect("instance-1 rebuilds on its own GPU outcome change");
    assert_ne!(id_1b, id_1);

    // That instance's binding rotation rebuilds only it.
    let bind_round = chutes_evidence(b"round-bind");
    record(
        &svc,
        &FleetRound {
            evidence: bind_round.clone(),
            instance_2_tcb: "OutOfDate",
            instance_1_gpu_verified: false,
            instance_2_material: "bb",
            fleet_gpu_verified: false,
            with_third_instance: false,
        },
    );
    assert_eq!(count_log_lines(&path), 5, "exactly one rebuild is appended");
    assert_eq!(
        current_id_with_evidence(&svc, "instance-1", &gpu_round).as_deref(),
        Some(id_1b.as_str()),
        "a sibling's binding rotation must not rebuild instance-1"
    );
    let id_2c = current_id_with_evidence(&svc, "instance-2", &bind_round)
        .expect("instance-2 rebuilds on its binding rotation");
    assert_ne!(id_2c, id_2b);

    // Fleet churn — a new sibling joining — rebuilds nobody.
    let join_round = chutes_evidence(b"round-join");
    record(
        &svc,
        &FleetRound {
            evidence: join_round.clone(),
            instance_2_tcb: "OutOfDate",
            instance_1_gpu_verified: false,
            instance_2_material: "bb",
            fleet_gpu_verified: true,
            with_third_instance: true,
        },
    );
    assert_eq!(
        count_log_lines(&path),
        6,
        "only the joining instance's record is appended; nobody is rebuilt"
    );
    assert_eq!(
        current_id_with_evidence(&svc, "instance-1", &gpu_round).as_deref(),
        Some(id_1b.as_str())
    );
    assert_eq!(
        current_id_with_evidence(&svc, "instance-2", &bind_round).as_deref(),
        Some(id_2c.as_str())
    );
    let id_3 = current_id_with_evidence(&svc, "instance-3", &join_round)
        .expect("instance-3 is sealed by its joining round");

    // Every retained record satisfies §8.2 and carries the evidence of a
    // round that genuinely sealed an instance session: instance-1 keeps its
    // round-1 and round-gpu records, instance-2 its round-1, round-tcb and
    // round-bind ones, and instance-3 its joining record.
    let allowed_rounds: std::collections::BTreeMap<&str, Vec<serde_json::Value>> = [
        ("instance-1", vec![round_1.clone(), gpu_round.clone()]),
        (
            "instance-2",
            vec![round_1.clone(), tcb_round.clone(), bind_round.clone()],
        ),
        ("instance-3", vec![join_round.clone()]),
    ]
    .into_iter()
    .collect();
    let listed = svc.list_attested_sessions(Some("stub-upstream"));
    assert_eq!(listed.len(), 6, "every rebuild is retained, and no others");
    for session in &listed {
        let instance = chutes_instance_id_of(session);
        let evidence = &session.document().evidence;
        assert!(
            evidence.digest_matches_data(),
            "instance {instance}'s record must satisfy the §8.2 digest check"
        );
        assert!(
            allowed_rounds
                .get(instance.as_str())
                .expect("listed instances are expected")
                .iter()
                .any(|round| *evidence == EvidenceRef::from_value(round)),
            "instance {instance}'s record must carry the evidence of one of its establishing rounds"
        );
    }

    // Replay after a restart: nobody reseals, nothing appends.
    drop(svc);
    let (svc, _) = make_service_raw(br#"{"id":"chat-xyz","model":"x"}"#);
    let svc = svc.with_session_store(Arc::new(
        JsonlSessionStore::open(&path, 1_700_000_000).unwrap(),
    ));
    record(
        &svc,
        &FleetRound {
            evidence: chutes_evidence(b"round-replay"),
            instance_2_tcb: "OutOfDate",
            instance_1_gpu_verified: false,
            instance_2_material: "bb",
            fleet_gpu_verified: false,
            with_third_instance: true,
        },
    );
    assert_eq!(
        count_log_lines(&path),
        6,
        "a replayed round must dedupe onto the persisted per-instance sessions"
    );
    assert_eq!(
        current_id_with_evidence(&svc, "instance-1", &gpu_round).as_deref(),
        Some(id_1b.as_str()),
        "replay must keep instance-1 on its latest session"
    );
    assert_eq!(
        current_id_with_evidence(&svc, "instance-2", &bind_round).as_deref(),
        Some(id_2c.as_str()),
        "replay must keep instance-2 on its latest session"
    );
    assert_eq!(
        current_id_with_evidence(&svc, "instance-3", &join_round).as_deref(),
        Some(id_3.as_str()),
        "replay must keep instance-3 on its joining session"
    );

    drop(svc);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("jsonl.lock"));
    let _ = std::fs::remove_file(path.with_extension("jsonl.tmp"));
}

/// A clock a test can advance between rounds, for validity-window behaviour.
struct TickingClock(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl Clock for TickingClock {
    fn now_secs(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The validity period — not the evidence rounds — is what renews an instance
/// session: inside the window every round dedupes, past it a fresh session is
/// sealed, and the expired record stays resolvable by id until its retention
/// (last extended by a citation) lapses.
#[tokio::test]
async fn chutes_instance_session_renews_after_validity_lapses() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let now = Arc::new(AtomicU64::new(1_700_000_000));
    let (svc, _) = make_service_with_clock(
        br#"{"id":"chat-xyz","model":"x"}"#,
        Arc::new(TickingClock(now.clone())),
    );
    let chutes_event = |round: &str| UpstreamVerifiedEvent {
        provider_type: Some("chutes".to_string()),
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "private-ai-verifier/chutes/v1".to_string(),
        evidence: Some(chutes_evidence(round.as_bytes())),
        channel_bindings: vec![ChannelBinding::E2eePublicKeySha256 {
            provider: "chutes".to_string(),
            key_id: Some("instance-1".to_string()),
            algorithm: "chutes-ml-kem-768".to_string(),
            public_key_sha256: "aa".repeat(32),
        }],
        ..verified_event("stub-upstream", "x")
    };
    let forward = |evidence_round: &str| chutes_forward(&svc, chutes_event(evidence_round));

    // Inside the validity window, every evidence round dedupes.
    let first = forward("abc").await;
    now.store(1_700_000_100, Ordering::Relaxed);
    assert_eq!(
        forward("def").await,
        first,
        "a later round inside the window resolves to the same session"
    );

    // Past the window (established 1_700_000_000 + 3600s TTL), a fresh session.
    now.store(1_700_003_600, Ordering::Relaxed);
    let renewed = forward("ghi").await;
    assert_ne!(
        renewed, first,
        "a lapsed validity period must renew the session"
    );
    let old = svc
        .get_attested_session(&first)
        .expect("the expired record stays resolvable by id until retention lapses");
    assert_eq!(old.document().expires_at, 1_700_003_600);
    assert_eq!(
        old.document().evidence,
        EvidenceRef::from_value(&chutes_evidence(b"abc")),
        "the expired record keeps its establishing round's evidence"
    );
    let fresh = svc.get_attested_session(&renewed).unwrap();
    assert_eq!(fresh.document().established_at, 1_700_003_600);
    assert_eq!(
        fresh.document().evidence,
        EvidenceRef::from_value(&chutes_evidence(b"ghi")),
        "the renewed session carries the round that established it"
    );

    // The renewed session dedupes its own subsequent rounds.
    now.store(1_700_003_700, Ordering::Relaxed);
    assert_eq!(forward("jkl").await, renewed);
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
    };

    let err = svc
        .forward_chat_completion(br#"{"model":"x","messages":[]}"#, None, false, Some(event))
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
    // And only one session is stored for the channel.
    assert_eq!(svc.list_attested_sessions(Some("stub-upstream")).len(), 1);
}

#[tokio::test]
async fn attested_session_id_changes_when_verification_material_changes() {
    let (svc, _) = make_service(br#"{"id":"chat-xyz","model":"x"}"#);
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
        supported_e2ee_versions: vec!["2".to_string()],
        serving: "aggregator".to_string(),
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
async fn background_verification_writes_inspectable_session_into_the_store() {
    let (service, _) = make_service(b"{}");
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
