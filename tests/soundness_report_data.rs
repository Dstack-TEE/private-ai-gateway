//! Hermetic regression guard for the TDX report_data binding (NEAR AI / dstack path).
//!
//! Runs scripts/soundness_report_data.py, which asserts that verify_report_data
//! rejects a wrong nonce and a swapped TLS fingerprint, and that report_data is
//! parsed from the canonical TDX offset and fails closed on bad input. This pins the
//! fix for the gap where the NEAR AI gateway verifier skipped the report_data check
//! and accepted any nonce / TLS fingerprint. Offline; runs under cargo test.

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
fn report_data_binding_is_sound() {
    if which("uv").is_none() {
        eprintln!(
            "skipping: uv not on PATH; run `uv run python scripts/soundness_report_data.py` manually"
        );
        return;
    }

    let out = Command::new("uv")
        .args(["run", "python", "scripts/soundness_report_data.py"])
        .current_dir(repo_root())
        .env_remove("PRIVATE_AI_VERIFIER_DIR")
        .output()
        .expect("failed to invoke soundness_report_data.py via uv");

    if !out.status.success() {
        eprintln!(
            "soundness_report_data.py stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        eprintln!(
            "soundness_report_data.py stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        panic!("report_data binding soundness check failed");
    }
}
