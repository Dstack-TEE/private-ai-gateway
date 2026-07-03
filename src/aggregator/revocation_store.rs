//! Issued keyset revocation statements (§4.7) and their persistence.
//!
//! An operator revokes the current keyset through the admin surface; the
//! service signs the [revocation payload](crate::aci::identity::keyset_revocation_payload)
//! with the identity key and records the statement here. Statements are
//! transparency artifacts served at `GET /v1/aci/revocations`, so they persist
//! across restarts in the gateway state directory: a service that publicly
//! repudiated a keyset must not silently resume serving it after a restart.
//!
//! The store also answers "is this digest revoked?", which the service uses to
//! stop serving a revoked keyset.

use std::io;
use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::aci::types::KEYSET_REVOCATION_PURPOSE;

/// The identity-key signature over a revocation payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationSignature {
    /// Identity-key algorithm; matches the report's `keyset_endorsement.algo`.
    pub algo: String,
    #[serde(rename = "value")]
    pub value_hex: String,
}

/// One issued keyset revocation statement. Self-describing (§3.1): it carries
/// the revoked digest, the constant purpose tag, and the identity-key
/// signature, so a verifier can reconstruct the signed
/// [payload](crate::aci::identity::keyset_revocation_payload) and check it
/// under the workload identity without out-of-band context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationStatement {
    pub workload_id: String,
    pub workload_keyset_digest: String,
    /// Always `aci.keyset.revocation.v1`; the domain-separation tag inside the
    /// signed payload, restated for self-description.
    #[serde(default = "revocation_purpose")]
    pub purpose: String,
    pub revocation: RevocationSignature,
    /// Unix seconds when the service issued the statement (self-asserted).
    pub revoked_at: u64,
}

fn revocation_purpose() -> String {
    KEYSET_REVOCATION_PURPOSE.to_string()
}

impl RevocationStatement {
    pub fn new(
        workload_id: String,
        workload_keyset_digest: String,
        algo: String,
        value_hex: String,
        revoked_at: u64,
    ) -> Self {
        Self {
            workload_id,
            workload_keyset_digest,
            purpose: revocation_purpose(),
            revocation: RevocationSignature { algo, value_hex },
            revoked_at,
        }
    }
}

/// In-memory index of issued revocations, optionally mirrored to a JSON file.
pub struct RevocationStore {
    path: Option<PathBuf>,
    statements: RwLock<Vec<RevocationStatement>>,
}

impl RevocationStore {
    /// A store with no backing file (tests, and deployments that do not persist).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            statements: RwLock::new(Vec::new()),
        }
    }

    /// Open a file-backed store, loading any previously issued statements.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let statements = match std::fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => Vec::new(),
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid revocation store {}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: Some(path),
            statements: RwLock::new(statements),
        })
    }

    /// All issued statements, oldest first.
    pub fn list(&self) -> Vec<RevocationStatement> {
        self.statements.read().expect("revocation lock").clone()
    }

    /// Whether a revocation has been issued for `workload_keyset_digest`.
    pub fn is_revoked(&self, workload_keyset_digest: &str) -> bool {
        self.statements
            .read()
            .expect("revocation lock")
            .iter()
            .any(|s| s.workload_keyset_digest == workload_keyset_digest)
    }

    /// Record a statement and persist the full set. Idempotent per digest: a
    /// second revocation of the same digest keeps the first and does not
    /// duplicate it.
    pub fn record(&self, statement: RevocationStatement) -> io::Result<()> {
        let snapshot = {
            let mut guard = self.statements.write().expect("revocation lock");
            if guard
                .iter()
                .any(|s| s.workload_keyset_digest == statement.workload_keyset_digest)
            {
                return Ok(());
            }
            guard.push(statement);
            guard.clone()
        };
        self.persist(&snapshot)
    }

    fn persist(&self, statements: &[RevocationStatement]) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = serde_json::to_string_pretty(statements)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        text.push('\n');
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

impl Default for RevocationStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(digest: &str) -> RevocationStatement {
        RevocationStatement::new(
            "sha256:id".to_string(),
            digest.to_string(),
            "ed25519".to_string(),
            "ab".repeat(32),
            1_750_000_000,
        )
    }

    #[test]
    fn records_and_reports_revocation() {
        let store = RevocationStore::in_memory();
        assert!(!store.is_revoked("sha256:one"));
        store.record(statement("sha256:one")).unwrap();
        assert!(store.is_revoked("sha256:one"));
        assert!(!store.is_revoked("sha256:two"));
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn record_is_idempotent_per_digest() {
        let store = RevocationStore::in_memory();
        store.record(statement("sha256:one")).unwrap();
        store.record(statement("sha256:one")).unwrap();
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn persists_and_reloads_from_disk() {
        let dir = std::env::temp_dir().join(format!(
            "revocation-store-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("revocations.json");

        {
            let store = RevocationStore::open(&path).unwrap();
            store.record(statement("sha256:one")).unwrap();
        }
        // A fresh process reopening the same file sees the statement.
        let reopened = RevocationStore::open(&path).unwrap();
        assert!(reopened.is_revoked("sha256:one"));
        assert_eq!(reopened.list()[0].purpose, KEYSET_REVOCATION_PURPOSE);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
