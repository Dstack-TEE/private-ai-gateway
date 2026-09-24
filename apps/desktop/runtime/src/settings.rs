//! The settings files the backend owns: `config.toml` (see
//! `desktop_core::config`) and `credentials.toml`, which holds the user's
//! credentials: provider API keys and the web UI password hash. Like Cargo's
//! `credentials.toml` and AWS's `credentials` file it is plain TOML, always
//! owner-only (0600 on Unix, a protected owner-only DACL on Windows; see
//! `private_fs::write_private_atomic`). Both
//! files describe the user, not this device, so the settings directory can be
//! synced; device-local secrets are in `crate::local_state`.
//!
//! Every write goes through this store and edits the file in place with
//! `toml_edit`, as `cargo add` does: only keys whose values changed are
//! touched, so comments, ordering and formatting survive. A write re-reads the
//! file first and replaces it only if it is still unchanged right before the
//! atomic rename; if someone saved it in between, the write fails with a
//! retryable error instead of discarding their edit, and the watcher applies
//! their edit. External edits are applied through [`Settings::reload`], driven
//! by a debounced `notify` watcher on the settings directory (as Alacritty and
//! Zed watch theirs). An invalid file keeps the last good settings in effect
//! and reports the error with its line and column; an unknown key only warns
//! (see `desktop_core::config::parse_toml`).

pub(crate) mod legacy;
#[cfg(test)]
mod tests;

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use desktop_core::{
    config::{
        self, Config, Invalid, Parsed, CONFIG_FILE, CONFIG_HEADER, CREDENTIALS_FILE, SCHEMA_FILE,
    },
    contracts::{ConfidentialProfile, ConfigFiles},
    private_fs,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, Item, TableLike};

const CREDENTIALS_HEADER: &str =
    "# Private AI Proxy credentials: provider API keys and the web UI password
# hash, in plain text. Keep this file owner-only (0600); sync it only where you
# accept plaintext secrets. Saved edits apply immediately.
#
# [profiles.<profile id>]
# api-key = \"...\"
";

/// Why writes are refused while this device's 0.1 settings are not imported;
/// the cause, which can name OS errors, is only in [`Settings::files`].
const IMPORT_PENDING: &str = "Settings from 0.1 are not imported yet, so settings cannot be changed. Fix the problem shown in Settings or by `pap doctor`, then restart Private AI Proxy.";

/// Everything in `credentials.toml`. Keys are kebab-case like `config.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Credentials {
    /// API keys by profile ID.
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub profiles: IndexMap<String, ProfileCredential>,
    #[serde(skip_serializing_if = "WebUiCredential::is_empty")]
    pub web_ui: WebUiCredential,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ProfileCredential {
    pub api_key: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct WebUiCredential {
    /// Argon2id PHC string of the sign-in password.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
}

impl WebUiCredential {
    fn is_empty(&self) -> bool {
        self.password_hash.is_none()
    }
}

/// The settings in effect.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub config: Config,
    pub credentials: Credentials,
}

#[derive(Default)]
struct Applied {
    current: Snapshot,
    config_error: Option<String>,
    credentials_error: Option<String>,
    /// Unknown keys of the files in effect, ignored.
    config_unknown: Vec<String>,
    credentials_unknown: Vec<String>,
    /// What the 0.1 import could not bring over (see [`legacy`]).
    import_notices: Vec<String>,
    revision: u64,
}

pub struct Settings {
    dir: PathBuf,
    /// Keys the credential identities; never leaves this process.
    identity_key: [u8; 32],
    /// Why this device's 0.1 settings are not imported yet. Nothing is
    /// written until a later start imports them, so the import still sees
    /// the files as they were.
    import_error: Option<String>,
    applied: Mutex<Applied>,
}

