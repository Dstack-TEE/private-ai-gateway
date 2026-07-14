//! Persistence for attested sessions.
//!
//! [`SessionStore`] is the registry behind the audit endpoints. The durable
//! implementation, [`JsonlSessionStore`], is an append-only log of one record
//! per line, replayed into an in-memory index on open:
//!
//! ```text
//! {"seq":0,"ts":1700000000,"type":"session","fingerprint":"…","retention_until":1700003600,"payload_b64":"…"}
//! ```
//!
//! `payload_b64` carries the **exact served document bytes** (§9: the bytes
//! are the artifact), so replay reproduces byte-identical records; the
//! session id is always recomputed from those bytes, never trusted from disk.
//!
//! Two deadlines govern a record:
//!
//! * the document's own `expires_at` — the validity period for new
//!   forwarding decisions and for the list endpoint;
//! * the store-side `retention_until` — how long the record keeps being
//!   served by id. Retention MUST outlive every receipt citing the session,
//!   so each citation pushes it forward without touching the sealed bytes.
//!
//! `fingerprint` is a local, implementation-owned key over a channel's
//! verified material. It exists only so the hot path can find "the current
//! session for this channel" without re-sealing per request; it is never
//! served and carries no protocol meaning.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};

use super::session::AttestedSession;

/// Record type tag for a session line.
const RECORD_TYPE_SESSION: &str = "session";

/// One line in the append-only session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionLogRecord {
    seq: u64,
    ts: u64,
    #[serde(rename = "type")]
    record_type: String,
    fingerprint: String,
    retention_until: u64,
    payload_b64: String,
}

/// The session registry behind the audit endpoints.
pub trait SessionStore: Send + Sync {
    /// Persist a newly sealed session under its channel `fingerprint`. `ts` is
    /// the wall-clock second the record is written; `retention_until` is the
    /// initial retention deadline. The store assigns and returns the log
    /// sequence number.
    fn put_session(
        &self,
        fingerprint: &str,
        session: AttestedSession,
        retention_until: u64,
        now: u64,
    ) -> io::Result<u64>;

    /// Fetch a session by its full `sha256:<hex>` id. Served until its
    /// retention deadline — past `expires_at` too, because receipts citing it
    /// may still be live (§9 retention).
    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession>;

    /// The channel's current session — the one whose validity period covers
    /// `now` — extending its retention to at least `retention_until` (each
    /// citation obligates retention for another receipt TTL). `None` tells the
    /// caller to seal and [`put_session`](Self::put_session) a fresh document.
    fn current_session(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
    ) -> Option<AttestedSession>;

    /// List sessions whose validity period covers `now`, optionally filtered
    /// by `upstream_name` (the operator's upstream config name). Sessions are
    /// per-TEE-channel; a model→channel lookup belongs to the caller.
    fn list_sessions(&self, upstream_name: Option<&str>, now: u64) -> Vec<AttestedSession>;
}

struct SessionEntry {
    session: AttestedSession,
    fingerprint: String,
    retention_until: u64,
}

/// In-memory session index shared by both stores: id→entry plus a
/// fingerprint→current-id map and a retention-deadline index so eviction
/// costs only what actually lapsed.
#[derive(Default)]
struct SessionIndex {
    by_id: HashMap<String, SessionEntry>,
    by_fingerprint: HashMap<String, String>,
    by_retention: BTreeMap<u64, HashSet<String>>,
}

impl SessionIndex {
    fn insert(&mut self, fingerprint: String, session: AttestedSession, retention_until: u64) {
        let id = session.session_id().to_string();
        if let Some(prev) = self.by_id.insert(
            id.clone(),
            SessionEntry {
                session,
                fingerprint: fingerprint.clone(),
                retention_until,
            },
        ) {
            if prev.retention_until != retention_until {
                self.drop_retention_hint(&id, prev.retention_until);
            }
        }
        self.by_retention
            .entry(retention_until)
            .or_default()
            .insert(id.clone());
        self.by_fingerprint.insert(fingerprint, id);
    }

    fn drop_retention_hint(&mut self, id: &str, retention_until: u64) {
        if let Some(ids) = self.by_retention.get_mut(&retention_until) {
            ids.remove(id);
            if ids.is_empty() {
                self.by_retention.remove(&retention_until);
            }
        }
    }

