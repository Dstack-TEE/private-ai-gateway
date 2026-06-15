//! Persistence for attested sessions.
//!
//! [`SessionStore`] is the registry behind the audit endpoints. The durable
//! implementation, [`JsonlSessionStore`], is an append-only log of one record
//! per line, replayed into an in-memory index on open:
//!
//! ```text
//! {"seq":0,"ts":1700000000,"type":"session","payload":{…AttestedSession…}}
//! ```
//!
//! Integrity comes from **content-addressing**, not from a per-record
//! signature. Each record's `session_id` is a hash of its own verified material
//! ([`AttestedSession::content_id`]); the store recomputes it on replay and
//! refuses any record that no longer matches its contents, and a relying party
//! reaches the session through a *signed receipt* that commits to that id. So a
//! tampered log line is caught (its id won't match), and the chain that proves
//! the gateway vouched for the session is the receipt signature, not the log.
//! At-rest confidentiality/durability is the deployment's concern (TEE-sealed
//! volume). A hash-chained transparency log is a later enhancement.
//!
//! Sessions are immutable and content-addressed, so re-persisting an identical
//! session is idempotent in the index. `expires_at` is a retention window;
//! expired records are dropped lazily on read.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::session::AttestedSession;

/// Record type tag for a session line.
const RECORD_TYPE_SESSION: &str = "session";

/// One line in the append-only session log, as read back on replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLogRecord {
    pub seq: u64,
    pub ts: u64,
    #[serde(rename = "type")]
    pub record_type: String,
    pub payload: Value,
}

/// Write-side view of a log record that borrows the session, so a line is
/// serialized in one pass instead of building an intermediate `Value` tree.
#[derive(Serialize)]
struct SessionLogRecordRef<'a> {
    seq: u64,
    ts: u64,
    #[serde(rename = "type")]
    record_type: &'a str,
    payload: &'a AttestedSession,
}

/// The session registry behind the audit endpoints.
pub trait SessionStore: Send + Sync {
    /// Persist an immutable session. `ts` is the wall-clock second the record is
    /// written. The store assigns and returns the log sequence number.
    ///
    /// Integrity rests on content-addressing, not on a per-record signature: the
    /// `session_id` is recomputable from the record's own contents (see
    /// [`AttestedSession::content_id`]), and a relying party reaches it through a
    /// signed receipt that commits to that id. The store re-checks the id on
    /// replay and refuses records that no longer match their contents.
    fn put_session(&self, session: AttestedSession, ts: u64) -> io::Result<u64>;

    /// Fetch a session by id if it exists and has not passed `expires_at`.
    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession>;

    /// List non-expired sessions, optionally filtered by provider (the upstream
    /// config name). Sessions are per-TEE-channel, so there is no model filter
    /// here; a model→channel lookup (via the upstream config) belongs to the
    /// caller.
    fn list_sessions(&self, provider: Option<&str>, now: u64) -> Vec<AttestedSession>;
}

/// In-memory session index shared by both stores: the id→session map plus an
/// `expires_at`→ids index so eviction costs only what actually expired (O(k)),
/// not a full scan of the store on every write.
#[derive(Default)]
struct SessionIndex {
    by_id: HashMap<String, AttestedSession>,
    by_expiry: BTreeMap<u64, HashSet<String>>,
}

impl SessionIndex {
    /// Insert (or refresh) a session, then evict everything whose retention
    /// deadline has passed at `ts`. A session is content-addressed but re-put on
    /// every request with a later `expires_at`; when the deadline moves we drop
    /// the stale expiry hint so the index never points an id at the wrong bucket.
    fn put_and_evict(&mut self, session: AttestedSession, ts: u64) {
        self.insert(session);
        self.evict_expired(ts);
    }

