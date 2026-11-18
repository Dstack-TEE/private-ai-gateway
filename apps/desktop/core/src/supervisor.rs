//! The systemd user unit the Linux packages install. Once enabled, systemd
//! starts the backend at login and again after a crash, and the backend
//! resumes the protection session it left. Elsewhere, and where no user
//! service manager runs, nothing here does anything.

#[cfg(target_os = "linux")]
mod systemd {
    use std::{path::Path, process::Command, process::Stdio};

    const UNIT: &str = "private-ai-proxy.service";
    const UNIT_FILE: &str = "/usr/lib/systemd/user/private-ai-proxy.service";
    const UNIT_BACKEND: &str = "/usr/libexec/private-ai-proxy/private-ai-proxy-service";

    /// Whether the unit starts this installation's backend.
    fn owned() -> bool {
        Path::new(UNIT_FILE).is_file()
            && crate::launch::service_executable().is_ok_and(|path| path == Path::new(UNIT_BACKEND))
    }

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

    pub fn enabled() -> bool {
        owned() && systemctl(&["is-enabled", "--quiet", UNIT])
    }

    pub fn enable() -> bool {
        owned() && !enabled() && systemctl(&["enable", UNIT])
    }

    pub fn start() -> bool {
        enabled() && systemctl(&["start", UNIT])
    }
}

/// Has the service manager start the backend at login and after a crash.
/// True when this call turned that on.
pub fn enable() -> bool {
    #[cfg(target_os = "linux")]
    return systemd::enable();
    #[cfg(not(target_os = "linux"))]
    false
}

/// Starts the backend under the service manager, if it is enabled there.
/// False leaves the caller to start it itself.
pub fn start() -> bool {
    #[cfg(target_os = "linux")]
    return systemd::start();
    #[cfg(not(target_os = "linux"))]
    false
}
