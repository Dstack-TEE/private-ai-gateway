//! Where Private AI Proxy keeps per-user state, resolved identically by the
//! desktop shell, the CLI, the backend and the credential helper.

use std::{env, path::PathBuf};

use crate::brand::APP_IDENTIFIER;

/// Test-only override for the home directory (and the app data directory).
pub const HOME_OVERRIDE_ENV: &str = "PRIVATE_AI_PROXY_HOME";
/// Exact app-data override used by sandboxed desktop distributions.
pub const APP_DATA_OVERRIDE_ENV: &str = "PRIVATE_AI_PROXY_DATA_DIR";

/// The per-user app data directory (tokens, connection record, locks),
/// resolved the same way by the desktop shell and the
/// bundled helper.
pub fn app_data_dir() -> Result<PathBuf, String> {
    if let Some(path) = env_path(APP_DATA_OVERRIDE_ENV) {
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err("The app data override must be an absolute path".to_string())
        };
    }
    if let Some(home) = env_path(HOME_OVERRIDE_ENV) {
        return Ok(home.join(".private-ai-proxy"));
    }
    let base = if cfg!(target_os = "macos") {
        home_dir()?.join("Library").join("Application Support")
    } else if cfg!(windows) {
        env_path("APPDATA").ok_or_else(|| "APPDATA is not set".to_string())?
    } else {
        env_path("XDG_DATA_HOME").map_or_else(
            || home_dir().map(|home| home.join(".local").join("share")),
            Ok,
        )?
    };
    Ok(base.join(APP_IDENTIFIER))
}

pub fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn home_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    if let Some(home) = env_path("USERPROFILE") {
        return Ok(home);
    }
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .ok_or_else(|| "Cannot determine the home directory".to_string())
}
