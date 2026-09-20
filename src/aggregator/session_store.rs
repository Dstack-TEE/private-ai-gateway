//! Persistence for attested sessions.
//!
//! [`SessionStore`] is the registry behind the audit endpoints. The durable
//! implementation, [`JsonlSessionStore`], is an append-only log replayed into
//! an in-memory index on open, with two record types:
//!
//! ```text
//! {"seq":0,"ts":…,"type":"evidence","digest":"sha256:…","retention_until":…,"payload_b64":"…"}
//! {"seq":1,"ts":…,"type":"session","fingerprint":"…","retention_until":…,"payload_b64":"…","evidence_data_prefix":"data:…;base64,"}
//! ```
//!
//! Session `payload_b64` carries the sealed document bytes (JCS form, whose
//! hash is the session id), so replay reproduces identical records; the id is
//! always recomputed from those bytes, never trusted from disk. A document
//! whose §8.2 evidence is a complete, canonical bundle is stored *stripped* —
//! `evidence.data` removed — with the bundle bytes in a shared `evidence`
//! record keyed by digest: a fleet-wide Chutes bundle is byte-identical for
//! every per-instance session of a round, so embedding it per record
//! multiplied the log by the fleet size (measured 93-99% duplicate bytes).
//! `evidence_data_prefix` (the data-URI head through `;base64,`) plus the
//! shared bytes rebuild the exact served document on replay; anything that
//! cannot be rebuilt byte-identically is stored whole, as older builds did.
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

use crate::aci::digest;

use super::session::AttestedSession;

/// Record type tag for a session line.
const RECORD_TYPE_SESSION: &str = "session";

/// Record type tag for a shared evidence line: the decoded §8.2 bundle bytes,
/// stored once per digest. A fleet-wide Chutes bundle is identical for every
/// per-instance session sealed in the same round, so each session record
/// references the digest instead of embedding the bundle (measured: 93-99% of
/// evidence bytes in the log were duplicate copies of the same bundle).
const RECORD_TYPE_EVIDENCE: &str = "evidence";

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
    /// Set when the document's §8.2 evidence data was externalized into a
    /// shared `evidence` record: the data-URI prefix (everything through
    /// `;base64,`) which, prepended to the base64 of the shared bytes,
    /// rebuilds the exact served document bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_data_prefix: Option<String>,
}

/// The minimal envelope every log record carries, used to dispatch on the
/// record type before parsing the full struct.
#[derive(Debug, Deserialize)]
struct RecordEnvelope<'a> {
    seq: u64,
    #[serde(rename = "type", borrow)]
    record_type: &'a str,
    retention_until: u64,
}

/// One shared-evidence line in the log. `payload_b64` carries the RAW bundle
/// bytes (a single base64 layer, unlike session payloads, whose document
/// embeds the bytes a second time inside the JCS form).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvidenceLogRecord {
    seq: u64,
    ts: u64,
    #[serde(rename = "type")]
    record_type: String,
    digest: String,
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
    fingerprint: String,
    retention_until: u64,
    /// Insertion order within this index. Several retained records can share
    /// a fingerprint (an evidence upgrade reseals a channel under the same
    /// fingerprint while the superseded record stays resolvable by id); the
    /// *latest* inserted one is the channel's current session, so `compact`
    /// writes records in this order and replay — inserting in file order —
    /// rebuilds the same fingerprint mapping.
    seq: u64,
}

/// A shared evidence bundle, stored once per digest and referenced by every
/// session whose document carries that digest. `retention_until` tracks the
/// longest-lived citing session (bumped on seal and on every retention
/// extension), so the bundle outlives every record that resolves through it.
struct EvidenceEntry {
    bytes: Vec<u8>,
    retention_until: u64,
    /// The retention deadline actually persisted in the log. Invariant: an
    /// entry in this map means the current log holds an evidence line with
    /// this deadline. A cite carrying a later deadline must append a renewed
    /// evidence line — otherwise a restart between the two deadlines skips
    /// the stale line as lapsed and drops the live citing session with it.
    written_retention_until: u64,
}

/// In-memory session index shared by both stores: id→entry plus a
/// fingerprint→current-id map and a retention-deadline index so eviction
/// costs only what actually lapsed.
#[derive(Default)]
struct SessionIndex {
    by_id: HashMap<String, SessionEntry>,
    by_fingerprint: HashMap<String, String>,
    by_retention: BTreeMap<u64, HashSet<String>>,
    /// Shared evidence bundles by digest. Only the durable store populates
    /// this; the in-memory store never externalizes evidence.
    evidence: HashMap<String, EvidenceEntry>,
    /// Monotonic insertion counter behind every entry's `seq`. It never
    /// wraps in practice; saturating keeps the order total even if it did.
    next_seq: u64,
}

