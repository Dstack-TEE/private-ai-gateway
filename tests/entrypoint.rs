//! Static audit of `entrypoint.sh`.
//!
//! These tests pin the small but trust-bearing invariants of the
//! aggregator's repo-owned entry script: it is fail-closed, it uses
//! `--locked` for the cargo build, it actually `exec`s the produced
//! binary, and it never bakes deployment policy into its bytes
//! (upstream config, dstack endpoint, etc.).
//!
//! When `shellcheck` is on `PATH` we additionally run it; otherwise the
//! shellcheck invariant test is skipped with a note so CI environments
//! without shellcheck do not silently lose coverage.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("entrypoint.sh")
}

fn script_text() -> String {
    std::fs::read_to_string(script_path()).expect("entrypoint.sh must exist at repo root")
}

#[test]
fn entrypoint_sh_exists_and_is_executable() {
    let p = script_path();
    assert!(p.exists(), "{} must exist", p.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        // The launcher will run us via `bash entrypoint.sh`, so the exec
        // bit is not strictly required - but checking it in keeps the
        // local `./entrypoint.sh` developer flow honest.
        assert_ne!(
            mode & 0o111,
            0,
            "entrypoint.sh should be executable; got mode {:o}",
            mode
        );
    }
}

#[test]
fn entrypoint_sh_is_fail_closed() {
    let body = script_text();
    assert!(
        body.contains("set -euo pipefail"),
        "entrypoint.sh must use `set -euo pipefail`"
    );
}

#[test]
fn entrypoint_sh_uses_locked_release_build() {
    let body = script_text();
    assert!(
        body.contains("cargo build --release --locked --bin private-ai-gateway"),
        "entrypoint.sh must call the exact `cargo build --release --locked --bin private-ai-gateway` command"
    );
}

#[test]
fn entrypoint_sh_execs_the_built_binary() {
    let body = script_text();
    // We `exec "$BIN"` so the binary becomes the persistent process. A
    // `cargo run` would leave cargo in the process tree; that is rejected
    // by this invariant.
    assert!(
        body.contains("exec \"$BIN\""),
        "entrypoint.sh must `exec` the built binary, not `cargo run`"
    );
    assert!(
        !body.contains("cargo run"),
        "entrypoint.sh must not invoke `cargo run`"
    );
}

#[test]
fn entrypoint_sh_does_not_export_runtime_policy() {
    // Runtime policy lives in CHILD_ENV_FILE (audited deployment policy), not
    // inside the workload bytes. entrypoint.sh must never bake it in.
    let body = script_text();
    for needle in &[
        "PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT=",
        "export PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT",
    ] {
        assert!(
            !body.contains(needle),
            "entrypoint.sh must not set runtime deployment policy itself \
             (found {needle:?}); it belongs in CHILD_ENV_FILE so a verifier audits it"
        );
    }
}

#[test]
fn entrypoint_sh_does_not_bake_upstream_config_policy() {
    // Upstream choice is trust-bearing deployment policy that must come
    // from CHILD_ENV_FILE. The script must not set or default any of the
    // upstream-related env names.
    let body = script_text();
    for needle in &[
        "PRIVATE_AI_GATEWAY_UPSTREAM_URL=",
        "PRIVATE_AI_GATEWAY_UPSTREAMS_JSON=",
        "PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH=",
        "PRIVATE_AI_GATEWAY_ADMIN_TOKEN=",
        "export PRIVATE_AI_GATEWAY_UPSTREAM_URL",
        "export PRIVATE_AI_GATEWAY_UPSTREAMS_JSON",
        "export PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH",
        "export PRIVATE_AI_GATEWAY_ADMIN_TOKEN",
    ] {
        assert!(
            !body.contains(needle),
            "entrypoint.sh must not set or export upstream config policy (found {needle:?}); \
             upstream choice is deployment policy and belongs in CHILD_ENV_FILE"
        );
    }
}

#[test]
fn entrypoint_sh_apt_install_is_strict() {
    // The runtime-bootstrap path is `apt-get install -y --no-install-recommends`
    // followed by `rustup default stable`. If the script ever drops --no-install-recommends
    // or stops pinning `stable`, we want a noisy failure here.
    let body = script_text();
    assert!(
        body.contains("apt-get install -y --no-install-recommends"),
        "apt-get install line must use --no-install-recommends to keep the trust surface minimal"
    );
    assert!(
        body.contains("rustup default stable"),
        "entrypoint.sh must call `rustup default stable` after toolchain install"
    );
    assert!(
        body.contains("--no-self-update"),
        "rustup toolchain install must pass --no-self-update so the rustup binary cannot silently upgrade itself at deploy time"
    );
}

