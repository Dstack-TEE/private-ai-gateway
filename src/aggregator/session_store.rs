//! Persistence for attested sessions.
//!
//! [`SessionStore`] is the registry behind the audit endpoints. The durable
//! implementation, [`JsonlSessionStore`], is an append-only log of one record
//! per line, replayed into an in-memory index on open:
//!
//! ```text
//! {"seq":0,"ts":1700000000,"type":"evidence","payload_b64":"…"}
//! {"seq":1,"ts":1700000000,"type":"attested_session","fingerprint":"…","retention_until":1700003600,"content_type":"application/json","payload_b64":"…"}
//! ```
//!
//! A session's evidence bundle is stored once, as an `evidence` record whose
//! `sha256:` digest is its key. Many Chutes instance sessions cite the same
//! fleet bundle, so storing it inline would repeat it once per session. An
//! `attested_session` record carries the document without `evidence.data`;
//! replay rebuilds the served bytes from the bundle the document's
//! `evidence.digest` names, and recomputes the session id from them. Every
//! stored session has a complete bundle (§8.2): digest plus the data it hashes.
//! Records of any other type, including the pre-bundle `session` type, are
//! skipped.
//!
//! Two deadlines govern a record:
//!
//! * the document's own `expires_at` — the validity period for new
//!   forwarding decisions and for the list endpoint;
//! * the store-side `retention_until` — how long the record keeps being
//!   served by id. Retention MUST outlive every receipt citing the session,
//!   so each citation pushes it forward without touching the sealed bytes.
//!
//! A bundle has no deadline of its own: it lives while a retained session
//! cites it.
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

use super::session::{AttestedSession, EvidenceRef, SessionDocument};
use crate::aci::digest;

/// Record type tag for an evidence bundle line.
const RECORD_TYPE_EVIDENCE: &str = "evidence";
/// Record type tag for a session line.
const RECORD_TYPE_SESSION: &str = "attested_session";

/// One line in the append-only session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionLogRecord {
    seq: u64,
    ts: u64,
    #[serde(rename = "type")]
    record_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retention_until: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    /// Evidence: the bundle bytes. Session: the document without `evidence.data`.
    payload_b64: String,
}

/// The session registry behind the audit endpoints.
pub trait SessionStore: Send + Sync {
    /// Fetch a session by its full `bare 64-hex` id. Served until its
    /// retention deadline — past `expires_at` too, because receipts citing it
    /// may still be live (§8 retention).
    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession>;

    /// The id of the channel's current session, with its retention extended to
    /// at least `retention_until`. Without one, atomically seal and store a new
    /// session, which must carry a complete evidence bundle.
    fn current_or_seal(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
        seal: &mut dyn FnMut() -> io::Result<AttestedSession>,
    ) -> io::Result<String>;
}

/// A session split for storage: its id, the document without `evidence.data`,
/// and the evidence bundle the document's digest names.
struct SplitSession {
    session_id: String,
    stripped: SessionDocument,
    digest: String,
    content_type: String,
    bundle: Vec<u8>,
}

impl SplitSession {
    fn new(session: AttestedSession) -> io::Result<Self> {
        let (content_type, bundle) = session.document().evidence.decode().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "attested session has no complete evidence bundle (§8.2)",
            )
        })?;
        let session_id = session.session_id().to_string();
        let mut stripped = session.document().clone();
        stripped.evidence.data_uri = None;
        let digest = stripped
            .evidence
            .digest
            .clone()
            .expect("decode requires a digest");
        Ok(Self {
            session_id,
            stripped,
            digest,
            content_type,
            bundle,
        })
    }
}

struct SessionEntry {
    stripped: SessionDocument,
    content_type: String,
    fingerprint: String,
    retention_until: u64,
}

impl SessionEntry {
    fn digest(&self) -> &str {
        self.stripped
            .evidence
            .digest
            .as_deref()
            .expect("stored sessions carry a digest")
    }
}

struct Bundle {
    bytes: Vec<u8>,
    /// Retained sessions citing this bundle; it is dropped at zero.
    citers: usize,
    /// Whether the log already holds this bundle's `evidence` record.
    persisted: bool,
}

