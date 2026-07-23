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

fn privatemode_compose_text() -> String {
    std::fs::read_to_string(repo_root().join("deploy/compose.privatemode.yaml"))
        .expect("Privatemode deployment compose must exist")
}

#[test]
fn privatemode_proxy_is_a_pinned_unpublished_compose_service() {
    let body = privatemode_compose_text();
    let service = body
        .split("\n  privatemode-proxy:\n")
        .nth(1)
        .and_then(|rest| rest.split("\nconfigs:\n").next())
        .expect("compose must declare a privatemode-proxy service");
    assert!(service.contains(
        "ghcr.io/edgelesssys/privatemode/privatemode-proxy@sha256:ff900b263a51a437633d15da809e7893a31fa4b1f4acfa4e526c075682d84307"
    ));
    assert!(
        !service.contains("ports:"),
        "proxy port must not be published"
    );
    assert!(service.contains("--manifestPath"));
    assert!(service.contains("--apiKey"));
    assert!(service.contains("@/run/secrets/privatemode-api-key"));
    assert!(service.contains("--nvidiaOCSPAllowUnknown=false"));
    assert!(service.contains("--nvidiaOCSPRevokedGracePeriod=0"));
    assert!(service.contains("source: privatemode-manifest"));
    assert!(service.contains("tmpfs:"));
    assert!(service.contains("source: privatemode-api-key"));
    assert!(!service.contains("privatemode-state"));
    assert!(body.contains("environment: PRIVATEMODE_API_KEY"));
    assert!(!body.contains("--apiKey=${PRIVATEMODE_API_KEY}"));
}

