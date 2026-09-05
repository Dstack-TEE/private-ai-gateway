use std::{sync::Arc, time::Duration};

use desktop_runtime::controller::DesktopRuntime;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct PendingUpdate(tokio::sync::Mutex<Option<Update>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    enabled: bool,
    current_version: String,
    version: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    downloaded: u64,
    total: Option<u64>,
}

#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<UpdateInfo, String> {
    let mut pending = pending
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let enabled = app.config().plugins.0.contains_key("updater");
    let mut info = UpdateInfo {
        enabled,
        current_version: app.package_info().version.to_string(),
        version: None,
    };
    if !enabled {
        return Ok(info);
    }
    *pending = None;
    let updater = app
        .updater_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Updates are not configured correctly for this build")?;
    *pending = updater
        .check()
        .await
        .map_err(|_| "Could not check for updates. Try again later.")?;
    info.version = pending.as_ref().map(|update| update.version.clone());
    Ok(info)
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    runtime: State<'_, Arc<DesktopRuntime>>,
) -> Result<(), String> {
    let mut pending = pending
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let update = pending
        .take()
        .ok_or("Check for an update before installing")?;
    let mut downloaded = 0_u64;
    // The official plugin verifies the archive signature before returning these bytes.
    let bytes = update.download(|chunk, total| {
        downloaded = downloaded.saturating_add(chunk as u64);
        let _ = app.emit_to("main", "gateway://update-progress", DownloadProgress { downloaded, total });
    }, || {}).await.map_err(|_| "The update could not be downloaded or its signature could not be verified. Check for updates to retry.")?;
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        runtime.install_update(|| {
            update.install(bytes).map_err(|_| {
                "Installation failed. Protection is stopped; check for updates to retry."
                    .to_string()
            })
        })
    })
    .await
    .map_err(|_| "The update task could not complete")??;
    app.restart();
}
