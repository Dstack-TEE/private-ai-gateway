use std::{sync::Arc, time::Duration};

use desktop_core::{
    client::{CallError, Client},
    config::UpdateChannel,
    protocol::{rpc, Preference},
    updates::{self, Installation, UpdateInfo},
};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

pub(crate) struct DownloadedUpdate {
    channel: UpdateChannel,
    update: Update,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct PreparedUpdate(pub(crate) tokio::sync::Mutex<Option<DownloadedUpdate>>);

fn configured_endpoint(app: &AppHandle) -> Result<String, String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|plugin| plugin.get("endpoints"))
        .and_then(serde_json::Value::as_array)
        .and_then(|endpoints| endpoints.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Updates are not configured correctly for this build".to_string())
}

#[tauri::command]
pub async fn set_update_channel(
    channel: UpdateChannel,
    prepared: State<'_, PreparedUpdate>,
    client: State<'_, Arc<Client>>,
) -> Result<UpdateChannel, CallError> {
    crate::distribution::require(
        crate::distribution::CAPABILITIES.native_updates,
        "Updates are managed by the App Store",
    )?;
    let mut prepared = prepared
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let client = client.inner().clone();
    crate::run_blocking(move || {
        client
            .call(rpc::SetPreference {
                change: Preference::UpdateChannel(channel),
            })
            .map(|_| ())
            .map_err(|_| "Could not save update channel".to_string())
    })
    .await?;
    *prepared = None;
    Ok(channel)
}

#[tauri::command]
pub async fn prepare_update(
    app: AppHandle,
    prepared: State<'_, PreparedUpdate>,
) -> Result<UpdateInfo, CallError> {
    let configured = crate::distribution::CAPABILITIES.native_updates
        && app.config().plugins.0.contains_key("updater");
    let mut prepared = prepared
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let current_version = app.package_info().version.to_string();
    let default = updates::build_channel(&current_version);
    // The backend's settings, like every other preference the app reads, so
    // the app and the backend always agree on the file in effect.
    let client = app.state::<Arc<Client>>().inner().clone();
    let (channel, system_managed) = crate::run_blocking(move || {
        let channel = match client.call(rpc::Settings) {
            Ok(saved) => saved.update_channel.unwrap_or(default),
            Err(error) => {
                tracing::warn!(
                    "Could not read the update channel; using the build channel: {error}"
                );
                default
            }
        };
        Ok((
            channel,
            updates::installation() == Installation::DesktopPacman,
        ))
    })
    .await?;
    let mut info = UpdateInfo {
        enabled: configured && !system_managed,
        system_managed,
        current_version,
        channel,
        version: None,
        upgrade_commands: Vec::new(),
        download_url: None,
    };
    if !configured || system_managed {
        *prepared = None;
    }
    if !configured {
        return Ok(info);
    }
    let feed = configured_endpoint(&app)?;
    if system_managed {
        // pacman owns the files: announce the release and its exact upgrade steps.
        let notice = updates::check(
            &feed,
            channel,
            &info.current_version,
            Installation::DesktopPacman,
        )
        .await?;
        info.version = notice.version;
        info.upgrade_commands = notice.commands;
        return Ok(info);
    }
    let endpoint = updates::feed_url(&feed, channel)?;
    let update = match feed_update(&app, endpoint, channel).await {
        Ok(Some(update)) => update,
        Ok(None) => {
            *prepared = None;
            return Ok(info);
        }
        Err(error) => {
            *prepared = None;
            return Err(error.into());
        }
    };
    let version = update.version.clone();
    if prepared
        .as_ref()
        .is_some_and(|download| download.channel == channel && download.update.version == version)
    {
        info.version = Some(version);
        return Ok(info);
    }
    // Only one release can be installed. Drop an older prepared archive before
    // downloading the latest release selected by the official updater.
    *prepared = None;
    // Tauri verifies the downloaded archive signature before returning the bytes.
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|_| "The update could not be downloaded or verified. Retrying automatically.")?;
    *prepared = Some(DownloadedUpdate {
        channel,
        update,
        bytes,
    });
    info.version = Some(version);
    Ok(info)
}

/// The newer release `feed` announces. A release outside the feed's channel
/// is not an update for it.
async fn feed_update(
    app: &AppHandle,
    endpoint: tauri::Url,
    feed: UpdateChannel,
) -> Result<Option<Update>, String> {
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|_| "Invalid update endpoint")?
        .version_comparator(move |current, release| {
            release.version > current && updates::belongs_to_feed(&release.version, feed)
        })
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Updates are not configured correctly for this build")?;
    updater.check().await.map_err(|error| match error {
        // The plugin reports every unsuccessful feed response this way.
        tauri_plugin_updater::Error::ReleaseNotFound => {
            "The update channel is temporarily unavailable".to_string()
        }
        _ => "Could not check for updates. Try again later.".to_string(),
    })
}

#[tauri::command]
pub async fn restart_to_update(
    app: AppHandle,
    prepared: State<'_, PreparedUpdate>,
    client: State<'_, Arc<Client>>,
) -> Result<(), CallError> {
    crate::distribution::require(
        crate::distribution::CAPABILITIES.native_updates,
        "Updates are managed by the App Store",
    )?;
    let mut prepared = prepared
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let download = prepared.take().ok_or("The update is not ready yet")?;
    drop(prepared);
    let client = client.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        client.install_update(|| {
            download
                .update
                .install(download.bytes)
                .map_err(|_| "Installation failed.".to_string())
        })
    })
    .await
    .map_err(|_| "The update task could not complete")??;
    // The Windows updater exits and relaunches the process itself. macOS and
    // Linux return after installation and restart here.
    app.restart();
}
