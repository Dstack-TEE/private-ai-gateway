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
/// The settings directory name under `$XDG_CONFIG_HOME` (default `~/.config`).
const CONFIG_DIR_NAME: &str = "private-ai-proxy";
/// The settings subdirectory of the app directory where settings cannot live
/// in `~/.config`: the Mac App Store container and the overrides. Direct macOS
/// and Windows builds used it up to 0.2.0-beta.8 (see [`legacy_config_dir`]).
const CONFIG_SUBDIR: &str = "Config";

/// The layout a build follows; only the tests pick another than their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Linux,
    MacOs,
    MacAppStore,
    Windows,
}

impl Platform {
    const CURRENT: Self = if cfg!(all(target_os = "macos", feature = "mac-app-store")) {
        Self::MacAppStore
    } else if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };
}

/// Reads an environment variable as a path; see [`env_path`].
type Env<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;

/// The per-user app data directory (tokens, connection record, locks),
/// resolved the same way by the desktop shell and the
/// bundled helper.
pub fn app_data_dir() -> Result<PathBuf, String> {
    app_data_dir_in(Platform::CURRENT, &env_path)
}

fn app_data_dir_in(platform: Platform, env: Env) -> Result<PathBuf, String> {
    if let Some(path) = env(APP_DATA_OVERRIDE_ENV) {
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err("The app data override must be an absolute path".to_string())
        };
    }
    if let Some(home) = env(HOME_OVERRIDE_ENV) {
        return Ok(home.join(".private-ai-proxy"));
    }
    let base = match platform {
        Platform::MacOs | Platform::MacAppStore => home_dir_in(platform, env)?
            .join("Library")
            .join("Application Support"),
        Platform::Windows => env("APPDATA").ok_or_else(|| "APPDATA is not set".to_string())?,
        // The XDG base directory spec: a relative path is invalid and ignored.
        Platform::Linux => env("XDG_DATA_HOME")
            .filter(|path| path.is_absolute())
            .map_or_else(
                || home_dir_in(platform, env).map(|home| home.join(".local").join("share")),
                Ok,
            )?,
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
/// own. Every platform uses `$XDG_CONFIG_HOME/private-ai-proxy` (default
/// `~/.config/private-ai-proxy`, `%USERPROFILE%\.config\private-ai-proxy` on
/// Windows), as the XDG base directories specify and as gh and starship do on
/// macOS and Windows too. The Mac App Store build cannot write the real home,
/// so its settings stay in the `Config` subdirectory of its container's app
/// directory; an explicit data or home override (tests) does the same.
pub fn config_dir() -> Result<PathBuf, String> {
    config_dir_in(Platform::CURRENT, &env_path)
}

fn config_dir_in(platform: Platform, env: Env) -> Result<PathBuf, String> {
    if let Some(path) = env(CONFIG_OVERRIDE_ENV) {
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err("The settings directory override must be an absolute path".to_string())
        };
    }
    if platform == Platform::MacAppStore
        || env(APP_DATA_OVERRIDE_ENV).is_some()
        || env(HOME_OVERRIDE_ENV).is_some()
    {
        return Ok(app_data_dir_in(platform, env)?.join(CONFIG_SUBDIR));
    }
    // The XDG base directory spec: a relative path is invalid and ignored.
    let base = env("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .map_or_else(
            || home_dir_in(platform, env).map(|home| home.join(".config")),
            Ok,
        )?;
    Ok(base.join(CONFIG_DIR_NAME))
}

/// Where direct macOS and Windows builds kept the settings up to
/// 0.2.0-beta.8 (the `Config` subdirectory of the app data directory), from
/// which the backend moves them once to [`config_dir`]. `None` where the
/// location did not change: Linux, the Mac App Store build and the overrides.
pub fn legacy_config_dir() -> Option<PathBuf> {
    legacy_config_dir_in(Platform::CURRENT, &env_path)
}

fn legacy_config_dir_in(platform: Platform, env: Env) -> Option<PathBuf> {
    let overridden = [
        CONFIG_OVERRIDE_ENV,
        APP_DATA_OVERRIDE_ENV,
        HOME_OVERRIDE_ENV,
    ]
    .into_iter()
    .any(|name| env(name).is_some());
    if overridden || !matches!(platform, Platform::MacOs | Platform::Windows) {
        return None;
    }
    app_data_dir_in(platform, env)
        .ok()
        .map(|dir| dir.join(CONFIG_SUBDIR))
}

