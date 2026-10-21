//! Command-line access: the bundled `pap` sidecar registers and unregisters
//! the `pap` command. On macOS a launch registers it once a backend answers,
//! unless the user turned that off.

use std::{process::Stdio, sync::Arc, time::Duration};

use desktop_core::{
    client::{CallError, Client},
    contracts::{CliRegistration, CommandRegistration},
    protocol::{rpc, Preference},
};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncReadExt;

use crate::{distribution, run_blocking};

const INSTALL: [&str; 3] = ["cli", "install", "--json"];

/// `pap cli` only reads and writes a few local files; one still running after
/// this is stuck, and holding the registration lock.
const TIMEOUT: Duration = Duration::from_secs(30);

/// `pap cli --json` prints one small JSON object.
const MAX_OUTPUT: u64 = 64 * 1024;

/// The automatic registration of a launch. It holds the lock while it
/// runs, so reading or changing the registration waits for it.
#[derive(Clone, Default)]
pub(crate) struct CliStartup(Arc<tokio::sync::Mutex<CliStartupState>>);

#[derive(Default)]
struct CliStartupState {
    #[cfg(target_os = "macos")]
    attempted: bool,
    last_error: Option<String>,
}

/// Runs the bundled `pap`. Tauri bundles it next to the app's executable
/// (`Contents/MacOS` in a macOS bundle), where its shell plugin finds sidecars.
async fn run(arguments: &[&str]) -> Result<CommandRegistration, String> {
    let directory = tauri::utils::platform::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(ToOwned::to_owned))
        .ok_or("The bundled pap command is unavailable in this installation")?;
    let mut command = tokio::process::Command::new(
        directory.join(format!("private-ai-proxy{}", std::env::consts::EXE_SUFFIX)),
    );
    command.args(arguments);
    let stdout = output(command, TIMEOUT).await?;
    serde_json::from_slice(&stdout)
        .map_err(|_| "The pap command returned an invalid response".to_string())
}

/// The output of `command` once it succeeds. One that writes more than
/// `MAX_OUTPUT` or runs past `timeout` is killed, and at most `MAX_OUTPUT`
/// and a byte is ever read.
async fn output(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // CREATE_NO_WINDOW: the console-subsystem `pap` opens no console window.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .map_err(|_| "The pap command could not complete")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("The pap command could not complete")?;
    let finished = tokio::time::timeout(timeout, async {
        let mut output = Vec::new();
        stdout.take(MAX_OUTPUT + 1).read_to_end(&mut output).await?;
        if output.len() as u64 > MAX_OUTPUT {
            return Ok(None);
        }
        Ok::<_, std::io::Error>(Some((child.wait().await?, output)))
    })
    .await;
    let error = match finished {
        Ok(Ok(Some((status, output)))) if status.success() => return Ok(output),
        Ok(Ok(Some(_))) => {
            return Err("The pap command could not update command-line access".to_string())
        }
        Ok(Ok(None)) => "The pap command returned an invalid response",
        Ok(Err(_)) => "The pap command could not complete",
        Err(_) => "The pap command did not finish in time",
    };
    let _ = child.kill().await;
    Err(error.to_string())
}

/// Registers the command once per launch, when a backend first answers.
/// The lock is taken before the window hears of this backend, so its first
/// read of the registration waits for this one.
#[cfg(target_os = "macos")]
pub(crate) fn register_on_startup(app: &AppHandle) {
    let app = app.clone();
    let startup = app.state::<CliStartup>().inner().clone();
    let taken = startup.0.clone().try_lock_owned();
    tauri::async_runtime::spawn(async move {
        let mut state = match taken {
            Ok(state) => state,
            Err(_) => startup.0.lock_owned().await,
        };
        if state.attempted {
            return;
        }
        let client = app.state::<Arc<Client>>().inner().clone();
        if client.cached_state().backend_connected == Some(false) {
            return;
        }
        let enabled = run_blocking(move || {
            Ok(client
                .call(rpc::Settings)?
                .auto_cli_registration
                .unwrap_or(true))
        })
        .await;
        state.attempted = enabled.is_ok();
        let result = match enabled {
            Ok(true) => match stable_location() {
                Ok(()) => run(&INSTALL).await.map(|_| ()),
                Err(error) => Err(error),
            },
            other => other.map(|_| ()),
        };
        state.last_error = result
            .err()
            .map(|error| format!("Command-line registration failed: {error}"));
    });
}

