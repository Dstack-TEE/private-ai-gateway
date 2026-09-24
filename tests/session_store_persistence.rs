//! Persistence-contract tests for the session log's externalized evidence
//! storage: every scenario below lost or corrupted a live session at some
//! point during review, and now pins one piece of the storage contract —
//! byte-identical rebuilds, deadline coverage for every citer, orphan
//! handling, and compaction/replay round-trips.

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

/// Write a pre-externalization (inline) session record directly.
fn write_inline(path: &std::path::Path, original: &AttestedSession, retention: u64) {
    let line = serde_json::json!({ "seq": 0, "ts": 1000, "type": "session", "fingerprint": "legacy",
        "retention_until": retention, "payload_b64": BASE64.encode(original.bytes()) });
    std::fs::write(path, format!("{line}\n")).unwrap();
}

/// (shared evidence lines, stripped session lines) in the log.
fn count_externalized(path: &std::path::Path) -> (usize, usize) {
    let mut bundles = 0;
    let mut stripped = 0;
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        if record["type"] == "evidence" {
            bundles += 1;
        }
        if record["type"] == "session" && record["evidence_data_prefix"].is_string() {
            stripped += 1;
        }
    }
    (bundles, stripped)
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
    write_inline(&path, &original, 9000);
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

#[test]
fn mixed_inline_and_externalized_same_digest_preserves_longer_inline_retention() {
    let path = path("mixed-long-inline");
    let legacy = session("legacy", 1000, 9000);
    let newer = session("new-shorter", 1200, 2000);
    write_inline(&path, &legacy, 9000);
    {
        let store = JsonlSessionStore::open(&path, 1200).unwrap();
        store.put_session("new", newer, 2000, 1200).unwrap();
        assert_eq!(store.compact(1500).unwrap(), 2);
    }
    let store = JsonlSessionStore::open(&path, 2100).unwrap();
    let got = store
        .get_session(legacy.session_id(), 2100)
        .expect("legacy long-lived session must not inherit new evidence's shorter lifetime");
    assert_eq!(got.bytes(), legacy.bytes());
}

#[test]
fn failed_compact_does_not_claim_extended_evidence_deadline_is_persisted() {
    let path = path("failed-compact");
    let first = session("first", 1000, 9000);
    let later = session("later", 1600, 7000);
    {
        let store = JsonlSessionStore::open(&path, 1000).unwrap();
        store.put_session("first", first, 2000, 1000).unwrap();
        store.current_session("first", 9000, 1500).unwrap();
        // A directory in place of the temporary file forces compact's I/O failure.
        let tmp = path.with_extension("jsonl.tmp");
        std::fs::create_dir(&tmp).unwrap();
        assert!(store.compact(1500).is_err());
        std::fs::remove_dir(tmp).unwrap();
        store
            .put_session("later", later.clone(), 7000, 1600)
            .unwrap();
    }
    let store = JsonlSessionStore::open(&path, 2100).unwrap();
    assert_eq!(
        store.get_session(later.session_id(), 2100).unwrap().bytes(),
        later.bytes()
    );
}

#[test]
fn same_digest_at_equal_deadline_is_deduplicated_and_roundtrips_twice() {
    let path = path("equal-deadline");
    let a = session("a", 1000, 9000);
    let b = session("b", 1200, 9000);
    {
        let store = JsonlSessionStore::open(&path, 1000).unwrap();
        store.put_session("a", a.clone(), 9000, 1000).unwrap();
        store.put_session("b", b.clone(), 9000, 1200).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| l.contains("\"type\":\"evidence\""))
                .count(),
            1
        );
        store.compact(1300).unwrap();
    }
    {
        let store = JsonlSessionStore::open(&path, 1400).unwrap();
        store.compact(1500).unwrap();
    }
    let store = JsonlSessionStore::open(&path, 8999).unwrap();
    for original in [&a, &b] {
        assert_eq!(
            store
                .get_session(original.session_id(), 8999)
                .unwrap()
                .bytes(),
            original.bytes()
        );
    }
    for original in [&a, &b] {
        assert!(store.get_session(original.session_id(), 9000).is_none());
    }
}

