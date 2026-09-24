//! One-time import of 0.1.x settings on the first start of 0.2: the
//! `confidential-ai.json`, `local-api.json` and `preferences.json` files in
//! the app data directory, and the secrets 0.1.x kept in the OS credential
//! store (macOS Keychain, Windows Credential Manager, Secret Service).
//!
//! This module is the only code that still reads the OS credential store, and
//! the reason the `keyring` dependency remains. Remove both once upgrades from
//! 0.1.x are no longer supported (planned for 0.3).
//!
//! The import is idempotent and crash-safe. Secrets come first: user
//! credentials go to `credentials.toml` and device-local secrets (agent
//! restore values, pending key revocations) to `local-state.json` in the data
//! directory; both are written, synced and read back, and only then are the
//! imported credential store entries deleted. `config.toml` is written next;
//! its existence marks the import as done. The old files are moved to
//! `migrated-0.1/` in the data directory last, as a one-time backup (they
//! hold no API keys; `preferences.json` holds the web UI password hash).
//! An interrupted import reruns and merges with whatever it already wrote.
//! The credential store is touched only when an old file references an entry.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use desktop_core::{
    config::{
        self, Appearance, Config, NotificationPreferences, Profile, UpdateChannel, WebUiConfig,
        CONFIG_FILE, CONFIG_HEADER, CREDENTIALS_FILE,
    },
    contracts::{ConfidentialProfile, ListenConfig, ServiceProvider},
    private_fs,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;

use super::{parse_credentials, Credentials, ProfileCredential};
use crate::local_state::{self, LocalSecrets, RetiredCredential, LOCAL_STATE_FILE};

const SERVICE_FILE: &str = "confidential-ai.json";
const LOCAL_API_FILE: &str = "local-api.json";
const PREFERENCES_FILE: &str = "preferences.json";
const CLEANUP_FILE: &str = "account-cleanup.pending";
const AGENTS_FILE: &str = "agent-connections.json";
/// The one-time backup of the imported files, in the data directory.
pub(crate) const BACKUP_DIR: &str = "migrated-0.1";
const LEGACY_FILES: [&str; 4] = [SERVICE_FILE, LOCAL_API_FILE, PREFERENCES_FILE, CLEANUP_FILE];

/// Read access to the entries 0.1.x saved in the OS credential store.
pub(crate) trait Keychain {
    fn get(&self, entry: &str) -> Result<Option<String>, String>;
    fn delete(&self, entry: &str) -> Result<(), String>;
}

/// The OS credential store, under the service name 0.1.x used.
pub(crate) struct OsKeychain;

impl OsKeychain {
    fn entry(name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(desktop_core::brand::APP_IDENTIFIER, name).map_err(store_error)
    }

    #[cfg(target_os = "linux")]
    fn run<T: Send>(operation: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
        // The Secret Service backend uses zbus's blocking API, which creates a
        // Tokio runtime internally; run it on a plain thread.
        std::thread::scope(|scope| {
            scope
                .spawn(operation)
                .join()
                .map_err(|_| "The system credential store operation panicked".to_string())?
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn run<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        operation()
    }
}

impl Keychain for OsKeychain {
    fn get(&self, entry: &str) -> Result<Option<String>, String> {
        Self::run(|| match Self::entry(entry)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error)),
        })
    }

    fn delete(&self, entry: &str) -> Result<(), String> {
        Self::run(|| match Self::entry(entry)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(store_error(error)),
        })
    }
}

fn store_error(error: keyring::Error) -> String {
    format!("the system credential store is unavailable ({error})")
}

/// 0.1.x `confidential-ai.json`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceSettings {
    active_profile_id: String,
    profiles: Vec<ConfidentialProfile>,
    require_production_os: bool,
}

/// 0.1.x `preferences.json`.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Preferences {
    auto_cli_registration: Option<bool>,
    notifications: NotificationPreferences,
    connect_on_launch: bool,
    update_channel: Option<UpdateChannel>,
    appearance: Appearance,
    web_ui: WebUiConfig,
    web_ui_password_hash: Option<String>,
}

/// 0.1.x queued key cleanup, stored as JSON in the credential store.
#[derive(Deserialize)]
struct CleanupRecord {
    profile_id: String,
    action: String,
    provider: ServiceProvider,
    key: String,
    entry: String,
    revoke: bool,
}