#[test]
fn privatemode_gateway_and_proxy_share_measured_pins() {
    let body = privatemode_compose_text();
    assert!(body.contains(r#""base_url": "http://privatemode-proxy:8080""#));
    assert!(body.contains(r#""manifest_path": "/run/privatemode/manifest.json""#));
    assert!(body.contains(r#""credential_path": "/run/secrets/privatemode-api-key""#));
    assert!(body.contains("PRIVATEMODE_MANIFEST_SHA256:?"));
    assert!(body.contains("PRIVATEMODE_CREDENTIAL_SHA256:?"));
    assert!(body.contains("PRIVATEMODE_MANIFEST_PATH:?"));
    assert!(body.contains("PRIVATE_AI_GATEWAY_ADMIN_TOKEN_SHA256:?"));
    assert!(body.contains("PRIVATE_AI_GATEWAY_INFERENCE_TOKEN_SHA256:?"));
    assert!(body.contains("/dstack/.host-shared/.decrypted-env"));
    assert!(body.contains("PRIVATE_AI_GATEWAY_ENV_FILE: /run/secrets/dstack-encrypted-env"));
    assert!(!body.contains(r#""admin_token": "${PRIVATE_AI_GATEWAY_ADMIN_TOKEN"#));
    assert_eq!(body.matches("source: privatemode-manifest").count(), 2);
    assert_eq!(body.matches("source: privatemode-api-key").count(), 2);
}

#[test]
fn privatemode_renderer_preserves_exact_manifest_bytes() {
    let path = repo_root().join("deploy/render-privatemode-compose.sh");
    let body = std::fs::read_to_string(&path).expect("Privatemode renderer must exist");
    assert!(body.contains("jq --rawfile manifest"));
    assert!(body.contains("jq -j"));
    assert!(body.contains("rendered_manifest_sha256"));
    assert!(body.contains("PRIVATEMODE_CREDENTIAL_SHA256=$("));
    assert!(body.contains(r#"printf '%s' "$PRIVATEMODE_API_KEY""#));
    for secret in [
        "PRIVATE_AI_GATEWAY_ADMIN_TOKEN",
        "PRIVATE_AI_GATEWAY_INFERENCE_TOKEN",
        "PRIVATEMODE_API_KEY",
    ] {
        assert!(
            body.contains(secret),
            "renderer must reject embedding {secret}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "Privatemode renderer must be executable");
    }
}

#[test]
fn privatemode_config_pins_force_container_reconciliation() {
    let body = privatemode_compose_text();
    let gateway = body
        .split("\n  private-ai-gateway:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n  privatemode-proxy:\n").next())
        .expect("compose must declare a private-ai-gateway service");
    let proxy = body
        .split("\n  privatemode-proxy:\n")
        .nth(1)
        .and_then(|rest| rest.split("\nconfigs:\n").next())
        .expect("compose must declare a privatemode-proxy service");

    for pin in [
        "ai.private-gateway.source-commit",
        "ai.private-gateway.admin-token-sha256",
        "ai.private-gateway.inference-token-sha256",
        "ai.private-gateway.privatemode-manifest-sha256",
        "ai.private-gateway.privatemode-credential-sha256",
    ] {
        assert!(
            gateway.contains(pin),
            "gateway service labels must include {pin} so an inline config change recreates a stale container"
        );
    }
    for pin in [
        "ai.private-gateway.privatemode-manifest-sha256",
        "ai.private-gateway.privatemode-credential-sha256",
    ] {
        assert!(
            proxy.contains(pin),
            "proxy service labels must include {pin} so startup manifest or credential changes recreate a stale proxy"
        );
    }
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
    // Runtime policy lives in audited deployment config, not inside the
    // workload bytes. entrypoint.sh must never bake it in.
    let body = script_text();
    assert!(
        !body.contains("dstack_endpoint"),
        "entrypoint.sh must not set runtime deployment policy itself; \
         dstack_endpoint belongs in the static gateway config"
    );
}

#[test]
fn entrypoint_sh_does_not_bake_upstream_config_policy() {
    // Upstream choice is trust-bearing deployment policy that must come
    // from audited deployment config. The script must not set or default
    // any of the upstream-related env names.
    let body = script_text();
    for needle in &[
        "PRIVATE_AI_GATEWAY_UPSTREAM_URL=",
        "PRIVATE_AI_GATEWAY_UPSTREAMS_JSON=",
        "PRIVATE_AI_GATEWAY_ADMIN_TOKEN=",
        "export PRIVATE_AI_GATEWAY_UPSTREAM_URL",
        "export PRIVATE_AI_GATEWAY_UPSTREAMS_JSON",
        "export PRIVATE_AI_GATEWAY_ADMIN_TOKEN",
    ] {
        assert!(
            !body.contains(needle),
            "entrypoint.sh must not set or export upstream config policy (found {needle:?}); \
             upstream choice is deployment policy and belongs in compose environment"
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
        body.contains("command -v cc") && body.contains("apt_packages+=(build-essential)"),
        "entrypoint.sh must install a native compiler/linker when the launcher image only supplies Rust"
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
fn entrypoint_sh_keeps_mutable_build_state_outside_checkout() {
    // git-launcher scrubs the source checkout on every boot. Cargo/Rustup
    // state and build artifacts must live in the gateway state volume, not
    // under the pinned source tree.
    let body = script_text();
    for required in &[
        r#"PRIVATE_AI_GATEWAY_CACHE_DIR=${PRIVATE_AI_GATEWAY_CACHE_DIR:-/var/lib/private-ai-gateway/cache}"#,
        r#"CARGO_HOME=${CARGO_HOME:-$PRIVATE_AI_GATEWAY_CACHE_DIR/cargo}"#,
        r#"RUSTUP_HOME=${RUSTUP_HOME:-$PRIVATE_AI_GATEWAY_CACHE_DIR/rustup}"#,
        r#"CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PRIVATE_AI_GATEWAY_CACHE_DIR/target}"#,
        r#"mkdir -p "$CARGO_HOME" "$RUSTUP_HOME" "$CARGO_TARGET_DIR""#,
        r#"BIN="$CARGO_TARGET_DIR/release/private-ai-gateway""#,
    ] {
        assert!(
            body.contains(required),
            "entrypoint.sh must keep mutable build state outside the source checkout; missing {required:?}"
        );
    }
    assert!(
        !body.contains(r#"BIN="$SCRIPT_DIR/target/release/private-ai-gateway""#),
        "entrypoint.sh must not execute a binary from the scrubbed source checkout target dir"
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
    for name in &["aggregator.conf", "compose.yaml", "README.md"] {
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
            repo_root().join("deploy").join("compose.yaml"),
            "deploy/compose.yaml",
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
// The launcher is generic and build-system agnostic; the gateway owns
// its install/build/run logic. These tests pin that boundary so a
// well-meaning change can't drift either side back across it.

fn deploy_text(name: &str) -> String {
    let p = repo_root().join("deploy").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn repo_text(path: &str) -> String {
    let p = repo_root().join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn launcher_config_uses_only_default_mode_keys() {
    // aggregator.conf is the launcher config. The launcher contract is
    // intentionally minimal: REPO_URL, COMMIT_SHA, and WORK_DIR.
    // Setting INSTALL_CMD or RUN_CMD would move
    // gateway-owned install/run logic back into the launcher config,
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
                "aggregator.conf must not set {forbidden}; the gateway owns its install/run via entrypoint.sh"
            );
        }
    }
    // Sanity: the keys we DO use are present.
    for required in &["REPO_URL=", "COMMIT_SHA=", "WORK_DIR="] {
        assert!(
            body.contains(required),
            "aggregator.conf should set {required}... so the launcher has a complete default-mode pin"
        );
    }
    assert!(
        !body
            .lines()
            .any(|line| line.trim_start().starts_with("CHILD_ENV_FILE=")),
        "aggregator.conf must not set CHILD_ENV_FILE; app config comes through compose environment"
    );
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
        "compose.yaml must not set INSTALL_CMD; the gateway owns its install via entrypoint.sh"
    );
    assert!(
        !body.contains("RUN_CMD="),
        "compose.yaml must not set RUN_CMD; the gateway owns its run via entrypoint.sh"
    );
    assert!(
        !body.contains("REPO_SUBDIR="),
        "compose.yaml must not set REPO_SUBDIR now that the public repo root is the gateway"
    );
    assert!(
        !body.contains("CHILD_ENV_FILE="),
        "compose.yaml must not set CHILD_ENV_FILE; app config comes through service environment"
    );
    assert!(
        body.contains("environment:"),
        "compose.yaml must pass gateway runtime config through normal Compose environment"
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
        body.contains("gateway-owned image"),
        "deploy/README.md must describe the production image as gateway-owned"
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
        body.contains("upstream_config_seed_path"),
        "deploy/README.md must document the compose-mounted upstream seed"
    );
    assert!(
        body.contains("does not set `REPO_SUBDIR`"),
        "deploy/README.md must state that the public gateway repo runs from repo root"
    );
}

#[test]
fn docs_include_config_and_env_reference() {
    let reference = repo_text("docs/configuration-reference.md");
    for needle in [
        "Configuration Reference",
        "Config Fields",
        "Environment Variables",
        "`state_dir`",
        "`PRIVATE_AI_GATEWAY_CONFIG_PATH`",
    ] {
        assert!(
            reference.contains(needle),
            "configuration reference must document {needle:?}"
        );
    }

    assert!(
        repo_text("README.md").contains("docs/configuration-reference.md"),
        "README.md must link the configuration reference"
    );
    assert!(
        deploy_text("README.md").contains("../docs/configuration-reference.md"),
        "deploy/README.md must link the configuration reference"
    );
}

#[test]
fn entrypoint_sh_header_claims_gateway_ownership() {
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
