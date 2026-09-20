//! Persistence for attested sessions.
//!
//! [`SessionStore`] is the registry behind the audit endpoints. The durable
//! implementation, [`JsonlSessionStore`], stores each session in field-level
//! content-addressed form (see [`super::session_cas`] and
//! docs/aci-session-storage-study.md):
//!
//! * `<log>.docs/<session-id>` — the **skeleton**: the document with large
//!   strings and subtrees replaced by chunk references (or, in the `whole`
//!   fallback, the untouched document bytes);
//! * `<log>.chunks/<sha256>` — immutable **chunks**, written at most once
//!   and shared across sessions, upstreams, and rounds;
//! * the JSONL log itself — one small index record per session:
//!
//! ```text
//! {"seq":0,"ts":1700000000,"type":"session2","fingerprint":"…","id":"…","upstream_name":"…","expires_at":…,"retention_until":…,"whole":false}
//! ```
//!
//! Rebuilt bytes re-hash to the session id and `evidence.data` re-hashes to
//! `evidence.digest`; both are verified on every read, so a tampered or
//! corrupted store only ever resolves to a different id than any receipt
//! cites. Packing self-checks byte-exactness and degrades to `whole`
//! (unpacked bytes) on any unfamiliar shape, so an upstream format change
//! can cost dedup but never correctness.
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
use super::session_cas::{pack, unpack, Packed};

/// Record type tag for a legacy (full-bytes) session line, kept for replay
/// of logs written before field-level CAS storage.
const RECORD_TYPE_SESSION: &str = "session";
/// Record type tag for a CAS index record.
const RECORD_TYPE_SESSION_V2: &str = "session2";

/// One line in the legacy append-only session log (pre-CAS format).
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

/// One line in the CAS session log: the index record. The document bytes
/// live in `<log>.docs/<id>` (skeleton or whole) and its chunks in
/// `<log>.chunks/<digest>`; this record is only the replayable index.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionLogRecordV2 {
    seq: u64,
    ts: u64,
    #[serde(rename = "type")]
    record_type: String,
    fingerprint: String,
    id: String,
    upstream_name: String,
    expires_at: u64,
    retention_until: u64,
    whole: bool,
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

    /// Fetch a session by its full `bare 64-hex` id. Served until its
    /// retention deadline — past `expires_at` too, because receipts citing it
    /// may still be live (§8 retention).
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
            let (_, ids) = self.by_retention.pop_first().expect("bucket exists");
            for id in ids {
                self.by_id.remove(&id);
                self.by_fingerprint.retain(|_, v| v != &id);
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
        let id = self.by_fingerprint.get(fingerprint)?.clone();
        let entry = self.by_id.get_mut(&id)?;
        if now >= entry.session.document().expires_at {
            return None; // validity lapsed; record stays for retention only
        }
        let session = entry.session.clone();
        let old = entry.retention_until;
        if retention_until <= old {
            return Some(session);
        }
        entry.retention_until = retention_until;
        // `entry` is not used past this point, so the index-wide updates below
        // no longer conflict with its borrow.
        self.drop_retention_hint(&id, old);
        self.by_retention
            .entry(retention_until)
            .or_default()
            .insert(id);
        Some(session)
    }

    fn list(&self, upstream_name: Option<&str>, now: u64) -> Vec<AttestedSession> {
        let mut out: Vec<AttestedSession> = self
            .by_id
            .values()
            .filter(|e| now < e.session.document().expires_at)
            .filter(|e| {
                upstream_name.is_none_or(|n| n == e.session.document().upstream_name.as_str())
            })
            .map(|e| e.session.clone())
            .collect();
        sort_sessions_newest_first(&mut out);
        out
    }
}

/// Order a merged multi-upstream listing the same way the single-channel
/// path does: newest `established_at` first, ties by session id ascending
/// (deterministic for tests and for clients diffing list responses).
pub(crate) fn sort_sessions_newest_first(sessions: &mut [AttestedSession]) {
    sessions.sort_by(|a, b| {
        b.document()
            .established_at
            .cmp(&a.document().established_at)
            .then_with(|| a.session_id().cmp(b.session_id()))
    });
}

/// The metadata a CAS store keeps resident per session. The document itself
/// (skeleton or whole bytes) stays on disk and is read through on demand —
/// with a 30-day retention horizon the resident set would otherwise be the
/// full store.
#[derive(Debug, Clone)]
struct CasEntry {
    fingerprint: String,
    upstream_name: String,
    expires_at: u64,
    retention_until: u64,
    whole: bool,
}