/// In-memory session index shared by both stores: id→entry plus a
/// fingerprint→current-id map, a retention-deadline index so eviction costs
/// only what actually lapsed, and one copy of each evidence bundle.
#[derive(Default)]
struct SessionIndex {
    by_id: HashMap<String, SessionEntry>,
    by_fingerprint: HashMap<String, String>,
    by_retention: BTreeMap<u64, HashSet<String>>,
    bundles: HashMap<String, Bundle>,
}

impl SessionIndex {
    fn insert(&mut self, fingerprint: String, split: SplitSession, retention_until: u64) {
        let SplitSession {
            session_id: id,
            stripped,
            digest,
            content_type,
            bundle,
        } = split;
        let entry = SessionEntry {
            stripped,
            content_type,
            fingerprint: fingerprint.clone(),
            retention_until,
        };
        match self.by_id.insert(id.clone(), entry) {
            Some(prev) => self.drop_retention_hint(&id, prev.retention_until),
            None => {
                self.bundles
                    .entry(digest)
                    .or_insert(Bundle {
                        bytes: bundle,
                        citers: 0,
                        persisted: false,
                    })
                    .citers += 1;
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

    /// Pop every bucket whose retention deadline is at or before `now`, and
    /// drop bundles no retained session cites any more.
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
                let Some(entry) = self.by_id.remove(&id) else {
                    continue;
                };
                if self.by_fingerprint.get(&entry.fingerprint) == Some(&id) {
                    self.by_fingerprint.remove(&entry.fingerprint);
                }
                if let Some(bundle) = self.bundles.get_mut(entry.digest()) {
                    bundle.citers -= 1;
                    if bundle.citers == 0 {
                        self.bundles.remove(entry.digest());
                    }
                }
            }
        }
    }

    /// Rebuild the served session: the stored document plus its bundle.
    fn rebuild(&self, id: &str, entry: &SessionEntry) -> Option<AttestedSession> {
        let bundle = self.bundles.get(entry.digest())?;
        let mut document = entry.stripped.clone();
        document.evidence = EvidenceRef::from_bytes(&entry.content_type, &bundle.bytes);
        let session = AttestedSession::seal(document).ok()?;
        (session.session_id() == id).then_some(session)
    }

    fn get(&mut self, session_id: &str, now: u64) -> Option<AttestedSession> {
        self.evict_lapsed(now);
        let entry = self.by_id.get(session_id)?;
        self.rebuild(session_id, entry)
    }

    fn current(&mut self, fingerprint: &str, retention_until: u64, now: u64) -> Option<String> {
        self.evict_lapsed(now);
        let id = self.by_fingerprint.get(fingerprint)?.clone();
        let entry = self.by_id.get_mut(&id)?;
        if now >= entry.stripped.expires_at {
            return None; // validity lapsed; record stays for retention only
        }
        if retention_until > entry.retention_until {
            let old = entry.retention_until;
            entry.retention_until = retention_until;
            self.drop_retention_hint(&id, old);
            self.by_retention
                .entry(retention_until)
                .or_default()
                .insert(id.clone());
        }
        Some(id)
    }

    fn bundle_persisted(&self, digest: &str) -> bool {
        self.bundles.get(digest).is_some_and(|b| b.persisted)
    }

