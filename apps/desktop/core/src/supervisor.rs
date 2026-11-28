//! Keeps the backend running without a command. The user's service manager
//! (systemd on Linux, launchd on macOS) starts it at login and again after a
//! crash, and the backend resumes the protection session it left.
//!
//! The service manager is used only through a registration that starts this
//! installation's backend: a Linux package's unit, or one written here. A
//! registration of the same name that the user or another installation wrote
//! is neither used nor changed. Where no user service manager runs
//! (containers, WSL without systemd) enabling fails and commands start the
//! backend as before.

use std::path::PathBuf;

use serde::Serialize;

#[cfg(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mac-app-store"))
))]
mod registration;

use crate::updates::{installation, Installation};

const UNSUPPORTED: &str = "Starting the backend at login is not available for this installation";
const SKIP_NOTICE_MARKER: &str = "login-service-skip-notice-shown";
const PROXY_ENVIRONMENT: &[&str] = &[
    // Reqwest's default system proxy matcher uses these variables.
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "REQUEST_METHOD",
];

/// Records that the user turned this off, so `pap start` does not turn it on
/// again.
fn declined() -> Option<PathBuf> {
    Some(crate::paths::app_data_dir().ok()?.join("login-service-off"))
}

/// A backend with its data elsewhere is not the one a login would start.
fn relocated() -> bool {
    [
        crate::paths::HOME_OVERRIDE_ENV,
        crate::paths::APP_DATA_OVERRIDE_ENV,
        crate::paths::CONFIG_OVERRIDE_ENV,
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
}

fn supported() -> bool {
    let available = service_manager_platform_supported() && !relocated();
    #[cfg(target_os = "macos")]
    let available = available
        && std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .is_ok_and(|path| !crate::paths::transient_macos_app_path(&path));
    available
}

fn service_manager_platform_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        all(target_os = "macos", not(feature = "mac-app-store"))
    ))
}

/// The CLI path that survives an upgrade. Desktop shells and their bundled
/// CLI share the same registration, even when the CLI was invoked via PATH.
fn command() -> Result<(PathBuf, bool), String> {
    if matches!(
        installation(),
        Installation::DesktopApp | Installation::DesktopPacman | Installation::DesktopLinuxRepo | Installation::DesktopAur
    ) {
        return Ok((
            crate::launch::sibling_executable("private-ai-proxy")?,
            false,
        ));
    }
    let current = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("Cannot locate this command: {error}"))?;
    let name = current.file_name().ok_or("This command has no name")?;
    let link = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join(name))
            .find(|entry| {
                *entry != current && std::fs::canonicalize(entry).is_ok_and(|to| to == current)
            })
    });
    Ok(match link {
        Some(link) => (link, true),
        None => (current, false),
    })
}

fn user_service_manager_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl")
            .args(["--user", "show-environment"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(all(target_os = "macos", not(feature = "mac-app-store")))]
    {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        std::process::Command::new("launchctl")
            .args(["print", &domain])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(any(
        target_os = "linux",
        all(target_os = "macos", not(feature = "mac-app-store"))
    )))]
    {
        false
    }
}

fn installation_supports_automatic_supervision() -> bool {
    match installation() {
        Installation::Deb
        | Installation::Rpm
        | Installation::LinuxRepo
        | Installation::Pacman
        | Installation::Aur
        | Installation::SystemPackage
        | Installation::Npm
        | Installation::Pnpm
        | Installation::Bun
        | Installation::Yarn
        | Installation::Homebrew
        | Installation::WindowsInstallScript => true,
        Installation::InstallScript | Installation::Portable => {
            command().is_ok_and(|(_, linked)| linked)
        }
        Installation::DesktopApp | Installation::DesktopPacman | Installation::DesktopLinuxRepo | Installation::DesktopAur => {
            platform::installed_desktop()
        }
    }
}