impl Settings {
    /// Imports this device's 0.1 settings (see [`legacy`]), then loads both
    /// files. Problems are returned for the state rather than failing
    /// startup; an unreadable file keeps the defaults in effect.
    pub fn open(dir: PathBuf, data_dir: &Path) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        if let Err(error) = private_fs::create_private_dir(&dir) {
            problems.push(format!(
                "Cannot create the settings directory {}: {error}",
                dir.display()
            ));
        }
        let (import_error, import_notices) = match legacy::import_settings(&dir, data_dir) {
            Ok(notices) => (None, notices),
            Err(error) => (
                Some(format!(
                    "Settings: The settings from 0.1 could not be imported: {error}. The 0.1 files are unchanged and the import is retried on the next start; until then settings cannot be changed."
                )),
                Vec::new(),
            ),
        };
        problems.extend(import_error.iter().chain(&import_notices).cloned());
        let settings = Self {
            dir,
            identity_key: rand::random(),
            import_error,
            applied: Mutex::new(Applied {
                import_notices,
                ..Applied::default()
            }),
        };
        if let Err(error) = settings.create_missing() {
            problems.push(error);
        }
        settings.reload();
        (settings, problems)
    }

    /// A new `config.toml` starts with its header and schema directive; the
    /// schema beside it always matches this build. A pending 0.1 import
    /// creates `config.toml` itself.
    fn create_missing(&self) -> Result<(), String> {
        let path = self.dir.join(CONFIG_FILE);
        if self.import_error.is_none() && !path.exists() {
            private_fs::write_atomic(&path, CONFIG_HEADER, Some(None))
                .map_err(|error| format!("Cannot create {}: {error}", path.display()))?;
        }
        let schema = config::schema();
        let path = self.dir.join(SCHEMA_FILE);
        if fs::read_to_string(&path).ok().as_deref() != Some(schema.as_str()) {
            private_fs::write_atomic(&path, &schema, None)
                .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
        }
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Applied>, String> {
        self.applied
            .lock()
            .map_err(|_| "Settings are unavailable".to_string())
    }

    pub fn snapshot(&self) -> Result<Snapshot, String> {
        Ok(self.lock()?.current.clone())
    }

    pub fn config(&self) -> Result<Config, String> {
        Ok(self.lock()?.current.config.clone())
    }

    pub fn files(&self) -> ConfigFiles {
        let (error, warnings, revision) = match self.lock() {
            Ok(applied) => (
                [
                    &self.import_error,
                    &applied.config_error,
                    &applied.credentials_error,
                ]
                .into_iter()
                .flatten()
                .cloned()
                .reduce(|left, right| format!("{left}\n{right}")),
                applied
                    .import_notices
                    .iter()
                    .chain(&applied.config_unknown)
                    .chain(&applied.credentials_unknown)
                    .cloned()
                    .collect(),
                applied.revision,
            ),
            Err(error) => (Some(error), Vec::new(), 0),
        };
        ConfigFiles {
            config_path: self.dir.join(CONFIG_FILE).to_string_lossy().into_owned(),
            credentials_path: self
                .dir
                .join(CREDENTIALS_FILE)
                .to_string_lossy()
                .into_owned(),
            error,
            warnings,
            revision,
        }
    }

    /// Adds what the 0.1 credential import could not bring over.
    pub(crate) fn add_import_notices(&self, notices: &[String]) {
        if notices.is_empty() {
            return;
        }
        if let Ok(mut applied) = self.lock() {
            applied.import_notices.extend_from_slice(notices);
            applied.revision += 1;
        }
    }

    /// Whether this device's 0.1 settings are imported (or there were none).
    pub(crate) fn import_ready(&self) -> bool {
        self.import_error.is_none()
    }

    fn writable(&self) -> Result<MutexGuard<'_, Applied>, String> {
        if self.import_error.is_some() {
            return Err(IMPORT_PENDING.to_string());
        }
        self.lock()
    }

    pub fn profile_key(&self, profile_id: &str) -> Result<Option<String>, String> {
        Ok(self
            .lock()?
            .current
            .credentials
            .profiles
            .get(profile_id)
            .map(|credential| credential.api_key.clone()))
    }

    /// The profiles as clients see them. A saved key's identity changes with
    /// the key (clients drop cached account data) but reveals nothing about it.
    pub fn profile_views(&self, snapshot: &Snapshot) -> Vec<ConfidentialProfile> {
        snapshot.config.profile_views(|id| {
            snapshot.credentials.profiles.get(id).map(|credential| {
                let digest = Sha256::new()
                    .chain_update(self.identity_key)
                    .chain_update(credential.api_key.as_bytes())
                    .finalize();
                format!("credential-{}", hex::encode(&digest[..8]))
            })
        })
    }

    /// Changes `config.toml`; returns the settings now in effect.
    pub fn update_config(
        &self,
        change: impl FnOnce(&mut Config) -> Result<(), String>,
    ) -> Result<Config, String> {
        let mut applied = self.writable()?;
        let mut next = applied.current.config.clone();
        change(&mut next)?;
        config::validate(&mut next).map_err(|invalid| invalid.message)?;
        if next != applied.current.config {
            write(
                &self.dir,
                CONFIG_FILE,
                CONFIG_HEADER,
                &applied.current.config,
                &next,
                config::parse,
            )?;
            applied.current.config = next;
            applied.config_error = None;
            applied.revision += 1;
        }
        Ok(applied.current.config.clone())
    }

    /// Changes `credentials.toml`, which is always left owner-only.
    pub fn update_credentials(
        &self,
        change: impl FnOnce(&mut Credentials) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut applied = self.writable()?;
        let mut next = applied.current.credentials.clone();
        change(&mut next)?;
        if next != applied.current.credentials {
            write(
                &self.dir,
                CREDENTIALS_FILE,
                CREDENTIALS_HEADER,
                &applied.current.credentials,
                &next,
                parse_credentials,
            )?;
            applied.current.credentials = next;
            applied.credentials_error = None;
            applied.revision += 1;
        }
        Ok(())
    }

    /// Re-reads both files. Returns the previous and current settings when
    /// what is in effect, or an error about it, changed.
    pub fn reload(&self) -> Option<(Snapshot, Snapshot)> {
        // Read under the lock so a concurrent write is either fully seen or not at all.
        let mut applied = self.lock().ok()?;
        let config = read(&self.dir.join(CONFIG_FILE), false)
            .map_err(|error| format!("Cannot read {CONFIG_FILE}: {error}"))
            .and_then(|text| {
                text.map_or_else(|| Ok(Parsed::default()), |text| config::parse(&text))
            });
        let credentials = read(&self.dir.join(CREDENTIALS_FILE), true)
            .map_err(|error| format!("Cannot read {CREDENTIALS_FILE}: {error}"))
            .and_then(|text| {
                text.map_or_else(|| Ok(Parsed::default()), |text| parse_credentials(&text))
            });
        let previous = applied.current.clone();
        let status = |applied: &Applied| {
            (
                applied.config_error.clone(),
                applied.credentials_error.clone(),
                applied.config_unknown.clone(),
                applied.credentials_unknown.clone(),
            )
        };
        let before = status(&applied);
        match config {
            Ok(config) => {
                applied.current.config = config.value;
                applied.config_unknown = config.unknown;
                applied.config_error = None;
            }
            Err(error) => applied.config_error = Some(error),
        }
        match credentials {
            Ok(credentials) => {
                applied.current.credentials = credentials.value;
                applied.credentials_unknown = credentials.unknown;
                applied.credentials_error = None;
            }
            Err(error) => applied.credentials_error = Some(error),
        }
        if applied.current == previous && status(&applied) == before {
            return None;
        }
        applied.revision += 1;
        Some((previous, applied.current.clone()))
    }

    /// Calls `changed` after either file changes on disk, debounced.
    pub fn watch(&self, changed: impl Fn() + Send + 'static) -> Result<Watcher, String> {
        use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
        let mut debouncer = new_debouncer(
            Duration::from_millis(200),
            move |events: DebounceEventResult| match events {
                Ok(events) => {
                    if events.iter().any(|event| {
                        event
                            .path
                            .file_name()
                            .is_some_and(|name| name == CONFIG_FILE || name == CREDENTIALS_FILE)
                    }) {
                        changed();
                    }
                }
                Err(error) => tracing::warn!("Settings watcher error: {error}"),
            },
        )
        .map_err(|error| format!("Cannot watch the settings files: {error}"))?;
        debouncer
            .watcher()
            .watch(&self.dir, RecursiveMode::NonRecursive)
            .map_err(|error| format!("Cannot watch the settings files: {error}"))?;
        Ok(debouncer)
    }
}