    fn mark_persisted(&mut self, digest: &str) {
        if let Some(bundle) = self.bundles.get_mut(digest) {
            bundle.persisted = true;
        }
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

fn evidence_line(seq: u64, ts: u64, bundle: &[u8]) -> io::Result<String> {
    record_line(&SessionLogRecord {
        seq,
        ts,
        record_type: RECORD_TYPE_EVIDENCE.to_string(),
        fingerprint: None,
        retention_until: None,
        content_type: None,
        payload_b64: BASE64.encode(bundle),
    })
}

fn session_line(
    seq: u64,
    ts: u64,
    fingerprint: &str,
    retention_until: u64,
    content_type: &str,
    stripped: &SessionDocument,
) -> io::Result<String> {
    let document =
        serde_json::to_vec(stripped).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    record_line(&SessionLogRecord {
        seq,
        ts,
        record_type: RECORD_TYPE_SESSION.to_string(),
        fingerprint: Some(fingerprint.to_string()),
        retention_until: Some(retention_until),
        content_type: Some(content_type.to_string()),
        payload_b64: BASE64.encode(document),
    })
}

fn record_line(record: &SessionLogRecord) -> io::Result<String> {
    let mut line =
        serde_json::to_string(record).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    Ok(line)
}

/// Rebuild a replayed session from its record and the bundles read so far.
/// `None` for anything that does not reproduce a complete, consistent
/// session: a missing bundle, an unparsable document, or a content type that
/// is not a single `data:` media type.
fn replay_session(
    record: &SessionLogRecord,
    bundles: &HashMap<String, Vec<u8>>,
) -> Option<SplitSession> {
    let content_type = record.content_type.as_deref()?;
    if content_type.contains(";base64") || content_type.contains(',') {
        return None;
    }
    let document = BASE64.decode(record.payload_b64.as_bytes()).ok()?;
    let mut document: SessionDocument = serde_json::from_slice(&document).ok()?;
    let digest = document.evidence.digest.clone()?;
    let bundle = bundles.get(&digest)?;
    document.evidence = EvidenceRef::from_bytes(content_type, bundle);
    let session = AttestedSession::seal(document).ok()?;
    SplitSession::new(session).ok()
}

/// Append-only JSONL-backed [`SessionStore`]. The append log and the in-memory
/// index sit behind separate locks. Creating a session holds the writer lock
/// throughout, so concurrent callers seal a channel's session once, while
/// reads never wait on the file.
///
/// The hot path appends only when a *new* session is sealed, plus its bundle's
/// `evidence` record the first time that bundle is written; a repeat request
/// extends the current session's retention in the index without writing (see
/// [`SessionStore::current_or_seal`]). [`JsonlSessionStore::compact`] then
/// rewrites the file from the live index, dropping lapsed records and
/// uncited bundles and persisting the extended retention deadlines.
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
    /// A failed append may have left a partial line; the next append starts
    /// with a newline so its records never merge into that line.
    needs_separator: bool,
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
    /// into the in-memory index. Malformed lines, records of unknown type, and
    /// sessions whose bundle is missing are skipped, so a partially written
    /// tail or an older log format never blocks startup.
    ///
    /// Takes an advisory exclusive lock on `<path>.lock` *before* reading the
    /// log, so only one process ever writes it — failing with
    /// [`io::ErrorKind::WouldBlock`] if another holds the lock.
    ///
    /// `now` drops records whose retention already lapsed instead of loading
    /// them, so startup is proportional to the *live* set rather than to
    /// everything appended since the last compaction. Taken as a parameter
    /// rather than read from the clock so the store stays deterministic, like
    /// `compact`.
    pub fn open(path: impl AsRef<Path>, now: u64) -> io::Result<Self> {
        let path: PathBuf = path.as_ref().to_path_buf();

        // Single-writer lock first, before we read or write the log.
        let lock_file = acquire_exclusive_lock(&lock_path_for(&path))?;

        let mut next_seq = 0u64;
        let mut index = SessionIndex::default();
        let mut bundles: HashMap<String, Vec<u8>> = HashMap::new();
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
                match record.record_type.as_str() {
                    // A bundle is keyed by its own hash, so a tampered payload
                    // simply answers to a digest no session names.
                    RECORD_TYPE_EVIDENCE => {
                        if let Ok(bytes) = BASE64.decode(record.payload_b64.as_bytes()) {
                            bundles.insert(digest::sha256_hex(&bytes), bytes);
                        }
                    }
                    RECORD_TYPE_SESSION => {
                        let (Some(fingerprint), Some(retention_until)) =
                            (record.fingerprint.as_ref(), record.retention_until)
                        else {
                            continue;
                        };
                        // Ahead of the rebuild: a lapsed record costs a decode, a
                        // parse and a digest hash before eviction would drop it.
                        if retention_until <= now {
                            continue;
                        }
                        // The id is recomputed from the rebuilt bytes, so a
                        // tampered document resolves to an id no receipt cites.
                        if let Some(split) = replay_session(&record, &bundles) {
                            index.insert(fingerprint.clone(), split, retention_until);
                        }
                    }
                    _ => continue,
                }
            }
        }
        // Every bundle that reached the index came from an `evidence` record.
        for bundle in index.bundles.values_mut() {
            bundle.persisted = true;
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            _lock_file: lock_file,
            writer: Mutex::new(LogWriter {
                file,
                next_seq,
                needs_separator: false,
            }),
            index: Mutex::new(index),
        })
    }

    /// Rewrite the log from the retained (non-lapsed) index: drop lapsed
    /// records and uncited bundles, collapse duplicates, and persist each
    /// record's current retention deadline (which the hot path extends in the
    /// index without appending). Bundles are written before the sessions that
    /// cite them, and sessions oldest first, so replay's last-insert-wins
    /// fingerprint map resolves to the newest session. Returns the number of
    /// session records kept.
    ///
    /// Records are written and synced to a temp file before an atomic rename.
    /// The replacement append handle is opened before the rename, so a
    /// successful swap never leaves the writer pointing at the old, unlinked
    /// file.
    pub fn compact(&self, now: u64) -> io::Result<usize> {
        // Hold the writer across the whole rewrite so no append races the swap.
        // Lock order is writer → index, matching `current_or_seal`, so the two paths
        // can never deadlock against each other.
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());

        // Serialize under the index lock, then write without it, so reads
        // never wait on the file.
        let (lines, kept) = {
            let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
            index.evict_lapsed(now);
            let mut sessions: Vec<(&String, &SessionEntry)> = index.by_id.iter().collect();
            sessions.sort_by(|(a_id, a), (b_id, b)| {
                a.stripped
                    .established_at
                    .cmp(&b.stripped.established_at)
                    .then_with(|| a_id.cmp(b_id))
            });
            let mut lines = Vec::with_capacity(index.bundles.len() + sessions.len());
            for bundle in index.bundles.values() {
                lines.push(evidence_line(lines.len() as u64, now, &bundle.bytes)?);
            }
            for (_, entry) in &sessions {
                lines.push(session_line(
                    lines.len() as u64,
                    now,
                    &entry.fingerprint,
                    entry.retention_until,
                    &entry.content_type,
                    &entry.stripped,
                )?);
            }
            (lines, sessions.len())
        };

        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = File::create(&tmp)?;
            for line in &lines {
                out.write_all(line.as_bytes())?;
            }
            out.sync_all()?; // durable temp contents before it becomes the log
        }

        // Open the replacement append handle before the rename, so the only
        // fallible step left is the rename — the writer is never left pointing at
        // the stale inode.
        let new_file = OpenOptions::new().append(true).open(&tmp)?;
        std::fs::rename(&tmp, &self.path)?;

        // Every retained bundle is now in the log. A bundle added after the
        // snapshot was appended by `current_or_seal`, which needs the writer lock
        // held here, so none exists.
        for bundle in self
            .index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .bundles
            .values_mut()
        {
            bundle.persisted = true;
        }
        w.file = new_file;
        w.next_seq = lines.len() as u64;
        w.needs_separator = false;
        Ok(kept)
    }
}