    /// Pop every bucket whose retention deadline is at or before `now`.
    fn evict_lapsed(&mut self, now: u64) {
        while let Some((&retention_until, _)) = self.by_retention.first_key_value() {
            if retention_until > now {
                break;
            }
            let (_, ids) = self
                .by_retention
                .pop_first()
                .expect("first_key_value just returned a bucket");
            for id in ids {
                if let Some(entry) = self.by_id.remove(&id) {
                    if self.by_fingerprint.get(&entry.fingerprint) == Some(&id) {
                        self.by_fingerprint.remove(&entry.fingerprint);
                    }
                }
            }
        }
    }

    fn get(&mut self, session_id: &str, now: u64) -> Option<AttestedSession> {
        self.evict_lapsed(now);
        self.by_id.get(session_id).map(|e| e.session.clone())
    }

    fn current(
        &mut self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
    ) -> Option<AttestedSession> {
        self.evict_lapsed(now);
        let id = self.by_fingerprint.get(fingerprint)?.clone();
        let entry = self.by_id.get_mut(&id)?;
        if now >= entry.session.document().expires_at {
            return None; // validity lapsed; record stays for retention only
        }
        if retention_until > entry.retention_until {
            let old = entry.retention_until;
            entry.retention_until = retention_until;
            let session = entry.session.clone();
            self.drop_retention_hint(&id, old);
            self.by_retention
                .entry(retention_until)
                .or_default()
                .insert(id);
            return Some(session);
        }
        Some(entry.session.clone())
    }

    fn list(&self, upstream_name: Option<&str>, now: u64) -> Vec<AttestedSession> {
        let mut out: Vec<AttestedSession> = self
            .by_id
            .values()
            .filter(|e| now < e.session.document().expires_at)
            .filter(|e| upstream_name.is_none_or(|p| e.session.document().upstream_name == p))
            .map(|e| e.session.clone())
            .collect();
        sort_sessions_newest_first(&mut out);
        out
    }
}

/// Stable presentation order for a session listing: newest first, then by id.
pub(crate) fn sort_sessions_newest_first(sessions: &mut [AttestedSession]) {
    sessions.sort_by(|a, b| {
        b.document()
            .established_at
            .cmp(&a.document().established_at)
            .then_with(|| a.session_id().cmp(b.session_id()))
    });
}

/// Append-only JSONL-backed [`SessionStore`]. The append log and the in-memory
/// index sit behind separate locks, so a read never waits on a write.
///
/// The hot path appends a line only when a *new* session is sealed; a repeat
/// request extends the current session's retention in the index without
/// writing (see [`SessionStore::current_session`]).
/// [`JsonlSessionStore::compact`] then rewrites the file from the live index,
/// dropping lapsed records and persisting the extended retention deadlines.
///
/// Single-writer is enforced with an advisory lock on a *separate* lock file
/// (`<log>.lock`) that is never renamed, held for the whole lifetime of the
/// store. The data log itself is rename-swapped by compaction, so a lock on the
/// log inode would migrate off the path during the swap and let a racing opener
/// slip in; the lock file has no such window.
pub struct JsonlSessionStore {
    path: PathBuf,
    /// Held for its side effect: the advisory lock lives as long as this handle.
    _lock_file: File,
    writer: Mutex<LogWriter>,
    index: Mutex<SessionIndex>,
}

struct LogWriter {
    file: File,
    next_seq: u64,
}

/// Take the advisory exclusive lock that enforces single-writer (see
/// [`JsonlSessionStore`]). The returned handle must be held for the writer's
/// lifetime; the lock releases when it is dropped, including on crash.
fn acquire_exclusive_lock(lock_path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false) // the lock file carries no content; never truncate it
        .open(lock_path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "another gateway instance holds the session log lock at {}; \
                 refusing to start to avoid forking the log",
                lock_path.display()
            ),
        )),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// The lock-file path that guards the log at `path` (`<log>.lock`).
fn lock_path_for(path: &Path) -> PathBuf {
    path.with_extension("jsonl.lock")
}

impl JsonlSessionStore {
    /// Open (creating if absent) the log at `path`, replaying existing records
    /// into the in-memory index. Malformed lines are skipped so a partially
    /// written tail never blocks startup.
    ///
    /// Takes an advisory exclusive lock on `<path>.lock` *before* reading the
    /// log, so only one process ever writes it — failing with
    /// [`io::ErrorKind::WouldBlock`] if another holds the lock.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path: PathBuf = path.as_ref().to_path_buf();

