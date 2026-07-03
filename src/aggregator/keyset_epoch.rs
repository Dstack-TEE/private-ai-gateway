//! Managed keyset epoch: bounded expiry plus a monotonic version persisted in
//! the gateway state directory (§4.2, §4.7).
//!
//! The launcher hands the ACI service a concrete [`KeysetEpoch`]. This module
//! decides that value at startup:
//!
//! * `not_after` is bounded — `now + window` — so a keyset stops producing
//!   acceptable reports without any coordination once the window lapses.
//! * `version` increases every time the keyset content changes: new key
//!   material (a different [material fingerprint](crate::aci::identity::keyset_material_fingerprint))
//!   or a rolled-over expiry window. A restart that serves the *same* keys
//!   within a still-valid window keeps the version, so verifiers see no
//!   spurious rollback churn.
//!
//! The persisted record carries the version, the current `not_after`, and the
//! material fingerprint — enough to detect "unchanged" across a restart.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::aci::identity;
use crate::aci::types::{KeysetEpoch, WorkloadKeyset};
use crate::aggregator::revocation_store::RevocationStore;

/// Default bounded validity window for a keyset epoch: 30 days (~4 weeks).
/// On the order of weeks per §4.7 ("a verifier profile SHOULD reject an
/// implausibly distant expiry"); overridable via gateway config.
pub const DEFAULT_KEYSET_EPOCH_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;

/// The keyset-epoch state persisted alongside the upstream config and session
/// log in the gateway state directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedKeysetEpoch {
    pub version: u64,
    pub not_after: u64,
    /// Fingerprint of the keyset material (all keys + identity + subject),
    /// excluding the epoch itself. A change here means new key material and
    /// forces a version bump.
    pub material_fingerprint: String,
}

impl PersistedKeysetEpoch {
    /// The wire [`KeysetEpoch`] this state represents.
    pub fn epoch(&self) -> KeysetEpoch {
        KeysetEpoch {
            version: self.version,
            not_after: self.not_after,
        }
    }
}

/// Decide the epoch for the current keyset material given the previously
/// persisted state.
///
/// Same material within a still-valid window keeps the version and expiry;
/// changed material or an expired window rolls forward to a fresh window with a
/// bumped version. A missing prior state starts at version 1.
pub fn resolve_epoch(
    previous: Option<&PersistedKeysetEpoch>,
    material_fingerprint: &str,
    now: u64,
    window_seconds: u64,
) -> PersistedKeysetEpoch {
    match previous {
        // Same keys, still inside the validity window: unchanged, keep going.
        Some(prev) if prev.material_fingerprint == material_fingerprint && prev.not_after > now => {
            prev.clone()
        }
        // New keys, or the window rolled over: a new epoch, higher version.
        Some(prev) => PersistedKeysetEpoch {
            version: prev.version + 1,
            not_after: now.saturating_add(window_seconds),
            material_fingerprint: material_fingerprint.to_string(),
        },
        None => PersistedKeysetEpoch {
            version: 1,
            not_after: now.saturating_add(window_seconds),
            material_fingerprint: material_fingerprint.to_string(),
        },
    }
}

/// Roll to the next version, keeping the same material and opening a fresh
/// window. Used to move past a revoked keyset digest at startup: the same keys
/// under a new epoch produce a new, un-revoked digest.
pub fn roll_forward(
    previous: &PersistedKeysetEpoch,
    now: u64,
    window_seconds: u64,
) -> PersistedKeysetEpoch {
    PersistedKeysetEpoch {
        version: previous.version + 1,
        not_after: now.saturating_add(window_seconds),
        material_fingerprint: previous.material_fingerprint.clone(),
    }
}

