//! Release channel policy shared by the in-app updater and notify-only installs.
//!
//! The desktop DMG, NSIS, DEB, and RPM installs download and verify signed
//! packages through the Tauri updater. Every other installation belongs to a
//! package manager or was installed by hand, so it only learns that a newer
//! release exists and shows the exact steps that install it.

use std::{path::Path, time::Duration};

use serde::{Deserialize, Serialize};

use crate::config::{self, UpdateChannel};

/// The channel feed configured for release builds; development builds have none.
pub const RELEASE_FEED: Option<&str> = option_env!("TAURI_UPDATER_ENDPOINT");

const INVALID_FEED: &str = "Updates are not configured correctly for this build";
const PACKAGE_MANAGER_MARKER: &str = "/usr/share/private-ai-proxy/package-manager";

pub fn channel_name(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Beta => "beta",
        UpdateChannel::Stable => "stable",
    }
}

/// Feeds consulted for a channel. Beta users also follow the stable feed so a
/// stable release newer than the latest beta is never withheld from them.
pub fn feeds(channel: UpdateChannel) -> &'static [UpdateChannel] {
    match channel {
        UpdateChannel::Beta => &[UpdateChannel::Beta, UpdateChannel::Stable],
        UpdateChannel::Stable => &[UpdateChannel::Stable],
    }
}

/// Whether a release version may be published in `feed`.
pub fn belongs_to_feed(version: &str, feed: UpdateChannel) -> bool {
    let Ok(version) = semver::Version::parse(version) else {
        return false;
    };
    match feed {
        UpdateChannel::Stable => version.pre.is_empty(),
        UpdateChannel::Beta => version.pre.as_str().starts_with("beta."),
    }
}

/// The saved channel, defaulting to the channel of the running build.
pub fn selected_channel(current_version: &str) -> UpdateChannel {
    let default =
        if semver::Version::parse(current_version).is_ok_and(|version| !version.pre.is_empty()) {
            UpdateChannel::Beta
        } else {
            UpdateChannel::Stable
        };
    match config::load() {
        Ok(saved) => saved.update_channel.unwrap_or(default),
        Err(error) => {
            crate::diagnostic!(
                "Could not read the update channel; using the build channel: {error}"
            );
            default
        }
    }
}

/// Splits `https://github.com/<repo>/releases/download/desktop-updates-<channel>/latest.json`
/// into the release download root and validates the configured channel feed.
fn release_root(configured: &str) -> Result<url::Url, String> {
    let mut root = url::Url::parse(configured).map_err(|_| INVALID_FEED)?;
    let path = root.path().to_owned();
    let marker = path.rfind("/desktop-updates-").ok_or(INVALID_FEED)?;
    let feed = &path[marker + 1..];
    if feed != "desktop-updates-beta/latest.json" && feed != "desktop-updates-stable/latest.json" {
        return Err(INVALID_FEED.into());
    }
    root.set_path(&path[..=marker]);
    Ok(root)
}

/// The per-platform manifest of `feed` derived from the configured feed.
pub fn feed_url(configured: &str, feed: UpdateChannel, target: &str) -> Result<url::Url, String> {
    release_root(configured)?
        .join(&format!(
            "desktop-updates-{}/latest-{target}.json",
            channel_name(feed)
        ))
        .map_err(|_| INVALID_FEED.into())
}

/// The updater's `{os}-{arch}` manifest target for this build.
pub fn target() -> Option<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "windows"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    Some(format!("{os}-{arch}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum Installation {
    /// Desktop app that installs signed updates in-app.
    DesktopApp,
    /// Desktop app owned by pacman.
    DesktopPacman,
    /// CLI-only native packages.
    Deb,
    Rpm,
    Pacman,
    /// A native package from a release that did not record its package manager.
    SystemPackage,
    Npm,
    Portable,
}

/// Classifies the installation that owns the running executable.
pub fn installation() -> Installation {
    let Some(executable) = std::env::current_exe().and_then(std::fs::canonicalize).ok() else {
        return Installation::Portable;
    };
    let directory = executable.parent().unwrap_or(Path::new(""));
    let desktop = directory
        .join(if cfg!(windows) {
            "private-ai-proxy-desktop.exe"
        } else {
            "private-ai-proxy-desktop"
        })
        .is_file();
    let package_manager = std::fs::read_to_string(PACKAGE_MANAGER_MARKER).ok();
    classify(
        directory,
        desktop,
        package_manager.as_deref().map(str::trim),
    )
}

fn classify(directory: &Path, desktop: bool, package_manager: Option<&str>) -> Installation {
    let packaged = cfg!(target_os = "linux")
        && (directory == Path::new("/usr/bin")
            || directory == Path::new("/usr/libexec/private-ai-proxy"));
    let package_manager = package_manager.filter(|_| packaged);
    if desktop {
        return if package_manager == Some("pacman") && directory == Path::new("/usr/bin") {
            Installation::DesktopPacman
        } else {
            Installation::DesktopApp
        };
    }
    match package_manager {
        Some("deb") => Installation::Deb,
        Some("rpm") => Installation::Rpm,
        Some("pacman") => Installation::Pacman,
        _ if packaged => Installation::SystemPackage,
        _ if directory
            .components()
            .any(|component| component.as_os_str() == "node_modules") =>
        {
            Installation::Npm
        }
        _ => Installation::Portable,
    }
}

#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNotice {
    pub installation: Installation,
    pub channel: UpdateChannel,
    pub current_version: String,
    /// A newer release in the selected channel.
    pub version: Option<String>,
    /// Shell steps that install `version`, when the installation has them.
    pub commands: Vec<String>,
    /// Portable archive to extract into a fresh directory.
    pub download_url: Option<String>,
    pub channel_published: bool,
}