        // Single-writer lock first, before we read or write the log.
        let lock_file = acquire_exclusive_lock(&lock_path_for(&path))?;

        let mut next_seq = 0u64;
        let mut index = SessionIndex::default();
        let replay_file = match File::open(&path) {
            Ok(file) => Some(file),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        if let Some(file) = replay_file {
            let mut reader = BufReader::new(file);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                // Read raw bytes rather than `lines()`: a crash can truncate the
                // tail mid-multibyte, which `lines()` surfaces as an InvalidData
                // error. Only a genuine read error should stop startup; corrupt
                // or non-UTF-8 bytes are skipped and compaction drops them.
                if reader.read_until(b'\n', &mut buf)? == 0 {
                    break; // EOF
                }
                let trimmed = buf.trim_ascii();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(record) = serde_json::from_slice::<SessionLogRecord>(trimmed) else {
                    continue; // malformed line; compaction will drop it
                };
                let Some(seq_after) = record.seq.checked_add(1) else {
                    continue; // corrupt seq at u64::MAX; skip rather than overflow
                };
                next_seq = next_seq.max(seq_after);
                if record.record_type != RECORD_TYPE_SESSION {
                    continue;
                }
                let Ok(bytes) = BASE64.decode(record.payload_b64.as_bytes()) else {
                    continue;
                };
                // The id is recomputed from the exact persisted bytes
                // (`AttestedSession::from_bytes`), so a tampered payload simply
                // resolves to a different id than any receipt cites. The
                // evidence `data`, however, must still hash to its in-document
                // `digest` (§9.2) — refuse to serve a swapped payload.
                let Ok(session) = AttestedSession::from_bytes(bytes) else {
                    continue;
                };
                if !session.document().evidence.digest_matches_data() {
                    continue;
                }
                index.insert(record.fingerprint, session, record.retention_until);
            }
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            _lock_file: lock_file,
            writer: Mutex::new(LogWriter { file, next_seq }),
            index: Mutex::new(index),
        })
    }

    /// Rewrite the log from the retained (non-lapsed) index: drop lapsed
    /// records, collapse duplicates, and persist each record's current
    /// retention deadline (which the hot path extends in the index without
    /// appending). Returns the number of records kept.
    ///
    /// Records are written and synced to a temp file before an atomic rename.
    /// The replacement append handle is opened before the rename, so a
    /// successful swap never leaves the writer pointing at the old, unlinked
    /// file.
    pub fn compact(&self, now: u64) -> io::Result<usize> {
        // Hold the writer across the whole rewrite so no append races the swap.
        // Lock order is writer → index, matching `put_session`, so the two paths
        // can never deadlock against each other.
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());

        let live: Vec<(String, AttestedSession, u64)> = {
            let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
            index.evict_lapsed(now);
            index
                .by_id
                .values()
                .map(|e| (e.fingerprint.clone(), e.session.clone(), e.retention_until))
                .collect()
        };

        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = File::create(&tmp)?;
            for (seq, (fingerprint, session, retention_until)) in live.iter().enumerate() {
                let mut line = serde_json::to_string(&SessionLogRecord {
                    seq: seq as u64,
                    ts: now,
                    record_type: RECORD_TYPE_SESSION.to_string(),
                    fingerprint: fingerprint.clone(),
                    retention_until: *retention_until,
                    payload_b64: BASE64.encode(session.bytes()),
                })
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
            }
            out.sync_all()?; // durable temp contents before it becomes the log
        }

        // Open the replacement append handle before the rename, so the only
        // fallible step left is the rename — the writer is never left pointing at
        // the stale inode.
        let new_file = OpenOptions::new().append(true).open(&tmp)?;
        std::fs::rename(&tmp, &self.path)?;

        w.file = new_file;
        w.next_seq = live.len() as u64;
        Ok(live.len())
    }
}