/// Metadata-only index for the CAS store: id→entry plus fingerprint→current
/// and retention-deadline hints, mirroring [`SessionIndex`].
#[derive(Default)]
struct CasIndex {
    by_id: HashMap<String, CasEntry>,
    by_fingerprint: HashMap<String, String>,
    by_retention: BTreeMap<u64, HashSet<String>>,
}

impl CasIndex {
    fn insert(&mut self, fingerprint: String, id: String, entry: CasEntry) {
        let retention_until = entry.retention_until;
        if let Some(prev) = self.by_id.insert(id.clone(), entry) {
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

    /// Pop every bucket whose retention deadline is at or before `now`;
    /// returns the evicted ids so the caller can delete their doc files.
    fn evict_lapsed(&mut self, now: u64) -> Vec<String> {
        let mut evicted = Vec::new();
        while let Some((&retention_until, _)) = self.by_retention.first_key_value() {
            if retention_until > now {
                break;
            }
            let (_, ids) = self.by_retention.pop_first().expect("bucket exists");
            for id in ids {
                if self.by_id.remove(&id).is_some() {
                    evicted.push(id.clone());
                }
                self.by_fingerprint.retain(|_, v| v != &id);
            }
        }
        evicted
    }

    fn get(&mut self, session_id: &str, now: u64) -> Option<CasEntry> {
        self.evict_lapsed(now);
        self.by_id.get(session_id).cloned()
    }

    fn current(
        &mut self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
    ) -> Option<(String, CasEntry)> {
        let id = self.by_fingerprint.get(fingerprint)?.clone();
        let entry = self.by_id.get_mut(&id)?;
        if now >= entry.expires_at {
            return None; // validity lapsed; record stays for retention only
        }
        let snapshot = entry.clone();
        let old = entry.retention_until;
        if retention_until <= old {
            return Some((id, snapshot));
        }
        entry.retention_until = retention_until;
        // `entry` is not used past this point, so the index-wide updates below
        // no longer conflict with its borrow.
        self.drop_retention_hint(&id, old);
        self.by_retention
            .entry(retention_until)
            .or_default()
            .insert(id.clone());
        Some((id, snapshot))
    }

    fn list(&self, upstream_name: Option<&str>, now: u64) -> Vec<(String, CasEntry)> {
        let mut out: Vec<(String, CasEntry)> = self
            .by_id
            .iter()
            .filter(|(_, e)| now < e.expires_at)
            .filter(|(_, e)| upstream_name.is_none_or(|n| n == e.upstream_name.as_str()))
            .map(|(id, e)| (id.clone(), e.clone()))
            .collect();
        out.sort_by(|a, b| {
            b.1.expires_at
                .cmp(&a.1.expires_at)
                .then_with(|| a.0.cmp(&b.0))
        });
        out
    }
}

/// Persistence for attested sessions.
///
/// [`JsonlSessionStore`] keeps the session registry durable: an append-only
/// index log replayed on open, with documents and chunks as immutable
/// content-addressed files beside it (see the module docs).
///
/// Single-writer is enforced with an advisory lock on a *separate* lock file
/// (`<log>.lock`) that is never renamed, held for the whole lifetime of the
/// store. The data log itself is rename-swapped by compaction, so a lock on
/// the log inode would migrate off the path during the swap and let a racing
/// opener slip in; the lock file has no such window.
pub struct JsonlSessionStore {
    path: PathBuf,
    docs_dir: PathBuf,
    chunks_dir: PathBuf,
    /// Held for its side effect: the advisory lock lives as long as this handle.
    _lock_file: File,
    writer: Mutex<LogWriter>,
    index: Mutex<CasIndex>,
}

struct LogWriter {
    file: File,
    next_seq: u64,
}

/// Take the advisory exclusive lock that enforces single-writer (see
/// [`JsonlSessionStore`]). The returned handle must be held for the writer's
/// lifetime; the lock releases when it is dropped, including on crash.
fn acquire_exclusive_lock(lock_path: &Path) -> io::Result<File> {
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false) // the lock file carries no content; never truncate it
        .open(lock_path)?;
    match lock_file.try_lock() {
        Ok(()) => Ok(lock_file),
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

/// The doc-file directory beside the log (`<log>.docs/`).
fn docs_dir_for(path: &Path) -> PathBuf {
    path.with_extension("docs")
}

/// The chunk-file directory beside the log (`<log>.chunks/`).
fn chunks_dir_for(path: &Path) -> PathBuf {
    path.with_extension("chunks")
}

impl JsonlSessionStore {
    /// Open (creating if absent) the log at `path`, replaying existing records
    /// into the in-memory index. Malformed lines are skipped so a partially
    /// written tail never blocks startup.
    ///
    /// Takes an advisory exclusive lock on `<path>.lock` *before* reading the
    /// log, so only one process ever writes it — failing with
    /// [`io::ErrorKind::WouldBlock`] if another holds the lock.
    ///
    /// `now` drops records whose retention already lapsed instead of loading
    /// them. They would be evicted by the first `compact` anyway, so this only
    /// changes when the work happens — but it decides whether startup is
    /// proportional to the *live* set or to everything appended since the last
    /// compaction.
    pub fn open(path: impl AsRef<Path>, now: u64) -> io::Result<Self> {
        let path: PathBuf = path.as_ref().to_path_buf();
        let docs_dir = docs_dir_for(&path);
        let chunks_dir = chunks_dir_for(&path);

        // Single-writer lock first, before we read or write the log.
        let lock_file = acquire_exclusive_lock(&lock_path_for(&path))?;

        std::fs::create_dir_all(&docs_dir)?;
        std::fs::create_dir_all(&chunks_dir)?;

        let mut next_seq = 0u64;
        let mut index = CasIndex::default();
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
                let Ok(record) = serde_json::from_slice::<serde_json::Value>(trimmed) else {
                    continue; // malformed line; compaction will drop it
                };
                let Some(seq) = record.get("seq").and_then(serde_json::Value::as_u64) else {
                    continue;
                };
                let Some(seq_after) = seq.checked_add(1) else {
                    continue; // corrupt seq at u64::MAX; skip rather than overflow
                };
                next_seq = next_seq.max(seq_after);
                match record.get("type").and_then(serde_json::Value::as_str) {
                    Some(RECORD_TYPE_SESSION_V2) => {
                        let Ok(record) = serde_json::from_value::<SessionLogRecordV2>(record)
                        else {
                            continue;
                        };
                        if record.retention_until <= now {
                            continue;
                        }
                        index.insert(
                            record.fingerprint.clone(),
                            record.id.clone(),
                            CasEntry {
                                fingerprint: record.fingerprint,
                                upstream_name: record.upstream_name,
                                expires_at: record.expires_at,
                                retention_until: record.retention_until,
                                whole: record.whole,
                            },
                        );
                    }
                    Some(RECORD_TYPE_SESSION) => {
                        // Legacy full-bytes record: adopt the document as a
                        // `whole` doc file (no re-packing at replay) and index
                        // it. The id is recomputed from the exact persisted
                        // bytes, so a tampered payload simply resolves to a
                        // different id than any receipt cites.
                        let Ok(record) = serde_json::from_value::<SessionLogRecord>(record) else {
                            continue;
                        };
                        if record.retention_until <= now {
                            continue;
                        }
                        let Ok(bytes) = BASE64.decode(record.payload_b64.as_bytes()) else {
                            continue;
                        };
                        let Ok(session) = AttestedSession::from_bytes(bytes) else {
                            continue;
                        };
                        if !session.document().evidence.digest_matches_data() {
                            continue;
                        }
                        let id = session.session_id().to_string();
                        write_file_if_absent(&docs_dir.join(&id), session.bytes())?;
                        index.insert(
                            record.fingerprint.clone(),
                            id,
                            CasEntry {
                                fingerprint: record.fingerprint,
                                upstream_name: session.document().upstream_name.clone(),
                                expires_at: session.document().expires_at,
                                retention_until: record.retention_until,
                                whole: true,
                            },
                        );
                    }
                    _ => continue, // unknown record type; skip
                }
            }
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            docs_dir,
            chunks_dir,
            _lock_file: lock_file,
            writer: Mutex::new(LogWriter { file, next_seq }),
            index: Mutex::new(index),
        })
    }