    /// Insert without evicting — used to replay a log into the index at startup.
    fn insert(&mut self, session: AttestedSession) {
        let id = session.session_id.clone();
        let expires_at = session.expires_at;
        if let Some(prev) = self.by_id.insert(id.clone(), session) {
            if prev.expires_at != expires_at {
                self.drop_expiry_hint(&id, prev.expires_at);
            }
        }
        self.by_expiry.entry(expires_at).or_default().insert(id);
    }

    fn drop_expiry_hint(&mut self, id: &str, expires_at: u64) {
        if let Some(ids) = self.by_expiry.get_mut(&expires_at) {
            ids.remove(id);
            if ids.is_empty() {
                self.by_expiry.remove(&expires_at);
            }
        }
    }

    /// Pop every bucket whose deadline is at or before `now`.
    fn evict_expired(&mut self, now: u64) {
        while let Some((&expires_at, _)) = self.by_expiry.first_key_value() {
            if expires_at > now {
                break;
            }
            let (_, ids) = self
                .by_expiry
                .pop_first()
                .expect("first_key_value just returned a bucket");
            for id in ids {
                self.by_id.remove(&id);
            }
        }
    }

    fn get(&mut self, session_id: &str, now: u64) -> Option<AttestedSession> {
        match self.by_id.get(session_id) {
            Some(session) if now >= session.expires_at => {
                let expires_at = session.expires_at;
                self.by_id.remove(session_id);
                self.drop_expiry_hint(session_id, expires_at);
                None
            }
            Some(session) => Some(session.clone()),
            None => None,
        }
    }

    fn list(&self, provider: Option<&str>, now: u64) -> Vec<AttestedSession> {
        let mut out: Vec<AttestedSession> = self
            .by_id
            .values()
            .filter(|s| now < s.expires_at)
            .filter(|s| provider.is_none_or(|p| s.provider == p))
            .cloned()
            .collect();
        sort_sessions_newest_first(&mut out);
        out
    }
}

/// Stable presentation order for a session listing: newest first, then by id.
/// Shared so a multi-channel listing (e.g. a `?model=` fan-out across upstreams)
/// orders the merged result the same way a single channel's listing does.
pub(crate) fn sort_sessions_newest_first(sessions: &mut [AttestedSession]) {
    sessions.sort_by(|a, b| {
        b.established_at
            .cmp(&a.established_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
}

/// Append-only JSONL-backed [`SessionStore`].
///
/// The append log and the in-memory index sit behind separate locks, so a read
/// (`get`/`list`) never waits on a write's `write_all`. Writes serialize through
/// the writer lock (preserving seq order) and the index is updated under its own
/// lock immediately after. The write still runs on the caller's thread — moving
/// it off the latency path via a dedicated writer task is a future enhancement
/// for a hot durable store, and a no-op today since the default store is
/// in-memory and the log is not fsync'd.
pub struct JsonlSessionStore {
    writer: Mutex<LogWriter>,
    index: Mutex<SessionIndex>,
}

struct LogWriter {
    file: File,
    next_seq: u64,
}

impl JsonlSessionStore {
    /// Open (creating if absent) the log at `path`, replaying existing records
    /// into the in-memory index. Malformed lines are skipped so a partially
    /// written tail never blocks startup.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path: PathBuf = path.as_ref().to_path_buf();

        let mut next_seq = 0u64;
        let mut index = SessionIndex::default();
        if let Ok(file) = File::open(&path) {
            for line in BufReader::new(file).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(_) => break, // truncated tail; stop replay
                };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(record) = serde_json::from_str::<SessionLogRecord>(&line) else {
                    continue; // skip malformed line
                };
                next_seq = next_seq.max(record.seq + 1);
                if record.record_type == RECORD_TYPE_SESSION {
                    if let Ok(session) = serde_json::from_value::<AttestedSession>(record.payload) {
                        // Enforce content-addressing on replay: a record whose
                        // session_id does not match a fresh hash of its own
                        // contents was tampered with (or written by an
                        // incompatible version). Also require the evidence
                        // `data` to hash to its `digest` — the content id commits
                        // to the digest, not the bytes, so this catches a swapped
                        // evidence payload. Skip either way rather than serve it.
                        if session.content_id().ok().as_deref() == Some(&session.session_id)
                            && session.evidence.digest_matches_data()
                        {
                            index.insert(session);
                        }
                    }
                }
            }
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            writer: Mutex::new(LogWriter { file, next_seq }),
            index: Mutex::new(index),
        })
    }
}