impl SessionStore for JsonlSessionStore {
    fn put_session(
        &self,
        fingerprint: &str,
        session: AttestedSession,
        retention_until: u64,
        now: u64,
    ) -> io::Result<u64> {
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let seq = w.next_seq;
        // Refuse to write a record we cannot assign a successor to, rather than
        // overflow. Only reachable from a corrupt replayed `seq` near u64::MAX;
        // the gateway's startup compaction renumbers from zero before serving.
        let Some(next_seq) = seq.checked_add(1) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session log sequence number overflowed u64::MAX",
            ));
        };
        let mut line = serde_json::to_string(&SessionLogRecord {
            seq,
            ts: now,
            record_type: RECORD_TYPE_SESSION.to_string(),
            fingerprint: fingerprint.to_string(),
            retention_until,
            payload_b64: BASE64.encode(session.bytes()),
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        // No flush: `File::flush` is a no-op and the log isn't fsync'd. If
        // `file` ever becomes a `BufWriter`, restore a flush or records can sit
        // unwritten on a crash.
        w.file.write_all(line.as_bytes())?;
        w.next_seq = next_seq;
        // Update the index under the writer lock so the log and index advance
        // together: `compact` rewrites the log *from* the index, so an index that
        // lagged a completed append could drop an on-disk record. Reads still
        // don't wait on the file write — only on this brief index update.
        let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
        index.insert(fingerprint.to_string(), session, retention_until);
        index.evict_lapsed(now);
        Ok(seq)
    }

    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id, now)
    }

    fn current_session(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
    ) -> Option<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .current(fingerprint, retention_until, now)
    }

    fn list_sessions(&self, upstream_name: Option<&str>, now: u64) -> Vec<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list(upstream_name, now)
    }
}

/// Non-persistent [`SessionStore`] — the default when no session-log path is
/// configured. A restart loses the audit trail; configure a
/// [`JsonlSessionStore`] for durability.
#[derive(Default)]
pub struct InMemorySessionStore {
    index: Mutex<SessionIndex>,
}

