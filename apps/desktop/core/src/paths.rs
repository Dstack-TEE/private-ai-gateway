//! Where Private AI Proxy keeps per-user settings and state, resolved
//! identically by the desktop shell, the CLI, the backend and the credential
//! helper.

use std::{env, path::PathBuf};

use crate::brand::APP_IDENTIFIER;

/// Test-only override for the home directory (and the app data directory).
pub const HOME_OVERRIDE_ENV: &str = "PRIVATE_AI_PROXY_HOME";
/// Exact app-data override used by sandboxed desktop distributions.
pub const APP_DATA_OVERRIDE_ENV: &str = "PRIVATE_AI_PROXY_DATA_DIR";
/// Exact override for the settings directory (`config.toml`, `credentials.toml`).
pub const CONFIG_OVERRIDE_ENV: &str = "PRIVATE_AI_PROXY_CONFIG_DIR";
/// The settings directory name under `$XDG_CONFIG_HOME` on Linux.
const CONFIG_DIR_NAME: &str = "private-ai-proxy";
/// The settings subdirectory of the app directory where the platform has no
/// separate settings location, like VS Code's `Code/User`.
const CONFIG_SUBDIR: &str = "Config";

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

/// The service's log files (`service.<date>.log`, one per day, a week kept),
/// in the app data directory like Ollama's `~/.ollama/logs`.
pub fn logs_dir() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("logs"))
}

/// The per-user settings directory. It holds only `config.toml`,
/// `credentials.toml` and the schema, never state, so it can be synced on its
/// own. Linux follows the XDG base directories
/// (`$XDG_CONFIG_HOME/private-ai-proxy`). macOS (including the Mac App Store
/// container) keeps one directory per app in Application Support, so settings
/// get their own `Config` subdirectory there, as VS Code keeps its synced
/// settings in `Application Support/Code/User`; an explicit data or home
/// override (tests) does the same.
pub fn config_dir() -> Result<PathBuf, String> {
    if let Some(path) = env_path(CONFIG_OVERRIDE_ENV) {
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err("The settings directory override must be an absolute path".to_string())
        };
    }
    if cfg!(any(target_os = "macos", windows))
        || env_path(APP_DATA_OVERRIDE_ENV).is_some()
        || env_path(HOME_OVERRIDE_ENV).is_some()
    {
        return Ok(app_data_dir()?.join(CONFIG_SUBDIR));
    }
    let base = env_path("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .map_or_else(|| home_dir().map(|home| home.join(".config")), Ok)?;
    Ok(base.join(CONFIG_DIR_NAME))
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
