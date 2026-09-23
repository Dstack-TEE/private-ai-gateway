//! Hermetic soundness guard for the Confidential AI (c8s) provider verifier bridge.

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
fn c8s_bridge_is_sound() {
    let uv = which("uv").expect(
        "uv is required for c8s verifier tests; install the pinned CI tool or run through uv",
    );

    let out = Command::new(uv)
        .args(["run", "python", "tests/provider_verifier/c8s_soundness.py"])
        .current_dir(repo_root())
        .env_remove("PRIVATE_AI_VERIFIER_DIR")
        .output()
        .expect("failed to invoke c8s soundness tests via uv");

    if !out.status.success() {
        eprintln!(
            "c8s soundness test stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        eprintln!(
            "c8s soundness test stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        panic!("c8s bridge soundness check failed");
    }
}
