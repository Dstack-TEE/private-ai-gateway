//! Codex's background service: the managed app-server daemon that the Codex
//! CLI starts and every Codex client reuses. It keeps the provider and model
//! catalog it started with, so a connection change applies once it stops;
//! the next `codex` run starts it again, with the user's own environment.
//!
//! Only Codex's own CLI manages it, run from the daemon's package in the Codex
//! home (`codex-rs/app-server-daemon/src/managed_install.rs`), so nothing
//! depends on PATH. The Mac App Store build cannot run it and never offers to.

use std::{fmt, process::Stdio, time::Duration};

use desktop_core::protocol::{self, ErrorCode};
use tokio::process::Command;

use super::*;

/// Whether this build may run Codex's CLI.
pub(super) const AVAILABLE: bool = !cfg!(all(target_os = "macos", feature = "mac-app-store"));
/// `daemon version` probes the socket for at most 2 s and runs `codex --version`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// Covers the daemon's default 60 s shutdown grace and its 10 s forced exit,
/// and stays under the management client's 120 s `REQUEST_TIMEOUT`, so the
/// window always hears how the stop ended. A grace period configured longer
/// (Codex allows up to 300 s) can outlast it: the daemon has been asked to
/// stop and may still exit, but the user sees the "could not be stopped"
/// alert with the terminal command.
const STOP_TIMEOUT: Duration = Duration::from_secs(90);
/// What a caller learns of a failed stop; the diagnostic stays in the log.
const STOP_FAILED: &str = "Codex's background service could not be stopped. To apply the new \
    settings, run \"codex app-server daemon restart\" in a terminal.";

/// Codex's background service for the Codex home this app configures.
pub struct CodexService {
    codex_home: PathBuf,
}

impl Projector {
    pub fn codex_service(&self) -> CodexService {
        let config = Agent::Codex.config_path(&self.home, self.tool_env);
        CodexService {
            codex_home: config.parent().unwrap_or(&config).to_path_buf(),
        }
    }
}

impl CodexService {
    /// Whether a managed daemon is running. `daemon version` reports it without
    /// ever starting one and fails when none answers; any failure counts as
    /// not running.
    pub async fn running(&self) -> bool {
        let Some(mut command) = self.daemon_command("version") else {
            return false;
        };
        let Ok(Ok(output)) = tokio::time::timeout(VERSION_TIMEOUT, command.output()).await else {
            return false;
        };
        output.status.success() && reports_managed_daemon(&output.stdout)
    }

    /// Stops the managed daemon, ending running Codex sessions.
    pub async fn stop(&self) -> Result<(), StopFailed> {
        let mut command = self
            .daemon_command("stop")
            .ok_or_else(|| StopFailed("Codex's CLI is unavailable".into()))?;
        let output = tokio::time::timeout(STOP_TIMEOUT, command.output())
            .await
            .map_err(|_| StopFailed("codex app-server daemon stop timed out".into()))?
            .map_err(|error| {
                StopFailed(format!("Cannot run codex app-server daemon stop: {error}"))
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(StopFailed(format!(
            "codex app-server daemon stop failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }

    fn daemon_command(&self, action: &str) -> Option<Command> {
        if !AVAILABLE {
            return None;
        }
        let codex = managed_codex(&self.codex_home)?;
        let mut command = Command::new(codex);
        command
            .args(["app-server", "daemon", action])
            // The daemon of the Codex home whose configuration changed.
            .env("CODEX_HOME", &self.codex_home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        Some(command)
    }
}

/// A failed stop. It displays the diagnostic for the service log; callers
/// receive only [`STOP_FAILED`].
#[derive(Debug)]
pub struct StopFailed(String);

impl fmt::Display for StopFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<StopFailed> for protocol::Error {
    fn from(_: StopFailed) -> Self {
        Self::new(ErrorCode::OperationFailed, STOP_FAILED)
    }
}

/// The daemon's own executable, in the package layouts Codex resolves
/// (`managed_codex_bin`): the dedicated daemon package, then the legacy
/// standalone one. A managed daemon always runs from one of them.
fn managed_codex(codex_home: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "codex.exe" } else { "codex" };
    ["app-server-daemon", "standalone"]
        .into_iter()
        .map(|package| codex_home.join("packages").join(package).join("current"))
        .flat_map(|current| [current.join("bin").join(name), current.join(name)])
        .find(|path| executable_metadata(path).is_some())
}

/// `daemon version` prints `{"status":"running","backend":"pid",…}` for a
/// managed daemon; an app server on the socket that the daemon does not
/// manage has no backend, and `daemon stop` refuses it.
fn reports_managed_daemon(stdout: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .is_ok_and(|output| output["status"] == "running" && output["backend"] == "pid")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agents::tests::write_executable;

    /// A Codex home whose daemon package holds a fake `codex` that answers
    /// only for this home and the expected subcommand.
    fn service(subcommand: &str, reply: &str) -> (tempfile::TempDir, CodexService) {
        let home = tempfile::tempdir().unwrap();
        let codex_home = home.path().join(".codex");
        write_executable(
            &codex_home.join("packages/app-server-daemon/current/bin/codex"),
            &format!(
                "#!/bin/sh\n[ \"$CODEX_HOME\" = '{}' ] && [ \"$*\" = 'app-server daemon {subcommand}' ] || exit 64\n{reply}\n",
                codex_home.display()
            ),
        );
        (home, CodexService { codex_home })
    }

    #[tokio::test]
    async fn only_a_running_managed_daemon_counts_as_running() {
        let running = r#"echo '{"status":"running","backend":"pid"}'"#;
        assert!(service("version", running).1.running().await);
        for reply in [
            // No daemon answers: Codex exits non-zero.
            &format!("{running}; exit 1"),
            // An app server on the socket that the daemon doesn't manage.
            r#"echo '{"status":"running"}'"#,
        ] {
            assert!(!service("version", reply).1.running().await, "{reply}");
        }
        // A fake for another Codex home or subcommand refuses.
        assert!(!service("stop", running).1.running().await);
        let empty = tempfile::tempdir().unwrap();
        let missing = CodexService {
            codex_home: empty.path().join(".codex"),
        };
        assert!(!missing.running().await);
    }

    #[tokio::test]
    async fn a_failed_stop_answers_the_fixed_message_without_codex_output() {
        service("stop", "exit 0").1.stop().await.unwrap();
        let (_home, failing) = service("stop", "echo 'failed at /private/secret-path' >&2; exit 1");
        let failure = failing.stop().await.unwrap_err();
        assert!(failure.to_string().contains("secret-path"));
        let public = protocol::Error::from(failure);
        assert_eq!(public.code, ErrorCode::OperationFailed);
        assert_eq!(public.message, STOP_FAILED);
    }
}
