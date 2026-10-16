//! Hermetic contract check between the provider-verifier bridge and the vendored
//! `confidential_verifier` package.
//!
//! The bridge (`scripts/private_ai_provider_verifier.py`) imports the vendored
//! verifier and calls specific classes/methods (e.g.
//! `NearAICloudVerifier.verify_gateway_component`). When the verifier drifts, the
//! gateway only finds out on a live request. `scripts/contract_verifier_bridge.py`
//! asserts the required symbol surface exists, offline; this test runs it under
//! `cargo test` so CI fails closed on drift.

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
fn verifier_bridge_contract_holds() {
    if which("uv").is_none() {
        eprintln!(
            "skipping: uv not on PATH; run `uv run python scripts/contract_verifier_bridge.py` manually"
        );
        return;
    }

    // Always check the vendored package (clear any external override) so the test
    // is hermetic and reflects what actually ships.
    let out = Command::new("uv")
        .args(["run", "python", "scripts/contract_verifier_bridge.py"])
        .current_dir(repo_root())
        .env_remove("PRIVATE_AI_VERIFIER_DIR")
        .output()
        .expect("failed to invoke contract_verifier_bridge.py via uv");

    if !out.status.success() {
        eprintln!(
            "contract_verifier_bridge.py stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        eprintln!(
            "contract_verifier_bridge.py stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        panic!(
            "provider-verifier bridge contract drift: the bridge depends on verifier \
             symbols missing from the vendored confidential_verifier package"
        );
    }
}