impl SessionIndex {
    /// Add or refresh a shared evidence bundle. Retention only ever moves
    /// forward: the bundle must outlive every session citing it.
    /// `written_retention_until` records the deadline persisted in the log
    /// (pass 0 from callers that only bump the in-memory deadline).
    fn insert_evidence(
        &mut self,
        digest: String,
        bytes: Vec<u8>,
        retention_until: u64,
        written_retention_until: u64,
    ) {
        self.evidence
            .entry(digest)
            .and_modify(|e| {
                e.retention_until = e.retention_until.max(retention_until);
                e.written_retention_until = e.written_retention_until.max(written_retention_until);
            })
            .or_insert(EvidenceEntry {
                bytes,
                retention_until,
                written_retention_until,
            });
    }

    fn insert(&mut self, fingerprint: String, session: AttestedSession, retention_until: u64) {
        let id = session.session_id().to_string();
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        if let Some(prev) = self.by_id.insert(
            id.clone(),
            SessionEntry {
                session,
                fingerprint: fingerprint.clone(),
                retention_until,
                seq,
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
        // Last insert wins, so with several retained records under one
        // fingerprint the latest-sealed session is the current one.
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
        // Evidence retention tracks the longest-lived citing session, so a
        // bundle lapses no earlier than its last citer.
        self.evidence.retain(|_, e| e.retention_until > now);
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
        let old_retention = entry.retention_until;
        let extends = retention_until > old_retention;
        if extends {
            entry.retention_until = retention_until;
        }
        let session = entry.session.clone();
        let final_retention = entry.retention_until;
        if extends {
            self.drop_retention_hint(&id, old_retention);
            self.by_retention
                .entry(retention_until)
                .or_default()
                .insert(id);
        }
        // Keep the cited evidence bundle alive as long as the session.
        if let Some(digest) = session.document().evidence.digest.as_deref() {
            if let Some(ev) = self.evidence.get_mut(digest) {
                ev.retention_until = ev.retention_until.max(final_retention);
            }
        }
        Some(session)
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

/// Refuse to write a record we cannot assign a successor to, rather than
/// overflow. Only reachable from a corrupt replayed `seq` near u64::MAX;
/// the gateway's startup compaction renumbers from zero before serving.
fn next_seq_after(seq: u64) -> io::Result<u64> {
    seq.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "session log sequence number overflowed u64::MAX",
        )
    })
}

/// The payload a session line actually stores.
enum StoredSession {
    /// The full sealed document bytes — the only form older builds write,
    /// and the fallback whenever the evidence cannot be externalized.
    Full,
    /// The document with `evidence.data` stripped: `data_prefix` plus the
    /// shared evidence record for `digest` rebuild the exact served bytes.
    Stripped {
        payload: Vec<u8>,
        data_prefix: String,
        digest: String,
        evidence_bytes: Vec<u8>,
    },
}

/// Split a complete, canonical §8.2 evidence bundle out of the document for
/// shared storage. Anything we cannot rebuild byte-identically stays inline
/// — missing digest or data, a non-`base64` data URI, non-canonical base64
/// (re-encoding must reproduce the original payload exactly, or the rebuilt
/// document would hash to a different session id), or bytes that do not hash
/// to the digest (the replay path's §8.2 check owns rejecting those).
fn externalize_evidence(session: &AttestedSession) -> Result<StoredSession, serde_json::Error> {
    let evidence = &session.document().evidence;
    let (Some(digest), Some(data_uri)) = (&evidence.digest, &evidence.data_uri) else {
        return Ok(StoredSession::Full);
    };
    let Some((head, b64)) = data_uri.split_once(";base64,") else {
        return Ok(StoredSession::Full);
    };
    let Ok(bytes) = BASE64.decode(b64.as_bytes()) else {
        return Ok(StoredSession::Full);
    };
    if BASE64.encode(&bytes) != *b64 || digest::sha256_hex(&bytes) != *digest {
        return Ok(StoredSession::Full);
    }
    let mut document = session.document().clone();
    document.evidence.data_uri = None;
    let payload =
        digest::jcs_bytes(&serde_json::to_value(&document)?).map_err(serde::ser::Error::custom)?;
    Ok(StoredSession::Stripped {
        payload,
        data_prefix: format!("{head};base64,"),
        digest: digest.clone(),
        evidence_bytes: bytes,
    })
}

/// Rebuild the exact sealed document bytes of an externalized session record:
/// parse the stripped payload, re-attach the shared evidence bytes under the
/// stored data-URI prefix, and re-canonicalize. The lookup is keyed by the
/// document's own digest and evidence bytes are hash-checked when their
/// record loads, so a rebuilt document is consistent by construction; `None`
/// means a malformed record or an unknown digest, and the caller skips it
/// (fail-closed, like a tampered payload).
fn rebuild_session_bytes(
    stripped: &[u8],
    data_prefix: &str,
    evidence: &HashMap<String, EvidenceEntry>,
) -> Option<Vec<u8>> {
    let mut value: serde_json::Value = serde_json::from_slice(stripped).ok()?;
    let digest = value.get("evidence")?.get("digest")?.as_str()?.to_string();
    let bytes = &evidence.get(&digest)?.bytes;
    value.get_mut("evidence")?.as_object_mut()?.insert(
        "data".to_string(),
        serde_json::Value::String(format!("{data_prefix}{}", BASE64.encode(bytes))),
    );
    digest::jcs_bytes(&value).ok()
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
    ///
    /// `now` drops records whose retention already lapsed instead of loading
    /// them. They would be evicted by the first `compact` anyway, so this only
    /// changes when the work happens — but it decides whether startup is
    /// proportional to the *live* set or to everything appended since the last
    /// compaction, which for a busy log is the difference between booting and
    /// exhausting memory before serving a request. Taken as a parameter rather
    /// than read from the clock so the store stays deterministic, like
    /// `compact`.
    pub fn open(path: impl AsRef<Path>, now: u64) -> io::Result<Self> {
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
                // Dispatch on the type tag first: session and evidence records
                // carry different fields.
                let Ok(envelope) = serde_json::from_slice::<RecordEnvelope>(trimmed) else {
                    continue; // malformed line; compaction will drop it
                };
                let Some(seq_after) = envelope.seq.checked_add(1) else {
                    continue; // corrupt seq at u64::MAX; skip rather than overflow
                };
                next_seq = next_seq.max(seq_after);
                // Ahead of the decode: a lapsed record costs a base64 decode, a
                // parse and a digest hash before eviction would drop it.
                if envelope.retention_until <= now {
                    continue;
                }
                match envelope.record_type {
                    RECORD_TYPE_EVIDENCE => {
                        let Ok(record) = serde_json::from_slice::<EvidenceLogRecord>(trimmed)
                        else {
                            continue;
                        };
                        let Ok(bytes) = BASE64.decode(record.payload_b64.as_bytes()) else {
                            continue;
                        };
                        // The §8.2 hash check happens once per bundle here, so
                        // every session citing this digest can trust it.
                        if digest::sha256_hex(&bytes) != record.digest {
                            tracing::warn!(
                                digest = %record.digest,
                                "session log evidence record fails its digest check; skipping"
                            );
                            continue;
                        }
                        index.insert_evidence(
                            record.digest,
                            bytes,
                            record.retention_until,
                            record.retention_until,
                        );
                    }
                    RECORD_TYPE_SESSION => {
                        let Ok(record) = serde_json::from_slice::<SessionLogRecord>(trimmed) else {
                            continue;
                        };
                        let Ok(payload) = BASE64.decode(record.payload_b64.as_bytes()) else {
                            continue;
                        };
                        let bytes = match &record.evidence_data_prefix {
                            Some(prefix) => {
                                let Some(rebuilt) =
                                    rebuild_session_bytes(&payload, prefix, &index.evidence)
                                else {
                                    tracing::warn!(
                                        fingerprint = %record.fingerprint,
                                        "session record cites evidence the log does not carry; skipping"
                                    );
                                    continue;
                                };
                                rebuilt
                            }
                            None => payload,
                        };
                        // The id is recomputed from the exact persisted bytes
                        // (`AttestedSession::from_bytes`), so a tampered payload simply
                        // resolves to a different id than any receipt cites. The
                        // evidence `data`, however, must still hash to its in-document
                        // `digest` (§8.2) — refuse to serve a swapped payload.
                        // Externalized records skip this per-record check: their
                        // evidence was hash-checked once when the evidence record
                        // loaded, and the lookup was keyed by the document's own
                        // digest, so the rebuilt document is consistent.
                        let Ok(session) = AttestedSession::from_bytes(bytes) else {
                            continue;
                        };
                        if record.evidence_data_prefix.is_none()
                            && !session.document().evidence.digest_matches_data()
                        {
                            continue;
                        }
                        // Restore the evidence deadline this record implies: a
                        // cite with a later retention must keep its bundle in
                        // the index, or the next compact would strip the
                        // session while the bundle is gone.
                        if record.evidence_data_prefix.is_some() {
                            if let Some(digest) = session.document().evidence.digest.as_deref() {
                                if let Some(ev) = index.evidence.get_mut(digest) {
                                    ev.retention_until =
                                        ev.retention_until.max(record.retention_until);
                                }
                            }
                        }
                        index.insert(record.fingerprint, session, record.retention_until);
                    }
                    _ => continue,
                }
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
    /// Records are written in index insertion order, so when several retained
    /// records share a fingerprint (an evidence upgrade reseals a channel while
    /// the superseded record stays resolvable by id), replay's last-insert-wins
    /// mapping resolves the fingerprint to the same — latest — session as the
    /// live index did.
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

        let (mut live, live_evidence) = {
            let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
            index.evict_lapsed(now);
            let sessions: Vec<(String, AttestedSession, u64, u64)> = index
                .by_id
                .values()
                .map(|e| {
                    (
                        e.fingerprint.clone(),
                        e.session.clone(),
                        e.retention_until,
                        e.seq,
                    )
                })
                .collect();
            let cited: HashSet<String> = sessions
                .iter()
                .filter_map(|(_, session, _, _)| {
                    let ev = &session.document().evidence;
                    (ev.digest.is_some() && ev.data_uri.is_some())
                        .then(|| ev.digest.clone().unwrap())
                })
                .collect();
            // Drop uncited bundles from the index as well as the file:
            // leaving one in the index would tell a later `put_session` the
            // log already carries it, and the stripped session written then
            // would cite a digest no evidence record provides.
            index.evidence.retain(|d, _| cited.contains(d));
            let evidence: Vec<(String, Vec<u8>, u64)> = index
                .evidence
                .iter()
                .map(|(d, e)| (d.clone(), e.bytes.clone(), e.retention_until))
                .collect();
            (sessions, evidence)
        };
        live.sort_by_key(|(_, _, _, seq)| *seq);
        let written_digests: HashSet<&str> =
            live_evidence.iter().map(|(d, _, _)| d.as_str()).collect();

        let tmp = self.path.with_extension("jsonl.tmp");
        let kept = live.len();
        {
            let mut out = File::create(&tmp)?;
            let mut seq = 0u64;
            // Evidence lines first: replay is single-pass, and a session line
            // whose digest has not been seen is skipped.
            for (digest, bytes, retention_until) in &live_evidence {
                let mut line = serde_json::to_string(&EvidenceLogRecord {
                    seq,
                    ts: now,
                    record_type: RECORD_TYPE_EVIDENCE.to_string(),
                    digest: digest.clone(),
                    retention_until: *retention_until,
                    payload_b64: BASE64.encode(bytes),
                })
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
                seq = next_seq_after(seq)?;
            }
            for (fingerprint, session, retention_until, _) in live.iter() {
                // Strip only when the bundle will actually be in the file — a
                // citing session proves a reference exists, not that the
                // bundle is available (a record replayed from a
                // pre-externalization log carries its evidence inline and
                // never entered the table). Anything else stays whole.
                let (payload_b64, evidence_data_prefix) = match externalize_evidence(session)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                {
                    StoredSession::Stripped {
                        payload,
                        data_prefix,
                        digest,
                        ..
                    } if written_digests.contains(digest.as_str()) => {
                        (BASE64.encode(payload), Some(data_prefix))
                    }
                    _ => (BASE64.encode(session.bytes()), None),
                };
                let mut line = serde_json::to_string(&SessionLogRecord {
                    seq,
                    ts: now,
                    record_type: RECORD_TYPE_SESSION.to_string(),
                    fingerprint: fingerprint.clone(),
                    retention_until: *retention_until,
                    payload_b64,
                    evidence_data_prefix,
                })
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
                seq = next_seq_after(seq)?;
            }
            out.sync_all()?; // durable temp contents before it becomes the log
            w.next_seq = seq;
        }

        // Open the replacement append handle before the rename, so the only
        // fallible step left is the rename — the writer is never left pointing at
        // the stale inode.
        let new_file = OpenOptions::new().append(true).open(&tmp)?;
        std::fs::rename(&tmp, &self.path)?;

        w.file = new_file;
        // The rename is durable: record what the log now holds. Updating
        // `written_retention_until` only after the swap keeps the invariant
        // (index entry ⇒ the current log carries a line with that deadline)
        // intact across a failed compaction.
        let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());
        for (digest, _, retention_until) in &live_evidence {
            if let Some(e) = index.evidence.get_mut(digest) {
                e.written_retention_until = e.written_retention_until.max(*retention_until);
            }
        }
        Ok(kept)
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
        let stored = externalize_evidence(&session)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Lock order is writer → index, matching `compact`, so the two paths
        // can never deadlock against each other. The index is acquired before
        // any write so the log and the index advance together.
        let mut index = self.index.lock().unwrap_or_else(|p| p.into_inner());

        // The evidence line MUST precede the first session line citing it:
        // replay is single-pass, and a session line whose digest has not been
        // seen is skipped. Crashing between the two leaves an orphan evidence
        // record, which is harmless (evicted when its retention lapses).
        //
        // A known digest still needs a new line when this cite's retention
        // outlives the deadline persisted in the log: the in-memory deadline
        // alone would leave a window where a restart skips the stale line as
        // lapsed and drops the live citing session with it.
        if let StoredSession::Stripped {
            digest,
            evidence_bytes,
            ..
        } = &stored
        {
            let persisted = index
                .evidence
                .get(digest)
                .map(|e| e.retention_until)
                .unwrap_or(0)
                .max(retention_until);
            let needs_line = match index.evidence.get(digest) {
                None => true,
                Some(e) => persisted > e.written_retention_until,
            };
            if needs_line {
                let evidence_line = serde_json::to_string(&EvidenceLogRecord {
                    seq: w.next_seq,
                    ts: now,
                    record_type: RECORD_TYPE_EVIDENCE.to_string(),
                    digest: digest.clone(),
                    retention_until: persisted,
                    payload_b64: BASE64.encode(evidence_bytes),
                })
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                w.next_seq = next_seq_after(w.next_seq)?;
                w.file.write_all(evidence_line.as_bytes())?;
                w.file.write_all(b"\n")?;
                index.insert_evidence(digest.clone(), evidence_bytes.clone(), persisted, persisted);
            } else {
                // Bump the in-memory deadline only; the log's line still
                // covers this cite (persisted <= written_retention_until).
                index.insert_evidence(digest.clone(), evidence_bytes.clone(), retention_until, 0);
            }
        }

        let seq = w.next_seq;
        let (payload_b64, evidence_data_prefix) = match &stored {
            StoredSession::Full => (BASE64.encode(session.bytes()), None),
            StoredSession::Stripped {
                payload,
                data_prefix,
                ..
            } => (BASE64.encode(payload), Some(data_prefix.clone())),
        };
        let mut line = serde_json::to_string(&SessionLogRecord {
            seq,
            ts: now,
            record_type: RECORD_TYPE_SESSION.to_string(),
            fingerprint: fingerprint.to_string(),
            retention_until,
            payload_b64,
            evidence_data_prefix,
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        // No flush: `File::flush` is a no-op and the log isn't fsync'd. If
        // `file` ever becomes a `BufWriter`, restore a flush or records can sit
        // unwritten on a crash.
        w.file.write_all(line.as_bytes())?;
        w.next_seq = next_seq_after(w.next_seq)?;
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
    fn compact_preserves_insertion_order_for_a_shared_fingerprint() {
        // An evidence upgrade reseals a channel under the SAME fingerprint
        // while the superseded record stays resolvable by id, so two live
        // records can share a fingerprint. Compaction must keep resolving the
        // fingerprint to the latest-sealed session after a restart, not to
        // whichever record `HashMap` iteration happened to write last.
        //
        // The re-put below makes the failure deterministic: it refreshes
        // whichever record the index's `by_id` iteration yields FIRST, so
        // that record is simultaneously the latest insertion (the current
        // one) and first in iteration order (a same-key re-insert does not
        // move a `HashMap` entry). Without insertion-order sorting,
        // compaction writes it first and replay's last-insert-wins hands the
        // fingerprint to the other record — every run, not just when the
        // iteration order happens to be unlucky.
        let path = temp_path();
        let first = session("https://x", 1_000, 9_000);
        let second = session("https://x", 2_000, 9_000);
        let store = open_store(&path);
        store
            .put_session("fp-shared", first.clone(), 9_000, 1_000)
            .unwrap();
        store
            .put_session("fp-shared", second.clone(), 9_000, 2_000)
            .unwrap();
        let iteration_first = {
            let index = store.index.lock().unwrap();
            index
                .by_id
                .values()
                .next()
                .expect("two entries are resident")
                .session
                .clone()
        };
        // Re-put that record: same id, same fingerprint, but now the latest
        // insertion — and still first in `by_id` iteration order.
        store
            .put_session("fp-shared", iteration_first.clone(), 9_000, 3_000)
            .unwrap();
        assert_eq!(
            store
                .current_session("fp-shared", 9_000, 3_000)
                .map(|s| s.session_id().to_string()),
            Some(iteration_first.session_id().to_string()),
            "the latest insertion is the fingerprint's current session"
        );
        assert_eq!(store.compact(3_000).unwrap(), 2);
        drop(store); // release the advisory lock before reopening

        let reopened = open_store(&path);
        assert_eq!(
            reopened
                .current_session("fp-shared", 9_000, 3_000)
                .map(|s| s.session_id().to_string()),
            Some(iteration_first.session_id().to_string()),
            "replay must resolve the shared fingerprint to the latest-sealed record"
        );
        // Both records stay resolvable by id for retention.
        assert!(reopened.get_session(first.session_id(), 3_000).is_some());
        assert!(reopened.get_session(second.session_id(), 3_000).is_some());

        drop(reopened);
        cleanup(&path);
    }

    #[test]
    fn compact_append_compact_replay_keeps_the_latest_session_current() {
        // Compaction renumbers the log, but the index's insertion order must
        // survive compact → append → compact → replay: a record sealed AFTER a
        // compaction (whose file seq was renumbered from zero) still wins the
        // fingerprint against records that predate it, and the log sequence
        // continues from the compacted file.
        let path = temp_path();
        let superseded = session("https://x", 1_000, 9_000);
        let resealed = session("https://x", 2_000, 9_000);
        {
            let store = open_store(&path);
            store
                .put_session("fp", superseded.clone(), 9_000, 1_000)
                .unwrap();
            assert_eq!(store.compact(1_500).unwrap(), 1);
            // The reseal lands after the log was renumbered.
            assert_eq!(
                store
                    .put_session("fp", resealed.clone(), 9_000, 2_000)
                    .unwrap(),
                1,
                "the log sequence continues from the compacted file"
            );
            assert_eq!(store.compact(2_500).unwrap(), 2);
        }

        let reopened = open_store(&path);
        assert_eq!(
            reopened
                .current_session("fp", 9_000, 2_500)
                .map(|s| s.session_id().to_string()),
            Some(resealed.session_id().to_string()),
            "the post-compaction reseal stays current after another compaction \
             and replay"
        );
        assert!(reopened
            .get_session(superseded.session_id(), 2_500)
            .is_some());
        let third = session("https://y", 3_000, 9_000);
        assert_eq!(reopened.put_session("fp2", third, 9_000, 3_000).unwrap(), 2);

        drop(reopened);
        cleanup(&path);
    }

    /// A session carrying a complete §8.2 evidence bundle over `bytes` — the
    /// shape that the store externalizes into a shared evidence record.
    fn session_with_evidence(
        endpoint: &str,
        established_at: u64,
        expires_at: u64,
        evidence_bytes: &[u8],
    ) -> AttestedSession {
        let mut session = session(endpoint, established_at, expires_at);
        let mut document = session.document().clone();
        document.evidence = crate::aggregator::session::EvidenceRef {
            digest: Some(crate::aci::digest::sha256_hex(evidence_bytes)),
            data_uri: Some(format!(
                "data:application/json;base64,{}",
                BASE64.encode(evidence_bytes)
            )),
        };
        session = AttestedSession::seal(document).unwrap();
        session
    }

    /// Read the log as parsed JSON lines.
    fn log_lines(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn count_type(lines: &[serde_json::Value], record_type: &str) -> usize {
        lines.iter().filter(|l| l["type"] == record_type).count()
    }

    /// The core fidelity contract of evidence externalization: several
    /// sessions sharing one fleet-wide bundle store the bundle once, and
    /// every session still serves the exact sealed bytes — the session id is
    /// a content address over those bytes, so byte equality IS the behavior
    /// contract.
    #[test]
    fn externalized_evidence_is_stored_once_and_serves_byte_identical_sessions() {
        let path = temp_path();
        let bundle: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let sessions: Vec<_> = (0..3)
            .map(|i| session_with_evidence(&format!("https://node-{i}"), 1_000, 9_000, &bundle))
            .collect();
        let inline_bytes: usize = sessions
            .iter()
            .map(|s| BASE64.encode(s.bytes()).len())
            .sum();

        {
            let store = open_store(&path);
            for (i, s) in sessions.iter().enumerate() {
                store
                    .put_session(&format!("fp-{i}"), s.clone(), 9_000, 1_000)
                    .unwrap();
            }
            // Served sessions are byte-identical to the sealed originals.
            for s in &sessions {
                let got = store
                    .get_session(s.session_id(), 1_000)
                    .expect("session is served");
                assert_eq!(got.bytes(), s.bytes());
                assert_eq!(got.document(), s.document());
            }
        }

        let lines = log_lines(&path);
        assert_eq!(
            count_type(&lines, "evidence"),
            1,
            "the bundle is stored once"
        );
        assert_eq!(count_type(&lines, "session"), 3);
        for line in lines.iter().filter(|l| l["type"] == "session") {
            assert!(line["evidence_data_prefix"].is_string());
            let payload = BASE64
                .decode(line["payload_b64"].as_str().unwrap())
                .unwrap();
            assert!(
                payload.len() < bundle.len() / 4,
                "the session payload must not carry the bundle"
            );
        }
        let actual = std::fs::metadata(&path).unwrap().len() as usize;
        assert!(
            actual < inline_bytes / 2,
            "externalized ({actual} bytes) must be far smaller than inline ({inline_bytes} bytes)"
        );
        drop(open_store(&path));
        cleanup(&path);
    }

    /// Replay of externalized records reproduces the exact sessions — same
    /// ids, same bytes — and the fingerprint mapping survives.
    #[test]
    fn externalized_sessions_replay_byte_identical() {
        let path = temp_path();
        let bundle: Vec<u8> = (0..40_000u32).map(|i| (i % 241) as u8).collect();
        let first = session_with_evidence("https://node-1", 1_000, 9_000, &bundle);
        let second = session_with_evidence("https://node-2", 1_100, 9_000, &bundle);
        {
            let store = open_store(&path);
            store
                .put_session("fp-1", first.clone(), 9_000, 1_000)
                .unwrap();
            store
                .put_session("fp-2", second.clone(), 9_000, 1_000)
                .unwrap();
        }

        let reopened = open_store(&path);
        for s in [&first, &second] {
            let got = reopened
                .get_session(s.session_id(), 1_000)
                .expect("replayed session is served");
            assert_eq!(got.bytes(), s.bytes(), "replayed bytes must be identical");
            assert_eq!(got.session_id(), s.session_id());
        }
        assert_eq!(
            reopened
                .current_session("fp-1", 9_000, 1_000)
                .map(|s| s.session_id().to_string()),
            Some(first.session_id().to_string())
        );
        assert!(
            reopened
                .get_session(first.session_id(), 1_000)
                .unwrap()
                .document()
                .evidence
                .digest_matches_data(),
            "the rebuilt document satisfies the §8.2 digest check"
        );

        drop(reopened);
        cleanup(&path);
    }

    /// Compaction writes evidence lines before their citers and replay after
    /// compaction is still byte-identical.
    #[test]
    fn compact_rewrites_externalized_records_and_replays_identically() {
        let path = temp_path();
        let bundle: Vec<u8> = (0..30_000u32).map(|i| (i % 239) as u8).collect();
        let first = session_with_evidence("https://node-1", 1_000, 9_000, &bundle);
        let second = session_with_evidence("https://node-2", 1_100, 9_000, &bundle);
        {
            let store = open_store(&path);
            store
                .put_session("fp-1", first.clone(), 9_000, 1_000)
                .unwrap();
            store
                .put_session("fp-2", second.clone(), 9_000, 1_000)
                .unwrap();
            assert_eq!(store.compact(1_500).unwrap(), 2);
        }

        // Evidence precedes sessions, once.
        let lines = log_lines(&path);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "evidence");
        assert_eq!(count_type(&lines, "session"), 2);

        let reopened = open_store(&path);
        for s in [&first, &second] {
            assert_eq!(
                reopened.get_session(s.session_id(), 2_000).unwrap().bytes(),
                s.bytes()
            );
        }
        drop(reopened);
        cleanup(&path);
    }

    /// Evidence retention tracks its citers: extending a session's retention
    /// keeps its bundle alive; once the last citer lapses, the bundle goes.
    #[test]
    fn evidence_lifetime_tracks_its_citers() {
        let path = temp_path();
        let bundle: Vec<u8> = (0..10_000u32).map(|i| (i % 233) as u8).collect();
        let s = session_with_evidence("https://node-1", 1_000, 9_000, &bundle);
        {
            let store = open_store(&path);
            store.put_session("fp-1", s.clone(), 2_000, 1_000).unwrap();
            // A citation extends the session's retention; the bundle must follow.
            store.current_session("fp-1", 9_000, 1_500).unwrap();
            assert_eq!(store.compact(3_000).unwrap(), 1);
            let lines = log_lines(&path);
            assert_eq!(count_type(&lines, "evidence"), 1);
            assert_eq!(count_type(&lines, "session"), 1);
            // Past the extended retention, both are dropped.
            assert_eq!(store.compact(10_000).unwrap(), 0);
            assert!(log_lines(&path).is_empty());
        }
        assert!(open_store(&path)
            .get_session(s.session_id(), 10_001)
            .is_none());
        cleanup(&path);
    }

    /// A session line citing an evidence digest the log does not carry is
    /// skipped on replay (fail-closed, like a tampered payload).
    #[test]
    fn session_citing_missing_evidence_is_skipped_on_replay() {
        let path = temp_path();
        let bundle: Vec<u8> = (0..5_000u32).map(|i| (i % 229) as u8).collect();
        let s = session_with_evidence("https://node-1", 1_000, 9_000, &bundle);
        {
            let store = open_store(&path);
            store.put_session("fp-1", s.clone(), 9_000, 1_000).unwrap();
        }
        // Drop the evidence line, keep the citing session line.
        let lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| l.contains("\"session\""))
            .map(|l| l.to_string())
            .collect();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let reopened = open_store(&path);
        assert!(reopened.get_session(s.session_id(), 1_000).is_none());
        assert!(reopened.current_session("fp-1", 9_000, 1_000).is_none());
        drop(reopened);
        cleanup(&path);
    }

    /// An evidence record whose bytes do not hash to its digest is rejected,
    /// and sessions citing it are skipped.
    #[test]
    fn tampered_evidence_record_is_rejected_and_citers_skipped() {
        let path = temp_path();
        let bundle: Vec<u8> = (0..5_000u32).map(|i| (i % 227) as u8).collect();
        let s = session_with_evidence("https://node-1", 1_000, 9_000, &bundle);
        {
            let store = open_store(&path);
            store.put_session("fp-1", s.clone(), 9_000, 1_000).unwrap();
        }
        // Swap the evidence payload for different bytes under the same digest.
        let mut lines = log_lines(&path);
        let ev = lines
            .iter_mut()
            .find(|l| l["type"] == "evidence")
            .expect("evidence line exists");
        ev["payload_b64"] = serde_json::Value::String(BASE64.encode(b"swapped"));
        let mut out = lines
            .iter()
            .map(|l| serde_json::to_string(l).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        out.push('\n');
        std::fs::write(&path, out).unwrap();

        let reopened = open_store(&path);
        assert!(reopened.get_session(s.session_id(), 1_000).is_none());
        drop(reopened);
        cleanup(&path);
    }

    /// Evidence that is not a complete, canonical bundle stays inline: the
    /// record is written whole (today's format), served from the index, and
    /// the replay-time §8.2 check owns rejecting a digest mismatch.
    #[test]
    fn non_canonical_evidence_stays_inline() {
        let path = temp_path();
        // A digest that does not match the data: not externalizable.
        let mut document = session("https://node-1", 1_000, 9_000).document().clone();
        document.evidence = crate::aggregator::session::EvidenceRef {
            digest: Some(crate::aci::digest::sha256_hex(b"something-else")),
            data_uri: Some(format!(
                "data:application/json;base64,{}",
                BASE64.encode(b"payload")
            )),
        };
        let s = AttestedSession::seal(document).unwrap();
        {
            let store = open_store(&path);
            store.put_session("fp-1", s.clone(), 9_000, 1_000).unwrap();
            assert!(store.get_session(s.session_id(), 1_000).is_some());
            let lines = log_lines(&path);
            assert_eq!(count_type(&lines, "evidence"), 0, "no evidence record");
            assert_eq!(count_type(&lines, "session"), 1);
            assert!(lines[0].get("evidence_data_prefix").is_none());
        }
        // Replay applies the lenient §8.2 rule and drops the mismatch.
        assert!(open_store(&path)
            .get_session(s.session_id(), 1_000)
            .is_none());
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
            evidence_data_prefix: None,
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