impl SessionStore for JsonlSessionStore {
    fn put_session(&self, session: AttestedSession, ts: u64) -> io::Result<u64> {
        let seq = {
            let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
            let seq = w.next_seq;
            let mut line = serde_json::to_string(&SessionLogRecordRef {
                seq,
                ts,
                record_type: RECORD_TYPE_SESSION,
                payload: &session,
            })
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            line.push('\n');
            // `write_all` hands the bytes to the kernel; there is no `flush` — for
            // a `File` it is a no-op, and the log is not fsync'd (durability is the
            // deployment's TEE-sealed-volume concern). On a write error we return
            // before touching the index, so it stays consistent with the log.
            w.file.write_all(line.as_bytes())?;
            w.next_seq = seq + 1;
            seq
        };
        // Update the index under its own lock — a concurrent get/list never waited
        // on the write above. Bound the in-memory index: drop entries past their
        // retention deadline. (The append-only log itself still grows; compaction
        // is an ops concern — rotate/replay the file. Relying parties fetch a live
        // id, and replay rebuilds the index, so a crash between write and index
        // update loses nothing.)
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .put_and_evict(session, ts);
        Ok(seq)
    }

    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id, now)
    }

    fn list_sessions(&self, provider: Option<&str>, now: u64) -> Vec<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list(provider, now)
    }
}

/// Non-persistent [`SessionStore`] — the default when no session-log path is
/// configured. A restart loses the audit trail, matching the prior in-memory
/// behavior; configure a [`JsonlSessionStore`] for durability.
#[derive(Default)]
pub struct InMemorySessionStore {
    inner: Mutex<InMemoryInner>,
}

#[derive(Default)]
struct InMemoryInner {
    index: SessionIndex,
}

