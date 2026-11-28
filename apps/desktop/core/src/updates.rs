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

mod node;
use node::NodeInstallation;

const PACKAGE_NAME: &str = "private-ai-proxy";
const HOMEBREW_FORMULA: &str = "dstack-tee/private-ai/private-ai-proxy";

/// The channel feed configured for release builds; development builds have none.
pub const RELEASE_FEED: Option<&str> = option_env!("TAURI_UPDATER_ENDPOINT");

const INVALID_FEED: &str = "Updates are not configured correctly for this build";
const PACKAGE_MANAGER_MARKER: &str = "/usr/share/private-ai-proxy/package-manager";

const ARCH_KEYRING: &str = "/usr/share/keyrings/private-ai-proxy-archive-keyring.gpg";
const RPM_SIGNING_KEY_URL: &str = "https://github.com/Dstack-TEE/private-ai-gateway/raw/main/apps/desktop/packaging/linux-signing-key.asc";
const RPM_SIGNING_FINGERPRINT: &str = "18579A1441C5278FB2B1A943060C5C651FC46140";
const RPM_SIGNING_KEY_FILE: &str = "/usr/share/keyrings/private-ai-proxy-archive-keyring.asc";
// RPM 4 uses the primary fingerprint's low 32 bits and the creation time in hex.
const RPM_SIGNING_KEY_PACKAGE: &str = "gpg-pubkey-1fc46140-6b0a7f59";
const RPM_KEY_SCRIPT: &str = include_str!("updates/rpm-key.sh");

fn rpm_key_command<'a>(mode: &'a str, argument: &'a str) -> [&'a str; 10] {
    [
        "sh",
        "-c",
        RPM_KEY_SCRIPT,
        "pap-rpm-key",
        RPM_SIGNING_FINGERPRINT,
        RPM_SIGNING_KEY_PACKAGE,
        RPM_SIGNING_KEY_FILE,
        RPM_SIGNING_KEY_URL,
        mode,
        argument,
    ]
}

/// Resolve a local RPM certificate, downloading and checking it if needed.
/// A missing result means the certificate is already in the RPM keyring.
#[cfg(target_os = "linux")]
pub fn prepare_rpm_signing_key(scratch: &Path) -> Result<Option<std::path::PathBuf>, String> {
    let scratch = scratch
        .to_str()
        .ok_or("The update directory is not UTF-8")?;
    let [program, arguments @ ..] = rpm_key_command("prepare", scratch);
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .map_err(|_| "Could not check the Private AI Proxy RPM signing key.")?;
    if !output.status.success() {
        return Err("Could not download or verify the Private AI Proxy RPM signing key.".into());
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|_| "Could not read the Private AI Proxy RPM signing key path.")?;
    Ok((!path.is_empty()).then(|| path.into()))
}

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
    /// Desktop app configured to update through its Linux repository.
    DesktopLinuxRepo,
    /// Desktop app managed by the Arch User Repository.
    DesktopAur,
    /// CLI-only native packages.
    Deb,
    Rpm,
    Pacman,
    /// CLI-only package configured to update through its Linux repository.
    LinuxRepo,
    Aur,
    /// CLI installed by the official install script.
    InstallScript,
    WindowsInstallScript,
    /// A native package from a release that did not record its package manager.
    SystemPackage,
    Homebrew,
    Npm,
    Pnpm,
    Bun,
    Yarn,
    Winget,
    Portable,
}

impl Installation {
    pub fn winget_update(self) -> Option<UpdateNotice> {
        if self != Self::Winget {
            return None;
        }
        Some(UpdateNotice {
            installation: self,
            channel: UpdateChannel::Stable,
            current_version: crate::protocol::BUILD_VERSION.into(),
            version: None,
            commands: vec![winget_upgrade()],
            package_url: None,
            download_url: None,
        })
    }
    pub fn delegates_updates(self) -> bool {
        matches!(
            self,
            Self::Homebrew
                | Self::Npm
                | Self::Pnpm
                | Self::Bun
                | Self::Yarn
                | Self::WindowsInstallScript
        )
    }
    pub fn aur_package_url(self) -> Option<String> {
        self.aur_package()
            .map(|name| format!("https://aur.archlinux.org/packages/{name}"))
    }

