use std::{sync::Arc, time::Duration};

use desktop_runtime::{client::Client, preferences::UpdateChannel, protocol::Preference};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
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
pub async fn get_update_channel(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<UpdateChannel, String> {
    let client = client.inner().clone();
    crate::run_blocking(move || {
        let saved = client
            .preferences()
            .map_err(|_| "Could not read update preferences")?;
        Ok(saved.update_channel.unwrap_or_else(|| {
            if app.package_info().version.pre.is_empty() {
                UpdateChannel::Stable
            } else {
                UpdateChannel::Beta
            }
        }))
    })
    .await
}

#[tauri::command]
pub async fn set_update_channel(
    channel: UpdateChannel,
    pending: State<'_, PendingUpdate>,
    client: State<'_, Arc<Client>>,
) -> Result<UpdateChannel, String> {
    let mut pending = pending
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let client = client.inner().clone();
    crate::run_blocking(move || {
        client
            .set_preference(Preference::UpdateChannel(channel))
            .map(|_| ())
            .map_err(|_| "Could not save update channel".to_string())
    })
    .await?;
    *pending = None;
    Ok(channel)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    enabled: bool,
    current_version: String,
    version: Option<String>,
    channel_published: bool,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
}

#[derive(Default)]
pub struct UpdateProgress(std::sync::Mutex<DownloadProgress>);

#[tauri::command]
pub fn get_update_progress(
    progress: State<'_, UpdateProgress>,
) -> Result<DownloadProgress, String> {
    progress
        .0
        .lock()
        .map(|value| value.clone())
        .map_err(|_| "Update progress is unavailable".to_string())
}

pub fn can_close_progress(app: &AppHandle) -> bool {
    app.state::<UpdateProgress>()
        .0
        .lock()
        .is_ok_and(|value| value.error.is_some())
}

pub fn reset_progress(app: &AppHandle) {
    publish_progress(app, DownloadProgress::default());
}

fn publish_progress(app: &AppHandle, progress: DownloadProgress) {
    if let Ok(mut current) = app.state::<UpdateProgress>().0.lock() {
        *current = progress.clone();
    }
    let _ = app.emit("gateway://update-progress", progress);
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
        channel_published: true,
    };
    if !enabled {
        return Ok(info);
    }
    *pending = None;
    let client = app.state::<Arc<Client>>();
    let channel = get_update_channel(app.clone(), client).await?;
    let channel_name = match channel {
        UpdateChannel::Beta => "beta",
        UpdateChannel::Stable => "stable",
    };
    let endpoint = format!("https://github.com/Dstack-TEE/private-ai-gateway/releases/download/desktop-updates-{channel_name}/latest.json");
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint
            .parse()
            .map_err(|_| "Invalid update endpoint")?])
        .map_err(|_| "Invalid update endpoint")?
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Updates are not configured correctly for this build")?;
    let update = match updater.check().await {
        Ok(update) => update,
        // The plugin groups HTTP failures as ReleaseNotFound. Only a confirmed
        // 404 means the selected channel has not published a feed yet.
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => {
            let response = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| "Could not check the update channel")?
                .head(&endpoint)
                .send()
                .await
                .map_err(|_| "Could not reach the update channel")?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                info.channel_published = false;
                return Ok(info);
            }
            return Err("The update channel is temporarily unavailable".to_string());
        }
        Err(_) => return Err("Could not check for updates. Try again later.".to_string()),
    };
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
    client: State<'_, Arc<Client>>,
) -> Result<(), String> {
    let result = install(app.clone(), pending, client).await;
    if let Err(error) = &result {
        publish_progress(
            &app,
            DownloadProgress {
                error: Some(error.clone()),
                ..Default::default()
            },
        );
    }
    result
}

async fn install(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    client: State<'_, Arc<Client>>,
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
        publish_progress(&app, DownloadProgress { downloaded, total, error: None });
    }, || {}).await.map_err(|_| "The update could not be downloaded or its signature could not be verified. Check for updates to retry.")?;
    let client = client.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        client.install_update(|| {
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