/// Writes the change from `from` to `to` into `dir/name` as it is on disk now
/// (a new file starts with its header), refusing a result that does not
/// parse; `credentials.toml` is replaced by an owner-only file.
pub(crate) fn write<T: Serialize>(
    dir: &Path,
    name: &str,
    header: &str,
    from: &T,
    to: &T,
    parse: fn(&str) -> Result<Parsed<T>, String>,
) -> Result<(), String> {
    let path = dir.join(name);
    let encode =
        |value: &T| toml_edit::ser::to_document(value).map_err(|_| format!("Cannot encode {name}"));
    let (from, to) = (encode(from)?, encode(to)?);
    let current = read(&path, name == CREDENTIALS_FILE)
        .map_err(|error| format!("Cannot read {name}: {error}"))?;
    let text =
        edit(current.as_deref().unwrap_or(header), &from, &to).map_err(|()| {
            match current.as_deref().map(parse) {
                Some(Err(error)) => format!("{error}. Fix {name} before changing settings."),
                _ => format!("{name} is not valid TOML. Fix it before changing settings."),
            }
        })?;
    parse(&text).map_err(|error| format!("{error}. Fix {name} before changing settings."))?;
    private_fs::create_private_dir(dir)
        .map_err(|error| format!("Cannot create the settings directory: {error}"))?;
    let private = name == CREDENTIALS_FILE;
    let replace = if private {
        private_fs::write_private_atomic
    } else {
        private_fs::write_atomic
    };
    replace(&path, &text, Some(current.as_deref())).map_err(|error| {
        if private_fs::ChangedOnDisk::is(&error) {
            format!("{name} changed on disk while saving; review it and retry")
        } else {
            format!("Cannot save {name}: {error}")
        }
    })?;
    Ok(())
}