#[test]
fn replay_restores_citer_deadline_from_pre_fix_stale_evidence_log() {
    let path = path("stale-replay");
    let a = session("a", 1000, 2000);
    let b = session("b", 1500, 2500);
    {
        let store = JsonlSessionStore::open(&path, 1000).unwrap();
        store.put_session("a", a, 2000, 1000).unwrap();
        store.put_session("b", b.clone(), 2500, 1500).unwrap();
    }
    // Recreate the pre-fix on-disk shape: only the first evidence line,
    // with its short persisted deadline, followed by both citing sessions.
    let mut saw_evidence = false;
    let old_log = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| {
            if !line.contains("\"type\":\"evidence\"") {
                return true;
            }
            if saw_evidence {
                return false;
            }
            saw_evidence = true;
            true
        })
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, old_log).unwrap();
    {
        let store = JsonlSessionStore::open(&path, 1600).unwrap();
        assert_eq!(
            store.get_session(b.session_id(), 1600).unwrap().bytes(),
            b.bytes()
        );
        store.compact(2100).unwrap();
    }
    let store = JsonlSessionStore::open(&path, 2200).unwrap();
    assert_eq!(
        store.get_session(b.session_id(), 2200).unwrap().bytes(),
        b.bytes()
    );
}

#[test]
fn failed_compact_after_orphan_prune_allows_reuse() {
    let path = path("orphan-failed-compact");
    let original = session("orphan-reuse", 1000, 9000);
    let line = serde_json::json!({ "seq": 0, "ts": 1000, "type": "evidence",
        "digest": digest::sha256_hex(b"shared bundle"), "retention_until": 9000,
        "payload_b64": BASE64.encode(b"shared bundle") });
    std::fs::write(&path, format!("{line}\n")).unwrap();
    {
        let store = JsonlSessionStore::open(&path, 1500).unwrap();
        let tmp = path.with_extension("jsonl.tmp");
        std::fs::create_dir(&tmp).unwrap();
        assert!(store.compact(1500).is_err());
        std::fs::remove_dir(tmp).unwrap();
        store
            .put_session("reuse", original.clone(), 9000, 1600)
            .unwrap();
    }
    let store = JsonlSessionStore::open(&path, 1700).unwrap();
    assert_eq!(
        store
            .get_session(original.session_id(), 1700)
            .unwrap()
            .bytes(),
        original.bytes()
    );
}

// --- Storage-shape probes: dedup must remain externalized, not merely
// readable — a silent revert to inline storage would pass every byte-
// equality check above while forfeiting the space bound. ----------------

#[test]
fn stale_replay_retains_shared_storage_after_evidence_original_deadline() {
    let path = path("ablate-replay-storage");
    let original = session("long-citer", 1000, 9000);
    {
        let store = JsonlSessionStore::open(&path, 1000).unwrap();
        store
            .put_session("fp", original.clone(), 9000, 1000)
            .unwrap();
    }
    // Recreate a pre-fix log: the evidence envelope expires before its live citer.
    let log = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
            if record["type"] == "evidence" {
                record["retention_until"] = 2000.into();
            }
            serde_json::to_string(&record).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, log).unwrap();
    let store = JsonlSessionStore::open(&path, 1500).unwrap();
    store.compact(2100).unwrap();
    assert_eq!(
        store
            .get_session(original.session_id(), 2200)
            .unwrap()
            .bytes(),
        original.bytes()
    );
    assert_eq!(
        count_externalized(&path),
        (1, 1),
        "live citer must retain its shared evidence, not silently revert to inline storage"
    );
}

#[test]
fn compact_citer_max_keeps_bundle_cached_for_second_compaction() {
    let path = path("ablate-compact-memory");
    let legacy = session("legacy", 1000, 9000);
    let short = session("short", 1200, 2000);
    write_inline(&path, &legacy, 9000);
    let store = JsonlSessionStore::open(&path, 1200).unwrap();
    store.put_session("short", short, 2000, 1200).unwrap();
    store.compact(1500).unwrap();
    assert_eq!(count_externalized(&path), (1, 2));
    store.get_session(legacy.session_id(), 2100).unwrap();
    store.compact(2200).unwrap();
    assert_eq!(
        count_externalized(&path),
        (1, 1),
        "second compaction must preserve shared bundle through longer citer retention"
    );
}

#[test]
fn compact_written_deadline_prevents_redundant_bundle_reappend() {
    let path = path("ablate-compact-written");
    let first = session("first", 1000, 9000);
    let second = session("second", 1700, 7000);
    let store = JsonlSessionStore::open(&path, 1000).unwrap();
    store.put_session("first", first, 2000, 1000).unwrap();
    store.current_session("first", 9000, 1500).unwrap();
    store.compact(1600).unwrap();
    assert_eq!(count_externalized(&path), (1, 1));
    store.put_session("second", second, 7000, 1700).unwrap();
    assert_eq!(
        count_externalized(&path),
        (1, 2),
        "already persisted deadline must suppress redundant bundle append"
    );
}
