//! Import of this device's 0.1.x settings on the first starts of 0.2: the
//! `confidential-ai.json`, `local-api.json`, `preferences.json` and
//! `account-cleanup.pending` files in the app data directory, and the secrets
//! 0.1.x kept in the OS credential store (macOS Keychain, Windows Credential
//! Manager, Secret Service).
//!
//! This module is the only code that still reads the OS credential store, and
//! the reason the `keyring` dependency remains. Remove both once upgrades from
//! 0.1.x are no longer supported (planned for 0.3; see the removal list in
//! docs/configuration.md).
//!
//! The settings directory can be synced from another device, so the presence
//! of `config.toml` says nothing about this device. What records progress is
//! device-local, in the app data directory, and each step records itself only
//! after everything it wrote is durable:
//!
//! 1. Settings ([`import_settings`], in `Settings::open` before anything uses
//!    them): the old files become `config.toml` and the web UI password hash
//!    goes to `credentials.toml`. Moving the old files into `migrated-0.1/`
//!    records the step.
//! 2. Secrets ([`import_secrets`], once the service is listening, off the
//!    startup path): the credential store entries the old files and
//!    `agent-connections.json` reference go to `credentials.toml` (user
//!    credentials) and `local-state.json` (device-local secrets), and only
//!    then are they deleted from the store, together with entries whose value
//!    the files already hold from an interrupted run.
//!    `migrated-0.1/import-complete` records the step once all are deleted.
//!    Every store operation has a deadline, because a macOS Keychain entry
//!    created by the 0.1 app can make the service wait for an authorization
//!    prompt.
//!
//! Values already in the files win, so a synced `config.toml` is never
//! overwritten and a rerun after a crash is harmless:
//!
//! - `config.toml`: if it exists, its settings stay; only this device's 0.1
//!   profiles whose IDs it lacks are added. Otherwise it is created from the
//!   0.1 settings.
//! - `credentials.toml`: an existing API key or password hash stays. A 0.1 API
//!   key is imported only for a profile that has none and uses the same
//!   service URL and sign-in (manual key, or the same account) as this
//!   device's 0.1 profile of that ID.
//! - `local-state.json`: existing entries stay.
//!
//! A failed step writes nothing that records it, keeps the old files and the
//! credential store entries where they are, is reported in the state and by
//! `pap doctor`, and reruns on the next start.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use desktop_core::{
    config::{
        self, Appearance, Config, NotificationPreferences, Profile, UpdateChannel, WebUiConfig,
        CONFIG_FILE, CONFIG_HEADER, CREDENTIALS_FILE,
    },
    contracts::{ConfidentialProfile, ListenConfig, ProfileAuth, ServiceProvider},
    private_fs,
};
use indexmap::IndexMap;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;

use super::{
    parse_credentials, write, Credentials, ProfileCredential, Settings, CREDENTIALS_HEADER,
};
use crate::local_state::{self, LocalSecrets, LocalState, RetiredCredential, LOCAL_STATE_FILE};

const SERVICE_FILE: &str = "confidential-ai.json";
const LOCAL_API_FILE: &str = "local-api.json";
const PREFERENCES_FILE: &str = "preferences.json";
const CLEANUP_FILE: &str = "account-cleanup.pending";
const AGENTS_FILE: &str = "agent-connections.json";
/// The old files after step 1, in the data directory; kept as a backup.
const BACKUP_DIR: &str = "migrated-0.1";
/// Written into [`BACKUP_DIR`] when step 2 is done.
const COMPLETE_FILE: &str = "import-complete";
const LEGACY_FILES: [&str; 4] = [SERVICE_FILE, LOCAL_API_FILE, PREFERENCES_FILE, CLEANUP_FILE];
/// How long one credential store operation may take, including a macOS
/// Keychain authorization prompt the user has to answer.
const STORE_TIMEOUT: Duration = Duration::from_secs(60);

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

    /// Runs one operation on its own thread and waits at most
    /// [`STORE_TIMEOUT`]. An operation still waiting after that (a prompt
    /// nobody answers) is abandoned and the store counts as unavailable. The
    /// Secret Service backend also needs the plain thread: it uses zbus's
    /// blocking API, which creates a Tokio runtime internally.
    fn run<T: Send + 'static>(
        operation: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("credential-store-import".into())
            .spawn(move || {
                let _ = sender.send(operation());
            })
            .map_err(|_| "the system credential store could not be queried".to_string())?;
        match receiver.recv_timeout(STORE_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "the system credential store did not answer within {} seconds",
                STORE_TIMEOUT.as_secs()
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("the system credential store operation panicked".to_string())
            }
        }
    }
}

