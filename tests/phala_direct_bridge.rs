//! Hermetic soundness guard for the PhalaDirect provider verifier bridge.
//!
//! Runs scripts/soundness_phala_direct.py, which drives verify_phala_direct with
//! the HTTP fetch, dstack verifier, and NVIDIA GPU verifier stubbed but the real
//! report_data binding logic. It pins that a genuine version-2 report verifies and
//! emits a tls_spki_sha256 channel binding (plus the granular tcb_status claim),
//! and that a missing/ swapped TLS fingerprint, a mismatched GPU nonce, a dstack
//! failure, and a GPU failure are each rejected. Offline; runs under cargo test.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[test]
fn phala_direct_bridge_is_sound() {
    if which("uv").is_none() {
        eprintln!(
            "skipping: uv not on PATH; run `uv run python scripts/soundness_phala_direct.py` manually"
        );
        return;
    }

    let out = Command::new("uv")
        .args(["run", "python", "scripts/soundness_phala_direct.py"])
        .current_dir(repo_root())
        .env_remove("PRIVATE_AI_VERIFIER_DIR")
        .output()
        .expect("failed to invoke soundness_phala_direct.py via uv");

    if !out.status.success() {
        eprintln!(
            "soundness_phala_direct.py stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        eprintln!(
            "soundness_phala_direct.py stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        panic!("phala-direct bridge soundness check failed");
    }
}