fn automatic_supervision_supported() -> bool {
    supported() && installation_supports_automatic_supervision() && user_service_manager_available()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EnvironmentOverrides {
    agent: Vec<&'static str>,
    proxy: Vec<&'static str>,
    xdg_data: bool,
}

fn process_environment() -> EnvironmentOverrides {
    let is_set = |name: &str| std::env::var_os(name).is_some();
    let proxy = PROXY_ENVIRONMENT
        .iter()
        .copied()
        .filter(|name| is_set(name))
        .collect();
    let mut agent = crate::paths::agent_environment::supervision_overrides();
    // On Linux this variable also selects PAP's own settings directory.
    if cfg!(target_os = "linux") && crate::paths::xdg_home_overridden(crate::paths::XDG_CONFIG_HOME)
    {
        agent.push(crate::paths::XDG_CONFIG_HOME);
    }
    EnvironmentOverrides {
        agent,
        proxy,
        xdg_data: crate::paths::xdg_home_overridden(crate::paths::XDG_DATA_HOME),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AutomaticBlockers {
    Eligible,
    Unsupported,
    Declined,
    Environment(Vec<&'static str>),
}

fn environment_blockers(environment: &EnvironmentOverrides) -> Vec<&'static str> {
    let mut names = environment.agent.clone();
    if environment.xdg_data {
        names.push(crate::paths::XDG_DATA_HOME);
    }
    let proxy_names: Vec<_> = environment
        .proxy
        .iter()
        .copied()
        .filter(|name| !names.contains(name))
        .collect();
    names.extend(proxy_names);
    names
}

fn automatic_blockers_for(
    environment: &EnvironmentOverrides,
    declined: bool,
    supported: bool,
) -> AutomaticBlockers {
    if declined {
        return AutomaticBlockers::Declined;
    }
    if !supported {
        return AutomaticBlockers::Unsupported;
    }
    let names = environment_blockers(environment);
    if names.is_empty() {
        AutomaticBlockers::Eligible
    } else {
        AutomaticBlockers::Environment(names)
    }
}

pub fn automatic_blockers() -> AutomaticBlockers {
    automatic_blockers_for(
        &process_environment(),
        declined().is_some_and(|marker| marker.exists()),
        automatic_supervision_supported(),
    )
}

impl AutomaticBlockers {
    pub fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible)
    }
}

fn record_automatic_skip_notice(data: &std::path::Path, names: &[&str]) -> Option<String> {
    crate::private_fs::create_private_dir(data).ok()?;
    crate::private_fs::write_private(&data.join(SKIP_NOTICE_MARKER), "").ok()?;
    let variables = match names {
        [name] => format!("{name} is set"),
        names => format!("{} are set", names.join(", ")),
    };
    Some(format!(
        "Automatic supervision was skipped because {variables}. To keep protection across logins, set those variables in the service manager's environment first, then run `pap service enable`."
    ))
}

/// Returns the one-time notice for environment overrides that prevent
/// automatic supervision. The marker lives with the installation's data.
pub fn automatic_skip_notice(blockers: AutomaticBlockers) -> Option<String> {
    let AutomaticBlockers::Environment(names) = blockers else {
        return None;
    };
    let data = crate::paths::app_data_dir().ok()?;
    record_automatic_skip_notice(&data, &names)
}

/// Has the service manager start the backend at login and after a crash.
/// True when this call turned that on.
pub fn enable() -> Result<bool, String> {
    if !supported() {
        return Err(UNSUPPORTED.into());
    }
    let enabled = platform::enable()?;
    if let Some(marker) = declined() {
        let _ = std::fs::remove_file(marker);
    }
    Ok(enabled)
}

/// Removes the login registration and remembers the user's opt-out.
pub fn disable() -> Result<(), String> {
    unregister()?;
    if let Some(marker) = declined() {
        crate::private_fs::write_private_atomic(&marker, "", None)
            .map_err(|error| format!("Cannot write {}: {error}", marker.display()))?;
    }
    Ok(())
}

/// Removes the login registration without changing the user's preference.
/// The caller quits a running backend first so that launchd does not preserve
/// a session the user ended.
pub fn unregister() -> Result<(), String> {
    if !supported() {
        return Err(UNSUPPORTED.into());
    }
    platform::disable()
}

/// Whether the service manager starts this installation's backend.
pub fn enabled() -> bool {
    running_pid().is_ok_and(|pid| pid.is_some())
}

#[derive(Serialize)]
pub struct LoginServiceStatus {
    enabled: bool,
    active: bool,
    reason: Option<LoginServiceReason>,
    detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum LoginServiceReason {
    DisabledByUser,
    SkippedEnvironmentOverrides,
    AutomaticRegistrationUnavailable,
    PathOverrides,
    Unsupported,
    ServiceManagerUnavailable,
    InspectionFailed,
}

impl LoginServiceStatus {
    fn unavailable(reason: LoginServiceReason, detail: impl Into<String>) -> Self {
        Self {
            enabled: false,
            active: false,
            reason: Some(reason),
            detail: detail.into(),
        }
    }
}

/// Read-only diagnosis of this installation's login service.
pub fn login_service_status() -> LoginServiceStatus {
    use LoginServiceReason as Reason;

    if relocated() {
        return LoginServiceStatus::unavailable(
            Reason::PathOverrides,
            "Unavailable with PAP path overrides",
        );
    }
    if !supported() {
        return LoginServiceStatus::unavailable(
            Reason::Unsupported,
            "Unavailable for this installation",
        );
    }
    if !user_service_manager_available() {
        return LoginServiceStatus::unavailable(
            Reason::ServiceManagerUnavailable,
            "Unavailable because no user service manager is running",
        );
    }
    #[cfg(target_os = "linux")]
    let registration = platform::inspect_login_service();
    #[cfg(not(target_os = "linux"))]
    let registration =
        platform::running_pid().map(|pid| (pid.is_some(), pid.is_some_and(|pid| pid != 0)));
    let (enabled, active) = match registration {
        Ok(status) => status,
        Err(error) => {
            return LoginServiceStatus::unavailable(
                Reason::InspectionFailed,
                format!("Cannot inspect login service: {error}"),
            )
        }
    };
    let (reason, detail) = match (enabled, active) {
        (true, true) => (None, "Enabled (active)".into()),
        (true, false) => (None, "Enabled (inactive)".into()),
        (false, true) => (None, "Active (disabled at login)".into()),
        (false, false) => match automatic_blockers_for(
            &process_environment(),
            declined().is_some_and(|marker| marker.exists()),
            installation_supports_automatic_supervision(),
        ) {
            AutomaticBlockers::Eligible => (None, "Disabled".into()),
            AutomaticBlockers::Declined => (
                Some(Reason::DisabledByUser),
                "Disabled by pap service disable".into(),
            ),
            AutomaticBlockers::Unsupported => (
                Some(Reason::AutomaticRegistrationUnavailable),
                "Automatic registration is unavailable for this installation".into(),
            ),
            AutomaticBlockers::Environment(names) => (
                Some(Reason::SkippedEnvironmentOverrides),
                format!(
                    "Automatic registration skipped because of environment overrides: {}",
                    names.join(", ")
                ),
            ),
        },
    };
    LoginServiceStatus {
        enabled,
        active,
        reason,
        detail,
    }
}

/// PID of the enabled, owned login service. Zero means it is idle.
pub(crate) fn running_pid() -> Result<Option<u32>, String> {
    if supported() {
        platform::running_pid()
    } else {
        Ok(None)
    }
}

/// Starts the backend under the service manager, if it is enabled there.
/// False leaves the caller to start it itself.
pub fn start() -> Result<bool, String> {
    if !enabled() {
        return Ok(false);
    }
    if !platform::start() {
        return Err("Cannot start the backend through its login service. Run pap doctor.".into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        automatic_blockers_for, environment_blockers, record_automatic_skip_notice,
        AutomaticBlockers, EnvironmentOverrides,
    };

    #[test]
    fn automatic_blockers_name_agent_and_proxy_overrides() {
        let codex = EnvironmentOverrides {
            agent: vec!["CODEX_HOME"],
            ..EnvironmentOverrides::default()
        };
        assert_eq!(environment_blockers(&codex), vec!["CODEX_HOME"]);
        assert_eq!(
            automatic_blockers_for(&codex, false, true),
            AutomaticBlockers::Environment(vec!["CODEX_HOME"])
        );

        let default_xdg = EnvironmentOverrides::default();
        assert!(environment_blockers(&default_xdg).is_empty());
        assert_eq!(
            automatic_blockers_for(&default_xdg, false, true),
            AutomaticBlockers::Eligible
        );

        let proxy = EnvironmentOverrides {
            proxy: vec!["HTTPS_PROXY"],
            ..EnvironmentOverrides::default()
        };
        assert_eq!(environment_blockers(&proxy), vec!["HTTPS_PROXY"]);
        assert_eq!(
            automatic_blockers_for(&proxy, false, true),
            AutomaticBlockers::Environment(vec!["HTTPS_PROXY"])
        );
    }

    #[test]
    fn automatic_skip_notice_is_recorded_only_once() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("app-data");
        let notice = record_automatic_skip_notice(&data, &["CODEX_HOME", "HTTPS_PROXY"])
            .expect("first start should show the notice");
        assert!(notice.contains("CODEX_HOME, HTTPS_PROXY"));
        assert!(notice.contains("pap service enable"));
        assert!(notice.contains("environment first, then run"));
        assert!(record_automatic_skip_notice(&data, &["CODEX_HOME", "HTTPS_PROXY"]).is_none());
    }

    #[test]
    fn declined_supervision_does_not_show_a_notice() {
        let environment = EnvironmentOverrides {
            agent: vec!["CODEX_HOME"],
            ..EnvironmentOverrides::default()
        };
        assert_eq!(
            automatic_blockers_for(&environment, true, true),
            AutomaticBlockers::Declined
        );
    }

    #[test]
    fn pap_path_relocation_is_unsupported_without_an_environment_notice() {
        let environment = EnvironmentOverrides {
            agent: vec!["CODEX_HOME"],
            ..EnvironmentOverrides::default()
        };
        assert_eq!(
            automatic_blockers_for(&environment, false, false),
            AutomaticBlockers::Unsupported
        );
        assert!(super::automatic_skip_notice(AutomaticBlockers::Unsupported).is_none());
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Command, Output, Stdio},
    };

    use super::{command, registration};

    const UNIT: &str = "private-ai-proxy.service";
    const UNIT_OBJECT: &str = "/org/freedesktop/systemd1/unit/private_2dai_2dproxy_2eservice";
    const PACKAGED_UNIT: &str = "/usr/lib/systemd/user/private-ai-proxy.service";
    const PACKAGED_BACKEND: &str = "/usr/libexec/private-ai-proxy/private-ai-proxy-service";
    const DESKTOP_BACKEND: &str = "/usr/bin/private-ai-proxy-service";
    const PACKAGED_CONTENTS: &str =
        include_str!("../../src-tauri/installer/private-ai-proxy.service");
    const MARKER: &str = "# Written by `pap service enable`; `pap service disable` removes it.";

    fn systemctl(arguments: &[&str]) -> Result<Output, String> {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("Cannot run systemctl --user: {error}"))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "systemctl --user {} failed ({}): {}",
                arguments[0],
                output.status,
                error.trim()
            ));
        }
        Ok(output)
    }

    fn properties(interface: &str, names: &[&str]) -> Option<Vec<serde_json::Value>> {
        #[derive(serde::Deserialize)]
        struct Property {
            data: serde_json::Value,
        }
        let output = Command::new("busctl")
            .args([
                "--user",
                "--json=short",
                "get-property",
                "org.freedesktop.systemd1",
                UNIT_OBJECT,
                interface,
            ])
            .args(names)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        serde_json::Deserializer::from_slice(&output.stdout)
            .into_iter::<Property>()
            .map(|property| property.ok().map(|property| property.data))
            .collect()
    }

    /// The D-Bus string array preserves paths containing spaces.
    fn safe_drop_ins() -> bool {
        let Some(properties) = properties("org.freedesktop.systemd1.Unit", &["DropInPaths"]) else {
            return false;
        };
        let [paths] = properties.as_slice() else {
            return false;
        };
        serde_json::from_value::<Vec<PathBuf>>(paths.clone()).is_ok_and(|paths| {
            paths.iter().all(|path| {
                path.parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name != "private-ai-proxy.service.d")
                    && fs::read_to_string(path).is_ok_and(|text| !overrides_execution(&text))
            })
        })
    }

    fn safe_restart_policy() -> bool {
        properties(
            "org.freedesktop.systemd1.Service",
            &[
                "Restart",
                "RestartPreventExitStatus",
                "RestartForceExitStatus",
                "SuccessExitStatus",
            ],
        )
        .is_some_and(|values| matches_restart_policy(&values))
    }

    fn matches_restart_policy(values: &[serde_json::Value]) -> bool {
        use serde_json::json;
        values
            == [
                json!("on-failure"),
                json!([[crate::launch::EXIT_ALREADY_RUNNING], []]),
                json!([[], []]),
                json!([[], []]),
            ]
    }

    fn overrides_execution(text: &str) -> bool {
        let mut service = false;
        text.lines().map(str::trim).any(|line| {
            if line.starts_with('[') {
                service = line == "[Service]";
            }
            if !service {
                return false;
            }
            line.split_once('=').is_some_and(|(key, _)| {
                let key = key.trim();
                [
                    "Exec",
                    "Environment",
                    "Root",
                    "Bind",
                    "Extension",
                    "Mount",
                    "Credential",
                    "LoadCredential",
                    "SetCredential",
                    "ImportCredential",
                ]
                .into_iter()
                .any(|prefix| key.starts_with(prefix))
                    || matches!(
                        key,
                        "User"
                            | "Group"
                            | "SupplementaryGroups"
                            | "DynamicUser"
                            | "PrivateUsers"
                            | "PAMName"
                            | "WorkingDirectory"
                            | "SetLoginEnvironment"
                            | "PassEnvironment"
                            | "UnsetEnvironment"
                            | "TemporaryFileSystem"
                            | "ProtectHome"
                            | "ReadOnlyPaths"
                            | "ReadWritePaths"
                            | "InaccessiblePaths"
                            | "Type"
                    )
            })
        })
    }

    /// The package's unit, when it starts this installation's backend.
    fn packaged_backend() -> Option<PathBuf> {
        if !Path::new(PACKAGED_UNIT).is_file() {
            return None;
        }
        crate::launch::service_executable().ok()
    }

    pub(super) fn installed_desktop() -> bool {
        packaged_backend().is_some_and(|backend| backend == Path::new(DESKTOP_BACKEND))
    }

    fn packaged() -> Option<String> {
        packaged_contents(&packaged_backend()?)
    }

    fn packaged_contents(backend: &Path) -> Option<String> {
        if backend == Path::new(PACKAGED_BACKEND) {
            Some(PACKAGED_CONTENTS.into())
        } else if backend == Path::new(DESKTOP_BACKEND) {
            Some(PACKAGED_CONTENTS.replace(PACKAGED_BACKEND, DESKTOP_BACKEND))
        } else {
            None
        }
    }

    /// The unit a user may write, as systemd.unit(5) places it.
    fn user_unit() -> Result<PathBuf, String> {
        let config = crate::paths::env_path(crate::paths::XDG_CONFIG_HOME)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::home_dir().map(|home| home.join(".config")))
            .ok_or("Cannot locate the user's configuration directory")?;
        Ok(config.join("systemd/user").join(UNIT))
    }

    /// The packaged unit's settings, running `command`.
    fn unit(command: &Path) -> Result<String, String> {
        // Quote the path, escape specifiers, and disable variable expansion.
        let path = command
            .to_str()
            .filter(|path| !path.chars().any(char::is_control))
            .ok_or("The command path cannot be represented in a systemd unit")?
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        Ok(format!(
            "{MARKER}\n\
             [Unit]\n\
             Description=Private AI Proxy backend\n\
             StartLimitIntervalSec=60\n\
             StartLimitBurst=4\n\
             \n\
             [Service]\n\
             ExecStart=:\"{path}\" service run\n\
             Restart=on-failure\n\
             RestartSec=1\n\
             RestartPreventExitStatus=75\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
        ))
    }

    struct Registration {
        path: PathBuf,
        contents: String,
        packaged: bool,
    }

    impl Registration {
        fn for_installation() -> Result<Self, String> {
            if let Some(contents) = packaged() {
                Ok(Self {
                    path: PathBuf::from(PACKAGED_UNIT),
                    contents,
                    packaged: true,
                })
            } else {
                Ok(Self {
                    path: user_unit()?,
                    contents: unit(&command()?.0)?,
                    packaged: false,
                })
            }
        }

        fn exists(&self) -> Result<bool, String> {
            if !self.packaged {
                return registration::exists(
                    &registration::record_path()?,
                    &self.path,
                    &self.contents,
                );
            }
            let user = user_unit()?;
            match fs::symlink_metadata(&user) {
                Ok(_) => return Err(registration::foreign(&user)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("Cannot inspect {}: {error}", user.display())),
            }
            // Package versions may differ in comments. Check the directives.
            let current = fs::read_to_string(&self.path)
                .map_err(|error| format!("Cannot read {}: {error}", self.path.display()))?;
            let directives = |line: &&str| !line.is_empty() && !line.starts_with('#');
            if current
                .lines()
                .filter(directives)
                .eq(self.contents.lines().filter(directives))
            {
                Ok(true)
            } else {
                Err(registration::foreign(&self.path))
            }
        }

        /// Inspect the effective unit. A user mask, an overlay in another
        /// search directory or an execution override takes precedence over
        /// our file. Distribution-wide timeout settings can still apply.
        fn state(&self) -> Result<Option<Loaded>, String> {
            let mut state = self.loaded_state()?;
            if matches!(state, State::Reload) {
                systemctl(&["daemon-reload"])?;
                state = self.loaded_state()?;
            }
            match state {
                State::Missing => Ok(None),
                State::Loaded(state) => Ok(Some(state)),
                State::Reload => Err(format!(
                    "The systemd user unit {UNIT} still has pending changes"
                )),
            }
        }

        fn loaded_state(&self) -> Result<State, String> {
            let output = systemctl(&[
                "show",
                "--all",
                "--property=LoadState,FragmentPath,DropInPaths,NeedDaemonReload,UnitFileState,ActiveState,MainPID",
                UNIT,
            ])?;
            let text = String::from_utf8_lossy(&output.stdout);
            let property = |key: &str| {
                text.lines()
                    .filter_map(|line| line.split_once('='))
                    .find_map(|(name, value)| (name == key).then_some(value))
            };
            if property("LoadState") == Some("masked") {
                return Err(format!("The systemd user unit {UNIT} is masked"));
            }
            if property("DropInPaths") != Some("") && !safe_drop_ins() {
                return Err(format!("The systemd user unit {UNIT} has overrides"));
            }
            match property("LoadState") {
                Some("not-found") => {
                    self.exists()?;
                    Ok(State::Missing)
                }
                Some("loaded") if property("FragmentPath").is_some_and(|path| Path::new(path) == self.path) && self.exists()? => {
                    if property("NeedDaemonReload") != Some("no") {
                        return Ok(State::Reload);
                    }
                    if !safe_restart_policy() {
                        return Err(format!("The systemd user unit {UNIT} has an unexpected restart policy"));
                    }
                    let file_state = property("UnitFileState").ok_or("systemd did not report the unit file state")?.to_owned();
                    let active_state = property("ActiveState").ok_or("systemd did not report the unit active state")?;
                    let active = matches!(active_state, "active" | "reloading" | "refreshing");
                    let pid = property("MainPID").and_then(|pid| pid.parse().ok()).ok_or("systemd did not report the backend PID")?;
                    Ok(State::Loaded(Loaded { file_state, active, pid }))
                }
                _ => Err(format!("The effective systemd user unit {UNIT} belongs to another installation or was customized")),
            }
        }
    }

    enum State {
        Missing,
        Loaded(Loaded),
        Reload,
    }

    struct Loaded {
        file_state: String,
        active: bool,
        pid: u32,
    }

    impl Loaded {
        fn enabled(&self) -> bool {
            matches!(self.file_state.as_str(), "enabled" | "enabled-runtime")
        }
    }

    pub(super) fn running_pid() -> Result<Option<u32>, String> {
        Ok(Registration::for_installation()?
            .state()?
            .and_then(|loaded| loaded.enabled().then_some(loaded.pid)))
    }

    pub(super) fn inspect_login_service() -> Result<(bool, bool), String> {
        match Registration::for_installation()?.loaded_state()? {
            State::Missing => Ok((false, false)),
            State::Loaded(loaded) => Ok((loaded.enabled(), loaded.active)),
            State::Reload => Err(format!("The systemd user unit {UNIT} has pending changes")),
        }
    }

    pub(super) fn enable() -> Result<bool, String> {
        let registration = Registration::for_installation()?;
        let state = registration.state()?;
        if state
            .as_ref()
            .is_some_and(|state| state.file_state == "enabled")
        {
            return Ok(false);
        }
        let created = state.is_none() && !registration.packaged && !registration.exists()?;
        if created {
            super::registration::create(
                &super::registration::record_path()?,
                &registration.path,
                &registration.contents,
            )?;
        }
        let result = (|| {
            if state.is_none() {
                systemctl(&["daemon-reload"])?;
                if registration.state()?.is_none() {
                    return Err(format!(
                        "systemd could not load {}",
                        registration.path.display()
                    ));
                }
            }
            systemctl(&["enable", UNIT])?;
            Ok(true)
        })();
        if result.is_err() && created {
            super::registration::remove(
                &super::registration::record_path()?,
                &registration.path,
                &registration.contents,
            )?;
            let _ = systemctl(&["daemon-reload"]);
        }
        result
    }

    pub(super) fn disable() -> Result<(), String> {
        let registration = Registration::for_installation()?;
        let state = registration.state()?;
        match state.as_ref().map(|state| state.file_state.as_str()) {
            Some("enabled") => {
                systemctl(&["disable", UNIT])?;
            }
            Some("enabled-runtime") => {
                systemctl(&["disable", "--runtime", UNIT])?;
            }
            _ => {}
        }
        if !registration.packaged && registration.exists()? {
            super::registration::remove(
                &super::registration::record_path()?,
                &registration.path,
                &registration.contents,
            )?;
            systemctl(&["daemon-reload"])?;
        }
        Ok(())
    }

    pub(super) fn start() -> bool {
        systemctl(&["start", UNIT]).is_ok()
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn native_packages_use_the_unit_for_their_own_bundled_backend() {
            use std::path::Path;
            for backend in [super::PACKAGED_BACKEND, super::DESKTOP_BACKEND] {
                let contents = super::packaged_contents(Path::new(backend)).unwrap();
                assert!(contents
                    .lines()
                    .any(|line| line == format!("ExecStart={backend} --supervised")));
                assert!(contents.contains("RestartPreventExitStatus=75\n"));
            }
            assert!(super::packaged_contents(Path::new("/opt/private-ai-proxy-service")).is_none());
        }

        #[test]
        fn effective_restart_policy_keeps_deliberate_quits_stopped() {
            use serde_json::json;
            let safe = [
                json!("on-failure"),
                json!([[75], []]),
                json!([[], []]),
                json!([[], []]),
            ];
            assert!(super::matches_restart_policy(&safe));
            for (index, override_value) in [
                (0, json!("always")),
                (1, json!([[], []])),
                (2, json!([[0], []])),
                (3, json!([[1], []])),
            ] {
                let mut changed = safe.clone();
                changed[index] = override_value;
                assert!(!super::matches_restart_policy(&changed));
            }
        }
        #[test]
        fn a_written_unit_quotes_the_command_path() {
            let unit = super::unit(std::path::Path::new("/opt/my apps/$100%/pap")).unwrap();
            assert!(unit.contains("ExecStart=:\"/opt/my apps/$100%%/pap\" service run\n"));
            assert!(unit.contains("Restart=on-failure\n"));
        }

        #[test]
        fn global_timeout_settings_do_not_override_execution() {
            assert!(!super::overrides_execution(
                "# Fedora\n[Service]\nTimeoutStopFailureMode=abort\n"
            ));
            for key in [
                "ExecStart",
                "ExecStartPre",
                "EnvironmentFile",
                "UnsetEnvironment",
                "User",
                "WorkingDirectory",
            ] {
                assert!(
                    super::overrides_execution(&format!("[Service]\n{key} = replacement\n")),
                    "{key}"
                );
            }
        }
    }
}

