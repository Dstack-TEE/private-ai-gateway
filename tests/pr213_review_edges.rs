// Independent reviewer-only edge coverage (not production edits).
include!("pr213_review.rs");

fn write_inline(path: &std::path::Path, original: &AttestedSession, retention: u64) {
    let line = serde_json::json!({ "seq": 0, "ts": 1000, "type": "session", "fingerprint": "legacy",
        "retention_until": retention, "payload_b64": BASE64.encode(original.bytes()) });
    std::fs::write(path, format!("{line}\n")).unwrap();
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