    pub fn aur_package(self) -> Option<&'static str> {
        match self {
            Self::DesktopAur => Some("private-ai-proxy-bin"),
            Self::Aur => Some("private-ai-proxy-cli-bin"),
            _ => None,
        }
    }

    pub fn desktop_package_managed(self) -> bool {
        matches!(
            self,
            Self::DesktopPacman | Self::DesktopLinuxRepo | Self::DesktopAur
        )
    }

    pub fn linux_repo_managed(self) -> bool {
        matches!(self, Self::LinuxRepo | Self::DesktopLinuxRepo)
    }
}

async fn tool_output(program: &str, arguments: &[&str]) -> Option<String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let path = std::env::var_os("PATH")?;
    // Ignore relative entries and resolve an absolute launcher before
    // spawning it. In particular, cmd must never search the project directory.
    let search: Vec<_> = std::env::split_paths(&path)
        .filter(|entry| entry.is_absolute())
        .collect();
    let executable = tool_on_path(&search, program, cfg!(windows))?;
    let directory = tempfile::tempdir().ok()?;
    let mut child = tokio::process::Command::new(executable)
        .args(arguments)
        .current_dir(directory.path())
        .env("PATH", std::env::join_paths(search).ok()?)
        .env("NoDefaultCurrentDirectoryInExePath", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?.take(1024 * 1024);
    let mut output = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        stdout.read_to_end(&mut output).await.ok()?;
        drop(stdout);
        child.wait().await.ok()?.success().then_some(())?;
        String::from_utf8(output)
            .ok()
            .map(|output| output.trim().to_owned())
    })
    .await
    .ok()?
}