impl JsonlSessionStore {
    /// Append `split` (and its bundle, if the log lacks it) in one write, then
    /// index it. The caller holds the writer lock; the index lock is taken only
    /// around the file write, so reads never wait on it.
    fn append(
        &self,
        w: &mut LogWriter,
        fingerprint: &str,
        split: SplitSession,
        retention_until: u64,
        now: u64,
    ) -> io::Result<u64> {
        let bundle_persisted = self
            .index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .bundle_persisted(&split.digest);

        // Refuse to write a record we cannot assign a successor to, rather than
        // overflow. Only reachable from a corrupt replayed `seq` near u64::MAX;
        // the gateway's startup compaction renumbers from zero before serving.
        let overflow = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session log sequence number overflowed u64::MAX",
            )
        };
        let mut seq = w.next_seq;
        let mut out = String::new();
        if w.needs_separator {
            out.push('\n');
        }
        // The bundle goes first, so a session record never precedes the bundle
        // it cites; both land in one write.
        if !bundle_persisted {
            out.push_str(&evidence_line(seq, now, &split.bundle)?);
            seq = seq.checked_add(1).ok_or_else(overflow)?;
        }
        let session_seq = seq;
        out.push_str(&session_line(
            seq,
            now,
            fingerprint,
            retention_until,
            &split.content_type,
            &split.stripped,
        )?);
        let next_seq = seq.checked_add(1).ok_or_else(overflow)?;
        // No flush: `File::flush` is a no-op and the log isn't fsync'd. If
        // `file` ever becomes a `BufWriter`, restore a flush or records can sit
        // unwritten on a crash.
        if let Err(err) = w.file.write_all(out.as_bytes()) {
            w.needs_separator = true;
            return Err(err);
        }
        w.next_seq = next_seq;
        w.needs_separator = false;
        // Update the index under the writer lock so the log and index advance
        // together: `compact` rewrites the log *from* the index, so an index that
        // lagged a completed append could drop an on-disk record.
        let digest = split.digest.clone();
        let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
        index.insert(fingerprint.to_string(), split, retention_until);
        index.mark_persisted(&digest);
        index.evict_lapsed(now);
        Ok(session_seq)
    }

    /// Store `session` unconditionally, returning its log sequence number.
    #[cfg(test)]
    fn put_session(
        &self,
        fingerprint: &str,
        session: AttestedSession,
        retention_until: u64,
        now: u64,
    ) -> io::Result<u64> {
        let split = SplitSession::new(session)?;
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        self.append(&mut w, fingerprint, split, retention_until, now)
    }
}

