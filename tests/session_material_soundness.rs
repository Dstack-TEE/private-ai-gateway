//! Hermetic reviewed-provider session material projections.

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
fn reviewed_provider_session_material_is_sound() {
    if which("uv").is_none() {
        eprintln!(
            "skipping: uv not on PATH; run `uv run python tests/provider_verifier/session_material_soundness.py` manually"
        );
        return;
    }

    // Always check the vendored package (clear any external override) so the test
    // is hermetic and reflects what actually ships.
    let out = Command::new("uv")
        .args([
            "run",
            "--locked",
            "python",
            "tests/provider_verifier/session_material_soundness.py",
        ])
        .current_dir(repo_root())
        .env_remove("PRIVATE_AI_VERIFIER_DIR")
        .output()
        .expect("failed to invoke session_material_soundness.py via uv");

    if !out.status.success() {
        eprintln!(
            "session_material_soundness.py stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        eprintln!(
            "session_material_soundness.py stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        panic!("reviewed session material projection regression");
    }
}