    /// Read a chunk payload by digest.
    fn read_chunk(&self, digest: &str) -> Option<Vec<u8>> {
        // A digest is 64 lowercase hex; refuse anything else as a path.
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        std::fs::read(self.chunks_dir.join(digest)).ok()
    }

    /// Rebuild a session from its doc file + chunks, verifying every link of
    /// the hash chain: rebuilt bytes must re-hash to the requested id, and
    /// `evidence.data` must hash to its in-document `digest` (§8.2).
    fn materialize(&self, id: &str, entry: &CasEntry) -> Option<AttestedSession> {
        let skeleton = std::fs::read(self.docs_dir.join(id)).ok()?;
        let bytes = if entry.whole {
            skeleton
        } else {
            unpack(&skeleton, false, &mut |digest| self.read_chunk(digest))?
        };
        let session = AttestedSession::from_bytes(bytes).ok()?;
        if session.session_id() != id {
            return None; // rebuilt bytes do not hash to the cited id
        }
        if !session.document().evidence.digest_matches_data() {
            return None;
        }
        Some(session)
    }

    /// Rewrite the log from the retained (non-lapsed) index: drop lapsed
    /// records, collapse duplicates, and persist each record's current
    /// retention deadline (which the hot path extends in the index without
    /// appending). Then garbage-collect the doc and chunk files the retained
    /// set no longer references. Returns the number of records kept.
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