/// Rejects registering the command for a copy of the app that runs from a
/// disk image or a translocated location, which goes away.
#[cfg(target_os = "macos")]
fn stable_location() -> Result<(), String> {
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| "Cannot locate the installed application".to_string())?;
    if transient_macos_app_path(&executable) {
        return Err(
            "Move Private AI Proxy to a stable location before installing the pap command"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn transient_macos_app_path(path: &std::path::Path) -> bool {
    path.starts_with("/Volumes")
        || path
            .components()
            .any(|component| component.as_os_str() == "AppTranslocation")
}

fn require_registration() -> Result<(), String> {
    distribution::require(
        distribution::CAPABILITIES.cli_registration,
        "Command registration is unavailable in this distribution",
    )
}

#[tauri::command]
pub(crate) async fn get_cli_registration(app: AppHandle) -> Result<CliRegistration, CallError> {
    require_registration()?;
    let startup = app.state::<CliStartup>().inner().clone();
    let state = startup.0.lock().await;
    let registration = run(&["cli", "status", "--json"]).await?;
    Ok(CliRegistration {
        registration,
        startup_error: state.last_error.clone(),
    })
}

/// Installs or uninstalls the command. Launches stop registering it before it
/// is uninstalled and resume once it is installed, so a failure never has a
/// later launch undo the user's choice.
#[tauri::command]
pub(crate) async fn set_cli_registration(
    app: AppHandle,
    installed: bool,
) -> Result<CliRegistration, CallError> {
    require_registration()?;
    let startup = app.state::<CliStartup>().inner().clone();
    let mut state = startup.0.lock().await;
    let client = app.state::<Arc<Client>>().inner().clone();
    let registration = if installed {
        let registration = run(&INSTALL).await?;
        set_automatic(client, true).await?;
        registration
    } else {
        set_automatic(client, false).await?;
        run(&["cli", "uninstall", "--json", "--yes"]).await?
    };
    state.last_error = None;
    Ok(CliRegistration {
        registration,
        startup_error: None,
    })
}

async fn set_automatic(client: Arc<Client>, enabled: bool) -> Result<(), String> {
    run_blocking(move || {
        client.call(rpc::SetPreference {
            change: Preference::AutoCliRegistration(enabled),
        })?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    use super::transient_macos_app_path;
    #[cfg(unix)]
    use super::{output, MAX_OUTPUT};

    #[cfg(unix)]
    fn shell(script: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_writes_too_much_is_stopped() {
        let started = Instant::now();
        let result =
            tauri::async_runtime::block_on(output(shell("exec yes"), Duration::from_secs(10)));
        assert_eq!(
            result,
            Err("The pap command returned an invalid response".to_string())
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        // The limit is inclusive.
        let script = format!("head -c {MAX_OUTPUT} /dev/zero");
        let result =
            tauri::async_runtime::block_on(output(shell(&script), Duration::from_secs(10)));
        assert_eq!(result.map(|output| output.len() as u64), Ok(MAX_OUTPUT));
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_runs_too_long_is_stopped() {
        let started = Instant::now();
        let result = tauri::async_runtime::block_on(output(
            shell("exec sleep 30"),
            Duration::from_millis(200),
        ));
        assert_eq!(
            result,
            Err("The pap command did not finish in time".to_string())
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn automatic_registration_rejects_transient_macos_locations() {
        assert!(transient_macos_app_path(std::path::Path::new(
            "/Volumes/Private AI Proxy/Private AI Proxy.app/Contents/MacOS/app"
        )));
        assert!(transient_macos_app_path(std::path::Path::new(
            "/private/var/folders/x/AppTranslocation/id/d/Private AI Proxy.app/Contents/MacOS/app"
        )));
        assert!(!transient_macos_app_path(std::path::Path::new(
            "/Applications/Private AI Proxy.app/Contents/MacOS/app"
        )));
    }
}