#[test]
fn shellcheck_passes_on_entrypoint_sh() {
    if which("shellcheck").is_none() {
        eprintln!("skipping: shellcheck not on PATH; run shellcheck entrypoint.sh manually");
        return;
    }
    let out = Command::new("shellcheck")
        .arg(script_path())
        .output()
        .expect("failed to invoke shellcheck");
    if !out.status.success() {
        eprintln!(
            "shellcheck stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        eprintln!(
            "shellcheck stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        panic!("shellcheck reported issues on entrypoint.sh");
    }
}

#[test]
fn deploy_examples_exist_and_reference_entrypoint_sh() {
    // The deploy examples are part of the same pin and a verifier reads
    // them too. Keep them in lockstep with entrypoint.sh.
    let deploy = repo_root().join("deploy");
    for name in &[
        "aggregator.conf",
        "aggregator.env",
        "compose.yaml",
        "README.md",
    ] {
        let p = deploy.join(name);
        assert!(p.exists(), "deploy/{name} must exist next to entrypoint.sh");
    }
    let readme = std::fs::read_to_string(deploy.join("README.md")).unwrap();
    assert!(
        readme.contains("entrypoint.sh"),
        "deploy/README.md must reference entrypoint.sh so the wiring is documented"
    );
}

#[test]
fn no_stale_tee_launch_sh_references_remain() {
    // The public gateway deploy path uses the current launcher contract:
    // default mode runs entrypoint.sh. The old tee-launch.sh name should not
    // appear in shipping gateway artefacts.
    let files: [(PathBuf, &str); 5] = [
        (script_path(), "entrypoint.sh"),
        (repo_root().join("README.md"), "top-level README.md"),
        (
            repo_root().join("deploy").join("README.md"),
            "deploy/README.md",
        ),
        (
            repo_root().join("deploy").join("aggregator.conf"),
            "deploy/aggregator.conf",
        ),
        (
            repo_root().join("deploy").join("aggregator.env"),
            "deploy/aggregator.env",
        ),
    ];
    for (path, label) in files.iter() {
        let body = std::fs::read_to_string(path).unwrap();
        for (lineno, line) in body.lines().enumerate() {
            if !line.contains("tee-launch.sh") {
                continue;
            }
            panic!(
                "{label}:{n}: stale tee-launch.sh reference:\n  {line}",
                n = lineno + 1
            );
        }
    }
}

// ---------- Ownership-boundary invariants ----------
//
// The launcher is generic and build-system agnostic; the aggregator owns
// its install/build/run logic. These tests pin that boundary so a
// well-meaning change can't drift either side back across it.

fn deploy_text(name: &str) -> String {
    let p = repo_root().join("deploy").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn launcher_config_uses_only_default_mode_keys() {
    // aggregator.conf is the launcher config. The launcher contract is
    // intentionally minimal: REPO_URL, COMMIT_SHA, WORK_DIR, and
    // CHILD_ENV_FILE. Setting INSTALL_CMD or RUN_CMD would move
    // aggregator-owned install/run logic back into the launcher config,
    // which is exactly the boundary we keep out of.
    let body = deploy_text("aggregator.conf");
    for forbidden in &["INSTALL_CMD", "RUN_CMD"] {
        for line in body.lines() {
            // Allow the substring in comments; only reject as a config key.
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }
            assert!(
                !trimmed.starts_with(&format!("{forbidden}=")),
                "aggregator.conf must not set {forbidden}; the aggregator owns its install/run via entrypoint.sh"
            );
        }
    }
    // Sanity: the keys we DO use are present.
    for required in &["REPO_URL=", "COMMIT_SHA=", "WORK_DIR=", "CHILD_ENV_FILE="] {
        assert!(
            body.contains(required),
            "aggregator.conf should set {required}... so the launcher has a complete default-mode pin"
        );
    }
    assert!(
        !body
            .lines()
            .any(|line| line.trim_start().starts_with("REPO_SUBDIR=")),
        "aggregator.conf must not set REPO_SUBDIR now that the public repo root is the gateway"
    );
}

#[test]
fn compose_yaml_inlines_only_default_mode_keys() {
    // The compose example inlines aggregator.conf as a dstack config.
    // Keep the same boundary: no INSTALL_CMD / RUN_CMD smuggling in via
    // compose either.
    let body = deploy_text("compose.yaml");
    assert!(
        !body.contains("INSTALL_CMD="),
        "compose.yaml must not set INSTALL_CMD; the aggregator owns its install via entrypoint.sh"
    );
    assert!(
        !body.contains("RUN_CMD="),
        "compose.yaml must not set RUN_CMD; the aggregator owns its run via entrypoint.sh"
    );
    assert!(
        !body.contains("REPO_SUBDIR="),
        "compose.yaml must not set REPO_SUBDIR now that the public repo root is the gateway"
    );
}

#[test]
fn deploy_readme_states_ownership_boundary() {
    // Drift here would dilute the contract we are pinning. The text test
    // is intentionally narrow: it checks for the specific phrases that
    // assert the boundary, not the surrounding prose.
    let body = deploy_text("README.md");
    assert!(
        body.contains("Ownership boundary"),
        "deploy/README.md must have an 'Ownership boundary' section"
    );
    assert!(
        body.contains("build-system agnostic"),
        "deploy/README.md must describe the launcher as build-system agnostic"
    );
    assert!(
        body.contains("aggregator-owned image"),
        "deploy/README.md must describe the production image as aggregator-owned"
    );
    assert!(
        !body.contains("launcher-derived image"),
        "deploy/README.md must not call the production image 'launcher-derived'; that frames the toolchain as a launcher feature"
    );
}

#[test]
fn deploy_readme_documents_one_command_deploy_and_seed_config() {
    let body = deploy_text("README.md");
    assert!(
        body.contains("One-Command Deploy"),
        "deploy/README.md must document the one-command compose path"
    );
    assert!(
        body.contains("UPSTREAM_CONFIG_SEED_PATH"),
        "deploy/README.md must document the compose-mounted upstream seed"
    );
    assert!(
        body.contains("does not set `REPO_SUBDIR`"),
        "deploy/README.md must state that the public gateway repo runs from repo root"
    );
}

#[test]
fn entrypoint_sh_header_claims_aggregator_ownership() {
    // Make sure the script's own header tells future readers who owns it.
    let body = script_text();
    assert!(
        body.contains("Ownership boundary"),
        "entrypoint.sh header must include the 'Ownership boundary' note"
    );
    assert!(
        body.contains("owned by private-ai-gateway"),
        "entrypoint.sh header must state it is owned by private-ai-gateway"
    );
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = Path::new(&dir).join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