impl Keychain for OsKeychain {
    fn get(&self, entry: &str) -> Result<Option<String>, String> {
        let entry = entry.to_string();
        Self::run(move || match Self::entry(&entry)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error)),
        })
    }

    fn delete(&self, entry: &str) -> Result<(), String> {
        let entry = entry.to_string();
        Self::run(move || match Self::entry(&entry)?.delete_credential() {
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

/// Step 1: imports this device's 0.1 settings files, if any are left in the
/// data directory, and moves them to [`BACKUP_DIR`]. Returns notices for
/// anything the user has to redo; an error leaves every old file in place.
pub(crate) fn import_settings(config_dir: &Path, data_dir: &Path) -> Result<Vec<String>, String> {
    if !LEGACY_FILES.iter().any(|name| data_dir.join(name).exists()) {
        return Ok(Vec::new());
    }
    let read = |name: &str| {
        private_fs::read_private_text(&data_dir.join(name))
            .map_err(|error| format!("Cannot read {name} from 0.1: {error}"))
    };
    let (service, local_api, preferences) = (
        read(SERVICE_FILE)?,
        read(LOCAL_API_FILE)?,
        read(PREFERENCES_FILE)?,
    );
    let mut notices = Vec::new();
    let backup_dir = data_dir.join(BACKUP_DIR);
    let service: Option<ServiceSettings> = parse(SERVICE_FILE, service, &backup_dir, &mut notices);
    let local_api: Option<ListenConfig> =
        parse(LOCAL_API_FILE, local_api, &backup_dir, &mut notices);
    let preferences: Preferences =
        parse(PREFERENCES_FILE, preferences, &backup_dir, &mut notices).unwrap_or_default();
    let password_hash = preferences.web_ui_password_hash.clone();
    let imported = settings_from(service, local_api, preferences, &mut notices);

    // Secrets first: the password hash, into credentials.toml if it has none.
    if let Some(hash) = password_hash {
        if crate::web_ui::password::is_hash(&hash) {
            let current: Credentials =
                read_setting(config_dir, CREDENTIALS_FILE, parse_credentials)?.unwrap_or_default();
            if current.web_ui.password_hash.is_none() {
                let mut next = current.clone();
                next.web_ui.password_hash = Some(hash);
                write(
                    config_dir,
                    CREDENTIALS_FILE,
                    CREDENTIALS_HEADER,
                    &current,
                    &next,
                    parse_credentials,
                )
                .map_err(|error| error.to_string())?;
            }
        } else {
            notices.push("Settings: The web UI password from 0.1 could not be read; set it again with `pap settings set web-ui.password`.".to_string());
        }
    }

    let (current, next) = match read_setting(config_dir, CONFIG_FILE, config::parse)? {
        None => (Config::default(), imported.clone()),
        Some(current) => {
            // Kept (for example synced from another device): only profiles it lacks are added.
            let mut merged = current.clone();
            for (id, profile) in &imported.profiles {
                if !merged.profiles.contains_key(id) {
                    merged.upsert(id.clone(), profile.clone())?;
                }
            }
            config::validate(&mut merged).map_err(|invalid| invalid.message)?;
            let preferences = |config: &Config| Config {
                active_profile: String::new(),
                profiles: IndexMap::new(),
                ..config.clone()
            };
            if preferences(&imported) != preferences(&current) {
                notices.push(format!(
                    "Settings: {CONFIG_FILE} already existed (for example synced from another device), so its settings were kept. This device's other 0.1 settings are in {}.",
                    backup_dir.display()
                ));
            }
            (current, merged)
        }
    };
    if next != current {
        write(
            config_dir,
            CONFIG_FILE,
            CONFIG_HEADER,
            &current,
            &next,
            config::parse,
        )
        .map_err(|error| error.to_string())?;
    }
    backup(data_dir)?;
    Ok(notices)
}

/// The 0.1 settings as a valid `config.toml`; invalid ones are reset.
fn settings_from(
    service: Option<ServiceSettings>,
    local_api: Option<ListenConfig>,
    preferences: Preferences,
    notices: &mut Vec<String>,
) -> Config {
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
    if let Some(service) = service {
        config.require_production_os = service.require_production_os;
        for profile in service.profiles {
            config.profiles.insert(
                profile.id,
                Profile {
                    name: profile.name,
                    provider: profile.provider,
                    remote_url: profile.remote_url,
                    auth: profile.auth,
                    verified_at: profile.verified_at,
                },
            );
        }
        config.active_profile = service.active_profile_id;
    }
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
    config
}

/// A settings file as it is on disk now, `None` when it does not exist. One
/// that does not parse stops the import until it is fixed.
fn read_setting<T>(
    config_dir: &Path,
    name: &str,
    parse: fn(&str) -> Result<config::Parsed<T>, String>,
) -> Result<Option<T>, String> {
    let text = private_fs::read_private_text(&config_dir.join(name))
        .map_err(|error| format!("Cannot read {name}: {error}"))?;
    text.map(|text| {
        parse(&text)
            .map(|parsed| parsed.value)
            .map_err(|error| format!("{error}. Fix {name} so the 0.1 settings can be imported"))
    })
    .transpose()
}

/// Whether saved credentials from 0.1 may still be in the credential store:
/// step 1 has not run, or step 2 has not been recorded.
pub(crate) fn secrets_pending(data_dir: &Path) -> bool {
    let backup = data_dir.join(BACKUP_DIR);
    LEGACY_FILES.iter().any(|name| data_dir.join(name).exists())
        || (backup.is_dir() && !backup.join(COMPLETE_FILE).exists())
}

/// Step 2: imports the credential store entries this device's 0.1 files
/// reference and deletes them from the store. Returns notices for anything
/// the user has to redo. The step is recorded as done unless the store
/// failed or timed out; then it reruns on the next start.
///
/// Until it is recorded, an agent restore value missing from
/// `local-state.json` may still be in the store, so [`LocalState`] reports it
/// as unavailable rather than absent: a disconnect then fails and is retried,
/// as it did in 0.1 while the credential store was unavailable
/// (`AgentError::CredentialStore`), instead of dropping the user's key.
pub(crate) fn import_secrets(
    settings: &Settings,
    local: &LocalState,
    data_dir: &Path,
    keychain: &dyn Keychain,
) -> Result<Vec<String>, String> {
    let result = import_pending_secrets(settings, local, data_dir, keychain);
    local.set_importing(secrets_pending(data_dir));
    result
}

fn import_pending_secrets(
    settings: &Settings,
    local: &LocalState,
    data_dir: &Path,
    keychain: &dyn Keychain,
) -> Result<Vec<String>, String> {
    let backup_dir = data_dir.join(BACKUP_DIR);
    let read = |path: PathBuf| {
        private_fs::read_private_text(&path).map_err(|error| {
            format!(
                "Cannot read {} from 0.1: {error}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )
        })
    };
    let service: Option<ServiceSettings> =
        read(backup_dir.join(SERVICE_FILE))?.and_then(|text| serde_json::from_str(&text).ok());
    let cleanup: Vec<String> = read(backup_dir.join(CLEANUP_FILE))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let connections: BTreeMap<String, Value> = read(data_dir.join(AGENTS_FILE))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let snapshot = settings.snapshot()?;
    let saved = local.read()?;

    let mut import = Import {
        keychain,
        imported: Vec::new(),
        retired: Vec::new(),
        failure: None,
    };
    let (mut keys, mut missing, mut elsewhere) = (IndexMap::new(), Vec::new(), Vec::new());
    for profile in service.map(|service| service.profiles).unwrap_or_default() {
        if !profile.credential_saved {
            continue;
        }
        let entry = format!(
            "service-profile-{}-api-key",
            profile.credential_ref.as_deref().unwrap_or(&profile.id)
        );
        if let Some(saved) = snapshot.credentials.profiles.get(&profile.id) {
            import.covered(&entry, &saved.api_key);
            continue;
        }
        let Some(current) = snapshot.config.profiles.get(&profile.id) else {
            continue;
        };
        // The same service and the same account, or the key belongs elsewhere.
        let same_service =
            config::normalize_url(&profile.remote_url).is_ok_and(|url| url == current.remote_url);
        if !same_service || !same_account(&profile.auth, &current.auth) {
            elsewhere.push(profile.name);
            continue;
        }
        match import
            .get(&entry)
            .and_then(|key| config::validate_api_key(&key).ok())
        {
            Some(key) => {
                import.imported.push(entry);
                keys.insert(profile.id, key);
            }
            None => missing.push(profile.name),
        }
    }
    let mut found = LocalSecrets::default();
    import.cleanup_records(&cleanup, &saved, &mut found);
    import.agent_restore_values(&connections, &saved, &mut found);

    // 1. Secrets, durable and verified, before anything is deleted.
    if !keys.is_empty() {
        settings.update_credentials(|credentials| {
            for (id, key) in &keys {
                credentials
                    .profiles
                    .entry(id.clone())
                    .or_insert_with(|| ProfileCredential {
                        api_key: key.clone(),
                    });
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
        let written = private_fs::read_private_text(&settings.dir.join(CREDENTIALS_FILE))
            .map_err(|error| format!("Cannot verify {CREDENTIALS_FILE}: {error}"))?
            .ok_or_else(|| format!("Cannot verify {CREDENTIALS_FILE}"))
            .and_then(|text| parse_credentials(&text))?
            .value;
        if keys.keys().any(|id| !written.profiles.contains_key(id)) {
            return Err(format!("{CREDENTIALS_FILE} did not read back as written"));
        }
    }
    if found != LocalSecrets::default() {
        local.update(|secrets| {
            for (entry, value) in &found.agent_restore {
                secrets
                    .agent_restore
                    .entry(entry.clone())
                    .or_insert_with(|| value.clone());
            }
            for (id, record) in &found.account_cleanup {
                secrets
                    .account_cleanup
                    .entry(id.clone())
                    .or_insert_with(|| record.clone());
            }
            Ok::<_, String>(())
        })?;
        let written = local_state::load(&data_dir.join(LOCAL_STATE_FILE))?;
        if found
            .agent_restore
            .keys()
            .any(|entry| !written.agent_restore.contains_key(entry))
            || found
                .account_cleanup
                .keys()
                .any(|id| !written.account_cleanup.contains_key(id))
        {
            return Err(format!("{LOCAL_STATE_FILE} did not read back as written"));
        }
    }
    // 2. The imported secrets now exist only in these files.
    import.delete_imported();

    let mut notices = Vec::new();
    if let Some(failure) = import.failure {
        let profiles = if missing.is_empty() {
            String::new()
        } else {
            format!(" Re-enter the API key for: {}.", missing.join(", "))
        };
        notices.push(format!(
            "Settings: Saved credentials from 0.1 could not be fully imported because {failure}.{profiles} The import is retried on the next start."
        ));
    } else {
        // 3. Done on this device.
        private_fs::write_atomic(
            &backup_dir.join(COMPLETE_FILE),
            "The settings and saved credentials of Private AI Proxy 0.1 were imported.\n",
            None,
        )
        .map_err(|error| format!("Cannot record the 0.1 import: {error}"))?;
        if !missing.is_empty() {
            notices.push(format!(
                "Settings: No saved API key was found for {}. Re-enter it in Settings.",
                missing.join(", ")
            ));
        }
    }
    if !elsewhere.is_empty() {
        notices.push(format!(
            "Settings: {CONFIG_FILE} gives {} another service URL or account than 0.1 did on this device, so the saved 0.1 API key was not imported. Re-enter it in Settings.",
            elsewhere.join(", ")
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

/// Whether two profile credentials come from the same kind of sign-in and,
/// for account sign-in, the same account.
fn same_account(left: &ProfileAuth, right: &ProfileAuth) -> bool {
    match (left, right) {
        (ProfileAuth::ApiKey, ProfileAuth::ApiKey) => true,
        (
            ProfileAuth::OAuth {
                account_id: left, ..
            },
            ProfileAuth::OAuth {
                account_id: right, ..
            },
        ) => left == right,
        _ => false,
    }
}

struct Import<'a> {
    keychain: &'a dyn Keychain,
    /// Entries whose values the files now hold, deleted once those are durable.
    imported: Vec<String>,
    /// Entries holding keys that were already replaced; the cleanup records keep them.
    retired: Vec<String>,
    failure: Option<String>,
}

impl Import<'_> {
    /// An entry whose value a file already holds (from an earlier, interrupted
    /// run) is deleted with the imported ones if it still holds that value.
    fn covered(&mut self, entry: &str, saved: &str) {
        if self.get(entry).as_deref() == Some(saved) {
            self.imported.push(entry.to_string());
        }
    }

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
    fn cleanup_records(
        &mut self,
        entries: &[String],
        saved: &LocalSecrets,
        found: &mut LocalSecrets,
    ) {
        for entry in entries {
            let id = entry.trim_start_matches("account-cleanup-").to_string();
            let Some(value) = self.get(entry) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<CleanupRecord>(&value) else {
                continue;
            };
            let retired = RetiredCredential {
                profile_id: record.profile_id,
                action: record.action,
                provider: record.provider,
                key: record.key,
                revoke: record.revoke,
            };
            match saved.account_cleanup.get(&id) {
                Some(saved) if saved != &retired => continue,
                Some(_) => {}
                None => {
                    found.account_cleanup.insert(id, retired);
                }
            }
            self.imported.push(entry.clone());
            self.retired.push(record.entry);
        }
    }

    /// Values connected agents held before, referenced from the connection record.
    fn agent_restore_values(
        &mut self,
        connections: &BTreeMap<String, Value>,
        saved: &LocalSecrets,
        found: &mut LocalSecrets,
    ) {
        let entries = connections
            .values()
            .filter_map(|connection| connection["fields"].as_array())
            .flatten()
            .filter_map(|field| field["previous"]["secret_ref"].as_str());
        for entry in entries {
            if found.agent_restore.contains_key(entry) {
                continue;
            }
            if let Some(saved) = saved.agent_restore.get(entry) {
                self.covered(entry, saved);
            } else if let Some(value) = self.get(entry) {
                self.imported.push(entry.to_string());
                found.agent_restore.insert(entry.to_string(), value);
            }
        }
    }

    /// Deletes what the files now hold. A failure leaves the step pending, so
    /// the next start deletes what is left.
    fn delete_imported(&mut self) {
        if self.failure.is_some() {
            return;
        }
        for entry in self.imported.iter().chain(&self.retired) {
            if let Err(error) = self.keychain.delete(entry) {
                self.failure = Some(error);
                return;
            }
        }
    }
}

/// Moves the 0.1 files left in the data directory into the backup directory.
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
