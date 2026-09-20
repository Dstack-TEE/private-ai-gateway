use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use private_ai_gateway::aci::digest;
use private_ai_gateway::aggregator::session::{
    AttestedSession, EvidenceRef, SessionClaims, SessionDocument,
};
use private_ai_gateway::aggregator::session_store::{JsonlSessionStore, SessionStore};

fn session(name: &str, established_at: u64, expires_at: u64) -> AttestedSession {
    AttestedSession::seal(SessionDocument {
        api_version: "aci/1".into(),
        upstream_name: name.into(),
        endpoint: None,
        verifier_id: "review/1".into(),
        established_at,
        expires_at,
        identity: None,
        channel_binding: vec![],
        claims: SessionClaims::default(),
        evidence: EvidenceRef {
            digest: Some(digest::sha256_hex(b"shared bundle")),
            data_uri: Some(format!(
                "data:application/json;base64,{}",
                BASE64.encode(b"shared bundle")
            )),
        },
    })
    .unwrap()
}
fn path(name: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("pr213-review-{name}-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn review_orphan_evidence_compact_then_reuse_survives_restart() {
    let path = path("orphan-reuse");
    let original = session("node-a", 1000, 9000);
    // Crash after writing evidence, before the citing session record lands.
    let line = serde_json::json!({ "seq": 0, "ts": 1000, "type": "evidence",
        "digest": digest::sha256_hex(b"shared bundle"), "retention_until": 9000,
        "payload_b64": BASE64.encode(b"shared bundle") });
    std::fs::write(&path, format!("{line}\n")).unwrap();
    {
        let store = JsonlSessionStore::open(&path, 1500).unwrap();
        assert_eq!(store.compact(1500).unwrap(), 0);
        store
            .put_session("fp-a", original.clone(), 9000, 1600)
            .unwrap();
        assert!(store.get_session(original.session_id(), 1600).is_some());
    }
    let store = JsonlSessionStore::open(&path, 1700).unwrap();
    assert!(
        store.get_session(original.session_id(), 1700).is_some(),
        "reused evidence was dropped from disk by compact but still cached in the index"
    );
}

#[test]
fn review_old_inline_log_survives_startup_compaction_and_restart() {
    let path = path("inline");
    let original = session("legacy-single-channel", 1000, 9000);
    let line = serde_json::json!({ "seq": 0, "ts": 1000, "type": "session", "fingerprint": "fp",
        "retention_until": 9000, "payload_b64": BASE64.encode(original.bytes()) });
    std::fs::write(&path, format!("{line}\n")).unwrap();
    {
        let store = JsonlSessionStore::open(&path, 1500).unwrap();
        assert_eq!(
            store
                .get_session(original.session_id(), 1500)
                .unwrap()
                .bytes(),
            original.bytes()
        );
        assert_eq!(store.compact(1500).unwrap(), 1);
    }
    let store = JsonlSessionStore::open(&path, 1600).unwrap();
    assert!(
        store.get_session(original.session_id(), 1600).is_some(),
        "legacy session lost after compact/restart"
    );
}

#[test]
fn review_later_citer_survives_restart_after_first_citer_expires() {
    let path = path("retention");
    let first = session("node-a", 1000, 2000);
    let later = session("node-b", 1500, 2500);
    {
        let store = JsonlSessionStore::open(&path, 1000).unwrap();
        store.put_session("fp-a", first, 2000, 1000).unwrap();
        store
            .put_session("fp-b", later.clone(), 2500, 1500)
            .unwrap();
        assert!(store.get_session(later.session_id(), 2100).is_some());
    }
    let store = JsonlSessionStore::open(&path, 2100).unwrap();
    assert!(
        store.get_session(later.session_id(), 2100).is_some(),
        "live later citer lost because evidence deadline on disk was not extended"
    );
}

#[test]
fn review_replay_preserves_evidence_retention_of_later_citer() {
    let path = path("retention-replay");
    let first = session("node-a", 1000, 2000);
    let later = session("node-b", 1500, 2500);
    {
        let store = JsonlSessionStore::open(&path, 1000).unwrap();
        store.put_session("fp-a", first, 2000, 1000).unwrap();
        store
            .put_session("fp-b", later.clone(), 2500, 1500)
            .unwrap();
    }
    {
        let store = JsonlSessionStore::open(&path, 1600).unwrap();
        assert!(store.get_session(later.session_id(), 1600).is_some());
        assert_eq!(store.compact(2100).unwrap(), 1);
    }
    let store = JsonlSessionStore::open(&path, 2200).unwrap();
    assert!(
        store.get_session(later.session_id(), 2200).is_some(),
        "replay did not extend evidence retention to the later citer"
    );
}
