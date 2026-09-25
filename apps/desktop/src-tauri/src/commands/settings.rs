use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use desktop_core::{
    client::{CallError, Client},
    contracts::CliRegistration,
    maintenance::ProfileBackup,
    protocol::{rpc, Preference},
    ui_api::Method,
};
use serde_json::json;
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};

use crate::{distribution, run_blocking, run_cli_command, CliStartup};

/// Imports a profile backup the user picks in the system open panel; `None`
/// when they cancel.
#[tauri::command]
pub(crate) async fn select_profile_backup(
    window: WebviewWindow,
) -> Result<Option<ProfileBackup>, CallError> {
    let (send, receive) = tokio::sync::oneshot::channel();
    json_dialog(&window, "Import Profile Configurations").pick_file(move |path| {
        let _ = send.send(path);
    });
    let Some(path) = chosen_path(receive.await)? else {
        return Ok(None);
    };
    Ok(Some(
        run_blocking(move || ProfileBackup::read(&path)).await?,
    ))
}

/// Exports the profiles, without keys, where the user chooses; `false` when
/// they cancel.
#[tauri::command]
pub(crate) async fn export_profiles(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<bool, CallError> {
    let content = crate::ui_api::invoke(
        window.clone(),
        client,
        Method::ExportProfilesContent,
        json!({}),
    )
    .await?;
    let content: String =
        serde_json::from_value(content).map_err(|_| "Management response failed")?;
    save(
        &window,
        "Export Profiles (No Keys)",
        "private-ai-proxy-profiles.json",
        content,
    )
    .await
}

/// Exports redacted diagnostics where the user chooses; `false` when they cancel.
#[tauri::command]
pub(crate) async fn export_diagnostics(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<bool, CallError> {
    let version = window.app_handle().package_info().version.to_string();
    let client = client.inner().clone();
    let content = run_blocking(move || {
        desktop_core::maintenance::json_content(&desktop_core::maintenance::diagnostics(
            &client.state()?,
            &version,
        ))
    })
    .await?;
    save(
        &window,
        "Export Redacted Diagnostics",
        "private-ai-proxy-diagnostics.json",
        content,
    )
    .await
}

fn json_dialog(window: &WebviewWindow, title: &str) -> FileDialogBuilder<tauri::Wry> {
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title(title)
        .add_filter("JSON", &["json"])
}

fn chosen_path(
    selection: Result<Option<FilePath>, tokio::sync::oneshot::error::RecvError>,
) -> Result<Option<PathBuf>, CallError> {
    let selection = selection.map_err(|_| "The file panel could not complete")?;
    Ok(selection
        .map(|path| {
            path.into_path()
                .map_err(|_| "The chosen file is not on this computer")
        })
        .transpose()?)
}

/// Writes `content` where the user chooses in the system save panel, which
/// asks before replacing an existing file. The app, not the backend, writes
/// it, so a sandboxed build uses the access the panel granted.
async fn save(
    window: &WebviewWindow,
    title: &str,
    file_name: &str,
    content: String,
) -> Result<bool, CallError> {
    let (send, receive) = tokio::sync::oneshot::channel();
    json_dialog(window, title)
        .set_file_name(file_name)
        .save_file(move |path| {
            let _ = send.send(path);
        });
    let Some(path) = chosen_path(receive.await)? else {
        return Ok(false);
    };
    run_blocking(move || {
        write_chosen(&path, content.as_bytes())
            .map_err(|_| "Could not save the file. Check that the folder is writable.".into())
    })
    .await?;
    Ok(true)
}

/// Writes the file the user chose in a save panel, which already asked before
/// replacing an existing one. The panel extends a sandboxed app's access to
/// exactly that file ("Accessing files from the macOS App Sandbox"), so it is
/// written in place rather than through a temporary file beside it.
fn write_chosen(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(content)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    #[test]
    fn chosen_files_are_replaced_whole() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("diagnostics.json");
        std::fs::write(&destination, "an original that is longer").unwrap();
        super::write_chosen(&destination, b"replacement").unwrap();
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "replacement"
        );
    }
}

#[tauri::command]
pub(crate) async fn get_cli_registration(app: AppHandle) -> Result<CliRegistration, CallError> {
    distribution::require(
        distribution::CAPABILITIES.cli_registration,
        "Command registration is unavailable in this distribution",
    )?;
    #[cfg(target_os = "macos")]
    crate::register_cli_on_startup(&app).await;
    let registration = run_cli_command(&app, vec!["cli", "status", "--json"]).await?;
    let startup_error = app.state::<CliStartup>().0.lock().await.last_error.clone();
    Ok(CliRegistration {
        registration,
        startup_error,
    })
}

#[tauri::command]
pub(crate) async fn set_cli_registration(
    app: AppHandle,
    installed: bool,
) -> Result<CliRegistration, CallError> {
    distribution::require(
        distribution::CAPABILITIES.cli_registration,
        "Command registration is unavailable in this distribution",
    )?;
    #[cfg(target_os = "macos")]
    crate::register_cli_on_startup(&app).await;
    let startup = app.state::<CliStartup>();
    let mut state = startup.0.lock().await;
    let client = app.state::<Arc<Client>>().inner().clone();
    if !installed {
        let writer = client.clone();
        run_blocking(move || {
            Ok(writer.call(rpc::SetPreference {
                change: Preference::AutoCliRegistration(false),
            })?)
        })
        .await?;
    }
    let registration = if installed {
        run_cli_command(&app, vec!["cli", "install", "--json"]).await
    } else {
        run_cli_command(&app, vec!["cli", "uninstall", "--json", "--yes"]).await
    }?;
    if installed {
        run_blocking(move || {
            Ok(client.call(rpc::SetPreference {
                change: Preference::AutoCliRegistration(true),
            })?)
        })
        .await?;
    }
    state.last_error = None;
    Ok(CliRegistration {
        registration,
        startup_error: None,
    })
}