#[cfg(all(target_os = "macos", not(feature = "mac-app-store")))]
mod platform {
    use std::{
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    use super::{command, registration};

    const MARKER: &str =
        "<!-- Written by `pap service enable`; `pap service disable` removes it. -->";

    pub(super) fn installed_desktop() -> bool {
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .is_ok_and(|path| super::macos_desktop_bundle(&path))
    }

    fn launchctl(arguments: &[&str]) -> bool {
        Command::new("launchctl")
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A launchd user agent: its label, and the property list that defines it.
    pub(super) struct Agent {
        pub label: String,
        pub plist: PathBuf,
        arguments: Vec<String>,
    }

    impl Agent {
        /// The agent for this user's backend, in `~/Library/LaunchAgents`,
        /// which launchd loads at login (launchd.plist(5)).
        fn backend() -> Result<Self, String> {
            let label = format!("{}.backend", crate::brand::APP_IDENTIFIER);
            let home = std::env::home_dir().ok_or("Cannot locate the home directory")?;
            let plist = home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"));
            let command = command()?.0;
            let command = command
                .to_str()
                .filter(|path| !path.chars().any(char::is_control))
                .ok_or("The command path cannot be represented in a launch agent")?;
            Ok(Self {
                label,
                plist,
                arguments: vec![command.into(), "service".into(), "run".into()],
            })
        }

        /// The user's GUI domain (launchctl(1)).
        fn domain() -> String {
            format!("gui/{}", unsafe { libc::getuid() })
        }

        fn target(&self) -> String {
            format!("{}/{}", Self::domain(), self.label)
        }

        fn loaded_pid(&self) -> Result<Option<u32>, String> {
            let output = Command::new("launchctl")
                .args(["print", &self.target()])
                .stdin(Stdio::null())
                .output()
                .map_err(|error| format!("Cannot inspect launchd agent {}: {error}", self.label))?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.code() == Some(113)
                || stderr
                    .lines()
                    .any(|line| line.trim_start().starts_with("Could not find service "))
            {
                return Ok(None);
            }
            if !output.status.success() {
                return Err(format!(
                    "Cannot inspect launchd agent {}: {}",
                    self.label,
                    stderr.trim()
                ));
            }
            let pid = std::str::from_utf8(&output.stdout)
                .ok()
                .and_then(|text| loaded_pid(text, &self.plist, &self.arguments))
                .ok_or_else(|| {
                    format!(
                        "Cannot verify the loaded launchd agent {} belongs to this installation",
                        self.label
                    )
                })?;
            Ok(Some(pid))
        }

        pub fn load(&self) -> bool {
            self.plist
                .to_str()
                .is_some_and(|plist| launchctl(&["bootstrap", &Self::domain(), plist]))
        }

        fn unload(&self) -> Result<(), String> {
            if self.loaded_pid()?.is_some() && !launchctl(&["bootout", &self.target()]) {
                return Err("Cannot remove the backend's login item".into());
            }
            Ok(())
        }

        fn start(&self) -> bool {
            self.loaded_pid().is_ok_and(|pid| pid.is_some())
                && launchctl(&["kickstart", &self.target()])
        }
    }

    /// `launchctl print` is diagnostic output, so require the complete identity
    /// to match before using a label, and fail closed on an unfamiliar format.
    fn loaded_pid(text: &str, plist: &Path, expected: &[String]) -> Option<u32> {
        let mut lines = text.lines();
        let opening = lines.next()?.strip_suffix(" = {")?;
        if !opening.starts_with("gui/") {
            return None;
        }
        let mut path = None;
        let mut program = None;
        let mut arguments = None;
        let mut pid = 0;
        while let Some(line) = lines.next() {
            if let Some(value) = line.strip_prefix("\tpath = ") {
                path = Some(Path::new(value));
            } else if let Some(value) = line.strip_prefix("\tprogram = ") {
                program = Some(value);
            } else if let Some(value) = line.strip_prefix("\tpid = ") {
                pid = value.parse().ok()?;
            } else if line == "\targuments = {" {
                let mut entries = Vec::new();
                loop {
                    let entry = lines.next()?;
                    if entry == "\t}" {
                        break;
                    }
                    entries.push(entry.strip_prefix("\t\t")?);
                }
                arguments = Some(entries);
            }
        }
        let source = path?;
        let same_source = source == plist
            || std::fs::canonicalize(source)
                .ok()
                .zip(std::fs::canonicalize(plist).ok())
                .is_some_and(|(source, plist)| source == plist);
        (same_source
            && program? == expected.first()?
            && arguments?
                .iter()
                .copied()
                .eq(expected.iter().map(String::as_str)))
        .then_some(pid)
    }

    /// Runs `arguments` at login and again whenever it exits with a failure;
    /// a clean exit, as `pap service stop` causes, is left alone.
    pub(super) fn plist(label: &str, arguments: &[&str]) -> String {
        let escape = |text: &str| {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let arguments: String = arguments
            .iter()
            .map(|argument| format!("\t\t<string>{}</string>\n", escape(argument)))
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             {MARKER}\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Label</key>\n\
             \t<string>{}</string>\n\
             \t<key>ProgramArguments</key>\n\
             \t<array>\n\
             {arguments}\
             \t</array>\n\
             \t<key>RunAtLoad</key>\n\
             \t<true/>\n\
             \t<key>KeepAlive</key>\n\
             \t<dict>\n\
             \t\t<key>SuccessfulExit</key>\n\
             \t\t<false/>\n\
             \t</dict>\n\
             \t<key>ProcessType</key>\n\
             \t<string>Background</string>\n\
             </dict>\n\
             </plist>\n",
            escape(label)
        )
    }

    fn written() -> Result<(Agent, String), String> {
        let agent = Agent::backend()?;
        let arguments = agent
            .arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let text = plist(&agent.label, &arguments);
        Ok((agent, text))
    }

    pub(super) fn running_pid() -> Result<Option<u32>, String> {
        let (agent, text) = written()?;
        if registration::exists(&registration::record_path()?, &agent.plist, &text)? {
            agent.loaded_pid()
        } else {
            Ok(None)
        }
    }

    pub(super) fn enable() -> Result<bool, String> {
        let (agent, text) = written()?;
        let record = registration::record_path()?;
        let exists = registration::exists(&record, &agent.plist, &text)?;
        if agent.loaded_pid()?.is_some() {
            return if exists {
                Ok(false)
            } else {
                Err(format!(
                    "The launchd agent {} belongs to another installation",
                    agent.label
                ))
            };
        }
        if !exists {
            registration::create(&record, &agent.plist, &text)?;
        }
        if agent.load() {
            Ok(true)
        } else {
            if !exists {
                registration::remove(&record, &agent.plist, &text)?;
            }
            Err("launchd did not accept the backend's login item".into())
        }
    }

    pub(super) fn disable() -> Result<(), String> {
        let (agent, text) = written()?;
        let record = registration::record_path()?;
        if !registration::exists(&record, &agent.plist, &text)? {
            if agent.loaded_pid()?.is_some() {
                return Err(registration::foreign(&agent.plist));
            }
            return Ok(());
        }
        agent.unload()?;
        registration::remove(&record, &agent.plist, &text)
    }

    pub(super) fn start() -> bool {
        Agent::backend().is_ok_and(|agent| agent.start())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs;

        #[test]
        fn a_plist_escapes_its_arguments() {
            let text = plist(
                "org.example.backend",
                &["/Apps/R&D <1>/pap", "service", "run"],
            );
            assert!(text.contains("<string>/Apps/R&amp;D &lt;1&gt;/pap</string>"));
            assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        }

        /// Loads, starts and removes a throwaway agent in the real launchd.
        /// It loads a unique temporary job, so only macOS CI runs it.
        #[test]
        #[ignore]
        fn launchd_round_trip() {
            let directory = tempfile::tempdir().unwrap();
            let label = format!("org.dstack.private-ai-proxy.test-{}", std::process::id());
            let agent = Agent {
                plist: directory.path().join(format!("{label}.plist")),
                label,
                arguments: vec!["/bin/sleep".into(), "600".into()],
            };
            fs::write(&agent.plist, plist(&agent.label, &["/bin/sleep", "600"])).unwrap();
            assert_eq!(agent.loaded_pid().unwrap(), None);
            assert!(agent.load());
            struct UnloadOnDrop<'a>(&'a Agent);
            impl Drop for UnloadOnDrop<'_> {
                fn drop(&mut self) {
                    launchctl(&["bootout", &self.0.target()]);
                }
            }
            let _unload = UnloadOnDrop(&agent);
            assert!(agent.loaded_pid().unwrap().is_some());
            assert!(agent.start());
            let other_file = Agent {
                label: agent.label.clone(),
                plist: directory.path().join("other.plist"),
                arguments: agent.arguments.clone(),
            };
            let other_args = Agent {
                label: agent.label.clone(),
                plist: agent.plist.clone(),
                arguments: vec!["/bin/sleep".into(), "42".into()],
            };
            for foreign in [other_file, other_args] {
                assert!(!foreign.start());
                assert!(foreign.unload().is_err());
                assert!(agent.loaded_pid().unwrap().is_some());
            }
            agent.unload().unwrap();
            assert_eq!(agent.loaded_pid().unwrap(), None);
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mac-app-store"))
)))]
mod platform {
    use super::UNSUPPORTED;

