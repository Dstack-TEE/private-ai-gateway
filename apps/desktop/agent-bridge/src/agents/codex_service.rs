//! Codex's background service: the managed app-server daemon that the Codex
//! CLI starts and every Codex client reuses. It keeps the provider and model
//! catalog it started with, so a connection change applies once it stops;
//! the next `codex` run starts it again, with the user's own environment.
//!
//! Only Codex's own CLI manages it, run from the daemon's package in the Codex
//! home (`codex-rs/app-server-daemon/src/managed_install.rs`), so nothing
//! depends on PATH. The Mac App Store build cannot run it and never offers to.

use std::{process::Stdio, time::Duration};

use tokio::process::Command;

use super::*;

/// Whether this build may run Codex's CLI.
pub(super) const AVAILABLE: bool = !cfg!(all(target_os = "macos", feature = "mac-app-store"));
/// `daemon version` probes the socket for at most 2 s and runs `codex --version`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// Covers the daemon's default 60 s shutdown grace and its 10 s forced exit.
const STOP_TIMEOUT: Duration = Duration::from_secs(90);

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

    /// Stops the managed daemon, ending running Codex sessions. The error is
    /// a diagnostic for the service log.
    pub async fn stop(&self) -> Result<(), String> {
        let mut command = self
            .daemon_command("stop")
            .ok_or("Codex's CLI is unavailable")?;
        let output = tokio::time::timeout(STOP_TIMEOUT, command.output())
            .await
            .map_err(|_| "codex app-server daemon stop timed out".to_string())?
            .map_err(|error| format!("Cannot run codex app-server daemon stop: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "codex app-server daemon stop failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::tests::write_executable;

    #[test]
    fn only_a_managed_running_daemon_counts() {
        assert!(reports_managed_daemon(
            br#"{"status":"running","backend":"pid","managedCodexPath":"/c","socketPath":"/s"}"#
        ));
        for output in [
            &br#"{"status":"running","managedCodexPath":"/c","socketPath":"/s"}"#[..],
            br#"{"status":"notRunning"}"#,
            b"",
            b"Error: failed to connect",
        ] {
            assert!(!reports_managed_daemon(output));
        }
    }

    #[test]
    fn the_daemon_executable_comes_from_its_package() {
        let home = tempfile::tempdir().unwrap();
        let codex_home = home.path().join(".codex");
        assert_eq!(managed_codex(&codex_home), None);
        let name = if cfg!(windows) { "codex.exe" } else { "codex" };
        let legacy = codex_home.join("packages/standalone/current").join(name);
        write_executable(&legacy, "");
        assert_eq!(managed_codex(&codex_home), Some(legacy));
        let packaged = codex_home
            .join("packages/app-server-daemon/current/bin")
            .join(name);
        write_executable(&packaged, "");
        assert_eq!(managed_codex(&codex_home), Some(packaged));
    }
}
