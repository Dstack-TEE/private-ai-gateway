use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use desktop_core::{
    client::{CallError, Client},
    maintenance::ProfileBackup,
    protocol::rpc,
};
use tauri::{Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};

use crate::run_blocking;

/// Imports a profile backup the user picks in the system open panel; `None`
/// when they cancel.
#[tauri::command]
pub(crate) async fn select_profile_backup(
    window: WebviewWindow,
) -> Result<Option<ProfileBackup>, CallError> {
    let dialog = json_dialog(&window, "Import Profile Configurations");
    Ok(run_blocking(move || {
        chosen(dialog.blocking_pick_file())?
            .map(|path| ProfileBackup::read(&path))
            .transpose()
    })
    .await?)
}

/// Exports the profiles, without keys, where the user chooses, unless they
/// cancel.
#[tauri::command]
pub(crate) async fn export_profiles(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<(), CallError> {
    let content = desktop_core::ui_api::call(client.inner(), rpc::ExportProfilesContent).await?;
    save(
        &window,
        "Export Profiles (No Keys)",
        "private-ai-proxy-profiles.json",
        content,
    )
    .await
}

/// Exports redacted diagnostics where the user chooses, unless they cancel.
#[tauri::command]
pub(crate) async fn export_diagnostics(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<(), CallError> {
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

/// The file the user chose in a file panel the plugin showed off the main
/// thread; `None` when they cancelled.
fn chosen(selection: Option<FilePath>) -> Result<Option<PathBuf>, String> {
    selection
        .map(|path| {
            path.into_path()
                .map_err(|_| "The chosen file is not on this computer".to_string())
        })
        .transpose()
}

/// Writes `content` where the user chooses in the system save panel, which
/// asks before replacing an existing file. The app, not the backend, writes
/// it, so a sandboxed build uses the access the panel granted.
async fn save(
    window: &WebviewWindow,
    title: &str,
    file_name: &str,
    content: String,
) -> Result<(), CallError> {
    let dialog = json_dialog(window, title).set_file_name(file_name);
    Ok(run_blocking(move || {
        let Some(path) = chosen(dialog.blocking_save_file())? else {
            return Ok(());
        };
        write_chosen(&path, content.as_bytes())
            .map_err(|_| "Could not save the file. Check that the folder is writable.".into())
    })
    .await?)
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