/// Read the persisted epoch state, or `None` when the file is absent or empty.
pub fn load(path: &Path) -> io::Result<Option<PersistedKeysetEpoch>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid keyset epoch state {}: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Launcher-side epoch resolution: decide the epoch for the current keyset
/// material, persist it, and never return one whose keyset digest has been
/// revoked.
///
/// `make_keyset` builds the workload keyset for a candidate epoch — the same
/// keyset the service later assembles — so the digest checked here matches what
/// the service serves. If the resolved digest is revoked (an admin revoked it
/// before this restart), the epoch rolls forward to a fresh version, changing
/// the digest, until it lands on an un-revoked one. The revocation list is
/// finite and each roll yields a distinct version, so this terminates.
pub fn resolve_launcher_epoch(
    state_path: &Path,
    make_keyset: impl Fn(KeysetEpoch) -> WorkloadKeyset,
    revocations: &RevocationStore,
    now: u64,
    window_seconds: u64,
) -> io::Result<KeysetEpoch> {
    let invalid = |e: crate::aci::canonical::CanonicalError| {
        io::Error::new(io::ErrorKind::InvalidData, e.to_string())
    };

    // Fingerprint the epoch-independent key material.
    let fingerprint = identity::keyset_material_fingerprint(&make_keyset(KeysetEpoch {
        version: 0,
        not_after: 0,
    }))
    .map_err(invalid)?;

    let previous = load(state_path)?;
    let mut resolved = resolve_epoch(previous.as_ref(), &fingerprint, now, window_seconds);

    let max_rolls = revocations.list().len() + 1;
    for _ in 0..=max_rolls {
        let digest =
            identity::workload_keyset_digest(&make_keyset(resolved.epoch())).map_err(invalid)?;
        if !revocations.is_revoked(&digest) {
            break;
        }
        resolved = roll_forward(&resolved, now, window_seconds);
    }

    if previous.as_ref() != Some(&resolved) {
        store(state_path, &resolved)?;
    }
    Ok(resolved.epoch())
}

/// Persist the epoch state via a temp file plus atomic rename, so a crash
/// mid-write never leaves a half-written record.
pub fn store(path: &Path, state: &PersistedKeysetEpoch) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    text.push('\n');
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP_A: &str = "sha256:aaaa";
    const FP_B: &str = "sha256:bbbb";
    const WINDOW: u64 = 1000;

    #[test]
    fn first_resolution_starts_at_version_one() {
        let epoch = resolve_epoch(None, FP_A, 100, WINDOW);
        assert_eq!(epoch.version, 1);
        assert_eq!(epoch.not_after, 1100);
        assert_eq!(epoch.material_fingerprint, FP_A);
    }

    #[test]
    fn same_material_within_window_keeps_version_and_expiry() {
        let first = resolve_epoch(None, FP_A, 100, WINDOW);
        // A later restart, still before not_after, with identical keys.
        let second = resolve_epoch(Some(&first), FP_A, 500, WINDOW);
        assert_eq!(second, first);
    }

    #[test]
    fn changed_material_bumps_version_and_opens_new_window() {
        let first = resolve_epoch(None, FP_A, 100, WINDOW);
        let second = resolve_epoch(Some(&first), FP_B, 500, WINDOW);
        assert_eq!(second.version, 2);
        assert_eq!(second.not_after, 1500);
        assert_eq!(second.material_fingerprint, FP_B);
    }

    #[test]
    fn expired_window_bumps_version_even_with_same_material() {
        let first = resolve_epoch(None, FP_A, 100, WINDOW);
        // Restart after not_after (1100) has passed: the window rolled over.
        let second = resolve_epoch(Some(&first), FP_A, 2000, WINDOW);
        assert_eq!(second.version, 2);
        assert_eq!(second.not_after, 3000);
    }

    #[test]
    fn roll_forward_bumps_version_and_preserves_material() {
        let first = resolve_epoch(None, FP_A, 100, WINDOW);
        let rolled = roll_forward(&first, 100, WINDOW);
        assert_eq!(rolled.version, 2);
        assert_eq!(rolled.material_fingerprint, FP_A);
    }
}