pub fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn home_dir() -> Result<PathBuf, String> {
    home_dir_in(Platform::CURRENT, &env_path)
}

fn home_dir_in(platform: Platform, env: Env) -> Result<PathBuf, String> {
    if platform == Platform::Windows {
        if let Some(home) = env("USERPROFILE") {
            return Ok(home);
        }
    }
    env("HOME")
        .or_else(|| env("USERPROFILE"))
        .ok_or_else(|| "Cannot determine the home directory".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// An absolute path on every platform the tests run on.
    fn absolute(name: &str) -> PathBuf {
        env::temp_dir().join(name)
    }

    fn resolve(platform: Platform, vars: &[(&str, PathBuf)]) -> (PathBuf, Option<PathBuf>) {
        let vars: HashMap<_, _> = vars.iter().cloned().collect();
        let env = |name: &str| vars.get(name).cloned();
        (
            config_dir_in(platform, &env).unwrap(),
            legacy_config_dir_in(platform, &env),
        )
    }

    #[test]
    fn settings_live_in_dot_config_on_every_direct_build() {
        let home = absolute("home");
        let appdata = absolute("appdata");
        let settings = home.join(".config").join("private-ai-proxy");
        let unix = [("HOME", home.clone())];
        let windows = [("USERPROFILE", home.clone()), ("APPDATA", appdata.clone())];

        assert_eq!(resolve(Platform::Linux, &unix), (settings.clone(), None));
        assert_eq!(
            resolve(Platform::MacOs, &unix),
            (
                settings.clone(),
                Some(home.join("Library/Application Support/org.dstack.private-ai-proxy/Config"))
            )
        );
        assert_eq!(
            resolve(Platform::Windows, &windows),
            (
                settings,
                Some(appdata.join("org.dstack.private-ai-proxy").join("Config"))
            )
        );
    }

    #[test]
    fn an_absolute_xdg_config_home_is_honoured_everywhere() {
        let home = absolute("home");
        let xdg = absolute("xdg");
        for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
            let vars = [
                ("HOME", home.clone()),
                ("USERPROFILE", home.clone()),
                ("APPDATA", absolute("appdata")),
                ("XDG_CONFIG_HOME", xdg.clone()),
            ];
            assert_eq!(
                resolve(platform, &vars).0,
                xdg.join("private-ai-proxy"),
                "{platform:?}"
            );
            // The spec: a relative value is invalid and ignored.
            let mut vars = vars.to_vec();
            vars[3].1 = PathBuf::from("relative");
            assert_eq!(
                resolve(platform, &vars).0,
                home.join(".config").join("private-ai-proxy"),
                "{platform:?}"
            );
        }
    }

    #[test]
    fn the_mac_app_store_keeps_settings_in_its_container() {
        // In the sandbox HOME is the container, and the app sets the data override.
        let container = absolute("Library/Containers/org.dstack.private-ai-proxy/Data");
        let data = container.join("Library/Application Support/org.dstack.private-ai-proxy");
        let expected = (data.join("Config"), None);
        assert_eq!(
            resolve(Platform::MacAppStore, &[("HOME", container.clone())]),
            expected
        );
        assert_eq!(
            resolve(
                Platform::MacAppStore,
                &[
                    ("HOME", container.clone()),
                    ("XDG_CONFIG_HOME", absolute("xdg")),
                    (APP_DATA_OVERRIDE_ENV, data)
                ]
            ),
            expected
        );
    }

    #[test]
    fn overrides_win_and_nothing_is_migrated_under_them() {
        let config = absolute("config");
        let data = absolute("data");
        let test_home = absolute("test-home");
        for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
            let base = [("HOME", absolute("home")), ("APPDATA", absolute("appdata"))];
            let with = |extra: (&str, PathBuf)| {
                let mut vars = base.to_vec();
                vars.push(extra);
                resolve(platform, &vars)
            };
            assert_eq!(
                with((CONFIG_OVERRIDE_ENV, config.clone())),
                (config.clone(), None)
            );
            assert_eq!(
                with((APP_DATA_OVERRIDE_ENV, data.clone())),
                (data.join("Config"), None)
            );
            assert_eq!(
                with((HOME_OVERRIDE_ENV, test_home.clone())),
                (test_home.join(".private-ai-proxy").join("Config"), None)
            );
        }
    }
}
