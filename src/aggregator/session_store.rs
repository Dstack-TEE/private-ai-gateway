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

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::session::AttestedSession;

/// Record type tag for a session line.
const RECORD_TYPE_SESSION: &str = "session";

/// One line in the append-only session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLogRecord {
    pub seq: u64,
    pub ts: u64,
    #[serde(rename = "type")]
    pub record_type: String,
    pub payload: Value,
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

    /// List non-expired sessions, optionally filtered by provider and/or public
    /// model id.
    fn list_sessions(
        &self,
        provider: Option<&str>,
        model_id: Option<&str>,
        now: u64,
    ) -> Vec<AttestedSession>;
}

/// Append-only JSONL-backed [`SessionStore`].
pub struct JsonlSessionStore {
    inner: Mutex<Inner>,
}

struct Inner {
    writer: File,
    next_seq: u64,
    by_id: HashMap<String, AttestedSession>,
}

impl JsonlSessionStore {
    /// Open (creating if absent) the log at `path`, replaying existing records
    /// into the in-memory index. Malformed lines are skipped so a partially
    /// written tail never blocks startup.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path: PathBuf = path.as_ref().to_path_buf();

        let mut next_seq = 0u64;
        let mut by_id = HashMap::new();
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
                        // incompatible version). Skip it rather than serve it.
                        if session.content_id().ok().as_deref() == Some(&session.session_id) {
                            by_id.insert(session.session_id.clone(), session);
                        }
                    }
                }
            }
        }

        let writer = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                writer,
                next_seq,
                by_id,
            }),
        })
    }
}

impl SessionStore for JsonlSessionStore {
    fn put_session(&self, session: AttestedSession, ts: u64) -> io::Result<u64> {
        let payload = serde_json::to_value(&session)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let seq = guard.next_seq;
        let record = SessionLogRecord {
            seq,
            ts,
            record_type: RECORD_TYPE_SESSION.to_string(),
            payload,
        };
        let mut line = serde_json::to_string(&record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        guard.writer.write_all(line.as_bytes())?;
        guard.writer.flush()?;
        guard.next_seq = seq + 1;
        guard.by_id.insert(session.session_id.clone(), session);
        // Bound the in-memory index: drop entries past their retention deadline.
        // (The append-only log itself still grows; compaction is an ops concern —
        // rotate/replay the log file. Relying parties always fetch a live id.)
        guard.by_id.retain(|_, s| ts < s.expires_at);
        Ok(seq)
    }

    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match guard.by_id.get(session_id) {
            Some(session) if now >= session.expires_at => {
                guard.by_id.remove(session_id);
                None
            }
            Some(session) => Some(session.clone()),
            None => None,
        }
    }

    fn list_sessions(
        &self,
        provider: Option<&str>,
        model_id: Option<&str>,
        now: u64,
    ) -> Vec<AttestedSession> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<AttestedSession> = guard
            .by_id
            .values()
            .filter(|s| now < s.expires_at)
            .filter(|s| provider.is_none_or(|p| s.provider == p))
            .filter(|s| model_id.is_none_or(|m| s.model_id == m))
            .cloned()
            .collect();
        // Stable order for callers/tests: newest first, then by id.
        out.sort_by(|a, b| {
            b.established_at
                .cmp(&a.established_at)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        out
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
    by_id: HashMap<String, AttestedSession>,
}

impl SessionStore for InMemorySessionStore {
    fn put_session(&self, session: AttestedSession, ts: u64) -> io::Result<u64> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.by_id.insert(session.session_id.clone(), session);
        // Bound the store: drop entries past their retention deadline so a
        // long-running gateway does not accumulate a session per key rotation.
        guard.by_id.retain(|_, s| ts < s.expires_at);
        Ok(0)
    }

    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match guard.by_id.get(session_id) {
            Some(session) if now >= session.expires_at => {
                guard.by_id.remove(session_id);
                None
            }
            Some(session) => Some(session.clone()),
            None => None,
        }
    }

    fn list_sessions(
        &self,
        provider: Option<&str>,
        model_id: Option<&str>,
        now: u64,
    ) -> Vec<AttestedSession> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<AttestedSession> = guard
            .by_id
            .values()
            .filter(|s| now < s.expires_at)
            .filter(|s| provider.is_none_or(|p| s.provider == p))
            .filter(|s| model_id.is_none_or(|m| s.model_id == m))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.established_at
                .cmp(&a.established_at)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        out
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
            "glm51-phala",
            Some("zai-org/GLM-5.1".to_string()),
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
        let listed = store.list_sessions(None, None, 5_000);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, b.session_id);
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
        assert_eq!(store.list_sessions(None, None, 2_000).len(), 2);
        assert_eq!(
            store
                .list_sessions(Some("phala-direct"), Some("glm51-phala"), 2_000)
                .len(),
            2
        );
        assert!(store.list_sessions(Some("nope"), None, 2_000).is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn expired_sessions_are_dropped_on_read() {
        let path = temp_path();
        let store = JsonlSessionStore::open(&path).unwrap();
        let s = session("https://node-7.example.net", "aa", 5_000);
        store.put_session(s.clone(), 1_000).unwrap();

        assert!(store.get_session(&s.session_id, 5_000).is_none());
        assert!(store.list_sessions(None, None, 5_000).is_empty());

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
        assert_eq!(store.list_sessions(None, None, 2_000).len(), 1);

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
        assert_eq!(store.list_sessions(None, None, 2_000).len(), 1);

        let _ = std::fs::remove_file(&path);
    }
}
