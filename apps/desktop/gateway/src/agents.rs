//! Agent configuration projection: point an agent at the local gateway by
//! editing only the fields this app owns, remember what those fields held
//! before, and put them back on disconnect. Credential fields a connection
//! takes over are parked in the OS credential store and referenced opaquely;
//! configs reference a machine-local agent token (through the bundled helper)
//! never the RedPill key.
//!
//! Codex, Claude Code, and OpenCode are projected through their documented
//! custom-provider settings. Disconnecting never depends on the endpoint or
//! the catalog, so a connection made by an older version can always be
//! restored.

mod discovery;
mod projection;
mod providers;
mod registry;
mod transactions;
mod validation;

pub use discovery::app_data_dir;
use discovery::*;
use projection::*;
use providers::*;
pub use registry::{
    Agent, AgentPreview, AgentRepairAction, AgentStatus, ConfigChange, ConnectOptions,
};

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use std::process::Command;

mod oh_my_pi;
mod openclaw;
mod selection;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    brand::PRODUCT_NAME,
    catalog::{Catalog, Surface},
    config_doc::{parse_jsonc, ConfigDoc, ConfigValue, Format},
    lock,
    secrets::SecretStore,
    tokens::{self, TokenFiles, TokenSet},
};

/// Test-only override for the home directory (and the app data directory).
pub const HOME_OVERRIDE_ENV: &str = "PRIVATE_AI_PROXY_HOME";
pub use crate::brand::APP_IDENTIFIER;
const STORE_FILE: &str = "agent-connections.json";
const CODEX_CATALOG_FILE: &str = "codex-model-catalog.json";
const HELPER_MISSING: &str =
    "The credential helper is missing or invalid in this installation, so \
                              agents cannot be connected";
const RESTORE_PATH_MISSING: &str = "This legacy connection has no recorded absolute config path. \
    Access is disabled. Automatic restoration is unsafe; the recovery record is retained.";

/// File name of the bundled console helper that prints an agent's token.
pub fn helper_binary_name() -> &'static str {
    if cfg!(windows) {
        "private-ai-proxy-helper.exe"
    } else {
        "private-ai-proxy-helper"
    }
}

#[derive(Debug)]
enum ConnectFailure {
    Conflict(String),
    Unavailable(String),
}

impl From<String> for ConnectFailure {
    fn from(message: String) -> Self {
        Self::Unavailable(message)
    }
}
impl From<&str> for ConnectFailure {
    fn from(message: &str) -> Self {
        Self::Unavailable(message.to_string())
    }
}
impl ConnectFailure {
    fn message(self) -> String {
        match self {
            Self::Conflict(message) | Self::Unavailable(message) => message,
        }
    }
}

/// What a connection wrote, kept so a disconnect can restore exactly that.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Connection {
    /// Last catalog reconciled, including failed attempts, to avoid repeated writes.
    #[serde(default)]
    catalog_revision: Option<String>,
    #[serde(default)]
    config_path: Option<PathBuf>,
    fields: Vec<OwnedField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selection: Option<selection::Journal>,
    /// User intent survives protection sessions; suspended links retain only provider definitions.
    #[serde(default)]
    suspended: bool,
    #[serde(default)]
    options: ConnectOptions,
    #[serde(default)]
    attention: Option<String>,
    /// The agent is not authorized (disconnect in progress).
    #[serde(default)]
    disabled: bool,
    /// A disconnect started; the record stays until token, parked secrets,
    /// and config are all cleaned up, so a retry is idempotent.
    #[serde(default)]
    cleanup_pending: bool,
}

impl Connection {
    fn restored(&self) -> bool {
        self.suspended
            && self.selection.is_none()
            && self
                .fields
                .iter()
                .all(|field| retain_provider_field(field, &self.fields))
    }

    fn disconnected(&self) -> bool {
        self.disabled && !self.cleanup_pending && self.restored()
    }

    fn validate_recovery(&self) -> Result<(), String> {
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        if self
            .fields
            .iter()
            .any(|field| matches!(field.previous, Some(Previous::Plain(ConfigValue::Json(_)))))
        {
            return Err("This legacy connection contains a structured plaintext backup. Access is disabled; secure manual recovery is required and the existing record is retained".to_string());
        }
        Ok(())
    }