    pub(super) fn installed_desktop() -> bool {
        false
    }

    pub(super) fn running_pid() -> Result<Option<u32>, String> {
        Ok(None)
    }
    pub(super) fn enable() -> Result<bool, String> {
        Err(UNSUPPORTED.into())
    }
    pub(super) fn disable() -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub(super) fn start() -> bool {
        false
    }
}

#[cfg(any(test, all(target_os = "macos", not(feature = "mac-app-store"))))]
fn macos_desktop_bundle(executable: &std::path::Path) -> bool {
    let Some(contents) = executable
        .parent()
        .filter(|path| path.ends_with("Contents/MacOS"))
        .and_then(std::path::Path::parent)
    else {
        return false;
    };
    contents
        .parent()
        .and_then(std::path::Path::extension)
        .is_some_and(|extension| extension == "app")
        && contents.join("Info.plist").is_file()
}

#[cfg(test)]
#[test]
fn macos_desktop_enrollment_requires_an_app_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let contents = temp.path().join("Private AI Proxy.app/Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    let executable = contents.join("MacOS/private-ai-proxy-desktop");
    assert!(!macos_desktop_bundle(&executable));
    std::fs::write(contents.join("Info.plist"), "").unwrap();
    assert!(macos_desktop_bundle(&executable));
    assert!(!macos_desktop_bundle(
        &temp.path().join("target/debug/private-ai-proxy-desktop")
    ));
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        all(target_os = "macos", not(feature = "mac-app-store"))
    )
))]
mod desktop_tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    #[test]
    fn unbundled_desktop_shell_and_cli_use_the_same_command_without_enrolling() {
        const CHILD: &str = "PAP_TEST_DESKTOP_ENROLLMENT";
        if std::env::var_os(CHILD).is_some() {
            let cli = PathBuf::from(std::env::var_os("PAP_TEST_CANONICAL_COMMAND").unwrap());
            assert_eq!(command().unwrap(), (cli, false));
            assert!(!automatic_blockers().is_eligible());
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let shell = root.join("private-ai-proxy-desktop");
        let cli = root.join("private-ai-proxy");
        let links = root.join("links");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let exe = std::env::current_exe().unwrap();
        for executable in [&shell, &cli] {
            if fs::hard_link(&exe, executable).is_err() {
                fs::copy(&exe, executable).unwrap();
            }
        }
        fs::create_dir(&links).unwrap();
        let manager = links.join(if cfg!(target_os = "linux") {
            "systemctl"
        } else {
            "launchctl"
        });
        fs::write(&manager, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&manager, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = links.join("private-ai-proxy");
        std::os::unix::fs::symlink(&cli, &alias).unwrap();

        for program in [&shell, &alias] {
            let output = Command::new(program)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &links)
                .env("PAP_TEST_CANONICAL_COMMAND", &cli)
                .env(CHILD, "1")
                .args([
                    "--exact",
                    "supervisor::desktop_tests::unbundled_desktop_shell_and_cli_use_the_same_command_without_enrolling",
                ])
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}
