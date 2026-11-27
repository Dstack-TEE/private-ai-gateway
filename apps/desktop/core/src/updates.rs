//! Release channel policy shared by the in-app updater and notify-only installs.
//!
//! The desktop DMG, NSIS, DEB, and RPM installs download and verify signed
//! packages through the Tauri updater. Every other installation belongs to a
//! package manager or was installed by hand, so it only learns that a newer
//! release exists and shows the exact steps that install it.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::config::{self, UpdateChannel};

/// The channel feed configured for release builds; development builds have none.
pub const RELEASE_FEED: Option<&str> = option_env!("TAURI_UPDATER_ENDPOINT");

const INVALID_FEED: &str = "Updates are not configured correctly for this build";
const PACKAGE_MANAGER_MARKER: &str = "/usr/share/private-ai-proxy/package-manager";
const RPM_SIGNING_KEY_URL: &str = "https://github.com/Dstack-TEE/private-ai-gateway/raw/main/apps/desktop/packaging/linux-signing-key.asc";

pub fn channel_name(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Beta => "beta",
        UpdateChannel::Stable => "stable",
    }
}

/// Whether a release version may be published in `feed`. Stable releases are
/// also published to the beta feed, so beta users receive them too.
pub fn belongs_to_feed(version: &semver::Version, feed: UpdateChannel) -> bool {
    version.pre.is_empty()
        || feed == UpdateChannel::Beta && version.pre.as_str().starts_with("beta.")
}

/// Whether `release`, announced by `feed`, updates `current`: a newer release
/// of the feed, never a reinstall or a downgrade (after switching from beta to
/// stable, the stable feed may still announce an older release).
pub fn offers_update(
    current: &semver::Version,
    release: &semver::Version,
    feed: UpdateChannel,
) -> bool {
    release > current && belongs_to_feed(release, feed)
}

/// The channel of the running build, used until one is saved.
pub fn build_channel(current_version: &str) -> UpdateChannel {
    if semver::Version::parse(current_version).is_ok_and(|version| !version.pre.is_empty()) {
        UpdateChannel::Beta
    } else {
        UpdateChannel::Stable
    }
}