impl SessionStore for InMemorySessionStore {
    fn put_session(
        &self,
        fingerprint: &str,
        session: AttestedSession,
        retention_until: u64,
        now: u64,
    ) -> io::Result<u64> {
        let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
        // Bound the store: drop entries past their retention deadline so a
        // long-running gateway does not accumulate a session per re-verification.
        index.insert(fingerprint.to_string(), session, retention_until);
        index.evict_lapsed(now);
        Ok(0)
    }

    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id, now)
    }

    fn current_session(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
    ) -> Option<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .current(fingerprint, retention_until, now)
    }

    fn list_sessions(&self, upstream_name: Option<&str>, now: u64) -> Vec<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list(upstream_name, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::session::{EvidenceRef, SessionClaims, SessionDocument};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("pag-sess-{}-{}.jsonl", std::process::id(), n))
    }

    /// Remove the log and the sibling files a store leaves beside it (the lock
    /// file and any stale compaction temp), so a test does not litter the temp
    /// directory.
    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(lock_path_for(path));
        let _ = std::fs::remove_file(path.with_extension("jsonl.tmp"));
    }

    /// Open a store, retrying briefly on `WouldBlock`. Other tests in this binary
    /// spawn child processes (the external-verifier tests); during their
    /// fork→exec window a child transiently inherits this store's advisory-lock
    /// fd, so a fresh open can momentarily see the lock as held. Production never
    /// hits this — it holds one lock for its whole life and never re-acquires —
    /// so the retry belongs only in the test harness.
    fn open_store(path: &Path) -> JsonlSessionStore {
        for _ in 0..200 {
            match JsonlSessionStore::open(path) {
                Ok(store) => return store,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("opening {} failed: {e}", path.display()),
            }
        }
        panic!("opening {} kept returning WouldBlock", path.display())
    }

    fn count_lines(path: &Path) -> usize {
        let file = File::open(path).unwrap();
        BufReader::new(file)
            .lines()
            .filter(|l| l.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false))
            .count()
    }

    fn session(endpoint: &str, established_at: u64, expires_at: u64) -> AttestedSession {
        AttestedSession::seal(SessionDocument {
            api_version: "aci/1".to_string(),
            upstream_name: "phala-direct".to_string(),
            endpoint: Some(endpoint.to_string()),
            verifier_id: "phala-direct/1".to_string(),
            established_at,
            expires_at,
            identity: None,
            channel_binding: vec![],
            claims: SessionClaims::default(),
            evidence: EvidenceRef::default(),
        })
        .unwrap()
    }

    #[test]
    fn current_session_extends_retention_without_a_log_append() {
        let path = temp_path();
        let store = open_store(&path);
        let s = session("https://x", 1_000, 2_000);
        let id = s.session_id().to_string();
        store.put_session("fp-x", s, 2_500, 1_000).unwrap();

        // A repeat request finds the current session and extends retention
        // without appending.
        let before = std::fs::metadata(&path).unwrap().len();
        assert!(store.current_session("fp-x", 9_000, 1_500).is_some());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);

        // Past the validity period, the channel needs a fresh session...
        assert!(store.current_session("fp-x", 12_000, 3_000).is_none());
        // ...but the record keeps serving by id until its retention deadline.
        assert!(store.get_session(&id, 5_000).is_some());
        assert!(store.get_session(&id, 9_000).is_none());

        cleanup(&path);
    }

    #[test]
    fn expired_validity_drops_out_of_listings_but_not_lookup() {
        let store = InMemorySessionStore::default();
        let s = session("https://x", 1_000, 2_000);
        let id = s.session_id().to_string();
        store.put_session("fp-x", s, 9_000, 1_000).unwrap();

        assert_eq!(store.list_sessions(None, 1_500).len(), 1);
        // Validity lapsed: gone from the preflight listing, still resolvable
        // by id for receipts that cite it.
        assert!(store.list_sessions(None, 2_000).is_empty());
        assert!(store.get_session(&id, 2_000).is_some());
    }

    #[test]
    fn lapsed_retention_evicts_so_the_store_stays_bounded() {
        let store = InMemorySessionStore::default();
        let a = session("https://a", 1_000, 2_000);
        let a_id = a.session_id().to_string();
        store.put_session("fp-a", a, 2_000, 1_000).unwrap();
        // A later write past A's retention deadline evicts it.
        let b = session("https://b", 5_000, 10_000);
        store.put_session("fp-b", b.clone(), 10_000, 5_000).unwrap();

        assert!(store.get_session(&a_id, 5_000).is_none());
        assert!(store.get_session(b.session_id(), 5_000).is_some());
    }

    #[test]
    fn sort_sessions_newest_first_orders_a_merged_listing() {
        let older = session("https://a", 1_000, 99_000);
        let newer = session("https://b", 3_000, 99_000);
        let tie_c = session("https://c", 2_000, 99_000);
        let tie_d = session("https://d", 2_000, 99_000);

        let mut merged = vec![older.clone(), tie_d.clone(), newer.clone(), tie_c.clone()];
        sort_sessions_newest_first(&mut merged);
        let order: Vec<&str> = merged.iter().map(|s| s.session_id()).collect();

        assert_eq!(order[0], newer.session_id(), "newest established_at first");
        assert_eq!(order[3], older.session_id(), "oldest last");
        let mut ties = [
            tie_c.session_id().to_string(),
            tie_d.session_id().to_string(),
        ];
        ties.sort();
        assert_eq!(&order[1..3], &[ties[0].as_str(), ties[1].as_str()]);
    }

    #[test]
    fn replay_rebuilds_byte_identical_records_and_continues_seq() {
        let path = temp_path();
        let a = session("https://node-7.example.net", 1_000, 5_000);
        let b = session("https://node-9.example.net", 1_001, 5_000);
        {
            let store = open_store(&path);
            let seq_a = store.put_session("fp-a", a.clone(), 5_000, 1_000).unwrap();
            let seq_b = store.put_session("fp-b", b.clone(), 5_000, 1_001).unwrap();
            assert_eq!((seq_a, seq_b), (0, 1));
        }

        let store = open_store(&path);
        let replayed = store.get_session(a.session_id(), 2_000).unwrap();
        assert_eq!(replayed.bytes(), a.bytes(), "served bytes survive replay");
        assert_eq!(store.get_session(b.session_id(), 2_000), Some(b));
        // The fingerprint index survives replay too.
        assert_eq!(
            store
                .current_session("fp-a", 5_000, 2_000)
                .map(|s| s.session_id().to_string()),
            Some(a.session_id().to_string())
        );
        let next = session("https://node-7.example.net", 1_002, 5_000);
        assert_eq!(store.put_session("fp-c", next, 5_000, 1_002).unwrap(), 2);

        drop(store);
        cleanup(&path);
    }

    #[test]
    fn compact_collapses_history_to_the_retained_set() {
        let path = temp_path();
        let live = session("https://node-7.example.net", 1_000, 9_000);
        let lapsed = session("https://node-9.example.net", 1_000, 4_000);
        let now = 5_000;
        {
            let store = open_store(&path);
            for ts in [1_000, 2_000, 3_000] {
                store
                    .put_session("fp-live", live.clone(), 9_000, ts)
                    .unwrap();
            }
            store
                .put_session("fp-gone", lapsed.clone(), 4_000, 1_000)
                .unwrap();
            assert_eq!(count_lines(&path), 4);

            let kept = store.compact(now).unwrap();
            assert_eq!(kept, 1, "only the retained record is kept");
            assert_eq!(count_lines(&path), 1);
            assert!(store.get_session(lapsed.session_id(), now).is_none());
            assert_eq!(
                store.get_session(live.session_id(), now),
                Some(live.clone())
            );
        }

        let reopened = open_store(&path);
        assert_eq!(reopened.get_session(live.session_id(), now), Some(live));
        drop(reopened);
        cleanup(&path);
    }

    #[test]
    fn malformed_lines_are_skipped_on_replay() {
        let path = temp_path();
        let good = session("https://node-7.example.net", 1_000, 5_000);
        {
            let store = open_store(&path);
            store.put_session("fp", good.clone(), 5_000, 1_000).unwrap();
        }
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"not json at all\n\n").unwrap();
            f.write_all(&[b'{', 0xff, 0xfe]).unwrap(); // truncated invalid UTF-8 tail
        }

        let store = open_store(&path);
        assert_eq!(store.get_session(good.session_id(), 2_000), Some(good));
        assert_eq!(store.list_sessions(None, 2_000).len(), 1);

        drop(store);
        cleanup(&path);
    }

    #[test]
    fn second_open_is_locked_out_while_the_first_writer_lives() {
        let path = temp_path();
        let first = open_store(&path);

        let blocked = JsonlSessionStore::open(&path);
        assert!(
            matches!(&blocked, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
            "a second open must fail while the first holds the lock"
        );

        drop(first);
        let reopened = open_store(&path); // panics if it cannot re-acquire
        drop(reopened);

        cleanup(&path);
    }

    #[test]
    fn evidence_data_not_matching_its_digest_is_skipped_on_replay() {
        use crate::aci::digest;

        // A document whose evidence digest covers "abc" but whose data was
        // swapped for "xyz": the session id still matches the (tampered) bytes,
        // so only the digest check catches the swap.
        let doc = SessionDocument {
            api_version: "aci/1".to_string(),
            upstream_name: "phala-direct".to_string(),
            endpoint: Some("https://node-7.example.net".to_string()),
            verifier_id: "phala-direct/1".to_string(),
            established_at: 1_000,
            expires_at: 9_000,
            identity: None,
            channel_binding: vec![],
            claims: SessionClaims::default(),
            evidence: EvidenceRef {
                digest: Some(digest::sha256_hex(b"abc")),
                data_uri: Some("data:text/plain;base64,eHl6".to_string()), // "xyz"
            },
        };
        let swapped = AttestedSession::seal(doc).unwrap();
        assert!(!swapped.document().evidence.digest_matches_data());

        let path = temp_path();
        open_store(&path)
            .put_session("fp", swapped.clone(), 9_000, 1_000)
            .unwrap();

        let reopened = open_store(&path);
        assert!(reopened.get_session(swapped.session_id(), 2_000).is_none());

        drop(reopened);
        cleanup(&path);
    }

    #[test]
    fn put_session_errors_instead_of_overflowing_seq() {
        // A replayed `seq` of u64::MAX - 1 leaves `next_seq` at u64::MAX; the
        // next append must return an error rather than overflow `seq + 1`.
        let path = temp_path();
        let good = session("https://node-7.example.net", 1_000, 9_000);
        let seeded = SessionLogRecord {
            seq: u64::MAX - 1,
            ts: 1_000,
            record_type: RECORD_TYPE_SESSION.to_string(),
            fingerprint: "fp".to_string(),
            retention_until: 9_000,
            payload_b64: BASE64.encode(good.bytes()),
        };
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&seeded).unwrap()),
        )
        .unwrap();

        let store = open_store(&path);
        let next = session("https://node-9.example.net", 2_000, 9_000);
        let err = store.put_session("fp2", next, 9_000, 2_000).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        drop(store);
        cleanup(&path);
    }
}
