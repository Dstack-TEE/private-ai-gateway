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

#[cfg(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mac-app-store"))
))]
mod registration;

use crate::updates::{installation, Installation};

const UNSUPPORTED: &str = "Starting the backend at login is not available for this installation";

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
    cfg!(any(
        target_os = "linux",
        all(target_os = "macos", not(feature = "mac-app-store"))
    )) && !relocated()
}

/// This command by a path that survives an upgrade: the entry on `PATH` that
/// links to it, as install scripts and Homebrew create, or else its own path,
/// which a package manager keeps.
fn command() -> Result<(PathBuf, bool), String> {
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

/// Whether `pap start` turns this on by itself: installations whose command
/// keeps a path across upgrades. A desktop app owns its backend's lifetime.
pub fn automatic(agent_environment_overridden: bool) -> bool {
    supported()
        && !agent_environment_overridden
        && ![
            // An XDG override can select different credentials or session data.
            "XDG_DATA_HOME",
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
        ]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some())
        && !declined().is_some_and(|marker| marker.exists())
        && match installation() {
            Installation::Deb
            | Installation::Rpm
            | Installation::Pacman
            | Installation::SystemPackage
            | Installation::Npm => true,
            Installation::Portable => command().is_ok_and(|(_, linked)| linked),
            Installation::DesktopApp | Installation::DesktopPacman => false,
        }
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

/// Removes the login registration. The caller quits a running backend first
/// so that launchd's signal does not preserve a session the user ended.
pub fn disable() -> Result<(), String> {
    if !supported() {
        return Err(UNSUPPORTED.into());
    }
    platform::disable()?;
    if let Some(marker) = declined() {
        crate::private_fs::write_private_atomic(&marker, "", None)
            .map_err(|error| format!("Cannot write {}: {error}", marker.display()))?;
    }
    Ok(())
}

/// Whether the service manager starts this installation's backend.
pub fn enabled() -> bool {
    supported() && platform::enabled()
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

    /// The D-Bus string array preserves paths containing spaces.
    fn safe_drop_ins() -> bool {
        #[derive(serde::Deserialize)]
        struct DropIns {
            data: Vec<PathBuf>,
        }

        let drop_ins = Command::new("busctl")
            .args([
                "--user",
                "--json=short",
                "get-property",
                "org.freedesktop.systemd1",
                UNIT_OBJECT,
                "org.freedesktop.systemd1.Unit",
                "DropInPaths",
            ])
            .stdin(Stdio::null())
            .output();
        drop_ins.is_ok_and(|output| {
            output.status.success()
                && serde_json::from_slice::<DropIns>(&output.stdout).is_ok_and(|drop_ins| {
                    drop_ins.data.iter().all(|path| {
                        path.parent()
                            .and_then(Path::file_name)
                            .is_some_and(|name| name != "private-ai-proxy.service.d")
                            && fs::read_to_string(path)
                                .is_ok_and(|text| !overrides_execution(&text))
                    })
                })
        })
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
    fn packaged() -> bool {
        Path::new(PACKAGED_UNIT).is_file()
            && crate::launch::service_executable()
                .is_ok_and(|backend| backend == Path::new(PACKAGED_BACKEND))
    }

    /// The unit a user may write, as systemd.unit(5) places it.
    fn user_unit() -> Result<PathBuf, String> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
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
            let packaged = packaged();
            let (path, contents) = if packaged {
                (PathBuf::from(PACKAGED_UNIT), PACKAGED_CONTENTS.into())
            } else {
                (user_unit()?, unit(&command()?.0)?)
            };
            Ok(Self {
                path,
                contents,
                packaged,
            })
        }

        fn exists(&self) -> Result<bool, String> {
            if !self.packaged {
                return registration::exists(&self.path, &self.contents);
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
        fn state(&self) -> Result<Option<String>, String> {
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
                "--property=LoadState,FragmentPath,DropInPaths,NeedDaemonReload,UnitFileState",
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
                    Ok(State::Loaded(property("UnitFileState").ok_or("systemd did not report the unit file state")?.to_owned()))
                }
                _ => Err(format!("The effective systemd user unit {UNIT} belongs to another installation or was customized")),
            }
        }
    }

    enum State {
        Missing,
        Loaded(String),
        Reload,
    }

    pub(super) fn enabled() -> bool {
        Registration::for_installation()
            .and_then(|registration| registration.state())
            .is_ok_and(|state| matches!(state.as_deref(), Some("enabled" | "enabled-runtime")))
    }

    pub(super) fn enable() -> Result<bool, String> {
        let registration = Registration::for_installation()?;
        let state = registration.state()?;
        if state.as_deref() == Some("enabled") {
            return Ok(false);
        }
        let created = state.is_none() && !registration.packaged && !registration.exists()?;
        if created {
            super::registration::create(&registration.path, &registration.contents)?;
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
            super::registration::remove(&registration.path, &registration.contents)?;
            let _ = systemctl(&["daemon-reload"]);
        }
        result
    }

    pub(super) fn disable() -> Result<(), String> {
        let registration = Registration::for_installation()?;
        match registration.state()?.as_deref() {
            Some("enabled") => {
                systemctl(&["disable", UNIT])?;
            }
            Some("enabled-runtime") => {
                systemctl(&["disable", "--runtime", UNIT])?;
            }
            _ => {}
        }
        if !registration.packaged && registration.exists()? {
            super::registration::remove(&registration.path, &registration.contents)?;
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
        path::PathBuf,
        process::{Command, Stdio},
    };

    use super::{command, registration};

    const MARKER: &str =
        "<!-- Written by `pap service enable`; `pap service disable` removes it. -->";

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
            Ok(Self { label, plist })
        }

        /// The user's GUI domain (launchctl(1)).
        fn domain() -> String {
            format!("gui/{}", unsafe { libc::getuid() })
        }

        fn target(&self) -> String {
            format!("{}/{}", Self::domain(), self.label)
        }

        pub fn loaded(&self) -> bool {
            launchctl(&["print", &self.target()])
        }

        pub fn load(&self) -> bool {
            self.plist
                .to_str()
                .is_some_and(|plist| launchctl(&["bootstrap", &Self::domain(), plist]))
        }

        pub fn unload(&self) -> bool {
            launchctl(&["bootout", &self.target()])
        }

        pub fn start(&self) -> bool {
            launchctl(&["kickstart", &self.target()])
        }
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
        let command = command()?.0;
        let command = command.to_str().ok_or("This command's path is not UTF-8")?;
        let text = plist(&agent.label, &[command, "service", "run"]);
        Ok((agent, text))
    }

    pub(super) fn enabled() -> bool {
        written().is_ok_and(|(agent, text)| {
            registration::exists(&agent.plist, &text) == Ok(true) && agent.loaded()
        })
    }

    pub(super) fn enable() -> Result<bool, String> {
        let (agent, text) = written()?;
        let exists = registration::exists(&agent.plist, &text)?;
        if agent.loaded() {
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
            registration::create(&agent.plist, &text)?;
        }
        if agent.load() {
            Ok(true)
        } else {
            if !exists {
                registration::remove(&agent.plist, &text)?;
            }
            Err("launchd did not accept the backend's login item".into())
        }
    }

    pub(super) fn disable() -> Result<(), String> {
        let (agent, text) = written()?;
        if !registration::exists(&agent.plist, &text)? {
            return Ok(());
        }
        if agent.loaded() && !agent.unload() {
            return Err("Cannot remove the backend's login item".into());
        }
        registration::remove(&agent.plist, &text)
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
            };
            fs::write(&agent.plist, plist(&agent.label, &["/bin/sleep", "600"])).unwrap();
            assert!(!agent.loaded());
            assert!(agent.load());
            struct UnloadOnDrop<'a>(&'a Agent);
            impl Drop for UnloadOnDrop<'_> {
                fn drop(&mut self) {
                    if self.0.loaded() {
                        self.0.unload();
                    }
                }
            }
            let _unload = UnloadOnDrop(&agent);
            assert!(agent.loaded());
            assert!(agent.start());
            assert!(agent.unload());
            assert!(!agent.loaded());
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mac-app-store"))
)))]
mod platform {
    use super::UNSUPPORTED;

    pub(super) fn enabled() -> bool {
        false
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
