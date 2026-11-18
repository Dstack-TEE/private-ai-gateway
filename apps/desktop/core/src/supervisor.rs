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

use std::path::{Path, PathBuf};

use crate::updates::{installation, Installation};

const UNSUPPORTED: &str = "Starting the backend at login is not available for this installation";
const FOREIGN: &str = "was not written by this command; it is left as it is";
/// The command that runs the backend in the foreground.
const RUN: [&str; 2] = ["service", "run"];

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
pub fn automatic() -> bool {
    supported()
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

/// Leaves starting the backend to commands again. On macOS this ends a
/// backend the service manager runs, so stop it for a restart first.
pub fn disable() -> Result<(), String> {
    if !supported() {
        return Err(UNSUPPORTED.into());
    }
    platform::disable()?;
    if let Some(marker) = declined() {
        std::fs::write(&marker, "")
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
pub fn start() -> bool {
    enabled() && platform::start()
}

/// Replaces a registration file only if it is absent or was written here.
fn write_registration(path: &Path, marker: &str, text: &str) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(current) if current == text => return Ok(false),
        Ok(current) if !current.contains(marker) => {
            return Err(format!("{} {FOREIGN}", path.display()))
        }
        _ => {}
    }
    let directory = path.parent().ok_or("The registration has no directory")?;
    std::fs::create_dir_all(directory)
        .and_then(|()| std::fs::write(path, text))
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
    Ok(true)
}

fn run(program: &str, arguments: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::{command, run, write_registration, FOREIGN, RUN, UNSUPPORTED};

    const UNIT: &str = "private-ai-proxy.service";
    const PACKAGED_UNIT: &str = "/usr/lib/systemd/user/private-ai-proxy.service";
    const PACKAGED_BACKEND: &str = "/usr/libexec/private-ai-proxy/private-ai-proxy-service";
    const MARKER: &str = "# Written by `pap service enable`; `pap service disable` removes it.";

    fn systemctl(arguments: &[&str]) -> bool {
        run("systemctl", &[&["--user"], arguments].concat())
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
    pub(super) fn unit(command: &Path) -> String {
        // Quoted, with systemd's specifier and escape characters doubled.
        let path = command
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$");
        format!(
            "{MARKER}\n\
             [Unit]\n\
             Description=Private AI Proxy backend\n\
             StartLimitIntervalSec=60\n\
             StartLimitBurst=4\n\
             \n\
             [Service]\n\
             ExecStart=\"{path}\" {}\n\
             Restart=on-failure\n\
             RestartSec=1\n\
             RestartPreventExitStatus=75\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            RUN.join(" ")
        )
    }

    fn written() -> Result<(PathBuf, String), String> {
        Ok((user_unit()?, unit(&command()?.0)))
    }

    fn registered() -> bool {
        packaged()
            || written().is_ok_and(|(path, unit)| {
                fs::read_to_string(path).is_ok_and(|current| current == unit)
            })
    }

    pub(super) fn enabled() -> bool {
        registered() && systemctl(&["is-enabled", "--quiet", UNIT])
    }

    pub(super) fn enable() -> Result<bool, String> {
        if !packaged() {
            let (path, unit) = written()?;
            if write_registration(&path, MARKER, &unit)? && !systemctl(&["daemon-reload"]) {
                let _ = fs::remove_file(&path);
                return Err(UNSUPPORTED.into());
            }
        }
        if systemctl(&["is-enabled", "--quiet", UNIT]) {
            return Ok(false);
        }
        if systemctl(&["enable", UNIT]) {
            Ok(true)
        } else {
            Err(UNSUPPORTED.into())
        }
    }

    pub(super) fn disable() -> Result<(), String> {
        if !registered() {
            return match user_unit() {
                Ok(path) if path.exists() => Err(format!("{} {FOREIGN}", path.display())),
                _ => Ok(()),
            };
        }
        if enabled() && !systemctl(&["disable", UNIT]) {
            return Err("Cannot disable the backend's user service".into());
        }
        if !packaged() && fs::remove_file(user_unit()?).is_ok() {
            systemctl(&["daemon-reload"]);
        }
        Ok(())
    }

    pub(super) fn start() -> bool {
        systemctl(&["start", UNIT])
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn a_written_unit_quotes_the_command_path() {
            let unit = super::unit(std::path::Path::new("/opt/my apps/100%/pap"));
            assert!(unit.contains("ExecStart=\"/opt/my apps/100%%/pap\" service run\n"));
            assert!(unit.contains("Restart=on-failure\n"));
        }
    }
}

#[cfg(all(target_os = "macos", not(feature = "mac-app-store")))]
mod platform {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::{command, run, write_registration, FOREIGN, RUN};

    const MARKER: &str =
        "<!-- Written by `pap service enable`; `pap service disable` removes it. -->";

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
            run("launchctl", &["print", &self.target()])
        }

        pub fn load(&self) -> bool {
            self.plist
                .to_str()
                .is_some_and(|plist| run("launchctl", &["bootstrap", &Self::domain(), plist]))
        }

        pub fn unload(&self) -> bool {
            run("launchctl", &["bootout", &self.target()])
        }

        pub fn start(&self) -> bool {
            run("launchctl", &["kickstart", &self.target()])
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
        let text = plist(&agent.label, &[command, RUN[0], RUN[1]]);
        Ok((agent, text))
    }

    fn registered(agent: &Agent, text: &str) -> bool {
        fs::read_to_string(&agent.plist).is_ok_and(|current| current == text)
    }

    pub(super) fn enabled() -> bool {
        written().is_ok_and(|(agent, text)| registered(&agent, &text) && agent.loaded())
    }

    pub(super) fn enable() -> Result<bool, String> {
        let (agent, text) = written()?;
        // launchd keeps the definition it loaded; a changed one is loaded anew.
        if write_registration(&agent.plist, MARKER, &text)? && agent.loaded() {
            agent.unload();
        }
        if agent.loaded() {
            return Ok(false);
        }
        if agent.load() {
            Ok(true)
        } else {
            let _ = fs::remove_file(&agent.plist);
            Err("launchd did not accept the backend's login item".into())
        }
    }

    pub(super) fn disable() -> Result<(), String> {
        let (agent, text) = written()?;
        if !registered(&agent, &text) {
            return if Path::new(&agent.plist).exists() {
                Err(format!("{} {FOREIGN}", agent.plist.display()))
            } else {
                Ok(())
            };
        }
        if agent.loaded() && !agent.unload() {
            return Err("Cannot remove the backend's login item".into());
        }
        fs::remove_file(&agent.plist)
            .map_err(|error| format!("Cannot remove {}: {error}", agent.plist.display()))
    }

    pub(super) fn start() -> bool {
        Agent::backend().is_ok_and(|agent| agent.start())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

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
        /// It changes the user's login items while it runs, so only the macOS
        /// packaging job runs it.
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