/// The desktop app's update check result for the renderer.
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub enabled: bool,
    pub system_managed: bool,
    pub current_version: String,
    pub channel: UpdateChannel,
    pub version: Option<String>,
    pub channel_published: bool,
    /// Steps that install `version` when a package manager or the user owns
    /// the installation.
    pub upgrade_commands: Vec<String>,
    /// Portable archive to extract into a fresh directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub download_url: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    channel: String,
}

/// Checks the release feeds of `channel` without downloading a package.
pub async fn check(
    configured: &str,
    channel: UpdateChannel,
    current_version: &str,
    installation: Installation,
) -> Result<UpdateNotice, String> {
    let target = target().ok_or("Updates are unavailable on this platform")?;
    let current = semver::Version::parse(current_version)
        .map_err(|_| "The installed version is invalid".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Could not check for updates")?;
    let mut latest: Option<semver::Version> = None;
    let mut channel_published = true;
    for &feed in feeds(channel) {
        match published_version(&client, feed_url(configured, feed, &target)?, feed).await {
            Ok(Some(version)) => {
                if version > current && latest.as_ref().is_none_or(|latest| version > *latest) {
                    latest = Some(version);
                }
            }
            Ok(None) if feed == channel => channel_published = false,
            Ok(None) => {}
            // The stable feed only supplements beta; its failure never hides beta releases.
            Err(_) if feed != channel => {}
            Err(error) => return Err(error),
        }
    }
    let (commands, download_url) = match &latest {
        Some(version) => upgrade_steps(
            installation,
            release_root(configured)?.as_str(),
            &version.to_string(),
        ),
        None => (Vec::new(), None),
    };
    Ok(UpdateNotice {
        installation,
        channel,
        current_version: current_version.into(),
        version: latest.map(|version| version.to_string()),
        commands,
        download_url,
        channel_published,
    })
}

/// The version a feed announces, or `None` when the feed is not published.
async fn published_version(
    client: &reqwest::Client,
    url: url::Url,
    feed: UpdateChannel,
) -> Result<Option<semver::Version>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Could not check for updates. Try again later.")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err("Could not check for updates. Try again later.".into());
    }
    let manifest: Manifest = response
        .json()
        .await
        .map_err(|_| "The update feed is invalid")?;
    if manifest.channel != channel_name(feed) || !belongs_to_feed(&manifest.version, feed) {
        return Err("The update does not match the selected channel".into());
    }
    semver::Version::parse(&manifest.version)
        .map(Some)
        .map_err(|_| "The update feed is invalid".into())
}

/// Checks the release feeds for the running executable's installation.
pub async fn check_installation() -> Result<UpdateNotice, String> {
    let configured = RELEASE_FEED.ok_or("Update checks are unavailable in this build")?;
    let version = crate::protocol::BUILD_VERSION;
    let (channel, installation) =
        tokio::task::spawn_blocking(move || (selected_channel(version), installation()))
            .await
            .map_err(|_| "Could not check for updates")?;
    check(configured, channel, version, installation).await
}

