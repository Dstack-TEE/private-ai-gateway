//! Static checks for checked-in smoke scripts.
//!
//! These scripts are not part of the production launch path, but they
//! encode the end-to-end Phala deployment assertions we rely on during
//! bring-up. Keep them auditable and fail-closed.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn smoke_script_path(name: &str) -> PathBuf {
    repo_root().join("scripts").join(name)
}

fn smoke_script_text(name: &str) -> String {
    std::fs::read_to_string(smoke_script_path(name)).expect("smoke script must exist")
}

#[test]
fn smoke_scripts_exist_and_are_executable() {
    for name in [
        "phala_multi_upstream_smoke.sh",
        "local_multi_upstream_smoke.sh",
    ] {
        let path = smoke_script_path(name);
        assert!(path.exists(), "{} must exist", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_ne!(
                mode & 0o111,
                0,
                "{name} should be executable; got mode {:o}",
                mode
            );
        }
    }
}

#[test]
fn phala_multi_upstream_smoke_script_keeps_core_assertions() {
    let body = smoke_script_text("phala_multi_upstream_smoke.sh");
    for needle in [
        "set -euo pipefail",
        "PRIVATE_AI_GATEWAY_CONFIG_PATH",
        "configs:",
        "--build-arg SOURCE_REPO_URL=local-build://private-ai-gateway",
        "--build-arg SOURCE_COMMIT=\"$COMMIT_SHA\"",
        "provider: \"aci-dcap\"",
        "routed-upstream-a-model",
        "routed-upstream-b-model",
        "request.forwarded",
        "transparency.request_modified",
        "upstream.verified",
        "model_id=\"public-a\"",
        "model_id=\"public-b\"",
    ] {
        assert!(
            body.contains(needle),
            "smoke script must retain assertion surface {needle:?}"
        );
    }
    assert!(
        !body.contains("\"provider\": \"preverified\""),
        "preverified must not be exposed as an upstream provider"
    );
}

#[test]
fn local_multi_upstream_smoke_script_keeps_core_assertions() {
    let body = smoke_script_text("local_multi_upstream_smoke.sh");
    for needle in [
        "set -euo pipefail",
        "DSTACK_SOCK",
        "docker compose",
        "\"admin_token\": \"${ADMIN_TOKEN}\"",
        "--build-arg SOURCE_REPO_URL=local-build://private-ai-gateway",
        "--build-arg SOURCE_COMMIT=\"$COMMIT_SHA\"",
        "provider: \"aci-dcap\"",
        "routed-upstream-a-model",
        "routed-upstream-b-model",
        "routed-upstream-a-embed-model",
        "/v1/embeddings",
        "assert_embeddings_receipt",
        "request.forwarded",
        "transparency.request_modified",
        "upstream.verified",
        "model_id=\"public-a\"",
        "model_id=\"public-b\"",
    ] {
        assert!(
            body.contains(needle),
            "local smoke script must retain assertion surface {needle:?}"
        );
    }
    assert!(
        !body.contains("\"provider\": \"preverified\""),
        "preverified must not be exposed as an upstream provider"
    );
}

#[test]
fn shellcheck_passes_on_smoke_scripts() {
    if which("shellcheck").is_none() {
        eprintln!("skipping: shellcheck not on PATH; run shellcheck scripts/*.sh manually");
        return;
    }
    for name in [
        "phala_multi_upstream_smoke.sh",
        "local_multi_upstream_smoke.sh",
    ] {
        let out = Command::new("shellcheck")
            .arg(smoke_script_path(name))
            .output()
            .expect("failed to invoke shellcheck");
        if !out.status.success() {
            eprintln!(
                "shellcheck stdout for {name}:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
            eprintln!(
                "shellcheck stderr for {name}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            panic!("shellcheck reported issues on {name}");
        }
    }
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