/// Imports 0.1.x settings into `config_dir` unless `config.toml` exists.
/// Returns notices for anything the user has to redo.
pub(crate) fn migrate(
    config_dir: &Path,
    data_dir: &Path,
    keychain: &dyn Keychain,
) -> Result<Vec<String>, String> {
    if config_dir.join(CONFIG_FILE).exists() {
        backup(data_dir)?;
        return Ok(Vec::new());
    }
    let read = |name: &str| {
        private_fs::read_private_text(&data_dir.join(name))
            .map_err(|error| format!("Cannot read {name} from 0.1: {error}"))
    };
    let service = read(SERVICE_FILE)?;
    let local_api = read(LOCAL_API_FILE)?;
    let preferences = read(PREFERENCES_FILE)?;
    if service.is_none() && local_api.is_none() && preferences.is_none() {
        return Ok(Vec::new());
    }
    let mut notices = Vec::new();
    let backup_dir = data_dir.join(BACKUP_DIR);
    let service: Option<ServiceSettings> = parse(SERVICE_FILE, service, &backup_dir, &mut notices);
    let local_api: Option<ListenConfig> =
        parse(LOCAL_API_FILE, local_api, &backup_dir, &mut notices);
    let preferences: Preferences =
        parse(PREFERENCES_FILE, preferences, &backup_dir, &mut notices).unwrap_or_default();

    let mut config = Config {
        connect_on_launch: preferences.connect_on_launch,
        appearance: preferences.appearance,
        update_channel: preferences.update_channel,
        auto_cli_registration: preferences.auto_cli_registration,
        notifications: preferences.notifications,
        web_ui: preferences.web_ui,
        local_api: local_api.unwrap_or_default(),
        ..Config::default()
    };
    // Existing credentials.toml content is from an interrupted import.
    let credentials_path = config_dir.join(CREDENTIALS_FILE);
    let mut credentials = match private_fs::read_private_text(&credentials_path)
        .map_err(|error| format!("Cannot read {CREDENTIALS_FILE}: {error}"))?
    {
        Some(text) => parse_credentials(&text)?,
        None => Credentials::default(),
    };
    if credentials.web_ui.password_hash.is_none() {
        credentials.web_ui.password_hash = preferences.web_ui_password_hash;
    }

    let mut import = Import {
        keychain,
        imported: Vec::new(),
        retired: Vec::new(),
        failure: None,
    };
    let mut missing = Vec::new();
    if let Some(service) = service {
        config.require_production_os = service.require_production_os;
        for profile in service.profiles {
            let entry = format!(
                "service-profile-{}-api-key",
                profile.credential_ref.as_deref().unwrap_or(&profile.id)
            );
            let id = profile.id.clone();
            let name = profile.name.clone();
            if profile.credential_saved && !credentials.profiles.contains_key(&id) {
                match import.get(&entry) {
                    Some(key) => {
                        import.imported.push(entry);
                        credentials
                            .profiles
                            .insert(id.clone(), ProfileCredential { api_key: key });
                    }
                    None => missing.push(name.clone()),
                }
            }
            config.profiles.insert(
                id,
                Profile {
                    name,
                    provider: profile.provider,
                    remote_url: profile.remote_url,
                    auth: profile.auth,
                    verified_at: profile.verified_at,
                },
            );
        }
        config.active_profile = service.active_profile_id;
    }
    // Existing local state is from an interrupted import too.
    let local_path = data_dir.join(LOCAL_STATE_FILE);
    let mut local = local_state::load(&local_path)?;
    import.cleanup_records(data_dir, &mut local)?;
    import.agent_restore_values(data_dir, &mut local)?;
    if let Err(invalid) = config::validate(&mut config) {
        notices.push(format!(
            "Settings: Some 0.1 settings were invalid and were reset ({}: {}).",
            invalid.path.join("."),
            invalid.message
        ));
        let profiles = std::mem::take(&mut config.profiles);
        config = Config {
            profiles,
            ..Config::default()
        };
        if config::validate(&mut config).is_err() {
            config = Config::default();
        }
    }
    credentials
        .profiles
        .retain(|id, _| config.profiles.contains_key(id));

    // 1. Secrets, durable and verified, before anything is deleted.
    if credentials != Credentials::default() || credentials_path.exists() {
        let text = super::render(super::CREDENTIALS_HEADER, &credentials)?;
        private_fs::write_atomic(&credentials_path, &text, None)
            .and_then(|()| private_fs::tighten_private(&credentials_path))
            .map_err(|error| format!("Cannot write {CREDENTIALS_FILE}: {error}"))?;
        let saved = private_fs::read_private_text(&credentials_path)
            .map_err(|error| format!("Cannot verify {CREDENTIALS_FILE}: {error}"))?
            .ok_or_else(|| format!("Cannot verify {CREDENTIALS_FILE}"))
            .and_then(|text| parse_credentials(&text))?;
        if saved != credentials {
            return Err(format!(
                "{CREDENTIALS_FILE} did not read back as written; nothing was imported"
            ));
        }
    }
    if local != LocalSecrets::default() || local_path.exists() {
        local_state::save(&local_path, &local)?;
        if local_state::load(&local_path)? != local {
            return Err(format!(
                "{LOCAL_STATE_FILE} did not read back as written; nothing was imported"
            ));
        }
    }
    // 2. The imported secrets now exist only in these files.
    import.delete_imported();
    // 3. Settings; their existence marks the import as done.
    let text = super::render(CONFIG_HEADER, &config)?;
    let config_path = config_dir.join(CONFIG_FILE);
    private_fs::write_atomic(&config_path, &text, None)
        .map_err(|error| format!("Cannot write {CONFIG_FILE}: {error}"))?;
    let saved = fs::read_to_string(&config_path)
        .map_err(|error| format!("Cannot verify {CONFIG_FILE}: {error}"))
        .and_then(|text| config::parse(&text))?;
    if saved != config {
        return Err(format!("{CONFIG_FILE} did not read back as written"));
    }
    // 4. The old files, kept once.
    backup(data_dir)?;

    if let Some(failure) = import.failure {
        let profiles = if missing.is_empty() {
            String::new()
        } else {
            format!(" Re-enter the API key for: {}.", missing.join(", "))
        };
        notices.push(format!(
            "Settings: Saved credentials could not be imported from 0.1 because {failure}.{profiles}"
        ));
    } else if !missing.is_empty() {
        notices.push(format!(
            "Settings: No saved API key was found for {}. Re-enter it in Settings.",
            missing.join(", ")
        ));
    }
    Ok(notices)
}