/// `root` and `version` come from the compiled feed and a parsed SemVer, so the
/// steps contain no text from the downloaded manifest.
fn upgrade_steps(
    installation: Installation,
    root: &str,
    version: &str,
) -> (Vec<String>, Option<String>) {
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    // Mirrors artifactName() in scripts/release-artifacts.mjs.
    let asset = |cli: bool, suffix: &str| {
        let kind = if cli { "-cli" } else { "" };
        format!(
            "{root}desktop-v{version}/private-ai-proxy{kind}-{version}-{platform}-{arch}{suffix}"
        )
    };
    let stop = "private-ai-proxy --yes service stop".to_string();
    match installation {
        Installation::DesktopApp | Installation::SystemPackage => (Vec::new(), None),
        Installation::DesktopPacman => (
            vec![
                stop,
                format!("sudo pacman -U {}", asset(false, ".pkg.tar.zst")),
            ],
            None,
        ),
        Installation::Pacman => (
            vec![
                stop,
                format!("sudo pacman -U {}", asset(true, ".pkg.tar.zst")),
            ],
            None,
        ),
        Installation::Rpm => (
            vec![stop, format!("sudo rpm -U {}", asset(true, ".rpm"))],
            None,
        ),
        Installation::Deb => {
            let url = asset(true, ".deb");
            let file = url.rsplit('/').next().unwrap_or_default().to_owned();
            (
                vec![
                    stop,
                    format!("curl -fLO {url}"),
                    format!("sudo apt install ./{file}"),
                ],
                None,
            )
        }
        Installation::Npm => (
            vec![
                stop,
                format!("npm install --global private-ai-proxy@{version}"),
            ],
            None,
        ),
        Installation::Portable => (
            Vec::new(),
            Some(asset(true, if cfg!(windows) { ".zip" } else { ".tar.gz" })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str =
        "https://example.test/o/r/releases/download/desktop-updates-beta/latest.json";

    #[test]
    fn beta_follows_stable_releases_but_stable_never_receives_betas() {
        assert_eq!(
            feeds(UpdateChannel::Beta),
            [UpdateChannel::Beta, UpdateChannel::Stable]
        );
        assert_eq!(feeds(UpdateChannel::Stable), [UpdateChannel::Stable]);
        assert!(belongs_to_feed("0.2.0", UpdateChannel::Stable));
        assert!(belongs_to_feed("0.2.0-beta.10", UpdateChannel::Beta));
        assert!(!belongs_to_feed("0.2.0-beta.1", UpdateChannel::Stable));
        assert!(!belongs_to_feed("0.2.0", UpdateChannel::Beta));
        assert!(!belongs_to_feed("0.2.0-rc.1", UpdateChannel::Beta));
        assert!(!belongs_to_feed("invalid", UpdateChannel::Stable));
    }

    #[test]
    fn channel_feeds_derive_from_the_configured_feed() {
        assert_eq!(
            feed_url(FEED, UpdateChannel::Stable, "windows-x86_64")
                .expect("valid feed")
                .as_str(),
            "https://example.test/o/r/releases/download/desktop-updates-stable/latest-windows-x86_64.json"
        );
        assert!(feed_url(
            "https://example.test/releases/latest.json",
            UpdateChannel::Beta,
            "darwin-aarch64"
        )
        .is_err());
    }

    #[test]
    fn installations_are_classified_by_their_owner() {
        let classify =
            |directory: &str, desktop, manager| classify(Path::new(directory), desktop, manager);
        assert_eq!(
            classify(
                "/Applications/Private AI Proxy.app/Contents/MacOS",
                true,
                None
            ),
            Installation::DesktopApp
        );
        assert_eq!(
            classify(
                "/home/u/.npm-global/lib/node_modules/@dstack/private-ai-proxy-linux-x64/bin",
                false,
                None
            ),
            Installation::Npm
        );
        assert_eq!(
            classify("/home/u/private-ai-proxy-cli-0.1.7-linux-x64", false, None),
            Installation::Portable
        );
        if cfg!(target_os = "linux") {
            assert_eq!(classify("/usr/bin", true, None), Installation::DesktopApp);
            assert_eq!(
                classify("/usr/bin", true, Some("pacman")),
                Installation::DesktopPacman
            );
            let libexec = "/usr/libexec/private-ai-proxy";
            assert_eq!(classify(libexec, false, Some("deb")), Installation::Deb);
            assert_eq!(classify(libexec, false, Some("rpm")), Installation::Rpm);
            assert_eq!(
                classify(libexec, false, Some("pacman")),
                Installation::Pacman
            );
            assert_eq!(classify(libexec, false, None), Installation::SystemPackage);
            assert_eq!(
                classify("/opt/pap", false, Some("deb")),
                Installation::Portable,
                "the marker only describes package-owned paths"
            );
        }
    }

    #[test]
    fn upgrade_steps_name_exact_release_assets() {
        let root = release_root(FEED).expect("valid feed");
        let root = root.as_str();
        assert_eq!(root, "https://example.test/o/r/releases/download/");
        let (npm, _) = upgrade_steps(Installation::Npm, root, "0.1.8");
        assert_eq!(
            npm,
            [
                "private-ai-proxy --yes service stop",
                "npm install --global private-ai-proxy@0.1.8"
            ]
        );
        assert_eq!(
            upgrade_steps(Installation::DesktopApp, root, "0.1.8"),
            (Vec::new(), None)
        );
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(
                upgrade_steps(Installation::DesktopPacman, root, "0.1.8-beta.2").0[1],
                "sudo pacman -U https://example.test/o/r/releases/download/desktop-v0.1.8-beta.2/private-ai-proxy-0.1.8-beta.2-linux-x64.pkg.tar.zst"
            );
            assert_eq!(
                upgrade_steps(Installation::Deb, root, "0.1.8").0[1..],
                [
                    "curl -fLO https://example.test/o/r/releases/download/desktop-v0.1.8/private-ai-proxy-cli-0.1.8-linux-x64.deb",
                    "sudo apt install ./private-ai-proxy-cli-0.1.8-linux-x64.deb"
                ]
            );
            assert_eq!(
                upgrade_steps(Installation::Portable, root, "0.1.8").1.as_deref(),
                Some("https://example.test/o/r/releases/download/desktop-v0.1.8/private-ai-proxy-cli-0.1.8-linux-x64.tar.gz")
            );
        }
    }
}