        let (live, evicted): (Vec<(String, String, CasEntry)>, Vec<String>) = {
            let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
            let evicted = index.evict_lapsed(now);
            let live = index
                .by_id
                .iter()
                .map(|(id, e)| (e.fingerprint.clone(), id.clone(), e.clone()))
                .collect();
            (live, evicted)
        };

        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = File::create(&tmp)?;
            for (seq, (fingerprint, id, entry)) in live.iter().enumerate() {
                let mut line = serde_json::to_string(&SessionLogRecordV2 {
                    seq: seq as u64,
                    ts: now,
                    record_type: RECORD_TYPE_SESSION_V2.to_string(),
                    fingerprint: fingerprint.clone(),
                    id: id.clone(),
                    upstream_name: entry.upstream_name.clone(),
                    expires_at: entry.expires_at,
                    retention_until: entry.retention_until,
                    whole: entry.whole,
                })
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
            }
            out.sync_all()?; // durable temp contents before it becomes the log
        }

        // Open the replacement append handle before the rename, so the only
        // fallible step left is the rename — the writer is never left pointing
        // at the stale inode.
        let new_file = OpenOptions::new().append(true).open(&tmp)?;
        std::fs::rename(&tmp, &self.path)?;

        w.file = new_file;
        w.next_seq = live.len() as u64;

        // Best-effort GC: doc files of evicted (or never-indexed) sessions,
        // then chunks no live skeleton still references. Failures only leak
        // disk until the next compaction; correctness never depends on them.
        self.gc_files(&live, &evicted);
        Ok(live.len())
    }

    /// Delete doc files that are not live and chunks no live doc references.
    fn gc_files(&self, live: &[(String, String, CasEntry)], evicted: &[String]) {
        let live_ids: HashSet<&str> = live.iter().map(|(_, id, _)| id.as_str()).collect();
        for id in evicted {
            let _ = std::fs::remove_file(self.docs_dir.join(id));
        }
        if let Ok(entries) = std::fs::read_dir(&self.docs_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if !live_ids.contains(name) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }

        // Reference collection is transitive: a skeleton references subtree
        // chunks ("s:<digest>") whose payloads may reference further chunks.
        // Walk to the fixpoint or the sweep would delete chunks a live
        // skeleton needs through one indirection.
        let mut referenced: HashSet<String> = HashSet::new();
        let mut subtree_worklist: Vec<String> = Vec::new();
        for (_, id, entry) in live {
            if entry.whole {
                continue; // whole docs reference no chunks
            }
            let Ok(skeleton) = std::fs::read(self.docs_dir.join(id)) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&skeleton) else {
                continue;
            };
            collect_chunk_refs(&value, &mut referenced, &mut subtree_worklist);
        }
        let mut expanded: HashSet<String> = HashSet::new();
        while let Some(digest) = subtree_worklist.pop() {
            if !expanded.insert(digest.clone()) {
                continue;
            }
            let Some(payload) = self.read_chunk(&digest) else {
                continue; // missing chunk: keep it referenced regardless
            };
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else {
                continue;
            };
            collect_chunk_refs(&value, &mut referenced, &mut subtree_worklist);
        }
        if let Ok(entries) = std::fs::read_dir(&self.chunks_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if !referenced.contains(name) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

/// Collect every chunk digest referenced by a packed skeleton (or by a
/// subtree-chunk payload, which uses the same marker vocabulary). Subtree
/// references (`s:<digest>`) are additionally pushed onto `subtrees` so the
/// caller can expand them transitively.
fn collect_chunk_refs(
    value: &serde_json::Value,
    out: &mut HashSet<String>,
    subtrees: &mut Vec<String>,
) {
    let mut note = |reference: &str| {
        if let Some(digest) = reference.get(2..) {
            out.insert(digest.to_string());
            if reference.starts_with("s:") {
                subtrees.push(digest.to_string());
            }
        }
    };
    match value {
        serde_json::Value::Object(map) => {
            if let Some(reference) = map.get("$r").and_then(serde_json::Value::as_str) {
                note(reference);
            }
            if map.contains_key("$db") {
                if let Some(reference) = map.get("r").and_then(serde_json::Value::as_str) {
                    note(reference);
                }
            }
            for v in map.values() {
                collect_chunk_refs(v, out, subtrees);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_chunk_refs(v, out, subtrees);
            }
        }
        _ => {}
    }
}

/// Write `bytes` to `path` unless the file already exists (doc and chunk
/// files are immutable and content-addressed, so an existing file is by
/// construction the same content).
fn write_file_if_absent(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => file.write_all(bytes),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
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

        // Chunks first, then the doc file, then the index record: a crash can
        // only leave orphans (swept by the next compaction), never a record
        // pointing at missing content.
        let id = session.session_id().to_string();
        let (doc_bytes, whole) = match pack(session.bytes()) {
            Packed::Whole(bytes) => (bytes, true),
            Packed::Cas { skeleton, chunks } => {
                for (digest, payload) in &chunks {
                    write_file_if_absent(&self.chunks_dir.join(digest), payload)?;
                }
                (skeleton, false)
            }
        };
        write_file_if_absent(&self.docs_dir.join(&id), &doc_bytes)?;

        let mut line = serde_json::to_string(&SessionLogRecordV2 {
            seq,
            ts: now,
            record_type: RECORD_TYPE_SESSION_V2.to_string(),
            fingerprint: fingerprint.to_string(),
            id: id.clone(),
            upstream_name: session.document().upstream_name.clone(),
            expires_at: session.document().expires_at,
            retention_until,
            whole,
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        // No flush: `File::flush` is a no-op and the log isn't fsync'd. If
        // `file` ever becomes a `BufWriter`, restore a flush or records can sit
        // unwritten on a crash.
        w.file.write_all(line.as_bytes())?;
        w.next_seq = next_seq;
        // Update the index under the writer lock so the log and index advance
        // together: `compact` rewrites the log *from* the index, so an index
        // that lagged a completed append could drop an on-disk record. Reads
        // still don't wait on the file write — only on this brief index update.
        let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
        index.insert(
            fingerprint.to_string(),
            id,
            CasEntry {
                fingerprint: fingerprint.to_string(),
                upstream_name: session.document().upstream_name.clone(),
                expires_at: session.document().expires_at,
                retention_until,
                whole,
            },
        );
        let evicted = index.evict_lapsed(now);
        drop(index);
        for id in evicted {
            let _ = std::fs::remove_file(self.docs_dir.join(id));
        }
        Ok(seq)
    }

    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        let entry = self
            .index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id, now)?;
        self.materialize(session_id, &entry)
    }

    fn current_session(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
    ) -> Option<AttestedSession> {
        let (id, entry) = self
            .index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .current(fingerprint, retention_until, now)?;
        self.materialize(&id, &entry)
    }

    fn list_sessions(&self, upstream_name: Option<&str>, now: u64) -> Vec<AttestedSession> {
        let metas = self
            .index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list(upstream_name, now);
        let mut out: Vec<AttestedSession> = metas
            .iter()
            .filter_map(|(id, entry)| self.materialize(id, entry))
            .collect();
        sort_sessions_newest_first(&mut out);
        out
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
            match JsonlSessionStore::open(path, 0) {
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

    /// Replay must be proportional to the live set, not to everything appended
    /// since the last compaction. A lapsed record is dropped before it is
    /// decoded, parsed and hashed — the work that made a busy log expensive to
    /// boot from — and never reaches the index.
    #[test]
    fn replay_drops_lapsed_records_before_loading_them() {
        let path = temp_path();
        {
            let store = open_store(&path);
            let live = session("live", 0, 9_000);
            let lapsed = session("lapsed", 0, 1_000);
            store.put_session("fp-live", live, 9_000, 0).unwrap();
            store.put_session("fp-lapsed", lapsed, 1_000, 0).unwrap();
        }
        assert_eq!(count_lines(&path), 2, "both records are on disk");

        // Reopened past the lapsed record's deadline but before the live one's.
        let reopened = JsonlSessionStore::open(&path, 5_000).unwrap();
        let index = reopened.index.lock().unwrap();
        assert_eq!(
            index.by_id.len(),
            1,
            "only the live record is resident after replay"
        );
        assert!(
            index.by_fingerprint.contains_key("fp-live"),
            "the live record survives"
        );
        assert!(
            !index.by_fingerprint.contains_key("fp-lapsed"),
            "the lapsed record was never inserted"
        );
    }

    /// The filter is a scheduling change, not a policy one: `now = 0` keeps
    /// everything, so a caller that does not care still replays the whole log.
    #[test]
    fn replay_keeps_everything_when_nothing_has_lapsed() {
        let path = temp_path();
        {
            let store = open_store(&path);
            store
                .put_session("fp-a", session("a", 0, 9_000), 9_000, 0)
                .unwrap();
            store
                .put_session("fp-b", session("b", 0, 1_000), 1_000, 0)
                .unwrap();
        }
        let reopened = JsonlSessionStore::open(&path, 0).unwrap();
        assert_eq!(reopened.index.lock().unwrap().by_id.len(), 2);
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

        let blocked = JsonlSessionStore::open(&path, 0);
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

    // ------------------------------------------------------------------
    // Field-CAS storage (real production documents)
    // ------------------------------------------------------------------

    fn fixture_session(name: &str) -> AttestedSession {
        let path = format!(
            "{}/tests/fixtures/sessions/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).expect("fixture readable");
        AttestedSession::from_bytes(bytes).expect("fixture is a sealed session")
    }

    fn count_dir_files(dir: &Path) -> usize {
        std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }

    /// A real 269KB production document persists as skeleton + shared chunks
    /// and serves back byte-exact — the session id a receipt cites still
    /// recomputes from the served bytes.
    #[test]
    fn real_document_persists_via_cas_and_serves_byte_exact() {
        let path = temp_path();
        let session = fixture_session("phala-direct-a.json");
        let id = session.session_id().to_string();
        let raw = session.bytes().to_vec();
        {
            let store = open_store(&path);
            store.put_session("fp", session, 9_000, 1_000).unwrap();

            // The doc file holds a small skeleton, not the 269KB document.
            let skeleton = std::fs::read(docs_dir_for(&path).join(&id)).unwrap();
            assert!(
                skeleton.len() < raw.len() / 4,
                "skeleton {} should be far below raw {}",
                skeleton.len(),
                raw.len()
            );
            assert!(
                count_dir_files(&chunks_dir_for(&path)) > 0,
                "chunks written"
            );

            let served = store.get_session(&id, 2_000).expect("serves by id");
            assert_eq!(served.bytes(), raw.as_slice(), "byte-exact after CAS");
        }
        // And after a replay (chunks + doc read back from disk only).
        let store = open_store(&path);
        let served = store.get_session(&id, 2_000).expect("serves after replay");
        assert_eq!(served.bytes(), raw.as_slice());
        drop(store);
        cleanup(&path);
        let _ = std::fs::remove_dir_all(docs_dir_for(&path));
        let _ = std::fs::remove_dir_all(chunks_dir_for(&path));
    }

    /// Two consecutive verification rounds of one upstream: the second put
    /// writes only the fresh chunks (quote / GPU evidence), reusing the rest.
    #[test]
    fn consecutive_rounds_share_chunks_on_disk() {
        let path = temp_path();
        let a = fixture_session("phala-direct-a.json");
        let b = fixture_session("phala-direct-b.json");
        let store = open_store(&path);
        store.put_session("fp-a", a, 9_000, 1_000).unwrap();
        let chunks_after_a = count_dir_files(&chunks_dir_for(&path));
        let size_after_a: u64 = std::fs::read_dir(chunks_dir_for(&path))
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        store.put_session("fp-b", b, 9_000, 1_100).unwrap();
        let new_bytes: u64 = std::fs::read_dir(chunks_dir_for(&path))
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum::<u64>()
            - size_after_a;
        assert!(
            new_bytes < 32 * 1024,
            "second round adds {new_bytes} bytes of chunks (measured ~9KB)"
        );
        assert!(count_dir_files(&chunks_dir_for(&path)) > chunks_after_a);
        drop(store);
        cleanup(&path);
        let _ = std::fs::remove_dir_all(docs_dir_for(&path));
        let _ = std::fs::remove_dir_all(chunks_dir_for(&path));
    }

    /// Compaction sweeps chunk files that no live skeleton still references.
    #[test]
    fn compact_garbage_collects_unreferenced_chunks() {
        let path = temp_path();
        let live = fixture_session("phala-direct-a.json");
        let gone = fixture_session("near-ai-a.json");
        let gone_id = gone.session_id().to_string();
        {
            let store = open_store(&path);
            store
                .put_session("fp-live", live.clone(), 9_000, 1_000)
                .unwrap();
            store.put_session("fp-gone", gone, 2_000, 1_000).unwrap();
            let chunks_before = count_dir_files(&chunks_dir_for(&path));
            assert!(chunks_before > 0);

            store.compact(5_000).unwrap(); // past fp-gone retention
            assert!(store.get_session(&gone_id, 5_000).is_none());
            assert!(!docs_dir_for(&path).join(&gone_id).exists());
            let chunks_after = count_dir_files(&chunks_dir_for(&path));
            assert!(
                chunks_after < chunks_before,
                "near-ai chunks swept: {chunks_before} -> {chunks_after}"
            );
            // The live session still serves byte-exact after the sweep.
            let served = store.get_session(live.session_id(), 5_000).unwrap();
            assert_eq!(served.bytes(), live.bytes());
        }
        let store = open_store(&path);
        let served = store.get_session(live.session_id(), 5_000).unwrap();
        assert_eq!(served.bytes(), live.bytes());
        drop(store);
        cleanup(&path);
        let _ = std::fs::remove_dir_all(docs_dir_for(&path));
        let _ = std::fs::remove_dir_all(chunks_dir_for(&path));
    }

    /// The Whole fallback: a document packing cannot reproduce is stored
    /// untouched and served byte-exact, no chunks involved.
    #[test]
    fn whole_fallback_roundtrips_without_chunks() {
        let path = temp_path();
        // Pretty-printed (non-JCS) bytes: pack() falls back to Whole.
        let raw = b"{\n  \"api_version\": \"aci/1\"\n}\n".to_vec();
        // Store through the pack/unpack path directly via a sealed session is
        // not possible here (seal produces JCS), so exercise the store-adjacent
        // helpers: pack -> Whole, unpack(whole=true) is the identity.
        match super::super::session_cas::pack(&raw) {
            super::super::session_cas::Packed::Whole(bytes) => {
                assert_eq!(
                    super::super::session_cas::unpack(&bytes, true, &mut |_| None).unwrap(),
                    raw
                );
            }
            super::super::session_cas::Packed::Cas { .. } => panic!("must be Whole"),
        }
        cleanup(&path);
    }

    /// A legacy (pre-CAS, full-bytes) record replays into a served session:
    /// the migration path for logs written before field-CAS storage.
    #[test]
    fn legacy_full_bytes_record_replays_and_serves() {
        let path = temp_path();
        let s = session("https://legacy.example.net", 1_000, 9_000);
        let id = s.session_id().to_string();
        let legacy = SessionLogRecord {
            seq: 0,
            ts: 1_000,
            record_type: RECORD_TYPE_SESSION.to_string(),
            fingerprint: "fp-legacy".to_string(),
            retention_until: 9_000,
            payload_b64: BASE64.encode(s.bytes()),
        };
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&legacy).unwrap()),
        )
        .unwrap();

        let store = open_store(&path);
        let served = store.get_session(&id, 2_000).expect("legacy record serves");
        assert_eq!(served.bytes(), s.bytes());
        drop(store);
        cleanup(&path);
        let _ = std::fs::remove_dir_all(docs_dir_for(&path));
        let _ = std::fs::remove_dir_all(chunks_dir_for(&path));
    }
}