/// An old file's settings; one that cannot be read is reported and kept in the backup.
fn parse<T: DeserializeOwned>(
    name: &str,
    text: Option<String>,
    backup_dir: &Path,
    notices: &mut Vec<String>,
) -> Option<T> {
    let value = serde_json::from_str(&text?).ok();
    if value.is_none() {
        notices.push(format!(
            "Settings: {name} from 0.1 could not be read and was not imported; it is kept in {}.",
            backup_dir.display()
        ));
    }
    value
}

struct Import<'a> {
    keychain: &'a dyn Keychain,
    /// Entries now held by credentials.toml, deleted once it is durable.
    imported: Vec<String>,
    /// Entries holding keys that were already replaced; the cleanup records keep them.
    retired: Vec<String>,
    failure: Option<String>,
}

impl Import<'_> {
    /// Reads one entry; after the first failure the store is not asked again.
    fn get(&mut self, entry: &str) -> Option<String> {
        if self.failure.is_some() {
            return None;
        }
        self.keychain
            .get(entry)
            .map_err(|error| self.failure = Some(error))
            .ok()
            .flatten()
    }

    /// Queued revocations: the manifest lists their entries.
    fn cleanup_records(&mut self, data_dir: &Path, local: &mut LocalSecrets) -> Result<(), String> {
        let Some(text) = private_fs::read_private_text(&data_dir.join(CLEANUP_FILE))
            .map_err(|error| format!("Cannot read {CLEANUP_FILE}: {error}"))?
        else {
            return Ok(());
        };
        let entries: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
        for entry in entries {
            let id = entry.trim_start_matches("account-cleanup-").to_string();
            if local.account_cleanup.contains_key(&id) {
                continue;
            }
            let Some(value) = self.get(&entry) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<CleanupRecord>(&value) else {
                continue;
            };
            self.imported.push(entry);
            self.retired.push(record.entry);
            local.account_cleanup.insert(
                id,
                RetiredCredential {
                    profile_id: record.profile_id,
                    action: record.action,
                    provider: record.provider,
                    key: record.key,
                    revoke: record.revoke,
                },
            );
        }
        Ok(())
    }

    /// Values connected agents held before, referenced from the connection record.
    fn agent_restore_values(
        &mut self,
        data_dir: &Path,
        local: &mut LocalSecrets,
    ) -> Result<(), String> {
        let Some(text) = private_fs::read_private_text(&data_dir.join(AGENTS_FILE))
            .map_err(|error| format!("Cannot read {AGENTS_FILE}: {error}"))?
        else {
            return Ok(());
        };
        let connections: BTreeMap<String, Value> = serde_json::from_str(&text).unwrap_or_default();
        let entries = connections
            .values()
            .filter_map(|connection| connection["fields"].as_array())
            .flatten()
            .filter_map(|field| field["previous"]["secret_ref"].as_str());
        for entry in entries {
            if local.agent_restore.contains_key(entry) {
                continue;
            }
            if let Some(value) = self.get(entry) {
                self.imported.push(entry.to_string());
                local.agent_restore.insert(entry.to_string(), value);
            }
        }
        Ok(())
    }

    /// Best effort: a leftover entry is unused and harmless.
    fn delete_imported(&self) {
        for entry in self.imported.iter().chain(&self.retired) {
            if let Err(error) = self.keychain.delete(entry) {
                desktop_core::diagnostic!(
                    "Cannot delete an imported credential store entry: {error}"
                );
                return;
            }
        }
    }
}

/// Moves the imported 0.1 files into the backup directory.
fn backup(data_dir: &Path) -> Result<(), String> {
    let present: Vec<PathBuf> = LEGACY_FILES
        .iter()
        .map(|name| data_dir.join(name))
        .filter(|path| path.exists())
        .collect();
    if present.is_empty() {
        return Ok(());
    }
    let backup = data_dir.join(BACKUP_DIR);
    private_fs::create_private_dir(&backup)
        .map_err(|error| format!("Cannot create {}: {error}", backup.display()))?;
    for path in present {
        if let Some(name) = path.file_name() {
            fs::rename(&path, backup.join(name)).map_err(|error| {
                format!("Cannot move {} to {BACKUP_DIR}: {error}", path.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
