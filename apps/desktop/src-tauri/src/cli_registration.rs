//! Command-line access: the bundled `pap` sidecar registers and unregisters
//! the `pap` command. On macOS a launch registers it once a backend answers,
//! unless the user turned that off.

use std::{sync::Arc, time::Duration};

use desktop_core::{
    client::{CallError, Client},
    contracts::{CliRegistration, CommandRegistration},
    protocol::{rpc, Preference},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};

use crate::{distribution, run_blocking};

const INSTALL: [&str; 3] = ["cli", "install", "--json"];

/// `pap cli` only reads and writes a few local files; one still running after
/// this is stuck, and holding the registration lock.
const TIMEOUT: Duration = Duration::from_secs(30);

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

async fn run(app: &AppHandle, arguments: &[&str]) -> Result<CommandRegistration, String> {
    let (mut events, child) = app
        .shell()
        .sidecar("private-ai-proxy")
        .map_err(|_| "The bundled pap command is unavailable in this installation")?
        .args(arguments)
        .spawn()
        .map_err(|_| "The pap command could not complete")?;
    let finished = async {
        let (mut stdout, mut code) = (Vec::new(), None);
        while let Some(event) = events.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    stdout.extend(line);
                    stdout.push(b'\n');
                }
                CommandEvent::Terminated(payload) => code = payload.code,
                _ => {}
            }
        }
        (stdout, code)
    };
    let Ok((stdout, code)) = tokio::time::timeout(TIMEOUT, finished).await else {
        let _ = child.kill();
        return Err("The pap command did not finish in time".to_string());
    };
    if code != Some(0) {
        return Err("The pap command could not update command-line access".to_string());
    }
    if stdout.len() > 64 * 1024 {
        return Err("The pap command returned an invalid response".to_string());
    }
    serde_json::from_slice(&stdout)
        .map_err(|_| "The pap command returned an invalid response".to_string())
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
                Ok(()) => run(&app, &INSTALL).await.map(|_| ()),
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
    let registration = run(&app, &["cli", "status", "--json"]).await?;
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
        let registration = run(&app, &INSTALL).await?;
        set_automatic(client, true).await?;
        registration
    } else {
        set_automatic(client, false).await?;
        run(&app, &["cli", "uninstall", "--json", "--yes"]).await?
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
    use super::transient_macos_app_path;

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
