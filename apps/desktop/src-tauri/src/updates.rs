use std::{sync::Arc, time::Duration};

use desktop_runtime::controller::DesktopRuntime;
use desktop_runtime::preferences::{self, UpdateChannel};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct PendingUpdate(tokio::sync::Mutex<Option<Update>>);

fn matches_channel(version: &str, channel: UpdateChannel) -> bool {
    let Ok(version) = semver::Version::parse(version) else {
        return false;
    };
    match channel {
        UpdateChannel::Stable => version.pre.is_empty(),
        UpdateChannel::Beta => version.pre.as_str().starts_with("beta."),
    }
}

#[tauri::command]
pub fn get_update_channel(app: AppHandle) -> Result<UpdateChannel, String> {
    let saved = preferences::load().map_err(|_| "Could not read update preferences")?;
    Ok(saved.update_channel.unwrap_or_else(|| {
        if app.package_info().version.pre.is_empty() {
            UpdateChannel::Stable
        } else {
            UpdateChannel::Beta
        }
    }))
}

#[tauri::command]
pub async fn set_update_channel(
    channel: UpdateChannel,
    pending: State<'_, PendingUpdate>,
) -> Result<UpdateChannel, String> {
    let mut pending = pending
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    preferences::update(|preferences| preferences.update_channel = Some(channel))
        .map_err(|_| "Could not save update channel")?;
    *pending = None;
    Ok(channel)
}

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
    let channel = get_update_channel(app.clone())?;
    let channel_name = match channel {
        UpdateChannel::Beta => "beta",
        UpdateChannel::Stable => "stable",
    };
    let endpoint = format!("https://github.com/Dstack-TEE/private-ai-gateway/releases/download/desktop-updates-{channel_name}/latest.json")
        .parse().map_err(|_| "Invalid update endpoint")?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|_| "Invalid update endpoint")?
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Updates are not configured correctly for this build")?;
    let update = updater
        .check()
        .await
        .map_err(|_| "Could not check for updates. Try again later.")?;
    if update.as_ref().is_some_and(|update| {
        update
            .raw_json
            .get("channel")
            .and_then(serde_json::Value::as_str)
            != Some(channel_name)
            || !matches_channel(&update.version, channel)
    }) {
        return Err("The update does not match the selected channel".to_string());
    }
    *pending = update;
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

#[cfg(test)]
mod tests {
    use super::{matches_channel, UpdateChannel};

    #[test]
    fn update_versions_must_belong_to_selected_channel() {
        assert!(matches_channel("0.2.0", UpdateChannel::Stable));
        assert!(matches_channel("0.2.0-beta.10", UpdateChannel::Beta));
        assert!(!matches_channel("0.2.0-beta.1", UpdateChannel::Stable));
        assert!(!matches_channel("0.2.0", UpdateChannel::Beta));
        assert!(!matches_channel("0.2.0-rc.1", UpdateChannel::Beta));
        assert!(!matches_channel("invalid", UpdateChannel::Stable));
    }
}