impl SessionStore for InMemorySessionStore {
    fn put_session(&self, session: AttestedSession, ts: u64) -> io::Result<u64> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        // Bound the store: drop entries past their retention deadline so a
        // long-running gateway does not accumulate a session per key rotation.
        guard.index.put_and_evict(session, ts);
        Ok(0)
    }

    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.index.get(session_id, now)
    }

    fn list_sessions(&self, provider: Option<&str>, now: u64) -> Vec<AttestedSession> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.index.list(provider, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::session::{EvidenceRef, SessionClaims};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("pag-sess-{}-{}.jsonl", std::process::id(), n))
    }

    // `marker` is folded into the sealed material (via the evidence digest) so
    // the sessions differ by content — and the resulting session_id stays a
    // valid content hash, which replay now enforces.
    fn session(endpoint: &str, marker: &str, expires_at: u64) -> AttestedSession {
        AttestedSession::seal(
            "phala-direct",
            Some(endpoint.to_string()),
            "phala-direct/1",
            None,
            vec![],
            SessionClaims::default(),
            EvidenceRef {
                digest: Some(format!("sha256:{}", marker.repeat(32))),
                data_uri: None,
            },
            1_000,
            expires_at,
        )
        .unwrap()
    }

    #[test]
    fn sort_sessions_newest_first_orders_a_merged_listing() {
        // What the `?model=` fan-out relies on: a concatenation of per-upstream
        // lists is re-sorted newest established_at first, with id as the tiebreak.
        let mk = |marker: &str, established_at: u64| {
            AttestedSession::seal(
                "phala-direct",
                Some("https://x".to_string()),
                "phala-direct/1",
                None,
                vec![],
                SessionClaims::default(),
                EvidenceRef {
                    digest: Some(format!("sha256:{}", marker.repeat(32))),
                    data_uri: None,
                },
                established_at,
                established_at + 1_000,
            )
            .unwrap()
        };
        let older = mk("aa", 1_000);
        let newer = mk("bb", 3_000);
        let tie_c = mk("cc", 2_000);
        let tie_d = mk("dd", 2_000);

        // Hand them in deliberately wrong order, as the fan-out concatenation would.
        let mut merged = vec![older.clone(), tie_d.clone(), newer.clone(), tie_c.clone()];
        sort_sessions_newest_first(&mut merged);
        let order: Vec<&str> = merged.iter().map(|s| s.session_id.as_str()).collect();

        assert_eq!(order[0], newer.session_id, "newest established_at first");
        assert_eq!(order[3], older.session_id, "oldest last");
        // The two established_at == 2000 ties sort by id ascending.
        let mut ties = [tie_c.session_id.clone(), tie_d.session_id.clone()];
        ties.sort();
        assert_eq!(&order[1..3], &[ties[0].as_str(), ties[1].as_str()]);
    }

    #[test]
    fn put_evicts_expired_sessions_so_the_store_stays_bounded() {
        let store = InMemorySessionStore::default();
        // A is live when written...
        let a = session("https://a", "aa", 2_000);
        store.put_session(a.clone(), 1_000).unwrap();
        // ...but a later write past A's retention deadline evicts it, so the
        // store does not accumulate a session per key rotation.
        let b = session("https://b", "bb", 10_000);
        store.put_session(b.clone(), 5_000).unwrap();

        assert!(store.get_session(&a.session_id, 5_000).is_none());
        assert!(store.get_session(&b.session_id, 5_000).is_some());
        let listed = store.list_sessions(None, 5_000);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, b.session_id);
    }

    #[test]
    fn refreshed_session_survives_its_old_deadline() {
        // A channel is content-addressed, so re-verifying it re-puts the same
        // session_id with a later expires_at. The expiry index must drop the
        // superseded deadline; otherwise eviction at the old deadline would
        // wrongly remove a still-live session.
        let store = InMemorySessionStore::default();
        let early = session("https://node.example", "same", 5_000);
        let id = early.session_id.clone();
        store.put_session(early, 1_000).unwrap();

        let refreshed = session("https://node.example", "same", 9_000);
        assert_eq!(refreshed.session_id, id, "same channel => same content id");
        store.put_session(refreshed, 4_000).unwrap();

        // A later write advances eviction past the OLD deadline (5_000). With a
        // stale expiry hint, this would drop the id even though it now lives to
        // 9_000.
        store
            .put_session(session("https://other.example", "x", 20_000), 6_000)
            .unwrap();

        assert!(
            store.get_session(&id, 7_000).is_some(),
            "refreshed session must outlive its superseded deadline"
        );
    }

    #[test]
    fn put_get_and_list_filtering() {
        let path = temp_path();
        let store = JsonlSessionStore::open(&path).unwrap();
        let a = session("https://node-7.example.net", "aa", 5_000);
        let b = session("https://node-9.example.net", "bb", 5_000);
        store.put_session(a.clone(), 1_000).unwrap();
        store.put_session(b.clone(), 1_001).unwrap();

        assert_eq!(store.get_session(&a.session_id, 2_000), Some(a.clone()));
        assert_eq!(store.list_sessions(None, 2_000).len(), 2);
        assert_eq!(store.list_sessions(Some("phala-direct"), 2_000).len(), 2);
        assert!(store.list_sessions(Some("nope"), 2_000).is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn expired_sessions_are_dropped_on_read() {
        let path = temp_path();
        let store = JsonlSessionStore::open(&path).unwrap();
        let s = session("https://node-7.example.net", "aa", 5_000);
        store.put_session(s.clone(), 1_000).unwrap();

        assert!(store.get_session(&s.session_id, 5_000).is_none());
        assert!(store.list_sessions(None, 5_000).is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replay_rebuilds_index_and_continues_seq() {
        let path = temp_path();
        let a = session("https://node-7.example.net", "aa", 5_000);
        let b = session("https://node-9.example.net", "bb", 5_000);
        {
            let store = JsonlSessionStore::open(&path).unwrap();
            let seq_a = store.put_session(a.clone(), 1_000).unwrap();
            let seq_b = store.put_session(b.clone(), 1_001).unwrap();
            assert_eq!((seq_a, seq_b), (0, 1));
        }

        // Reopen: index is rebuilt and the sequence continues from where it left.
        let store = JsonlSessionStore::open(&path).unwrap();
        assert_eq!(store.get_session(&a.session_id, 2_000), Some(a));
        assert_eq!(store.get_session(&b.session_id, 2_000), Some(b));
        let next = session("https://node-7.example.net", "cc", 5_000);
        let seq_c = store.put_session(next, 1_002).unwrap();
        assert_eq!(seq_c, 2, "seq continues after replay");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_lines_are_skipped_on_replay() {
        let path = temp_path();
        let good = session("https://node-7.example.net", "aa", 5_000);
        {
            let store = JsonlSessionStore::open(&path).unwrap();
            store.put_session(good.clone(), 1_000).unwrap();
        }
        // Append a garbage line + a blank line.
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"not json at all\n\n").unwrap();
        }

        let store = JsonlSessionStore::open(&path).unwrap();
        assert_eq!(store.get_session(&good.session_id, 2_000), Some(good));
        assert_eq!(store.list_sessions(None, 2_000).len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tampered_record_is_skipped_on_replay() {
        let path = temp_path();
        let good = session("https://node-7.example.net", "aa", 5_000);
        {
            let store = JsonlSessionStore::open(&path).unwrap();
            store.put_session(good.clone(), 1_000).unwrap();
        }
        // Hand-append a record whose contents were altered (provider flipped)
        // but whose session_id was left as the original — so the id no longer
        // matches a fresh hash of the contents.
        {
            let mut tampered = good.clone();
            tampered.provider = "attacker".to_string();
            let record = SessionLogRecord {
                seq: 1,
                ts: 1_001,
                record_type: "session".to_string(),
                payload: serde_json::to_value(&tampered).unwrap(),
            };
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(format!("{}\n", serde_json::to_string(&record).unwrap()).as_bytes())
                .unwrap();
        }

        // The genuine record survives; the tampered one is rejected because its
        // id is not the content hash of its (altered) contents.
        let store = JsonlSessionStore::open(&path).unwrap();
        assert_eq!(store.get_session(&good.session_id, 2_000), Some(good));
        assert_eq!(store.list_sessions(None, 2_000).len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn evidence_data_not_matching_its_digest_is_skipped_on_replay() {
        use crate::aci::canonical;

        // Seal a session whose evidence digest covers "abc" (base64 "YWJj").
        let mut s = AttestedSession::seal(
            "phala-direct",
            Some("https://node-7.example.net".to_string()),
            "phala-direct/1",
            None,
            vec![],
            SessionClaims::default(),
            EvidenceRef {
                digest: Some(canonical::sha256_hex(b"abc")),
                data_uri: Some("data:text/plain;base64,YWJj".to_string()),
            },
            1_000,
            9_000,
        )
        .unwrap();
        assert!(s.evidence.digest_matches_data());

        // Swap the evidence bytes but keep the digest — the content id is over
        // the digest, so the session_id still "matches" while the data does not.
        s.evidence.data_uri = Some("data:text/plain;base64,eHl6".to_string()); // "xyz"
        assert_eq!(s.content_id().unwrap(), s.session_id);
        assert!(!s.evidence.digest_matches_data());

        let path = temp_path();
        JsonlSessionStore::open(&path)
            .unwrap()
            .put_session(s.clone(), 1_000)
            .unwrap();

        // On replay the swapped-evidence record is rejected.
        let reopened = JsonlSessionStore::open(&path).unwrap();
        assert!(reopened.get_session(&s.session_id, 2_000).is_none());

        let _ = std::fs::remove_file(&path);
    }
}
