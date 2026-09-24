use std::{sync::Arc, time::Duration};

use desktop_core::{
    client::Client,
    config::UpdateChannel,
    protocol::{rpc, Preference},
    updates::{self, Installation, UpdateInfo},
};
use tauri::{AppHandle, State};
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
) -> Result<UpdateChannel, String> {
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
) -> Result<UpdateInfo, String> {
    let configured = crate::distribution::CAPABILITIES.native_updates
        && app.config().plugins.0.contains_key("updater");
    let mut prepared = prepared
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let current_version = app.package_info().version.to_string();
    let build_version = current_version.clone();
    let (channel, system_managed) = crate::run_blocking(move || {
        Ok((
            updates::selected_channel(&build_version),
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
        channel_published: true,
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
        info.channel_published = notice.channel_published;
        return Ok(info);
    }
    let target =
        tauri_plugin_updater::target().ok_or("Updates are unavailable on this platform")?;
    let mut selected: Option<Update> = None;
    for &source in updates::feeds(channel) {
        let endpoint = updates::feed_url(&feed, source, &target)?;
        match feed_update(&app, endpoint, source).await {
            Ok(Checked::Update(update)) => {
                if selected
                    .as_ref()
                    .is_none_or(|current| newer(&update.version, &current.version))
                {
                    selected = Some(*update);
                }
            }
            Ok(Checked::Unpublished) if source == channel => info.channel_published = false,
            Ok(_) => {}
            // The stable feed only supplements beta; its failure never hides beta releases.
            Err(_) if source != channel => {}
            Err(error) => {
                *prepared = None;
                return Err(error);
            }
        }
    }
    let Some(update) = selected else {
        *prepared = None;
        return Ok(info);
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

enum Checked {
    Update(Box<Update>),
    Current,
    Unpublished,
}

async fn feed_update(
    app: &AppHandle,
    endpoint: tauri::Url,
    feed: UpdateChannel,
) -> Result<Checked, String> {
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint.clone()])
        .map_err(|_| "Invalid update endpoint")?
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Updates are not configured correctly for this build")?;
    match updater.check().await {
        Ok(Some(update)) => {
            if update
                .raw_json
                .get("channel")
                .and_then(serde_json::Value::as_str)
                != Some(updates::channel_name(feed))
                || !updates::belongs_to_feed(&update.version, feed)
            {
                return Err("The update does not match the selected channel".into());
            }
            Ok(Checked::Update(Box::new(update)))
        }
        Ok(None) => Ok(Checked::Current),
        // The plugin groups HTTP failures as ReleaseNotFound. Only a confirmed
        // 404 means the channel has not published a feed yet.
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => {
            if feed_missing(endpoint).await? {
                Ok(Checked::Unpublished)
            } else {
                Err("The update channel is temporarily unavailable".into())
            }
        }
        Err(_) => Err("Could not check for updates. Try again later.".into()),
    }
}

async fn feed_missing(endpoint: tauri::Url) -> Result<bool, String> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Could not check the update channel")?
        .head(endpoint)
        .send()
        .await
        .map_err(|_| "Could not reach the update channel")?;
    Ok(response.status() == reqwest::StatusCode::NOT_FOUND)
}

fn newer(candidate: &str, current: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(current),
    ) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

#[tauri::command]
pub async fn restart_to_update(
    app: AppHandle,
    prepared: State<'_, PreparedUpdate>,
    client: State<'_, Arc<Client>>,
) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn a_stable_release_supersedes_its_betas() {
        assert!(newer("0.1.7", "0.1.7-beta.3"));
        assert!(newer("0.1.8-beta.1", "0.1.7"));
        assert!(!newer("0.1.7-beta.3", "0.1.7"));
        assert!(!newer("invalid", "0.1.7"));
    }
}
