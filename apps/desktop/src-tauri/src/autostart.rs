#[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
const AUTOSTART_ARG: &str = "--autostart";

#[cfg(any(test, all(target_os = "macos", not(feature = "mac-app-store"))))]
mod migration;

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod platform {
    use std::path::Path;

    use auto_launch::{AutoLaunch, AutoLaunchBuilder};
    use tauri::{AppHandle, Manager};

    pub struct ManagerState(AutoLaunch);

    pub fn setup(app: &AppHandle) -> Result<(), auto_launch::Error> {
        let executable = std::env::current_exe()?;
        #[cfg(target_os = "linux")]
        let executable = app
            .env()
            .appimage
            .map(std::path::PathBuf::from)
            .unwrap_or(executable);
        app.manage(ManagerState(build(
            app.package_info().name.as_str(),
            &executable,
        )?));
        Ok(())
    }

    pub fn is_enabled(app: &AppHandle) -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        validate_linux_home(app)?;
        app.state::<ManagerState>()
            .0
            .is_enabled()
            .map_err(|error| error.to_string())
    }

    pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        validate_linux_home(app)?;
        let manager = app.state::<ManagerState>();
        if enabled {
            manager.0.enable()
        } else {
            manager.0.disable()
        }
        .map_err(|error| error.to_string())
    }

    fn build(app_name: &str, executable: &Path) -> Result<AutoLaunch, auto_launch::Error> {
        let mut builder = AutoLaunchBuilder::new();
        builder
            .set_app_name(app_name)
            .set_app_path(&launch_path(executable)?)
            .set_args(&[super::AUTOSTART_ARG]);
        #[cfg(target_os = "linux")]
        builder.set_linux_launch_mode(auto_launch::LinuxLaunchMode::XdgAutostart);
        builder.build()
    }

    #[cfg(target_os = "windows")]
    fn launch_path(path: &Path) -> Result<String, auto_launch::Error> {
        Ok(windows_launch_path(path))
    }

    #[cfg(any(target_os = "windows", test))]
    fn windows_launch_path(path: &Path) -> String {
        format!("\"{}\"", path.display())
    }

    #[cfg(target_os = "linux")]
    fn launch_path(path: &Path) -> Result<String, auto_launch::Error> {
        let path = path.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Application path is not valid UTF-8",
            )
        })?;
        if path.contains('=') || path.contains('\0') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Application path cannot contain '=' or NUL in a desktop entry",
            )
            .into());
        }
        let mut escaped = String::with_capacity(path.len() + 2);
        escaped.push('"');
        // Desktop entry string escaping is applied before Exec argument unquoting.
        for character in path.chars() {
            match character {
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                '\\' => escaped.push_str("\\\\\\\\"),
                '"' => escaped.push_str("\\\\\\\""),
                '`' => escaped.push_str("\\\\`"),
                '$' => escaped.push_str("\\\\$"),
                '%' => escaped.push_str("%%"),
                _ => escaped.push(character),
            }
        }
        escaped.push('"');
        Ok(escaped)
    }

    #[cfg(target_os = "linux")]
    fn validate_linux_home(app: &AppHandle) -> Result<(), String> {
        let home = app
            .path()
            .home_dir()
            .map_err(|error| format!("Cannot locate the user home directory: {error}"))?;
        validate_home_path(&home)
    }

    #[cfg(target_os = "linux")]
    fn validate_home_path(path: &Path) -> Result<(), String> {
        if path.is_absolute() {
            Ok(())
        } else {
            Err("Cannot use a relative home directory for Open at Login".to_string())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[cfg(target_os = "linux")]
        #[test]
        fn escapes_linux_desktop_exec_path() {
            let cases = [
                (
                    "/usr/bin/private-ai-proxy-desktop",
                    r#""/usr/bin/private-ai-proxy-desktop""#,
                ),
                (
                    "/opt/Private AI Proxy/app",
                    r#""/opt/Private AI Proxy/app""#,
                ),
                (
                    r#"/opt/$cash/`tick`/back\slash/quo"te/%app"#,
                    r#""/opt/\\$cash/\\`tick\\`/back\\\\slash/quo\\\"te/%%app""#,
                ),
            ];

            for (path, expected) in cases {
                assert_eq!(launch_path(Path::new(path)).unwrap(), expected);
            }
            assert!(launch_path(Path::new("/opt/app=name")).is_err());
            assert!(launch_path(Path::new("/opt/app\0name")).is_err());
            assert_eq!(
                launch_path(Path::new("/opt/line\nreturn\rtab\tapp")).unwrap(),
                r#""/opt/line\nreturn\rtab\tapp""#
            );
        }

        #[test]
        fn quotes_windows_startup_path() {
            assert_eq!(
                windows_launch_path(Path::new(
                    r"C:\Program Files\Private AI Proxy\private-ai-proxy-desktop.exe"
                )),
                r#""C:\Program Files\Private AI Proxy\private-ai-proxy-desktop.exe""#
            );
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn requires_absolute_linux_home() {
            assert!(validate_home_path(Path::new("/home/user")).is_ok());
            assert!(validate_home_path(Path::new("relative/home")).is_err());
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub use platform::{is_enabled, set_enabled, setup};

#[cfg(target_os = "macos")]
mod platform {
    use objc2_core_services::{kAEOpenApplication, keyAELaunchedAsLogInItem, keyAEPropData};
    use objc2_foundation::NSAppleEventManager;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    use tauri::AppHandle;

    fn status() -> SMAppServiceStatus {
        let service = unsafe { SMAppService::mainAppService() };
        unsafe { service.status() }
    }

    pub fn is_enabled(_app: &AppHandle) -> Result<bool, String> {
        #[cfg(not(feature = "mac-app-store"))]
        if LegacyBackend(_app).legacy_enabled()? {
            return Ok(true);
        }
        match status() {
            SMAppServiceStatus::Enabled => Ok(true),
            SMAppServiceStatus::NotRegistered | SMAppServiceStatus::RequiresApproval => Ok(false),
            SMAppServiceStatus::NotFound => {
                Err("Open at Login is unavailable for this installation".to_string())
            }
            _ => Err("Open at Login returned an unknown system status".to_string()),
        }
    }

    pub fn set_enabled(_app: &AppHandle, enabled: bool) -> Result<(), String> {
        #[cfg(not(feature = "mac-app-store"))]
        if !enabled {
            return super::migration::disable(&LegacyBackend(_app));
        }
        set_native_enabled(enabled, true)?;
        #[cfg(not(feature = "mac-app-store"))]
        LegacyBackend(_app).remove_legacy()?;
        Ok(())
    }

    fn approval_required(interactive: bool) -> String {
        if interactive {
            unsafe { SMAppService::openSystemSettingsLoginItems() };
        }
        "Approve Private AI Proxy in System Settings > General > Login Items".to_string()
    }

    fn set_native_enabled(enabled: bool, interactive: bool) -> Result<(), String> {
        let service = unsafe { SMAppService::mainAppService() };
        let current = unsafe { service.status() };
        if enabled {
            if current == SMAppServiceStatus::Enabled {
                return Ok(());
            }
            if current == SMAppServiceStatus::RequiresApproval {
                return Err(approval_required(interactive));
            }
            unsafe { service.registerAndReturnError() }.map_err(|error| {
                tracing::warn!(
                    "Cannot register Open at Login: {}",
                    error.localizedDescription()
                );
                "Open at Login could not be enabled".to_string()
            })?;
            if unsafe { service.status() } == SMAppServiceStatus::RequiresApproval {
                return Err(approval_required(interactive));
            }
            if unsafe { service.status() } != SMAppServiceStatus::Enabled {
                return Err("Open at Login could not be enabled".to_string());
            }
        } else if current != SMAppServiceStatus::NotRegistered {
            unsafe { service.unregisterAndReturnError() }.map_err(|error| {
                tracing::warn!(
                    "Cannot unregister Open at Login: {}",
                    error.localizedDescription()
                );
                "Open at Login could not be disabled".to_string()
            })?;
        }
        Ok(())
    }

    pub fn launched_at_login() -> bool {
        // Keep legacy launches quiet until the one-time migration removes their registration.
        #[cfg(not(feature = "mac-app-store"))]
        if std::env::args_os().any(|arg| arg == super::AUTOSTART_ARG) {
            return true;
        }
        let manager = NSAppleEventManager::sharedAppleEventManager();
        let Some(event) = manager.currentAppleEvent() else {
            return false;
        };
        event.eventID() == kAEOpenApplication
            && event
                .paramDescriptorForKeyword(keyAEPropData)
                .is_some_and(|descriptor| descriptor.enumCodeValue() == keyAELaunchedAsLogInItem)
    }

    #[cfg(not(feature = "mac-app-store"))]
    use super::migration::Backend;

    #[cfg(not(feature = "mac-app-store"))]
    struct LegacyBackend<'a>(&'a AppHandle);

    #[cfg(not(feature = "mac-app-store"))]
    impl LegacyBackend<'_> {
        fn registration(&self) -> Result<auto_launch::AutoLaunch, String> {
            // Match tauri-plugin-autostart 2.5.1's LaunchAgent builder exactly:
            // package_info.name is the label/plist basename; the path is the canonical executable.
            // Reuse its auto-launch 0.5.0 implementation, never enable the legacy backend.
            let executable = std::env::current_exe()
                .and_then(|path| path.canonicalize())
                .map_err(|_| "Cannot locate the previous login registration".to_string())?;
            Ok(auto_launch::AutoLaunch::new(
                self.0.package_info().name.as_str(),
                &executable.display().to_string(),
                true,
                &[super::AUTOSTART_ARG],
            ))
        }
    }

    #[cfg(not(feature = "mac-app-store"))]
    impl Backend for LegacyBackend<'_> {
        fn legacy_enabled(&self) -> Result<bool, String> {
            self.registration()?
                .is_enabled()
                .map_err(|_| "Cannot read the previous login registration".into())
        }

        fn set_native_enabled(&self, enabled: bool) -> Result<(), String> {
            set_native_enabled(enabled, false)
        }

        fn remove_legacy(&self) -> Result<(), String> {
            self.registration()?
                .disable()
                .map_err(|_| "Cannot remove the previous login registration".into())
        }
    }

    #[cfg(not(feature = "mac-app-store"))]
    pub fn migrate_legacy(app: &AppHandle) {
        if let Err(error) = super::migration::migrate(&LegacyBackend(app)) {
            // Retain the old registration and retry on next launch; startup stays usable and silent.
            tracing::warn!("Open at Login migration deferred: {error}");
        }
    }
}

#[cfg(target_os = "macos")]
pub use platform::{is_enabled, launched_at_login, set_enabled};

#[cfg(all(target_os = "macos", not(feature = "mac-app-store")))]
pub use platform::migrate_legacy;

#[cfg(not(target_os = "macos"))]
pub fn launched_at_login() -> bool {
    std::env::args_os().any(|argument| argument == std::ffi::OsStr::new(AUTOSTART_ARG))
}