fn tool_on_path(path: &[PathBuf], program: &str, windows: bool) -> Option<PathBuf> {
    let extensions: &[&str] = if windows {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    path.iter()
        .filter(|directory| directory.is_absolute())
        .find_map(|directory| {
            extensions
                .iter()
                .map(|ext| directory.join(format!("{program}{ext}")))
                .find(|executable| executable.is_file())
        })
}

fn homebrew_formula(directory: &Path, cellar: &Path) -> bool {
    let formula = fs::canonicalize(cellar).unwrap_or_else(|_| cellar.to_owned());
    directory.strip_prefix(formula).is_ok_and(|relative| {
        relative.components().count() == 2 && relative.file_name() == Some(OsStr::new("libexec"))
    })
}

fn windows_script_install(directory: &Path, local_app_data: Option<&Path>) -> bool {
    fs::read_to_string(directory.join(".private-ai-proxy-install"))
        .is_ok_and(|marker| marker.trim() == "windows-script")
        || (local_app_data.is_some_and(|root| {
            fs::canonicalize(
                root.join("Programs")
                    .join(format!("{} CLI", crate::brand::PRODUCT_NAME)),
            )
            .is_ok_and(|path| path == directory)
        }) && [
            "pap.cmd",
            "private-ai-proxy-service.exe",
            "private-ai-proxy-helper.exe",
        ]
        .iter()
        .all(|file| directory.join(file).is_file()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinuxRepository {
    Apt,
    Dnf,
    Zypper,
}

impl LinuxRepository {
    fn detect(root: &Path, manager: &str) -> Option<(Self, UpdateChannel)> {
        let repositories: &[(Self, &str, &str)] = match manager {
            "deb" => &[(Self::Apt, "apt/sources.list.d", "sources")],
            "rpm" => &[
                (Self::Dnf, "yum.repos.d", "repo"),
                (Self::Zypper, "zypp/repos.d", "repo"),
            ],
            _ => return None,
        };
        repositories
            .iter()
            .find_map(|&(manager, directory, extension)| {
                let directory = root.join(directory);
                let enabled = |suffix: &str| {
                    fs::read_to_string(
                        directory.join(format!("{PACKAGE_NAME}{suffix}.{extension}")),
                    )
                    .is_ok_and(|contents| {
                        let (separator, source_key) = if manager == Self::Apt {
                            (':', "uris")
                        } else {
                            ('=', "baseurl")
                        };
                        let mut configured = false;
                        for (key, value) in contents
                            .lines()
                            .map(str::trim)
                            .filter_map(|line| line.split_once(separator))
                        {
                            if key.trim().eq_ignore_ascii_case("enabled")
                                && ["false", "0", "no"]
                                    .iter()
                                    .any(|disabled| value.trim().eq_ignore_ascii_case(disabled))
                            {
                                return false;
                            }
                            configured |= key.trim().eq_ignore_ascii_case(source_key);
                        }
                        configured
                    })
                };
                if enabled("-beta") {
                    Some((manager, UpdateChannel::Beta))
                } else if enabled("") {
                    Some((manager, UpdateChannel::Stable))
                } else {
                    None
                }
            })
    }

    fn upgrade(self, installation: Installation) -> String {
        let package = if installation == Installation::DesktopLinuxRepo {
            PACKAGE_NAME.to_owned()
        } else {
            format!("{PACKAGE_NAME}-cli")
        };
        match self {
            Self::Apt => format!("sudo apt update && sudo apt install --only-upgrade {package}"),
            Self::Dnf => format!("sudo dnf upgrade --refresh {package}"),
            Self::Zypper => format!("sudo zypper refresh && sudo zypper update {package}"),
        }
    }
}

/// The active repository and suite are controlled by the package manager.
fn linux_repository() -> Option<(LinuxRepository, UpdateChannel)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let marker = fs::read_to_string(PACKAGE_MANAGER_MARKER).ok()?;
    LinuxRepository::detect(Path::new("/etc"), marker.trim())
}

/// The command to show to a user whose configured repository owns upgrades.
pub fn linux_repo_update(installation: Installation) -> Option<UpdateNotice> {
    if !installation.linux_repo_managed() {
        return None;
    }
    let (manager, channel) = linux_repository()?;
    Some(UpdateNotice {
        installation,
        channel,
        current_version: crate::protocol::BUILD_VERSION.into(),
        version: None,
        commands: vec![manager.upgrade(installation)],
        package_url: None,
        download_url: None,
    })
}

// Uses the same publisher and product name as the manifests in package-winget.mjs.
fn winget_cli_identifier() -> String {
    let compact = |name: &str| {
        name.chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
    };
    format!(
        "{}.{}.CLI",
        compact(crate::brand::ORGANIZATION_NAME),
        compact(crate::brand::PRODUCT_NAME)
    )
}

fn winget_upgrade() -> String {
    format!("winget upgrade --id {} --exact", winget_cli_identifier())
}

fn winget_package_directory(directory: &Path) -> bool {
    // WinGet's portable ZIP installer extracts the archive to a directory
    // named <PackageIdentifier>_<SourceIdentifier>. The ZIP adds one level.
    let Some(product_code) = directory
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
    else {
        return false;
    };
    let Some((identifier, source)) = product_code.split_once('_') else {
        return false;
    };
    !source.is_empty() && identifier.eq_ignore_ascii_case(&winget_cli_identifier())
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
    let install = classify(
        directory,
        desktop,
        package_manager.as_deref().map(str::trim),
    );
    if install == Installation::Portable
        && cfg!(windows)
        && windows_script_install(
            directory,
            std::env::var_os("LOCALAPPDATA").as_deref().map(Path::new),
        )
    {
        return Installation::WindowsInstallScript;
    }
    if directory == Path::new("/usr/bin")
        && desktop
        && install == Installation::DesktopApp
        && linux_repository().is_some()
    {
        Installation::DesktopLinuxRepo
    } else if matches!(install, Installation::Deb | Installation::Rpm)
        && linux_repository().is_some()
    {
        Installation::LinuxRepo
    } else {
        install
    }
}

async fn update_owner(
    installation: Installation,
) -> (Installation, Option<&'static NodeInstallation>) {
    if installation == Installation::Npm {
        let node = NodeInstallation::current().await;
        return (node.map_or(installation, |node| node.manager), node);
    }
    if installation == Installation::Portable && cfg!(any(target_os = "macos", target_os = "linux"))
    {
        static HOMEBREW: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
        let owned = HOMEBREW
            .get_or_init(|| async {
                let Ok(executable) = std::env::current_exe().and_then(fs::canonicalize) else {
                    return false;
                };
                let Some(directory) = executable
                    .parent()
                    .filter(|directory| directory.file_name() == Some(OsStr::new("libexec")))
                else {
                    return false;
                };
                tool_output("brew", &["--cellar", HOMEBREW_FORMULA])
                    .await
                    .is_some_and(|cellar| homebrew_formula(directory, Path::new(&cellar)))
            })
            .await;
        if *owned {
            return (Installation::Homebrew, None);
        }
    }
    (installation, None)
}

fn classify(directory: &Path, desktop: bool, package_manager: Option<&str>) -> Installation {
    let packaged = cfg!(target_os = "linux")
        && (directory == Path::new("/usr/bin")
            || directory == Path::new("/usr/libexec/private-ai-proxy"));
    let package_manager = package_manager.filter(|_| packaged);
    if desktop {
        return match package_manager.filter(|_| directory == Path::new("/usr/bin")) {
            Some("pacman") => Installation::DesktopPacman,
            Some("aur") => Installation::DesktopAur,
            _ => Installation::DesktopApp,
        };
    }
    if cfg!(windows) && winget_package_directory(directory) {
        return Installation::Winget;
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
        Some("aur") => Installation::Aur,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub package_url: Option<String>,
    /// Portable archive to extract into a fresh directory.
    pub download_url: Option<String>,
}

/// The desktop app's update check result for the renderer.
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub enabled: bool,
    pub system_managed: bool,
    pub package_manager_channel: bool,
    /// A package page for update instructions from its owning repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub package_url: Option<String>,
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
    let channel = if installation.aur_package().is_some() || installation == Installation::Winget {
        UpdateChannel::Stable
    } else {
        channel
    };
    let repository = installation
        .linux_repo_managed()
        .then(linux_repository)
        .flatten();
    let channel = repository.map_or(channel, |(_, channel)| channel);
    let current = semver::Version::parse(current_version)
        .map_err(|_| "The installed version is invalid".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Could not check for updates")?;
    let published = published_version(&client, feed_url(configured, channel)?, channel).await?;
    let latest = offers_update(&current, &published, channel).then_some(published);
    let (installation, node) = if latest.is_some() {
        update_owner(installation).await
    } else {
        (installation, None)
    };
    let script = (installation == Installation::InstallScript && latest.is_some())
        .then(ScriptInstallation::current)
        .flatten();
    let (commands, download_url) = match (&latest, repository) {
        (Some(_), Some((manager, _))) => (vec![manager.upgrade(installation)], None),
        (Some(version), None) => upgrade_steps(
            installation,
            release_root(configured)?.as_str(),
            &version.to_string(),
            std::env::consts::ARCH,
            script.as_ref(),
            node,
        )?,
        (None, _) => (Vec::new(), None),
    };
    Ok(UpdateNotice {
        installation,
        channel,
        current_version: current_version.into(),
        version: latest.map(|version| version.to_string()),
        commands,
        package_url: installation.aur_package_url(),
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

fn powershell_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn node_steps(node: &NodeInstallation, spec: &str, windows: bool) -> Result<Vec<String>, String> {
    let Some(words) = node.words(spec) else {
        return Ok(Vec::new());
    };
    if windows {
        return Ok(vec![
            STOP_STEP.into(),
            format!(
                "{} {}",
                words[0],
                words[1..]
                    .iter()
                    .map(|word| powershell_argument(word))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ]);
    }
    Ok(vec![step(&words)?])
}

pub(super) fn without_windows_verbatim_prefix(value: &str) -> std::borrow::Cow<'_, str> {
    if let Some(network_path) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{network_path}").into()
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(value).into()
    }
}

fn windows_install_command(directory: &Path, version: &str) -> Result<String, String> {
    let parsed = semver::Version::parse(version).map_err(|_| "The update version is invalid")?;
    let channel = if parsed.pre.is_empty() {
        "stable"
    } else {
        "beta"
    };
    let directory = directory
        .to_str()
        .ok_or("The install directory is not UTF-8")?;
    let directory = without_windows_verbatim_prefix(directory);
    Ok(format!(
        "$installer = Join-Path $env:TEMP ('pap-install-' + [guid]::NewGuid() + '.ps1'); try {{ Invoke-WebRequest 'https://redpill.ai/private-ai-proxy/install.ps1' -UseBasicParsing -OutFile $installer -ErrorAction Stop; powershell -ExecutionPolicy Bypass -File $installer -Channel {channel} -Version {} -InstallDir {} }} finally {{ Remove-Item -LiteralPath $installer -ErrorAction SilentlyContinue }}",
        powershell_argument(version),
        powershell_argument(&directory),
    ))
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
    node: Option<&NodeInstallation>,
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
        Installation::DesktopApp
        | Installation::SystemPackage
        | Installation::DesktopLinuxRepo
        | Installation::LinuxRepo
        | Installation::DesktopAur
        | Installation::Aur => (Vec::new(), None),
        Installation::DesktopPacman | Installation::Pacman => {
            let file = name(installation == Installation::Pacman, ".pkg.tar.zst");
            let signature = format!("{file}.sig");
            // pacman checks an adjacent .sig against its own keyring. Give the
            // download a different name so gpgv can verify it with our keyring.
            let detached = format!("{signature}.download");
            (
                vec![
                    stop,
                    command(&["curl", "-fLO", &asset(&file)?])?,
                    command(&["curl", "-fL", "-o", &detached, &asset(&signature)?])?,
                    // Keep verification and installation in one step so copied
                    // commands also refuse to install when verification fails.
                    command(&[
                        "sh",
                        "-c",
                        r#"gpgv --keyring "$1" "$2" "$3" && sudo pacman -U "$3""#,
                        "pap-arch-update",
                        ARCH_KEYRING,
                        &detached,
                        &format!("./{file}"),
                    ])?,
                ],
                None,
            )
        }
        Installation::Rpm => {
            let package = asset(&name(true, ".rpm"))?;
            (
                vec![stop, command(&rpm_key_command("install", &package))?],
                None,
            )
        }
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
        Installation::Winget => (vec![winget_upgrade()], None),
        Installation::Homebrew => (vec![format!("brew upgrade {HOMEBREW_FORMULA}")], None),
        Installation::Npm | Installation::Pnpm | Installation::Bun | Installation::Yarn => {
            let spec = format!("{PACKAGE_NAME}@{version}");
            (
                node.map(|node| node_steps(node, &spec, cfg!(windows)))
                    .transpose()?
                    .unwrap_or_default(),
                None,
            )
        }
        Installation::WindowsInstallScript => {
            let executable = std::env::current_exe()
                .and_then(fs::canonicalize)
                .map_err(|_| "Cannot locate the installed CLI")?;
            let directory = executable
                .parent()
                .ok_or("Cannot locate the installation directory")?;
            (vec![windows_install_command(directory, version)?], None)
        }
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

    #[test]
    fn windows_tool_search_uses_only_absolute_path_entries() {
        let trusted = tempfile::tempdir().unwrap();
        let launcher = trusted.path().join("npm.cmd");
        fs::write(&launcher, "").unwrap();
        let path = [PathBuf::from("."), trusted.path().to_owned()];
        assert_eq!(tool_on_path(&path, "npm", true), Some(launcher));
        assert_eq!(tool_on_path(&[PathBuf::from(".")], "npm", true), None);
    }

    #[test]
    fn homebrew_formula_must_belong_to_the_reported_cellar() {
        let cellar = Path::new("/custom/brew-prefix/Cellar/private-ai-proxy");
        assert!(homebrew_formula(&cellar.join("1.2.3/libexec"), cellar));
        assert!(!homebrew_formula(
            Path::new("/other/prefix/Cellar/private-ai-proxy/1.2.3/libexec"),
            cellar
        ));
    }

    #[test]
    fn windows_script_uses_its_marked_directory_and_quotes_it_in_powershell() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!windows_script_install(temp.path(), None));
        fs::write(
            temp.path().join(".private-ai-proxy-install"),
            "windows-script\r\n",
        )
        .unwrap();
        assert!(windows_script_install(temp.path(), None));
        let command =
            windows_install_command(Path::new(r"\\?\C:\O'Brien PAP"), "1.2.3-beta.1").unwrap();
        assert!(command
            .contains(r"-Channel beta -Version '1.2.3-beta.1' -InstallDir 'C:\O''Brien PAP'"));
    }

    #[test]
    fn linux_repo_channel_and_upgrade_command_follow_enabled_sources() {
        let temp = tempfile::tempdir().unwrap();
        let etc = temp.path();
        fs::create_dir_all(etc.join("apt/sources.list.d")).unwrap();
        fs::create_dir_all(etc.join("zypp/repos.d")).unwrap();
        let source =
            |suffix| etc.join(format!("apt/sources.list.d/{PACKAGE_NAME}{suffix}.sources"));
        fs::write(source(""), "URIs: https://example.test/apt\n").unwrap();
        for disabled in ["no", "false", "0"] {
            fs::write(
                source("-beta"),
                format!("URIs: https://example.test/apt\nEnabled: {disabled}\n"),
            )
            .unwrap();
            assert_eq!(
                LinuxRepository::detect(etc, "deb"),
                Some((LinuxRepository::Apt, UpdateChannel::Stable))
            );
        }
        fs::write(
            source("-beta"),
            "URIs: https://example.test/apt\nEnabled: yes\n",
        )
        .unwrap();
        assert_eq!(
            LinuxRepository::detect(etc, "deb"),
            Some((LinuxRepository::Apt, UpdateChannel::Beta))
        );
        assert_eq!(
            LinuxRepository::Apt.upgrade(Installation::DesktopLinuxRepo),
            "sudo apt update && sudo apt install --only-upgrade private-ai-proxy"
        );
        fs::write(
            etc.join("zypp/repos.d/private-ai-proxy.repo"),
            "baseurl=https://example.test/stable\nenabled=1\n",
        )
        .unwrap();
        assert_eq!(
            LinuxRepository::detect(etc, "rpm"),
            Some((LinuxRepository::Zypper, UpdateChannel::Stable))
        );
        assert_eq!(
            LinuxRepository::Zypper.upgrade(Installation::LinuxRepo),
            "sudo zypper refresh && sudo zypper update private-ai-proxy-cli"
        );
    }

    #[test]
    fn detects_winget_cli_with_its_source_suffix() {
        let packages = Path::new("Microsoft/WinGet/Packages");
        let directory = packages
            .join(format!("{}_source", winget_cli_identifier()))
            .join("private-ai-proxy-cli-1.2.3-windows-x64");
        assert!(winget_package_directory(&directory));
        assert!(!winget_package_directory(
            &packages.join("private-ai-proxy-cli-1.2.3-windows-x64")
        ));
        assert!(!winget_package_directory(
            &packages.join("DstackTEE.PrivateAIProxy_source").join("app")
        ));
        let notice = Installation::Winget.winget_update().unwrap();
        assert_eq!(
            notice.commands,
            ["winget upgrade --id DstackTEE.PrivateAIProxy.CLI --exact"]
        );
        assert!(notice.download_url.is_none());
    }

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
            super::upgrade_steps(
                installation,
                root,
                version,
                std::env::consts::ARCH,
                None,
                None,
            )
            .unwrap()
        };
        assert_eq!(
            upgrade_steps(Installation::Homebrew, root, "0.1.8").0,
            ["brew upgrade dstack-tee/private-ai/private-ai-proxy"]
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
            for (installation, file) in [
                (
                    Installation::DesktopPacman,
                    "private-ai-proxy-0.1.8-beta.2-linux-x64.pkg.tar.zst",
                ),
                (
                    Installation::Pacman,
                    "private-ai-proxy-cli-0.1.8-beta.2-linux-x64.pkg.tar.zst",
                ),
            ] {
                let (commands, _) = upgrade_steps(installation, root, "0.1.8-beta.2");
                assert_eq!(
                    commands[..3],
                    [
                        STOP_STEP.to_string(),
                        format!("curl -fLO {root}desktop-v0.1.8-beta.2/{file}"),
                        format!("curl -fL -o {file}.sig.download {root}desktop-v0.1.8-beta.2/{file}.sig"),
                    ]
                );
                let install = shlex::split(&commands[3]).unwrap();
                assert_eq!(
                    install[2],
                    r#"gpgv --keyring "$1" "$2" "$3" && sudo pacman -U "$3""#
                );
                assert_eq!(
                    install[4..],
                    [
                        ARCH_KEYRING.to_string(),
                        format!("{file}.sig.download"),
                        format!("./{file}")
                    ]
                );
            }
            let (rpm, _) = upgrade_steps(Installation::Rpm, root, "0.1.8");
            let rpm = shlex::split(&rpm[1]).unwrap();
            assert_eq!(rpm[2], RPM_KEY_SCRIPT);
            assert_eq!(
                rpm[4..],
                [
                    RPM_SIGNING_FINGERPRINT,
                    RPM_SIGNING_KEY_PACKAGE,
                    RPM_SIGNING_KEY_FILE,
                    RPM_SIGNING_KEY_URL,
                    "install",
                    "https://example.test/o/r/releases/download/desktop-v0.1.8/private-ai-proxy-cli-0.1.8-linux-x64.rpm",
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
                    super::upgrade_steps(installation, FEED, "0.1.8", arch, None, None).unwrap(),
                    (Vec::new(), None)
                );
            }
        }
        for (arch, asset_arch) in [("aarch64", "arm64"), ("x86_64", "x64")] {
            let (_, url) =
                super::upgrade_steps(Installation::Portable, FEED, "0.1.8", arch, None, None)
                    .unwrap();
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