/// Applies the change from `from` to `to` to a file's text. The file's
/// header, its leading comment block up to a blank line (as in a new file),
/// stays at the top; comments directly above a key belong to that key.
fn edit(text: &str, from: &DocumentMut, to: &DocumentMut) -> Result<String, ()> {
    let (header, body) = split_header(text);
    let mut document: DocumentMut = body.parse().map_err(|_| ())?;
    patch(document.as_table_mut(), from.as_table(), to.as_table());
    let body = document.to_string();
    Ok(if header.trim().is_empty() {
        body
    } else if body.is_empty() || header.ends_with("\n\n") {
        format!("{header}{body}")
    } else {
        format!("{}\n\n{body}", header.trim_end())
    })
}

fn split_header(text: &str) -> (&str, &str) {
    let (mut header_end, mut offset) = (0, 0);
    for line in text.split_inclusive('\n') {
        let line_text = line.trim();
        if !line_text.is_empty() && !line_text.starts_with('#') {
            return text.split_at(header_end);
        }
        offset += line.len();
        if line_text.is_empty() {
            header_end = offset;
        }
    }
    (text, "")
}

/// Reads a settings file; `credentials.toml` is read without following symlinks.
fn read(path: &Path, private: bool) -> io::Result<Option<String>> {
    if private {
        return private_fs::read_private_text(path);
    }
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Parses `credentials.toml`. Errors carry positions and key paths but never
/// the parser's message, which can quote a value.
pub fn parse_credentials(text: &str) -> Result<Parsed<Credentials>, String> {
    let mut parsed: Parsed<Credentials> = config::parse_toml(CREDENTIALS_FILE, text, false)?;
    let credentials = &mut parsed.value;
    let invalid = |path: Vec<&str>, message: String| {
        Invalid {
            path: path.into_iter().map(str::to_string).collect(),
            message,
        }
        .located(CREDENTIALS_FILE, text)
    };
    for (id, profile) in &mut credentials.profiles {
        config::validate_profile_id(id)
            .map_err(|message| invalid(vec!["profiles", id], message))?;
        profile.api_key = config::validate_api_key(&profile.api_key)
            .map_err(|message| invalid(vec!["profiles", id, "api-key"], message))?;
    }
    if let Some(hash) = &credentials.web_ui.password_hash {
        if !crate::web_ui::password::is_hash(hash) {
            return Err(invalid(
                vec!["web-ui", "password-hash"],
                "Expected an Argon2id hash; set the password with `pap settings set web-ui.password`"
                    .into(),
            ));
        }
    }
    Ok(parsed)
}

/// Applies the difference between two serializations of the same settings to
/// `target`: changed values are replaced in place (keeping their comments),
/// removed keys are deleted and new ones appended. Nothing else is touched.
fn patch(target: &mut dyn TableLike, from: &dyn TableLike, to: &dyn TableLike) {
    let removed: Vec<String> = from
        .iter()
        .filter(|(key, _)| to.get(key).is_none())
        .map(|(key, _)| key.to_string())
        .collect();
    for key in removed {
        target.remove(&key);
    }
    let empty = toml_edit::Table::new();
    for (key, item) in to.iter() {
        let previous = from.get(key);
        if let Some(table) = item.as_table_like() {
            let previous = previous.and_then(Item::as_table_like).unwrap_or(&empty);
            match target.get_mut(key).and_then(Item::as_table_like_mut) {
                Some(child) => patch(child, previous, table),
                None => {
                    // Only a table with changed values is added.
                    let mut child = toml_edit::Table::new();
                    child.set_implicit(true);
                    patch(&mut child, previous, table);
                    if !child.is_empty() {
                        target.insert(key, Item::Table(child));
                    }
                }
            }
            continue;
        }
        if previous.is_some_and(|previous| previous.to_string() == item.to_string()) {
            continue;
        }
        match (target.get_mut(key), item.as_value()) {
            (Some(Item::Value(existing)), Some(value)) => {
                let decor = existing.decor().clone();
                *existing = value.clone();
                *existing.decor_mut() = decor;
            }
            _ => {
                target.insert(key, item.clone());
            }
        }
    }
}

/// Stops watching when dropped.
pub type Watcher =
    notify_debouncer_mini::Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>;

/// Watches the settings files and applies external edits while the runtime lives.
pub(crate) fn spawn_watcher(
    settings: &Settings,
    runtime: std::sync::Weak<crate::controller::DesktopRuntime>,
    handle: &tokio::runtime::Handle,
) -> Result<Watcher, String> {
    let changed = Arc::new(tokio::sync::Notify::new());
    let notifier = changed.clone();
    let watcher = settings.watch(move || notifier.notify_one())?;
    // Apply edits saved between loading the files and watching them.
    changed.notify_one();
    handle.spawn(async move {
        loop {
            changed.notified().await;
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            runtime.apply_settings_files().await;
        }
    });
    Ok(watcher)
}
