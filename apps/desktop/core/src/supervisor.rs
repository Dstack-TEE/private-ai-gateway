//! Keeps the backend running without a command: a systemd user unit starts it
//! at login and again after a crash, and the backend resumes the protection
//! session it left. The Linux packages install the unit; other installations
//! get one written for them. Where no user service manager runs (containers,
//! WSL without systemd) enabling fails and commands start the backend as
//! before.

use crate::updates::{installation, Installation};

const UNSUPPORTED: &str = "Starting the backend at login is not available for this installation";

/// Whether `pap start` turns this on by itself: installations whose backend
/// keeps its path across upgrades. A desktop app owns its backend's lifetime.
pub fn automatic() -> bool {
    cfg!(target_os = "linux")
        && matches!(
            installation(),
            Installation::Deb
                | Installation::Rpm
                | Installation::Pacman
                | Installation::SystemPackage
                | Installation::Npm
        )
}

/// Has the service manager start the backend at login and after a crash.
/// True when this call turned that on.
pub fn enable() -> Result<bool, String> {
    #[cfg(target_os = "linux")]
    return systemd::enable();
    #[cfg(not(target_os = "linux"))]
    Err(UNSUPPORTED.into())
}

/// Leaves starting the backend to commands again. A running backend goes on.
pub fn disable() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return systemd::disable();
    #[cfg(not(target_os = "linux"))]
    Err(UNSUPPORTED.into())
}

/// Starts the backend under the service manager, if it is enabled there.
/// False leaves the caller to start it itself.
pub fn start() -> bool {
    #[cfg(target_os = "linux")]
    return systemd::start();
    #[cfg(not(target_os = "linux"))]
    false
}

#[cfg(target_os = "linux")]
mod systemd {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    use super::UNSUPPORTED;

    const UNIT: &str = "private-ai-proxy.service";
    const PACKAGED_UNIT: &str = "/usr/lib/systemd/user/private-ai-proxy.service";
    const PACKAGED_BACKEND: &str = "/usr/libexec/private-ai-proxy/private-ai-proxy-service";
    /// Marks a unit this module wrote and may replace or remove.
    const HEADER: &str = "# Written by `pap service enable`; `pap service disable` removes it.";

    fn systemctl(arguments: &[&str]) -> bool {
        Command::new("systemctl")
            .arg("--user")
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn enabled() -> bool {
        systemctl(&["is-enabled", "--quiet", UNIT])
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

    /// The packaged unit's settings, for the backend at `executable`.
    pub(super) fn unit(executable: &Path) -> String {
        // Quoted, with systemd's specifier and escape characters doubled.
        let path = executable
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$");
        format!(
            "{HEADER}\n\
             [Unit]\n\
             Description=Private AI Proxy backend\n\
             StartLimitIntervalSec=60\n\
             StartLimitBurst=4\n\
             \n\
             [Service]\n\
             ExecStart=\"{path}\"\n\
             Restart=on-failure\n\
             RestartSec=1\n\
             RestartPreventExitStatus=75\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        )
    }

    /// The packaged unit, when it starts this installation's backend.
    fn packaged(backend: &Path) -> bool {
        backend == Path::new(PACKAGED_BACKEND) && Path::new(PACKAGED_UNIT).is_file()
    }

    /// Whether the unit starts this installation's backend. Another
    /// installation's unit, or one the user wrote, is theirs: it is neither
    /// used nor changed.
    fn registered() -> bool {
        crate::launch::service_executable().is_ok_and(|backend| {
            packaged(&backend)
                || user_unit()
                    .and_then(|path| fs::read_to_string(path).map_err(|error| error.to_string()))
                    .is_ok_and(|current| current == unit(&backend))
        })
    }

    /// Makes the unit start this installation's backend. A package's unit
    /// already does; otherwise one is written, replacing only one this
    /// command wrote before.
    fn register() -> Result<(), String> {
        let backend = crate::launch::service_executable()?;
        if packaged(&backend) {
            return Ok(());
        }
        let path = user_unit()?;
        let unit = unit(&backend);
        match fs::read_to_string(&path) {
            Ok(current) if current == unit => return Ok(()),
            Ok(current) if !current.starts_with(HEADER) => {
                return Err(format!(
                    "{} was not written by this command; it is left as it is",
                    path.display()
                ))
            }
            _ => {}
        }
        let directory = path.parent().ok_or("The unit path has no directory")?;
        fs::create_dir_all(directory)
            .and_then(|()| fs::write(&path, unit))
            .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
        if systemctl(&["daemon-reload"]) {
            Ok(())
        } else {
            let _ = fs::remove_file(&path);
            Err(UNSUPPORTED.into())
        }
    }

    pub(super) fn enable() -> Result<bool, String> {
        register()?;
        if enabled() {
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
                Ok(path) if path.exists() => Err(format!(
                    "{} was not written by this command; it is left as it is",
                    path.display()
                )),
                _ => Ok(()),
            };
        }
        if enabled() && !systemctl(&["disable", UNIT]) {
            return Err("Cannot disable the backend's user service".into());
        }
        if let Ok(path) = user_unit() {
            if fs::remove_file(path).is_ok() {
                systemctl(&["daemon-reload"]);
            }
        }
        Ok(())
    }

    pub(super) fn start() -> bool {
        registered() && enabled() && systemctl(&["start", UNIT])
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn a_written_unit_quotes_the_backend_path() {
            let unit = super::unit(std::path::Path::new("/opt/my apps/100%/pap-service"));
            assert!(unit.contains("ExecStart=\"/opt/my apps/100%%/pap-service\"\n"));
            assert!(unit.contains("Restart=on-failure\n"));
        }
    }
}