/// The saved channel as `config.toml` holds it, for processes that may run
/// without the backend (the CLI). The desktop app asks the backend instead.
pub fn selected_channel(current_version: &str) -> UpdateChannel {
    let default = build_channel(current_version);
    match config::load() {
        Ok(saved) => saved.update_channel.unwrap_or(default),
        Err(error) => {
            tracing::warn!("Could not read the update channel; using the build channel: {error}");
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

/// The static update manifest of `feed` derived from the configured feed.
pub fn feed_url(configured: &str, feed: UpdateChannel) -> Result<url::Url, String> {
    release_root(configured)?
        .join(&format!(
            "desktop-updates-{}/latest.json",
            channel_name(feed)
        ))
        .map_err(|_| INVALID_FEED.into())
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
    /// CLI installed by the official install script.
    InstallScript,
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
        .join(format!(
            "private-ai-proxy-desktop{}",
            std::env::consts::EXE_SUFFIX
        ))
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
    if cfg!(any(target_os = "linux", target_os = "macos"))
        && script_install_root(directory).is_some()
    {
        return Installation::InstallScript;
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

/// The official installer keeps each release in `versions/<semver>` and
/// points the user's command links at that directory. Canonicalizing the
/// executable before classification makes this work with custom install and
/// bin directories too.
fn script_install_root(directory: &Path) -> Option<&Path> {
    semver::Version::parse(directory.file_name()?.to_str()?).ok()?;
    let versions = directory.parent()?;
    if versions.file_name()? != "versions" {
        return None;
    }
    versions.parent()
}

struct ScriptInstallation {
    install_dir: PathBuf,
    command: Option<PathBuf>,
}

impl ScriptInstallation {
    fn current() -> Option<Self> {
        let executable = fs::canonicalize(std::env::current_exe().ok()?).ok()?;
        Self::discover(&executable, std::env::var_os("PATH").as_deref())
    }

    fn discover(executable: &Path, path: Option<&OsStr>) -> Option<Self> {
        let version_dir = executable.parent()?;
        let install_dir = script_install_root(version_dir)?.to_owned();
        let command = path.and_then(|path| {
            std::env::split_paths(path)
                .filter(|directory| directory.is_absolute())
                .flat_map(|directory| {
                    ["private-ai-proxy", "pap", "aci"].map(|name| directory.join(name))
                })
                .find(|command| {
                    fs::read_link(command).is_ok_and(|target| {
                        target.is_absolute()
                            && target.file_name() == Some(OsStr::new("private-ai-proxy"))
                            && target.parent().and_then(Path::file_name) == version_dir.file_name()
                            && target.parent().is_some_and(|parent| {
                                fs::canonicalize(parent).is_ok_and(|path| path == version_dir)
                            })
                    })
                })
        });
        Some(Self {
            install_dir,
            command,
        })
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
}

/// Checks the release feed of `channel` without downloading a package.
pub async fn check(
    configured: &str,
    channel: UpdateChannel,
    current_version: &str,
    installation: Installation,
) -> Result<UpdateNotice, String> {
    let current = semver::Version::parse(current_version)
        .map_err(|_| "The installed version is invalid".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Could not check for updates")?;
    let published = published_version(&client, feed_url(configured, channel)?, channel).await?;
    let latest = offers_update(&current, &published, channel).then_some(published);
    let script = (installation == Installation::InstallScript && latest.is_some())
        .then(ScriptInstallation::current)
        .flatten();
    let (commands, download_url) = match &latest {
        Some(version) => upgrade_steps(
            installation,
            release_root(configured)?.as_str(),
            &version.to_string(),
            std::env::consts::ARCH,
            script.as_ref(),
        )?,
        None => (Vec::new(), None),
    };
    Ok(UpdateNotice {
        installation,
        channel,
        current_version: current_version.into(),
        version: latest.map(|version| version.to_string()),
        commands,
        download_url,
    })
}

/// The version a feed announces.
async fn published_version(
    client: &reqwest::Client,
    url: url::Url,
    feed: UpdateChannel,
) -> Result<semver::Version, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Could not check for updates. Try again later.")?;
    if !response.status().is_success() {
        return Err("Could not check for updates. Try again later.".into());
    }
    let manifest: Manifest = response
        .json()
        .await
        .map_err(|_| "The update feed is invalid")?;
    let version =
        semver::Version::parse(&manifest.version).map_err(|_| "The update feed is invalid")?;
    if !belongs_to_feed(&version, feed) {
        return Err("The update does not match the selected channel".into());
    }
    Ok(version)
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

/// One step from its words, quoted as a POSIX shell would read them.
pub fn step(words: &[&str]) -> Result<String, String> {
    shlex::try_join(words.iter().copied()).map_err(|_| "Cannot write the upgrade command".into())
}

/// The first of the steps shown for a manual upgrade. It ends protection and
/// restores connected agents, as stopping by hand should.
pub const STOP_STEP: &str = "private-ai-proxy --yes service stop";

/// The command to restart through and the installer steps to run while the
/// caller holds the update locks and the backend is stopped.
pub fn install_script_update(version: &str) -> Result<(PathBuf, Vec<String>), String> {
    script_update(version, ScriptInstallation::current().as_ref())
}

fn script_update(
    version: &str,
    script: Option<&ScriptInstallation>,
) -> Result<(PathBuf, Vec<String>), String> {
    if let Some(script) = script {
        if let Some(command) = &script.command {
            return Ok((command.clone(), script_steps(version, Some(script), true)?));
        }
    }
    let manual = script_steps(version, script, false)?;
    Err(format!(
        "Cannot find a managed Private AI Proxy command link on PATH. Run these commands with the original install and bin directories:\n  {}",
        manual.join("\n  ")
    ))
}

fn script_steps(
    version: &str,
    script: Option<&ScriptInstallation>,
    backend_stopped: bool,
) -> Result<Vec<String>, String> {
    let version_number =
        semver::Version::parse(version).map_err(|_| "The update version is invalid")?;
    let channel = if version_number.pre.is_empty() {
        "stable"
    } else {
        "beta"
    };
    let install_dir = script.map_or(Path::new("/path/to/original/install"), |script| {
        &script.install_dir
    });
    let bin_dir = script
        .and_then(|script| script.command.as_deref())
        .and_then(Path::parent)
        .unwrap_or(Path::new("/path/to/original/bin"));
    let mut install = vec![
        "sh",
        "private-ai-proxy-install.sh",
        "--channel",
        channel,
        "--version",
        version,
        "--install-dir",
        install_dir
            .to_str()
            .ok_or("The install directory is not UTF-8")?,
        "--bin-dir",
        bin_dir.to_str().ok_or("The bin directory is not UTF-8")?,
    ];
    if backend_stopped {
        install.push("--skip-service-stop");
    }
    Ok(vec![
        step(&[
            "curl",
            "-fsSL",
            "https://redpill.ai/private-ai-proxy/install.sh",
            "-o",
            "private-ai-proxy-install.sh",
        ])?,
        step(&install)?,
    ])
}

/// Runs upgrade `steps` in order, in a scratch directory for what they
/// download, and stops at the first that fails. Each step and its output go
/// to stderr, leaving stdout to the caller's own result.
pub fn run(steps: &[String]) -> Result<(), String> {
    let directory = tempfile::tempdir()
        .map_err(|error| format!("Cannot create a download directory: {error}"))?;
    for step in steps {
        let words = shlex::split(step).ok_or("The upgrade command is invalid")?;
        let (program, arguments) = words
            .split_first()
            .ok_or("The upgrade command is invalid")?;
        eprintln!("$ {step}");
        let status = std::process::Command::new(program)
            .args(arguments)
            .current_dir(directory.path())
            .stdout(std::io::stderr())
            .status()
            .map_err(|error| format!("Cannot run `{step}`: {error}"))?;
        if !status.success() {
            return Err(format!("`{step}` failed"));
        }
    }
    Ok(())
}

/// `root` and `version` come from the compiled feed and a parsed SemVer, so the
/// steps contain no text from the downloaded manifest. Commands are POSIX
/// shell words joined by `shlex`.
fn upgrade_steps(
    installation: Installation,
    root: &str,
    version: &str,
    architecture: &str,
    script: Option<&ScriptInstallation>,
) -> Result<(Vec<String>, Option<String>), String> {
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = match architecture {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return Ok((Vec::new(), None)),
    };
    // Mirrors artifactName() in scripts/release-artifacts.mjs.
    let name = |cli: bool, suffix: &str| {
        let kind = if cli { "-cli" } else { "" };
        format!("private-ai-proxy{kind}-{version}-{platform}-{arch}{suffix}")
    };
    let tag = format!("desktop-v{version}");
    let asset =
        |name: &str| -> Result<String, String> { Ok(crate::endpoint(root, &[&tag, name])?.into()) };
    let command = step;
    let stop = STOP_STEP.to_string();
    Ok(match installation {
        Installation::DesktopApp | Installation::SystemPackage => (Vec::new(), None),
        Installation::DesktopPacman => (
            vec![
                stop,
                command(&[
                    "sudo",
                    "pacman",
                    "-U",
                    &asset(&name(false, ".pkg.tar.zst"))?,
                ])?,
            ],
            None,
        ),
        Installation::Pacman => (
            vec![
                stop,
                command(&["sudo", "pacman", "-U", &asset(&name(true, ".pkg.tar.zst"))?])?,
            ],
            None,
        ),
        Installation::Rpm => (
            vec![
                stop,
                command(&["sudo", "rpm", "--import", RPM_SIGNING_KEY_URL])?,
                command(&["sudo", "rpm", "-U", &asset(&name(true, ".rpm"))?])?,
            ],
            None,
        ),
        Installation::Deb => {
            let file = name(true, ".deb");
            (
                vec![
                    stop,
                    command(&["curl", "-fLO", &asset(&file)?])?,
                    command(&["sudo", "apt", "install", &format!("./{file}")])?,
                ],
                None,
            )
        }
        Installation::Npm => (
            vec![
                stop,
                command(&[
                    "npm",
                    "install",
                    "--global",
                    &format!("private-ai-proxy@{version}"),
                ])?,
            ],
            None,
        ),
        Installation::InstallScript => (script_steps(version, script, false)?, None),
        Installation::Portable => (
            Vec::new(),
            Some(asset(&name(
                true,
                if cfg!(windows) { ".zip" } else { ".tar.gz" },
            ))?),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str =
        "https://example.test/o/r/releases/download/desktop-updates-beta/latest.json";

    #[cfg(unix)]
    #[test]
    fn steps_run_in_order_and_stop_at_the_first_failure() {
        let directory = tempfile::tempdir().unwrap();
        let touch = |name: &str| {
            let path = directory.path().join(name);
            shlex::try_join(["touch", path.to_str().unwrap()]).unwrap()
        };
        let steps = [touch("first"), "false".into(), touch("second")];
        assert_eq!(run(&steps).unwrap_err(), "`false` failed");
        assert!(directory.path().join("first").exists());
        assert!(!directory.path().join("second").exists());
    }

    #[test]
    fn beta_feed_carries_stable_releases_but_stable_never_carries_betas() {
        let belongs =
            |version: &str, feed| belongs_to_feed(&semver::Version::parse(version).unwrap(), feed);
        assert!(belongs("0.2.0", UpdateChannel::Stable));
        assert!(belongs("0.2.0", UpdateChannel::Beta));
        assert!(belongs("0.2.0-beta.10", UpdateChannel::Beta));
        assert!(!belongs("0.2.0-beta.1", UpdateChannel::Stable));
        assert!(!belongs("0.2.0-rc.1", UpdateChannel::Beta));
    }

    #[test]
    fn only_newer_releases_of_the_feed_are_updates() {
        let offers = |current: &str, release: &str, feed| {
            offers_update(
                &semver::Version::parse(current).unwrap(),
                &semver::Version::parse(release).unwrap(),
                feed,
            )
        };
        use UpdateChannel::{Beta, Stable};
        assert!(offers("0.2.0-beta.8", "0.2.0-beta.9", Beta));
        assert!(
            offers("0.2.0-beta.8", "0.2.0", Beta),
            "beta follows a newer stable release"
        );
        assert!(offers("0.2.0-beta.8", "0.2.0", Stable));
        assert!(
            !offers("0.2.0-beta.8", "0.2.0-beta.8", Beta),
            "no reinstall"
        );
        assert!(
            !offers("0.2.0-beta.8", "0.2.0-beta.7", Beta),
            "no downgrade"
        );
        assert!(
            !offers("0.2.0-beta.8", "0.1.9", Stable),
            "switching to stable waits for a newer stable release"
        );
        assert!(!offers("0.2.0", "0.2.1-beta.1", Stable));
    }

    #[test]
    fn channel_feeds_derive_from_the_configured_feed() {
        assert_eq!(
            feed_url(FEED, UpdateChannel::Stable)
                .expect("valid feed")
                .as_str(),
            "https://example.test/o/r/releases/download/desktop-updates-stable/latest.json"
        );
        assert!(feed_url(
            "https://example.test/releases/latest.json",
            UpdateChannel::Beta
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
                "/home/u/.npm-global/lib/node_modules/private-ai-proxy/node_modules/private-ai-proxy-linux-x64/vendor",
                false,
                None
            ),
            Installation::Npm
        );
        assert_eq!(
            classify("/home/u/private-ai-proxy-cli-0.1.7-linux-x64", false, None),
            Installation::Portable
        );
        if cfg!(unix) {
            assert_eq!(
                classify(
                    "/home/u/.local/share/private-ai-proxy/versions/0.1.7",
                    false,
                    None
                ),
                Installation::InstallScript
            );
        }
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
        let upgrade_steps = |installation, root, version| {
            super::upgrade_steps(installation, root, version, std::env::consts::ARCH, None).unwrap()
        };
        let (npm, _) = upgrade_steps(Installation::Npm, root, "0.1.8");
        assert_eq!(
            npm,
            [
                "private-ai-proxy --yes service stop",
                "npm install --global private-ai-proxy@0.1.8"
            ]
        );
        assert_eq!(
            upgrade_steps(Installation::InstallScript, root, "0.1.8-beta.2").0,
            [
                "curl -fsSL https://redpill.ai/private-ai-proxy/install.sh -o private-ai-proxy-install.sh",
                "sh private-ai-proxy-install.sh --channel beta --version 0.1.8-beta.2 --install-dir /path/to/original/install --bin-dir /path/to/original/bin",
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
                upgrade_steps(Installation::Rpm, root, "0.1.8").0[1..],
                [
                    format!("sudo rpm --import {RPM_SIGNING_KEY_URL}"),
                    "sudo rpm -U https://example.test/o/r/releases/download/desktop-v0.1.8/private-ai-proxy-cli-0.1.8-linux-x64.rpm".to_string(),
                ]
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

    #[test]
    fn unsupported_architectures_have_no_download_or_asset_commands() {
        for arch in ["arm", "i686", "riscv64"] {
            for installation in [
                Installation::Portable,
                Installation::InstallScript,
                Installation::Deb,
                Installation::DesktopPacman,
            ] {
                assert_eq!(
                    super::upgrade_steps(installation, FEED, "0.1.8", arch, None).unwrap(),
                    (Vec::new(), None)
                );
            }
        }
        for (arch, asset_arch) in [("aarch64", "arm64"), ("x86_64", "x64")] {
            let (_, url) =
                super::upgrade_steps(Installation::Portable, FEED, "0.1.8", arch, None).unwrap();
            assert!(url.unwrap().contains(&format!("-{asset_arch}.")));
        }
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn script_updates_custom_links_while_the_backend_instance_is_locked() {
        use std::{
            os::unix::fs::{symlink, PermissionsExt},
            process::Command,
        };

        let temp = tempfile::Builder::new()
            .prefix("pap-uninstall-")
            .tempdir()
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        let install = root.join("custom install");
        let bin = root.join("custom bin");
        let tools = root.join("tools");
        let release = root.join("release");
        let data = root.join("data");
        let old = install.join("versions/0.1.7");
        let executable = old.join("private-ai-proxy");
        for directory in [&bin, &tools, &release, &old] {
            fs::create_dir_all(directory).unwrap();
        }
        let write_script = |path: &Path, body: &str| {
            fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        };
        write_script(
            &executable,
            r#"test "$*" = '--yes service stop' || exit 2
printf 'stop\n' >> "$PAP_TEST_STOP_LOG"
flock -n "$PAP_TEST_INSTANCE_LOCK" true"#,
        );
        // Existing links can spell the install root through a symlink.
        let old_root = root.join("install alias");
        symlink(&install, &old_root).unwrap();
        for name in ["private-ai-proxy", "pap", "aci"] {
            symlink(
                old_root.join("versions/0.1.7/private-ai-proxy"),
                bin.join(name),
            )
            .unwrap();
        }
        let arch = if cfg!(target_arch = "x86_64") {
            "x64"
        } else {
            "arm64"
        };
        let base = format!("private-ai-proxy-cli-0.1.8-linux-{arch}");
        let payload = release.join(&base);
        fs::create_dir(&payload).unwrap();
        write_script(
            &payload.join("private-ai-proxy"),
            "echo 'private-ai-proxy 0.1.8'",
        );
        for name in ["private-ai-proxy-helper", "private-ai-proxy-service"] {
            write_script(&payload.join(name), "exit 0");
        }
        for name in ["pap", "aci"] {
            symlink("private-ai-proxy", payload.join(name)).unwrap();
        }
        let archive = format!("{base}.tar.gz");
        assert!(Command::new("tar")
            .current_dir(&release)
            .args(["-czf", &archive, &base])
            .status()
            .unwrap()
            .success());
        let checksum = Command::new("sha256sum")
            .current_dir(&release)
            .arg(&archive)
            .output()
            .unwrap();
        assert!(checksum.status.success());
        fs::write(release.join("SHA256SUMS"), checksum.stdout).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../scripts/install-private-ai-proxy.sh"),
            release.join("install.sh"),
        )
        .unwrap();
        write_script(
            &tools.join("curl"),
            r#"while [ "$#" -gt 0 ]; do
  case "$1" in
    -o|--output) output=$2; shift ;;
    https://*) url=$1 ;;
  esac
  shift
done
cp "$PAP_TEST_RELEASE/${url##*/}" "$output""#,
        );

        let path = std::env::join_paths(
            [bin.clone(), tools]
                .into_iter()
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let script = ScriptInstallation::discover(&executable, Some(&path)).unwrap();
        let unlinked = ScriptInstallation::discover(&executable, Some(old.as_os_str())).unwrap();
        let refusal = script_update("0.1.8", Some(&unlinked)).unwrap_err();
        assert!(refusal.contains("--bin-dir /path/to/original/bin"));
        assert!(refusal.contains(&step(&[install.to_str().unwrap()]).unwrap()));
        assert!(!refusal.contains("--skip-service-stop"));
        let (command, upgrade) = script_update("0.1.8", Some(&script)).unwrap();
        assert_eq!(command, bin.join("private-ai-proxy"));

        let stop_log = root.join("stop-log");
        let _startup = crate::lock::startup(&data).unwrap().unwrap();
        let _instance = crate::lock::instance(&data).unwrap().unwrap();
        let run = |step: &str| {
            let words = shlex::split(step).unwrap();
            Command::new(&words[0])
                .args(&words[1..])
                .current_dir(&root)
                .env("PATH", &path)
                .env("XDG_DATA_HOME", root.join("changed XDG data home"))
                .env("PAP_TEST_RELEASE", &release)
                .env("PAP_TEST_STOP_LOG", &stop_log)
                .env("PAP_TEST_INSTANCE_LOCK", data.join("instance.lock"))
                .output()
                .unwrap()
        };
        let downloaded = run(&upgrade[0]);
        assert!(
            downloaded.status.success(),
            "{}",
            String::from_utf8_lossy(&downloaded.stderr)
        );
        let manual = script_steps("0.1.8", Some(&script), false).unwrap();
        let refused = run(&manual[1]);
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("cannot stop the existing"));
        let updated = run(&upgrade[1]);
        assert!(
            updated.status.success(),
            "{}",
            String::from_utf8_lossy(&updated.stderr)
        );
        assert_eq!(fs::read_to_string(stop_log).unwrap(), "stop\n");
        for name in ["private-ai-proxy", "pap", "aci"] {
            assert_eq!(
                fs::read_link(bin.join(name)).unwrap(),
                install.join("versions/0.1.8/private-ai-proxy")
            );
        }
        assert!(!old.exists());
        assert!(!root.join("changed XDG data home").exists());
        let restarted = Command::new(command).arg("--version").output().unwrap();
        assert!(restarted.status.success());
        assert_eq!(restarted.stdout, b"private-ai-proxy 0.1.8\n");
    }
}
