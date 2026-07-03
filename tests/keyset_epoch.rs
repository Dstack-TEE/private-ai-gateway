//! Keyset-epoch persistence across simulated gateway restarts (§4.2, §4.7).
//!
//! Each "restart" is a fresh `load` → `resolve_epoch` → `store` cycle against
//! the same on-disk state file, exactly as the launcher runs it at startup.

use private_ai_gateway::aci::identity::{keyset_material_fingerprint, workload_keyset_digest};
use private_ai_gateway::aci::types::{
    KeysetEpoch, PublicKeyMaterial, WorkloadIdentity, WorkloadKeyset,
};
use private_ai_gateway::aggregator::keyset_epoch::{
    load, resolve_epoch, resolve_launcher_epoch, store,
};
use private_ai_gateway::aggregator::revocation_store::{RevocationStatement, RevocationStore};

const FP_A: &str = "sha256:aaaaaaaa";
const FP_B: &str = "sha256:bbbbbbbb";
const WINDOW: u64 = 30 * 24 * 60 * 60;

fn temp_state_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "keyset-epoch-test-{name}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("keyset-epoch.json")
}

/// One launcher boot: read the persisted state, resolve the epoch, and persist
/// the result. Returns the resolved epoch as the running service would use it.
fn boot(
    path: &std::path::Path,
    fingerprint: &str,
    now: u64,
) -> private_ai_gateway::aggregator::keyset_epoch::PersistedKeysetEpoch {
    let previous = load(path).unwrap();
    let resolved = resolve_epoch(previous.as_ref(), fingerprint, now, WINDOW);
    store(path, &resolved).unwrap();
    resolved
}

#[test]
fn version_is_stable_across_restarts_with_unchanged_keys() {
    let path = temp_state_path("stable");

    let first = boot(&path, FP_A, 1_000_000);
    assert_eq!(first.version, 1);
    assert_eq!(first.not_after, 1_000_000 + WINDOW);

    // Restart later but still inside the validity window, same keys: no bump,
    // and the same expiry — a re-derived `now + window` must not leak in.
    let second = boot(&path, FP_A, 1_000_500);
    assert_eq!(second.version, 1);
    assert_eq!(second.not_after, first.not_after);

    // And once more, still unchanged.
    let third = boot(&path, FP_A, 1_050_000);
    assert_eq!(third, second);
}

#[test]
fn version_bumps_when_key_material_changes() {
    let path = temp_state_path("material-change");

    let first = boot(&path, FP_A, 1_000_000);
    assert_eq!(first.version, 1);

    // A restart with different key material bumps the version and opens a fresh
    // window, even though the old one had not expired.
    let second = boot(&path, FP_B, 1_000_500);
    assert_eq!(second.version, 2);
    assert_eq!(second.not_after, 1_000_500 + WINDOW);
    assert_eq!(second.material_fingerprint, FP_B);

    // The bump persists: reloading yields version 2, not a reset.
    let reloaded = load(&path).unwrap().unwrap();
    assert_eq!(reloaded.version, 2);
}

#[test]
fn version_bumps_when_window_rolls_over() {
    let path = temp_state_path("window-rollover");

    let first = boot(&path, FP_A, 1_000_000);
    assert_eq!(first.version, 1);

    // A restart after `not_after` has passed rolls to a new epoch even with the
    // same keys — the expired window is itself a keyset change.
    let after_expiry = first.not_after + 1;
    let second = boot(&path, FP_A, after_expiry);
    assert_eq!(second.version, 2);
    assert_eq!(second.not_after, after_expiry + WINDOW);
}

/// A fixed keyset material whose digest depends only on the epoch, so a version
/// bump yields a different `workload_keyset_digest`.
fn make_keyset(epoch: KeysetEpoch) -> WorkloadKeyset {
    WorkloadKeyset {
        workload_identity: WorkloadIdentity {
            public_key: PublicKeyMaterial {
                algo: "ed25519".to_string(),
                public_key_hex: "aa".repeat(32),
            },
            subject: None,
        },
        keyset_epoch: epoch,
        receipt_signing_keys: Vec::new(),
        e2ee_public_keys: Vec::new(),
        tls_public_keys: Vec::new(),
    }
}

#[test]
fn launcher_rolls_past_a_revoked_digest_on_restart() {
    let path = temp_state_path("revoked-roll");
    let now = 1_000_000;

    // The material fingerprint the launcher derives, and the digest of the
    // epoch a first boot would land on (version 1).
    let fingerprint = keyset_material_fingerprint(&make_keyset(KeysetEpoch {
        version: 0,
        not_after: 0,
    }))
    .unwrap();
    let first = resolve_epoch(None, &fingerprint, now, WINDOW);
    assert_eq!(first.version, 1);
    let digest_v1 = workload_keyset_digest(&make_keyset(first.epoch())).unwrap();

    // Revoke that digest (as an admin did before this restart), then resolve.
    let revocations = RevocationStore::in_memory();
    revocations
        .record(RevocationStatement::new(
            "sha256:id".to_string(),
            digest_v1.clone(),
            "ed25519".to_string(),
            "00".repeat(32),
            now,
        ))
        .unwrap();

    let resolved = resolve_launcher_epoch(&path, make_keyset, &revocations, now, WINDOW).unwrap();

    // The launcher rolled to a fresh epoch whose digest is not revoked.
    assert!(resolved.version >= 2);
    let resolved_digest = workload_keyset_digest(&make_keyset(resolved.clone())).unwrap();
    assert_ne!(resolved_digest, digest_v1);
    assert!(!revocations.is_revoked(&resolved_digest));

    // The rolled epoch persisted, so a subsequent boot is stable on it.
    let persisted = load(&path).unwrap().unwrap();
    assert_eq!(persisted.version, resolved.version);
}