impl SessionStore for JsonlSessionStore {
    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id, now)
    }

    fn current_or_seal(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
        seal: &mut dyn FnMut() -> io::Result<AttestedSession>,
    ) -> io::Result<String> {
        let current = |store: &Self| {
            store
                .index
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .current(fingerprint, retention_until, now)
        };
        if let Some(id) = current(self) {
            return Ok(id);
        }
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        // Another caller may have sealed it while this one waited for the writer.
        if let Some(id) = current(self) {
            return Ok(id);
        }
        let split = SplitSession::new(seal()?)?;
        let id = split.session_id.clone();
        self.append(&mut w, fingerprint, split, retention_until, now)?;
        Ok(id)
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
    fn get_session(&self, session_id: &str, now: u64) -> Option<AttestedSession> {
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id, now)
    }

    fn current_or_seal(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
        seal: &mut dyn FnMut() -> io::Result<AttestedSession>,
    ) -> io::Result<String> {
        let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(id) = index.current(fingerprint, retention_until, now) {
            return Ok(id);
        }
        let split = SplitSession::new(seal()?)?;
        let id = split.session_id.clone();
        index.insert(fingerprint.to_string(), split, retention_until);
        index.evict_lapsed(now);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::session::SessionClaims;
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
        open_store_at(path, 0)
    }

    fn open_store_at(path: &Path, now: u64) -> JsonlSessionStore {
        for _ in 0..200 {
            match JsonlSessionStore::open(path, now) {
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

    /// The bundle every test session cites, as Chutes instances share one.
    const FLEET_BUNDLE: &[u8] = br#"{"instances":["a","b"]}"#;

    fn session(endpoint: &str, established_at: u64, expires_at: u64) -> AttestedSession {
        session_with(
            endpoint,
            established_at,
            expires_at,
            EvidenceRef::from_bytes("application/json", FLEET_BUNDLE),
        )
    }

    fn session_with(
        endpoint: &str,
        established_at: u64,
        expires_at: u64,
        evidence: EvidenceRef,
    ) -> AttestedSession {
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
            evidence,
        })
        .unwrap()
    }

    /// The current session's id, or an error instead of sealing a new one.
    fn reuse_only(
        store: &dyn SessionStore,
        fp: &str,
        retention: u64,
        now: u64,
    ) -> io::Result<String> {
        store.current_or_seal(fp, retention, now, &mut || {
            Err(io::Error::other("cache miss"))
        })
    }

    fn seal_into(
        store: &dyn SessionStore,
        fp: &str,
        session: AttestedSession,
        retention: u64,
        now: u64,
    ) -> io::Result<String> {
        store.current_or_seal(fp, retention, now, &mut || Ok(session.clone()))
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
        assert_eq!(count_lines(&path), 3, "one shared bundle and both sessions");

        // Reopened past the lapsed record's deadline but before the live one's.
        let reopened = open_store_at(&path, 5_000);
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
        let reopened = open_store(&path);
        assert_eq!(reopened.index.lock().unwrap().by_id.len(), 2);
    }

    #[test]
    fn current_or_seal_extends_retention_without_a_log_append() {
        let path = temp_path();
        let store = open_store(&path);
        let s = session("https://x", 1_000, 2_000);
        let id = s.session_id().to_string();
        store.put_session("fp-x", s, 2_500, 1_000).unwrap();

        // A repeat request finds the current session and extends retention
        // without appending.
        let before = std::fs::metadata(&path).unwrap().len();
        assert!(reuse_only(&store, "fp-x", 9_000, 1_500).is_ok());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);

        // Past the validity period, the channel needs a fresh session...
        assert!(reuse_only(&store, "fp-x", 12_000, 3_000).is_err());
        // ...but the record keeps serving by id until its retention deadline.
        assert!(store.get_session(&id, 5_000).is_some());
        assert!(store.get_session(&id, 9_000).is_none());

        cleanup(&path);
    }

    #[test]
    fn expired_validity_drops_out_of_current_but_not_lookup() {
        let store = InMemorySessionStore::default();
        let s = session("https://x", 1_000, 2_000);
        let id = s.session_id().to_string();
        seal_into(&store, "fp-x", s, 9_000, 1_000).unwrap();

        assert!(reuse_only(&store, "fp-x", 9_000, 1_500).is_ok());
        // Validity lapsed: no longer current, still resolvable by id for
        // receipts that cite it.
        assert!(reuse_only(&store, "fp-x", 9_000, 2_000).is_err());
        assert!(store.get_session(&id, 2_000).is_some());
    }

    #[test]
    fn lapsed_retention_evicts_so_the_store_stays_bounded() {
        let store = InMemorySessionStore::default();
        let a = session("https://a", 1_000, 2_000);
        let a_id = a.session_id().to_string();
        seal_into(&store, "fp-a", a, 2_000, 1_000).unwrap();
        // A later write past A's retention deadline evicts it.
        let b = session("https://b", 5_000, 10_000);
        seal_into(&store, "fp-b", b.clone(), 10_000, 5_000).unwrap();

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
            // `a` also writes the shared bundle, at seq 0.
            assert_eq!((seq_a, seq_b), (1, 2));
        }

        let store = open_store(&path);
        let replayed = store.get_session(a.session_id(), 2_000).unwrap();
        assert_eq!(replayed.bytes(), a.bytes(), "served bytes survive replay");
        assert_eq!(store.get_session(b.session_id(), 2_000), Some(b));
        // The fingerprint index survives replay too.
        assert_eq!(
            reuse_only(&store, "fp-a", 5_000, 2_000).unwrap(),
            a.session_id()
        );
        let next = session("https://node-7.example.net", 1_002, 5_000);
        assert_eq!(store.put_session("fp-c", next, 5_000, 1_002).unwrap(), 3);

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
            assert_eq!(
                count_lines(&path),
                5,
                "the bundle plus four session records"
            );

            let kept = store.compact(now).unwrap();
            assert_eq!(kept, 1, "only the retained record is kept");
            assert_eq!(count_lines(&path), 2, "the retained record and its bundle");
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
        assert_eq!(store.index.lock().unwrap().by_id.len(), 1);

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
    fn a_session_without_a_complete_bundle_is_refused() {
        let swapped = EvidenceRef {
            digest: Some(digest::sha256_hex(b"abc")),
            data_uri: Some("data:text/plain;base64,eHl6".to_string()), // "xyz"
        };
        for evidence in [EvidenceRef::default(), swapped] {
            let s = session_with("https://x", 1_000, 9_000, evidence);
            let err =
                seal_into(&InMemorySessionStore::default(), "fp", s, 9_000, 1_000).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn a_shared_bundle_is_stored_once_and_dropped_with_its_last_citer() {
        let path = temp_path();
        let store = open_store(&path);
        for (i, endpoint) in ["https://a", "https://b", "https://c"].iter().enumerate() {
            let s = session(endpoint, 1_000, 2_000);
            store
                .put_session(&format!("fp-{i}"), s, 2_000 + i as u64, 1_000)
                .unwrap();
        }
        assert_eq!(count_lines(&path), 4, "one bundle and three sessions");
        assert_eq!(store.index.lock().unwrap().bundles.len(), 1);

        store.compact(5_000).unwrap();
        assert!(store.index.lock().unwrap().bundles.is_empty());
        assert_eq!(count_lines(&path), 0);

        drop(store);
        cleanup(&path);
    }

    #[test]
    fn older_record_types_are_skipped_on_replay() {
        let path = temp_path();
        let old = session("https://x", 1_000, 9_000);
        let line = serde_json::json!({
            "seq": 0, "ts": 1_000, "type": "session", "fingerprint": "fp",
            "retention_until": 9_000, "payload_b64": BASE64.encode(old.bytes()),
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let store = open_store(&path);
        assert!(store.get_session(old.session_id(), 2_000).is_none());
        assert!(reuse_only(&store, "fp", 9_000, 2_000).is_err());
        let next = session("https://y", 2_000, 9_000);
        assert_eq!(store.put_session("fp", next, 9_000, 2_000).unwrap(), 2);

        drop(store);
        cleanup(&path);
    }

    #[test]
    fn a_session_citing_a_tampered_bundle_is_skipped_on_replay() {
        let path = temp_path();
        let s = session("https://x", 1_000, 9_000);
        open_store(&path)
            .put_session("fp", s.clone(), 9_000, 1_000)
            .unwrap();
        let log = std::fs::read_to_string(&path).unwrap();
        let tampered = log.replace(&BASE64.encode(FLEET_BUNDLE), &BASE64.encode(b"forged"));
        assert_ne!(log, tampered);
        std::fs::write(&path, tampered).unwrap();

        let store = open_store(&path);
        assert!(store.get_session(s.session_id(), 2_000).is_none());

        drop(store);
        cleanup(&path);
    }

    #[test]
    fn compaction_keeps_the_newest_session_current_across_restart() {
        let path = temp_path();
        let old = session("https://x", 1_000, 2_000);
        let new = session("https://x", 2_500, 9_000);
        {
            let store = open_store(&path);
            store.put_session("fp", old, 9_000, 1_000).unwrap();
            store.put_session("fp", new.clone(), 9_000, 2_500).unwrap();
            store.compact(3_000).unwrap();
        }
        let store = open_store(&path);
        assert_eq!(
            reuse_only(&store, "fp", 9_000, 3_000).unwrap(),
            new.session_id()
        );

        drop(store);
        cleanup(&path);
    }

    fn concurrently_seal(store: &dyn SessionStore) -> String {
        let barrier = std::sync::Barrier::new(2);
        let seals = AtomicU64::new(0);
        let ids = std::thread::scope(|scope| {
            let tasks: Vec<_> = [1_000, 1_001]
                .into_iter()
                .map(|now| {
                    let barrier = &barrier;
                    let seals = &seals;
                    scope.spawn(move || {
                        barrier.wait();
                        store
                            .current_or_seal("shared", 9_000, now, &mut || {
                                seals.fetch_add(1, Ordering::SeqCst);
                                Ok(session("https://shared", now, 5_000))
                            })
                            .unwrap()
                    })
                })
                .collect();
            tasks
                .into_iter()
                .map(|task| task.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(seals.load(Ordering::SeqCst), 1);
        assert_eq!(ids[0], ids[1]);
        ids[0].clone()
    }

    #[test]
    fn concurrent_current_or_seal_is_atomic_in_memory() {
        let store = InMemorySessionStore::default();
        let id = concurrently_seal(&store);
        assert!(store.get_session(&id, 1_002).is_some());
    }

    #[test]
    fn concurrent_current_or_seal_writes_one_replayable_jsonl_record() {
        let path = temp_path();
        let store = open_store(&path);
        let id = concurrently_seal(&store);
        assert_eq!(count_lines(&path), 2, "the bundle and one session");
        drop(store);
        let replayed = open_store(&path);
        let current = replayed
            .current_or_seal("shared", 9_000, 1_002, &mut || {
                panic!("replayed session must be reused")
            })
            .unwrap();
        assert_eq!(current, id);
        assert_eq!(count_lines(&path), 2);
        drop(replayed);
        cleanup(&path);
    }

    #[test]
    fn put_session_errors_instead_of_overflowing_seq() {
        // A replayed `seq` of u64::MAX - 1 leaves `next_seq` at u64::MAX; the
        // next append must return an error rather than overflow `seq + 1`.
        let path = temp_path();
        let seeded = evidence_line(u64::MAX - 1, 1_000, FLEET_BUNDLE).unwrap();
        std::fs::write(&path, seeded).unwrap();

        let store = open_store(&path);
        let next = session("https://node-9.example.net", 2_000, 9_000);
        let err = store.put_session("fp2", next, 9_000, 2_000).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        drop(store);
        cleanup(&path);
    }
}