    fn restore_path(&self) -> Result<&Path, String> {
        self.config_path
            .as_deref()
            .filter(|path| path.is_absolute())
            .ok_or_else(|| RESTORE_PATH_MISSING.to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnedField {
    path: Vec<String>,
    /// What the connection wrote; `None` when it made the key absent.
    #[serde(default)]
    value: Option<ConfigValue>,
    previous: Option<Previous>,
}

/// The value a field held before the connection. Sensitive values are parked
/// in the credential store and referenced by entry name only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum Previous {
    Secret { secret_ref: String },
    Plain(ConfigValue),
}

type Store = BTreeMap<String, Connection>;

/// A secret to park in the credential store when the edit is committed.
struct PendingSecret {
    entry: String,
    value: String,
}

#[derive(Default)]
struct Edit {
    selection: Option<selection::Edit>,
    changes: Vec<ConfigChange>,
    record: Option<Connection>,
    pending_secrets: Vec<PendingSecret>,
    /// Restore entries whose value was put back (or confirmed gone); only
    /// these are released from the credential store.
    consumed_secrets: Vec<String>,
}

pub struct Projector {
    home: PathBuf,
    data_dir: PathBuf,
    helper_exe: PathBuf,
    endpoint: String,
    tool_env: bool,
    tokens: TokenFiles,
    secrets: Arc<dyn SecretStore>,
}

impl Projector {
    pub fn new(
        helper_exe: PathBuf,
        endpoint: &str,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, String> {
        let home_override = env_path(HOME_OVERRIDE_ENV);
        let tool_env = home_override.is_none();
        let home = home_override.map_or_else(home_dir, Ok)?;
        Ok(Self::at(
            home,
            app_data_dir()?,
            helper_exe,
            endpoint,
            tool_env,
            secrets,
        ))
    }

    fn at(
        home: PathBuf,
        data_dir: PathBuf,
        helper_exe: PathBuf,
        endpoint: &str,
        tool_env: bool,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            home,
            tokens: TokenFiles::new(&data_dir),
            data_dir,
            helper_exe,
            endpoint: endpoint.to_string(),
            tool_env,
            secrets,
        }
    }

    fn store_path(&self) -> PathBuf {
        self.data_dir.join(STORE_FILE)
    }

    fn codex_catalog_path(&self) -> PathBuf {
        self.data_dir.join(CODEX_CATALOG_FILE)
    }

    /// One scan: every agent's status and the token set those statuses
    /// authorize, from the same reads. Callers publish the returned set to
    /// the proxy, so what the UI reports and what the proxy accepts can
    /// never diverge; a drifted, corrupted, or unreadable config
    /// deauthorizes its token on the very next scan.
    pub fn scan(&self, catalog: Option<&Catalog>) -> Result<(Vec<AgentStatus>, TokenSet), String> {
        let store = self.load_store()?;
        let statuses: Vec<AgentStatus> = Agent::ALL
            .iter()
            .map(|agent| self.status(*agent, &store, catalog))
            .collect();
        let authorized: Vec<&str> = statuses
            .iter()
            .filter(|status| status.authorized)
            .map(|status| status.id.as_str())
            .collect();
        let tokens = self.tokens.load(&authorized)?;
        Ok((statuses, tokens))
    }

    /// Startup permission maintenance under the apply lock.
    pub fn migrate_legacy(&self) -> Result<bool, String> {
        lock::with_apply_lock(&self.data_dir, || {
            self.maintain_store_permissions()?;
            let _ = self.load_store()?;
            Ok(false)
        })
    }

    /// The exact edits `apply` would make, computed on a scratch copy, plus a
    /// revision of everything they were computed from. Nothing is persisted.
    pub fn preview(
        &self,
        agent: Agent,
        connect: bool,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<AgentPreview, String> {
        let store = self.load_store()?;
        if connect {
            self.require_helper()?;
        }
        let path = self.action_path(agent, store.get(agent.id()), connect);
        let (text, read_error) = self.config_text_at(agent, &path);
        if connect {
            if let Some(error) = read_error.clone() {
                return Err(error);
            }
        }
        let mut restore_problem = path.as_ref().err().cloned();
        let edit = match ConfigDoc::parse(agent.format(), text.as_deref().unwrap_or_default()) {
            _ if !connect && path.is_err() => Edit::default(),
            Ok(_) if connect && catalog.is_none() => Edit::default(),
            Ok(mut doc) => match self.edit(agent, connect, &mut doc, &store, catalog, options) {
                Ok(edit) => edit,
                Err(error) if !connect && store.contains_key(agent.id()) => {
                    restore_problem = Some(error);
                    Edit::default()
                }
                Err(error) => return Err(error),
            },
            // A broken config does not prevent revocation. Applying the
            // disconnect keeps its restoration journal until repair succeeds.
            Err(_) if !connect => {
                store
                    .get(agent.id())
                    .ok_or_else(|| format!("{} is not connected", agent.name()))?;
                Edit::default()
            }
            Err(reason) => return Err(self.parse_error(agent, &reason)),
        };
        Ok(AgentPreview {
            agent: self.status(agent, &store, catalog),
            connect,
            changes: edit.changes,
            note: restore_problem.map_or_else(
                || agent.note(connect).to_string(),
                |reason| {
                    format!("Access will be revoked before restoration is attempted. {reason}")
                },
            ),
            revision: revision(
                agent,
                connect,
                path.as_deref().ok(),
                text.as_deref(),
                store.get(agent.id()),
                catalog,
                options,
            ),
        })
    }

    /// Load the connection record. A pure read (symlinks refused, no
    /// permission or migration side effects); maintenance happens only under
    /// the apply lock.
    fn load_store(&self) -> Result<Store, String> {
        let text = tokens::read_private_text(&self.store_path())
            .map_err(|error| format!("Cannot read the agent connection record: {error}"))?;
        match text {
            None => Ok(Store::new()),
            Some(text) => serde_json::from_str(&text)
                .map_err(|_| "The agent connection record is corrupted".to_string()),
        }
    }

    /// Restore owner-only permissions on the record and token files, through
    /// `O_NOFOLLOW` descriptors. Called only under the apply lock (startup
    /// and transactions); reads never change permissions.
    fn maintain_store_permissions(&self) -> Result<(), String> {
        tokens::tighten_private(&self.store_path())
            .map_err(|error| format!("Cannot secure the agent connection record: {error}"))?;
        self.tokens.maintain(&Agent::ALL.map(Agent::id))
    }
}

#[derive(Default)]
struct Rollback {
    revoke_token: bool,
    secrets: Vec<(String, Option<String>)>,
    configs: Vec<(PathBuf, Option<String>)>,
}

/// SHA-256 over everything a preview was computed from: the config text, the
/// existing connection record, the catalog revision, and the user's choices.
/// Each part is length-prefixed so boundaries cannot shift. The digest is
/// compared only, never logged or shown, since the text may contain
/// credentials.
fn revision(
    agent: Agent,
    connect: bool,
    path: Option<&Path>,
    text: Option<&str>,
    record: Option<&Connection>,
    catalog: Option<&Catalog>,
    options: &ConnectOptions,
) -> String {
    let mut hasher = Sha256::new();
    let mut part = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    };
    part(agent.id().as_bytes());
    part(&[u8::from(connect)]);
    part(path.map_or(&[][..], |path| path.as_os_str().as_encoded_bytes()));
    part(text.unwrap_or_default().as_bytes());
    if let Some(path) = path {
        part(
            selection::fingerprint(agent, path, record.and_then(|r| r.selection.as_ref()))
                .unwrap_or_else(|error| error)
                .as_bytes(),
        );
    }
    part(
        serde_json::to_string(&record)
            .unwrap_or_default()
            .as_bytes(),
    );
    part(
        catalog
            .map_or("", |catalog| catalog.revision.as_str())
            .as_bytes(),
    );
    part(
        options
            .default_model
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn secret_entry(agent: Agent, path: &[String]) -> String {
    format!(
        "restore:{}:{}",
        agent.id(),
        hex(&Sha256::digest(path.join("\u{1f}")))
    )
}

/// Replace `path` atomically: refuse symlinks, re-check that the file still
/// holds `expected` right before the swap, write a random owner-only temp file
/// (never following links), keep the target's permissions when it exists,
/// rename, then fsync the directory on Unix. Callers that need cross-process
/// exclusion wrap this in `lock::with_apply_lock`.
pub fn write_atomic(path: &Path, content: &str, expected: Option<Option<&str>>) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    fs::create_dir_all(dir)?;
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to replace a symlink",
            ))
        }
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(expected) = expected {
        let current = match fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if current.as_deref() != expected {
            return Err(io::Error::other(
                "the file changed on disk since it was read",
            ));
        }
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let mut nonce = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let temp = dir.join(format!(".{name}.{}.tmp", hex(&nonce)));
    let result = (|| {
        tokens::write_private(&temp, content)?;
        if let Some(metadata) = &existing {
            fs::set_permissions(&temp, metadata.permissions())?;
        }
        fs::rename(&temp, path)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
