use std::{sync::Arc, time::Duration};

use desktop_runtime::{
    client::Client,
    preferences::{self, UpdateChannel},
    protocol::{rpc, Preference},
};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

pub(crate) struct DownloadedUpdate {
    channel: UpdateChannel,
    update: Update,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct PreparedUpdate(pub(crate) tokio::sync::Mutex<Option<DownloadedUpdate>>);

fn matches_channel(version: &str, channel: UpdateChannel) -> bool {
    let Ok(version) = semver::Version::parse(version) else {
        return false;
    };
    match channel {
        UpdateChannel::Stable => version.pre.is_empty(),
        UpdateChannel::Beta => version.pre.as_str().starts_with("beta."),
    }
}

fn channel_name(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Beta => "beta",
        UpdateChannel::Stable => "stable",
    }
}

fn channel_endpoint(
    configured: &str,
    channel: UpdateChannel,
    target: &str,
) -> Result<tauri::Url, String> {
    let mut endpoint = tauri::Url::parse(configured)
        .map_err(|_| "Updates are not configured correctly for this build")?;
    let path = endpoint.path();
    let marker = path
        .rfind("/desktop-updates-")
        .ok_or("Updates are not configured correctly for this build")?;
    let feed = &path[marker + 1..];
    if feed != "desktop-updates-beta/latest.json" && feed != "desktop-updates-stable/latest.json" {
        return Err("Updates are not configured correctly for this build".to_string());
    }
    endpoint.set_path(&format!(
        "{}/desktop-updates-{}/latest-{target}.json",
        &path[..marker],
        channel_name(channel)
    ));
    Ok(endpoint)
}

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

async fn update_channel(app: &AppHandle) -> Result<UpdateChannel, String> {
    let default = if app.package_info().version.pre.is_empty() {
        UpdateChannel::Stable
    } else {
        UpdateChannel::Beta
    };
    crate::run_blocking(move || {
        let channel = match preferences::load() {
            Ok(saved) => saved.update_channel.unwrap_or(default),
            Err(error) => {
                eprintln!("Could not read update preferences; using the build channel: {error}");
                default
            }
        };
        Ok(channel)
    })
    .await
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    enabled: bool,
    system_managed: bool,
    current_version: String,
    channel: UpdateChannel,
    version: Option<String>,
    channel_published: bool,
}

#[tauri::command]
pub async fn prepare_update(
    app: AppHandle,
    prepared: State<'_, PreparedUpdate>,
) -> Result<UpdateInfo, String> {
    let system_managed = system_managed();
    let enabled = crate::distribution::CAPABILITIES.native_updates
        && app.config().plugins.0.contains_key("updater")
        && !system_managed;
    let mut prepared = prepared
        .0
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let channel = update_channel(&app).await?;
    let mut info = UpdateInfo {
        enabled,
        system_managed,
        current_version: app.package_info().version.to_string(),
        channel,
        version: None,
        channel_published: true,
    };
    if !enabled {
        *prepared = None;
        return Ok(info);
    }
    let channel_name = channel_name(channel);
    let target =
        tauri_plugin_updater::target().ok_or("Updates are unavailable on this platform")?;
    let endpoint = channel_endpoint(&configured_endpoint(&app)?, channel, &target)?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint.clone()])
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
                .head(endpoint.clone())
                .send()
                .await
                .map_err(|_| "Could not reach the update channel")?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                *prepared = None;
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
        *prepared = None;
        return Err("The update does not match the selected channel".to_string());
    }
    let Some(update) = update else {
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

#[cfg(target_os = "linux")]
fn system_managed() -> bool {
    std::path::Path::new("/usr/share/private-ai-proxy/package-manager").is_file()
        && std::env::current_exe()
            .is_ok_and(|path| path == std::path::Path::new("/usr/bin/private-ai-proxy-desktop"))
}

#[cfg(not(target_os = "linux"))]
fn system_managed() -> bool {
    false
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
    use super::{channel_endpoint, matches_channel, UpdateChannel};

    #[test]
    fn update_versions_must_belong_to_selected_channel() {
        assert!(matches_channel("0.2.0", UpdateChannel::Stable));
        assert!(matches_channel("0.2.0-beta.10", UpdateChannel::Beta));
        assert!(!matches_channel("0.2.0-beta.1", UpdateChannel::Stable));
        assert!(!matches_channel("0.2.0", UpdateChannel::Beta));
        assert!(!matches_channel("0.2.0-rc.1", UpdateChannel::Beta));
        assert!(!matches_channel("invalid", UpdateChannel::Stable));
    }

    #[test]
    fn update_channels_derive_from_the_configured_feed() {
        let configured = "https://example.test/releases/download/desktop-updates-beta/latest.json";
        let endpoint = channel_endpoint(configured, UpdateChannel::Stable, "windows-x86_64")
            .expect("valid update endpoint");
        assert_eq!(
            endpoint.as_str(),
            "https://example.test/releases/download/desktop-updates-stable/latest-windows-x86_64.json"
        );
        assert!(channel_endpoint(
            "https://example.test/releases/latest.json",
            UpdateChannel::Beta,
            "darwin-aarch64"
        )
        .is_err());
    }
}
