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

use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    brand::PRODUCT_NAME,
    catalog::Catalog,
    config_doc::{ConfigDoc, ConfigValue, Format},
    lock,
    secrets::SecretStore,
    tokens::{self, TokenFiles, TokenSet},
};

/// Test-only override for the home directory (and the app data directory).
pub const HOME_OVERRIDE_ENV: &str = "PRIVATE_AI_GATEWAY_HOME";
pub use crate::brand::APP_IDENTIFIER;
const STORE_FILE: &str = "agent-connections.json";
const HELPER_MISSING: &str = "The credential helper is missing from this installation, so \
                              agents cannot be connected";

/// File name of the bundled console helper that prints an agent's token.
pub fn helper_binary_name() -> &'static str {
    if cfg!(windows) {
        "private-ai-gateway-helper.exe"
    } else {
        "private-ai-gateway-helper"
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub id: String,
    pub name: String,
    pub config_path: String,
    pub installed: bool,
    /// The config currently carries this app's projection and a token exists.
    pub connected: bool,
    /// A connection record exists (whatever the config now says).
    pub recorded: bool,
    /// The proxy would authorize this agent's token right now: recorded,
    /// enabled, config readable and still exactly the app's projection.
    pub authorized: bool,
    /// Something the user must act on (removed model, incomplete disconnect).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One config field a connection changes. Sensitive fields never show their
/// values; `None` means absent.
#[derive(Clone, Debug, Serialize)]
pub struct ConfigChange {
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPreview {
    pub agent: AgentStatus,
    pub connect: bool,
    pub changes: Vec<ConfigChange>,
    pub note: String,
    /// Fingerprint of the inputs the preview was computed from; `apply`
    /// refuses when it no longer matches.
    pub revision: String,
}

/// User choices a connection is projected with.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectOptions {
    /// Optional default selected from the verified catalog. The full model
    /// list is discovered natively or generated from that catalog.
    #[serde(default)]
    pub default_model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Codex,
    ClaudeCode,
    OpenCode,
    Pi,
    Hermes,
}

impl Agent {
    pub const ALL: [Agent; 5] = [
        Agent::Codex,
        Agent::ClaudeCode,
        Agent::OpenCode,
        Agent::Pi,
        Agent::Hermes,
    ];

    pub fn from_id(id: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|agent| agent.id() == id)
            .ok_or_else(|| "Unknown agent".to_string())
    }

    pub fn id(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::ClaudeCode => "claude-code",
            Agent::OpenCode => "opencode",
            Agent::Pi => "pi",
            Agent::Hermes => "hermes",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::ClaudeCode => "Claude Code",
            Agent::OpenCode => "OpenCode",
            Agent::Pi => "Pi",
            Agent::Hermes => "Hermes",
        }
    }

    /// The official CLI executable name, for install detection on PATH.
    fn cli_names(self) -> &'static [&'static str] {
        match self {
            Agent::Codex => &["codex"],
            Agent::ClaudeCode => &["claude"],
            Agent::OpenCode => &["opencode"],
            Agent::Pi => &["pi"],
            Agent::Hermes => &["hermes"],
        }
    }

    fn format(self) -> Format {
        match self {
            Agent::Codex => Format::Toml,
            Agent::ClaudeCode | Agent::OpenCode | Agent::Pi => Format::Json,
            Agent::Hermes => Format::Yaml,
        }
    }

    /// The live user-level config file. With `tool_env` each tool's own
    /// location override is honored.
    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf {
        let override_dir = |name: &str| tool_env.then(|| env_path(name)).flatten();
        match self {
            Agent::Codex => override_dir("CODEX_HOME")
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
            Agent::ClaudeCode => override_dir("CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|| home.join(".claude"))
                .join("settings.json"),
            Agent::OpenCode => override_dir("OPENCODE_CONFIG").unwrap_or_else(|| {
                override_dir("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| home.join(".config"))
                    .join("opencode")
                    .join("opencode.json")
            }),
            Agent::Pi => override_dir("PI_AGENT_DIR")
                .unwrap_or_else(|| home.join(".pi").join("agent"))
                .join("models.json"),
            Agent::Hermes => override_dir("HERMES_HOME")
                .unwrap_or_else(|| home.join(".hermes"))
                .join("config.yaml"),
        }
    }

    fn note(self, connect: bool) -> &'static str {
        if !connect {
            return "Only fields written by this app are restored; credentials taken \
                    over at connect come back from the system credential store; edits made \
                    since are left in place. The agent's local token is revoked.";
        }
        match self {
            Agent::Codex => {
                "Codex will use its official custom model provider with the Responses API, the \
                 selected model from the verified catalog, and command-backed authentication. \
                 Restart Codex after applying."
            }
            Agent::OpenCode => {
                "OpenCode will use an app-owned provider catalog generated from the verified \
                 service and a file-backed machine-local token. Restart OpenCode after applying."
            }
            Agent::ClaudeCode => {
                "Claude Code will authenticate through apiKeyHelper with a machine-local token \
                 and discover models from the verified service. Credentials set in this settings file are taken over and restored on \
                 disconnect; a token exported in your shell would still take priority, so unset \
                 ANTHROPIC_AUTH_TOKEN and ANTHROPIC_API_KEY there. A claude.ai login is not \
                 used through the gateway."
            }
            Agent::Pi => {
                "Pi will load an app-owned provider catalog generated from the verified service. Choose a model in Pi after applying. Restart Pi after applying."
            }
            Agent::Hermes => {
                "Hermes will discover models from the local gateway and authenticate with a machine-local token command. Start a new Hermes session after applying."
            }
        }
    }
}

/// Everything a projection is computed from.
struct Inputs<'a> {
    endpoint: &'a str,
    helper_exe: &'a Path,
    token_path: &'a Path,
    catalog: Option<&'a Catalog>,
    options: &'a ConnectOptions,
}

/// The fields this app owns for the agent and the values a connection writes.
fn fields(agent: Agent, inputs: &Inputs<'_>) -> Result<Vec<Field>, String> {
    let catalog = inputs
        .catalog
        .ok_or_else(|| "The verified model list is not available".to_string())?;
    let default_model = inputs
        .options
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if default_model.is_some_and(|model| catalog.get(model).is_none()) {
        return Err(format!(
            "`{}` is not in the verified model list",
            default_model.unwrap_or_default()
        ));
    }
    if agent == Agent::Codex && default_model.is_none() {
        return Err("Choose a verified default model for Codex".to_string());
    }
    let base = inputs.endpoint.trim_end_matches('/');
    Ok(match agent {
        Agent::Codex => {
            let mut fields = vec![
                set(&["model_provider"], "private_ai_gateway"),
                set(
                    &["model_providers", "private_ai_gateway", "name"],
                    PRODUCT_NAME,
                ),
                set(
                    &["model_providers", "private_ai_gateway", "base_url"],
                    format!("{base}/v1"),
                ),
                set(
                    &["model_providers", "private_ai_gateway", "wire_api"],
                    "responses",
                ),
                set(
                    &["model_providers", "private_ai_gateway", "auth", "command"],
                    inputs.helper_exe.display().to_string(),
                ),
                list(
                    &["model_providers", "private_ai_gateway", "auth", "args"],
                    &["--agent-token", "codex"],
                ),
                number(
                    &[
                        "model_providers",
                        "private_ai_gateway",
                        "auth",
                        "timeout_ms",
                    ],
                    5_000,
                ),
                number(
                    &[
                        "model_providers",
                        "private_ai_gateway",
                        "auth",
                        "refresh_interval_ms",
                    ],
                    0,
                ),
            ];
            if let Some(model) = default_model {
                fields.push(set(&["model"], model));
            }
            fields
        }
        Agent::ClaudeCode => {
            let mut fields = vec![
                set(&["env", "ANTHROPIC_BASE_URL"], base),
                set(&["env", "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1"),
                set(
                    &["apiKeyHelper"],
                    helper_command(inputs.helper_exe, "claude-code")?,
                ),
                absent(&["env", "ANTHROPIC_AUTH_TOKEN"]),
                absent(&["env", "ANTHROPIC_API_KEY"]),
            ];
            if let Some(model) = default_model {
                fields.push(set(&["env", "ANTHROPIC_MODEL"], model));
            }
            fields
        }
        Agent::OpenCode => {
            let provider = "private-ai-gateway";
            let mut fields = vec![generated_catalog(
                &["provider", provider],
                opencode_provider(catalog, base, inputs.token_path),
                catalog.models.len(),
            )];
            if let Some(model) = default_model {
                fields.push(set(&["model"], format!("{provider}/{model}")));
            }
            fields
        }
        Agent::Pi => vec![generated_catalog(
            &["providers", "private-ai-gateway"],
            pi_provider(catalog, base, inputs.helper_exe)?,
            catalog.models.len(),
        )],
        Agent::Hermes => {
            let provider = "private-ai-gateway";
            let mut fields = vec![
                set(&["providers", provider, "name"], PRODUCT_NAME),
                set(&["providers", provider, "api"], format!("{base}/v1")),
                set(&["providers", provider, "transport"], "chat_completions"),
                boolean(&["providers", provider, "discover_models"], true),
                set(
                    &["providers", provider, "key_cmd"],
                    helper_command(inputs.helper_exe, "hermes")?,
                ),
                set(&["model", "provider"], format!("custom:{provider}")),
            ];
            if let Some(model) = default_model {
                fields.push(set(&["providers", provider, "default_model"], model));
                fields.push(set(&["model", "default"], model));
            }
            fields
        }
    })
}

fn opencode_provider(catalog: &Catalog, base: &str, token_path: &Path) -> serde_json::Value {
    let models = catalog
        .models
        .iter()
        .map(|model| {
            let mut config = serde_json::Map::new();
            config.insert(
                "name".to_string(),
                serde_json::Value::String(model.display_name().to_string()),
            );
            if model.remote.context_length.is_some() || model.remote.max_output_length.is_some() {
                let mut limit = serde_json::Map::new();
                if let Some(value) = model.remote.context_length {
                    limit.insert("context".to_string(), serde_json::Value::from(value));
                }
                if let Some(value) = model.remote.max_output_length {
                    limit.insert("output".to_string(), serde_json::Value::from(value));
                }
                config.insert("limit".to_string(), serde_json::Value::Object(limit));
            }
            (model.id().to_string(), serde_json::Value::Object(config))
        })
        .collect();
    serde_json::json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": PRODUCT_NAME,
        "options": {
            "baseURL": format!("{base}/v1"),
            "apiKey": format!("{{file:{}}}", token_path.display()),
        },
        "models": serde_json::Value::Object(models),
    })
}

fn pi_provider(
    catalog: &Catalog,
    base: &str,
    helper_exe: &Path,
) -> Result<serde_json::Value, String> {
    let models: Vec<serde_json::Value> = catalog
        .models
        .iter()
        .map(|model| {
            let mut value = serde_json::Map::new();
            value.insert(
                "id".to_string(),
                serde_json::Value::String(model.id().to_string()),
            );
            value.insert(
                "name".to_string(),
                serde_json::Value::String(model.display_name().to_string()),
            );
            if let Some(context) = model.remote.context_length {
                value.insert(
                    "contextWindow".to_string(),
                    serde_json::Value::from(context),
                );
            }
            if let Some(output) = model.remote.max_output_length {
                value.insert("maxTokens".to_string(), serde_json::Value::from(output));
            }
            let input = model.string_array("input_modalities");
            if !input.is_empty() {
                value.insert("input".to_string(), serde_json::json!(input));
            }
            if model
                .string_array("supported_features")
                .iter()
                .any(|feature| feature == "reasoning")
            {
                value.insert("reasoning".to_string(), serde_json::Value::Bool(true));
            }
            let mut cost = serde_json::Map::new();
            for (source, target) in [
                ("prompt", "input"),
                ("completion", "output"),
                ("input_cache_read", "cacheRead"),
                ("input_cache_write", "cacheWrite"),
            ] {
                if let Some(price) = model
                    .price_per_million(source)
                    .and_then(serde_json::Number::from_f64)
                {
                    cost.insert(target.to_string(), serde_json::Value::Number(price));
                }
            }
            if !cost.is_empty() {
                value.insert("cost".to_string(), serde_json::Value::Object(cost));
            }
            serde_json::Value::Object(value)
        })
        .collect();
    Ok(serde_json::json!({
        "baseUrl": format!("{base}/v1"),
        "api": "openai-responses",
        "apiKey": format!("!{}", helper_command(helper_exe, "pi")?),
        "models": models,
    }))
}

struct Field {
    path: Vec<String>,
    /// `None` makes the key absent.
    value: Option<ConfigValue>,
    /// Concise preview text for generated structured values.
    preview: Option<String>,
}

fn set(path: &[&str], value: impl Into<String>) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Str(value.into())),
        preview: None,
    }
}

fn number(path: &[&str], value: u64) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Number(value)),
        preview: None,
    }
}

fn boolean(path: &[&str], value: bool) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Bool(value)),
        preview: None,
    }
}

fn generated_catalog(path: &[&str], value: serde_json::Value, models: usize) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Json(value)),
        preview: Some(format!("Generated catalog ({models} models)")),
    }
}

fn list(path: &[&str], values: &[&str]) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::List(
            values.iter().map(|value| (*value).to_string()).collect(),
        )),
        preview: None,
    }
}

fn absent(path: &[&str]) -> Field {
    Field {
        path: owned(path),
        value: None,
        preview: None,
    }
}

fn owned(path: &[&str]) -> Vec<String> {
    path.iter().map(|key| key.to_string()).collect()
}

/// The `apiKeyHelper` command line. Claude Code hands it to a POSIX `sh` on
/// every platform (Git's sh on Windows), so the path is quoted uniformly
/// with `shlex`.
fn helper_command(exe: &Path, agent: &str) -> Result<String, String> {
    let path = exe
        .to_str()
        .ok_or_else(|| "The app path is not valid Unicode".to_string())?;
    let quoted = shlex::try_quote(path)
        .map_err(|_| "The app path cannot be quoted for the shell".to_string())?;
    Ok(format!("{quoted} --agent-token {agent}"))
}

/// Credential-bearing keys: their values never reach previews, manifests,
/// or logs.
fn is_sensitive(path: &[String]) -> bool {
    let last = path
        .last()
        .map(|key| key.to_ascii_lowercase())
        .unwrap_or_default();
    [
        "apikey",
        "api_key",
        "token",
        "secret",
        "password",
        "bearer",
        "authorization",
    ]
    .iter()
    .any(|needle| last.contains(needle))
}

/// What a connection wrote, kept so a disconnect can restore exactly that.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Connection {
    fields: Vec<OwnedField>,
    /// The agent is not authorized (disconnect in progress).
    #[serde(default)]
    disabled: bool,
    /// A disconnect started; the record stays until token, parked secrets,
    /// and config are all cleaned up, so a retry is idempotent.
    #[serde(default)]
    cleanup_pending: bool,
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

struct Edit {
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
        let home = env_path(HOME_OVERRIDE_ENV).map_or_else(home_dir, Ok)?;
        Ok(Self::at(
            home,
            app_data_dir()?,
            helper_exe,
            endpoint,
            true,
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
        let (text, read_error) = self.config_text(agent);
        if connect {
            if let Some(error) = read_error.clone() {
                return Err(error);
            }
        }
        let edit = match ConfigDoc::parse(agent.format(), text.as_deref().unwrap_or_default()) {
            Ok(mut doc) => self.edit(agent, connect, &mut doc, &store, catalog, options)?,
            // Disconnect previews survive a broken config: nothing can be
            // restored, but token, parked secrets, and record still go.
            Err(_) if !connect => {
                store
                    .get(agent.id())
                    .ok_or_else(|| format!("{} is not connected", agent.name()))?;
                Edit {
                    changes: Vec::new(),
                    record: None,
                    pending_secrets: Vec::new(),
                    consumed_secrets: Vec::new(),
                }
            }
            Err(reason) => return Err(self.parse_error(agent, &reason)),
        };
        Ok(AgentPreview {
            agent: self.status(agent, &store, catalog),
            connect,
            changes: edit.changes,
            note: agent.note(connect).to_string(),
            revision: revision(text.as_deref(), store.get(agent.id()), catalog, options),
        })
    }

    /// Apply the previewed edits under the cross-process config lock.
    pub fn apply(
        &self,
        agent: Agent,
        connect: bool,
        revision_seen: &str,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<AgentStatus, String> {
        lock::with_apply_lock(&self.data_dir, || {
            self.maintain_store_permissions()?;
            let mut store = self.load_store()?;
            let (text, read_error) = self.config_text(agent);
            if revision(text.as_deref(), store.get(agent.id()), catalog, options) != revision_seen {
                return Err(format!(
                    "The {} config changed since the preview; review the changes again",
                    agent.name()
                ));
            }
            if connect {
                if let Some(error) = read_error {
                    return Err(error);
                }
                self.require_helper()?;
                self.connect(agent, &mut store, text, catalog, options)?;
            } else {
                self.disconnect(agent, &mut store)?;
            }
            Ok(self.status(agent, &store, catalog))
        })
    }

    /// Emergency restore: disconnect every recorded agent, whether or not
    /// this version supports it, the endpoint is bound, or the gateway runs.
    /// Every agent's token file is deleted before any manifest or config is
    /// touched — revoking the capability itself is durable, so no later
    /// failure (not even across a restart) can leave an agent authorized.
    /// `Err` means revocation or the tombstone step failed and callers must
    /// keep the in-memory token set empty; per-agent cleanup failures keep
    /// their tombstone for an idempotent retry.
    pub fn disconnect_all(&self) -> Result<Vec<(String, String)>, String> {
        lock::with_apply_lock(&self.data_dir, || {
            self.maintain_store_permissions()?;
            let mut store = self.load_store()?;
            let targets: Vec<Agent> = Agent::ALL
                .iter()
                .copied()
                .filter(|agent| store.contains_key(agent.id()))
                .collect();
            if targets.is_empty() {
                return Ok(Vec::new());
            }
            for agent in &targets {
                self.tokens.revoke(agent.id())?;
            }
            for agent in &targets {
                if let Some(record) = store.get_mut(agent.id()) {
                    record.disabled = true;
                    record.cleanup_pending = true;
                }
            }
            self.save_store(&store)?;
            let mut failures = Vec::new();
            for agent in targets {
                if let Err(error) = self.cleanup(agent, &mut store) {
                    failures.push((agent.id().to_string(), error));
                }
            }
            Ok(failures)
        })
    }

    /// Token, parked secrets, config, and record land together or are rolled
    /// back together.
    fn connect(
        &self,
        agent: Agent,
        store: &mut Store,
        text: Option<String>,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<(), String> {
        if store
            .get(agent.id())
            .is_some_and(|record| record.disabled || record.cleanup_pending)
        {
            return Err(format!(
                "{} has a disconnect in progress; finish it before connecting again",
                agent.name()
            ));
        }
        let mut doc = self.parse_config(agent, text.as_deref())?;
        let edit = self.edit(agent, true, &mut doc, store, catalog, options)?;
        let mut guard = Rollback::default();
        let result = (|| -> Result<(), String> {
            // A fresh token on every new connection; a leftover file from an
            // incomplete disconnect is never reused.
            if store.contains_key(agent.id()) {
                self.tokens.ensure(agent.id())?;
            } else {
                self.tokens.rotate(agent.id())?;
                guard.revoke_token = true;
            }
            for secret in &edit.pending_secrets {
                self.secrets.set(&secret.entry, &secret.value)?;
                guard.delete_secrets.push(secret.entry.clone());
            }
            if !edit.changes.is_empty() {
                let path = agent.config_path(&self.home, self.tool_env);
                write_atomic(&path, &doc.render()?, Some(text.as_deref())).map_err(|error| {
                    format!("Cannot write the {} config: {error}", agent.name())
                })?;
                guard.config = Some((path, text.clone()));
            }
            if let Some(record) = edit.record {
                store.insert(agent.id().to_string(), record);
            }
            self.save_store(store)
        })();
        if let Err(error) = result {
            let rollback = self.rollback(agent, guard);
            return Err(match rollback {
                Ok(()) => format!("{error}; nothing was changed"),
                Err(rollback) => format!("{error}; rolling back also failed: {rollback}"),
            });
        }
        Ok(())
    }

    /// Disconnect revokes the capability itself first: the token file is
    /// deleted before the record or any config is touched, so whatever fails
    /// afterwards — even the tombstone save — the agent can never be
    /// authorized again, not even across a restart. Cleanup never needs the
    /// old token; a new connection always issues a fresh one.
    fn disconnect(&self, agent: Agent, store: &mut Store) -> Result<(), String> {
        let Some(record) = store.get_mut(agent.id()) else {
            return Err(format!("{} is not connected", agent.name()));
        };
        self.tokens.revoke(agent.id())?;
        record.disabled = true;
        record.cleanup_pending = true;
        self.save_store(store)?;
        self.cleanup(agent, store)
    }

    /// Restore what can be restored and remove token, consumed parked
    /// secrets, and the record. An unreadable or unparseable config is left
    /// untouched (and its parked secrets stay in the credential store) rather
    /// than blocking the disconnect.
    fn cleanup(&self, agent: Agent, store: &mut Store) -> Result<(), String> {
        let Some(record) = store.get(agent.id()).cloned() else {
            return Ok(());
        };
        let (text, read_error) = self.config_text(agent);
        if read_error.is_none() {
            if let Ok(mut doc) =
                ConfigDoc::parse(agent.format(), text.as_deref().unwrap_or_default())
            {
                let edit = restore(&mut doc, &record, self.secrets.as_ref())?;
                if !edit.changes.is_empty() {
                    let path = agent.config_path(&self.home, self.tool_env);
                    write_atomic(&path, &doc.render()?, Some(text.as_deref())).map_err(
                        |error| format!("Cannot write the {} config: {error}", agent.name()),
                    )?;
                }
                for entry in &edit.consumed_secrets {
                    self.secrets.delete(entry)?;
                }
            }
        }
        self.tokens.revoke(agent.id())?;
        store.remove(agent.id());
        self.save_store(store)
    }

    fn rollback(&self, agent: Agent, guard: Rollback) -> Result<(), String> {
        let mut first_error = None;
        if let Some((path, original)) = guard.config {
            let restored = match original {
                Some(original) => write_atomic(&path, &original, None),
                None => fs::remove_file(&path),
            };
            if let Err(error) = restored {
                first_error.get_or_insert(format!("cannot restore the config: {error}"));
            }
        }
        for entry in guard.delete_secrets {
            if let Err(error) = self.secrets.delete(&entry) {
                first_error.get_or_insert(error);
            }
        }
        if guard.revoke_token {
            if let Err(error) = self.tokens.revoke(agent.id()) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Connecting references the bundled helper; an installation without it
    /// cannot issue agent credentials.
    fn require_helper(&self) -> Result<(), String> {
        if self.helper_exe.exists() {
            Ok(())
        } else {
            Err(HELPER_MISSING.to_string())
        }
    }

    fn edit(
        &self,
        agent: Agent,
        connect: bool,
        doc: &mut ConfigDoc,
        store: &Store,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<Edit, String> {
        if !connect {
            let record = store
                .get(agent.id())
                .ok_or_else(|| format!("{} is not connected", agent.name()))?;
            return restore(doc, record, self.secrets.as_ref());
        }
        if catalog.is_none() {
            return Err(format!(
                "Start the gateway and wait until it is verified; the model list for {} comes \
                 from it",
                agent.name()
            ));
        }
        let inputs = Inputs {
            endpoint: &self.endpoint,
            helper_exe: &self.helper_exe,
            token_path: &self.tokens.path(agent.id()),
            catalog,
            options,
        };
        project(doc, &fields(agent, &inputs)?, store.get(agent.id()), agent)
    }

    fn status(&self, agent: Agent, store: &Store, catalog: Option<&Catalog>) -> AgentStatus {
        let path = agent.config_path(&self.home, self.tool_env);
        let installed = cli_installed(agent, &self.home, self.tool_env);
        let record = store.get(agent.id());
        let mut status = AgentStatus {
            id: agent.id().to_string(),
            name: agent.name().to_string(),
            config_path: path.display().to_string(),
            installed,
            connected: false,
            recorded: record.is_some(),
            authorized: false,
            attention: None,
            error: None,
        };
        if !self.helper_exe.exists() {
            status.error = Some(HELPER_MISSING.to_string());
        }
        // A broken config is reported, never hidden behind "not connected";
        // the record, the attention line, and Disconnect all stay available.
        let (text, read_error) = self.config_text(agent);
        let doc = match read_error {
            Some(error) => {
                status.error = Some(error);
                None
            }
            None => match self.parse_config(agent, text.as_deref()) {
                Ok(doc) => Some(doc),
                Err(error) => {
                    status.error = Some(error);
                    None
                }
            },
        };
        let Some(record) = record else {
            return status;
        };
        let managed = doc.as_ref().is_some_and(|doc| {
            record
                .fields
                .iter()
                .all(|field| doc.get_value(&refs(&field.path)) == field.value)
        });
        let token = self.tokens.read(agent.id()).ok().flatten().is_some();
        status.connected = managed && token;
        status.authorized = !record.disabled && managed && token;
        if record.cleanup_pending {
            status.attention = Some(
                "Disconnect did not complete; this agent's access is disabled until Disconnect \
                 is retried"
                    .to_string(),
            );
        } else if record.disabled {
            status.attention =
                Some("This connection is disabled; Disconnect to restore your config".to_string());
        } else if !managed {
            status.attention = Some(
                "The config no longer matches what the app wrote (edited outside the app, or \
                 unreadable); this agent's access is disabled. Disconnect to clean up, or \
                 reconnect"
                    .to_string(),
            );
        } else if !token {
            status.attention = Some(
                "This agent's access is revoked; retry Disconnect to restore its config"
                    .to_string(),
            );
        } else if status.connected {
            if let (Some(catalog), Some(model)) = (catalog, selected_model(agent, doc.as_ref())) {
                if catalog.get(&model).is_none() {
                    status.attention = Some(format!(
                        "`{model}` is no longer served; choose another model and reconnect"
                    ));
                }
            }
        }
        status
    }

    /// Lenient read for flows that must survive a broken config (status,
    /// revisions, disconnect): the error is carried, never thrown.
    fn config_text(&self, agent: Agent) -> (Option<String>, Option<String>) {
        match self.read_config(agent) {
            Ok(text) => (text, None),
            Err(error) => (None, Some(error)),
        }
    }

    /// The config text, or `None` when the file does not exist yet.
    fn read_config(&self, agent: Agent) -> Result<Option<String>, String> {
        match fs::read_to_string(agent.config_path(&self.home, self.tool_env)) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("Cannot read the {} config: {error}", agent.name())),
        }
    }

    fn parse_config(&self, agent: Agent, text: Option<&str>) -> Result<ConfigDoc, String> {
        ConfigDoc::parse(agent.format(), text.unwrap_or_default())
            .map_err(|reason| self.parse_error(agent, &reason))
    }

    fn parse_error(&self, agent: Agent, reason: &str) -> String {
        format!(
            "The {} config at {} is {reason}; fix it before connecting",
            agent.name(),
            agent.config_path(&self.home, self.tool_env).display()
        )
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

    fn save_store(&self, store: &Store) -> Result<(), String> {
        let text = serde_json::to_string_pretty(store).map_err(|error| error.to_string())?;
        tokens::create_private_dir(&self.data_dir)
            .map_err(|error| format!("Cannot create the app data directory: {error}"))?;
        write_atomic(&self.store_path(), &text, None)
            .map_err(|error| format!("Cannot save the agent connection record: {error}"))
    }
}

#[derive(Default)]
struct Rollback {
    revoke_token: bool,
    delete_secrets: Vec<String>,
    config: Option<(PathBuf, Option<String>)>,
}

/// SHA-256 over everything a preview was computed from: the config text, the
/// existing connection record, the catalog revision, and the user's choices.
/// Each part is length-prefixed so boundaries cannot shift. The digest is
/// compared only, never logged or shown, since the text may contain
/// credentials.
fn revision(
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
    part(text.unwrap_or_default().as_bytes());
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

/// Write the owned fields, remembering what each held before. A field that
/// still holds what an earlier connection wrote keeps that connection's
/// `previous`, so reconnecting never records our own value as the original.
/// Previous values of sensitive fields are returned as pending secrets and
/// referenced by entry name; they never appear in changes or the record.
fn project(
    doc: &mut ConfigDoc,
    fields: &[Field],
    prior: Option<&Connection>,
    agent: Agent,
) -> Result<Edit, String> {
    let mut record = Connection::default();
    let mut changes = Vec::new();
    let mut pending_secrets = Vec::new();
    for field in fields {
        let path = refs(&field.path);
        let sensitive = is_sensitive(&field.path);
        let current = doc.get_value(&path);
        let still_ours = prior.and_then(|prior| {
            prior
                .fields
                .iter()
                .find(|owned| owned.path == field.path && current == owned.value)
        });
        let previous = match still_ours {
            Some(owned) => owned.previous.clone(),
            None => match current
                .clone()
                .filter(|held| Some(held) != field.value.as_ref())
            {
                Some(held) if sensitive => {
                    let entry = secret_entry(agent, &field.path);
                    pending_secrets.push(PendingSecret {
                        entry: entry.clone(),
                        value: held.display(),
                    });
                    Some(Previous::Secret { secret_ref: entry })
                }
                Some(held) => Some(Previous::Plain(held)),
                None => None,
            },
        };
        if current != field.value {
            match &field.value {
                Some(value) => doc.set_value(&path, value)?,
                None => doc.remove(&path),
            }
            changes.push(if sensitive {
                ConfigChange {
                    key: path.join("."),
                    before: current.as_ref().map(|_| "Existing secret".to_string()),
                    after: field
                        .value
                        .as_ref()
                        .map(|_| "Managed local credential".to_string()),
                    sensitive: true,
                }
            } else {
                preview_change(
                    &path,
                    current,
                    field.value.clone(),
                    field.preview.as_deref(),
                )
            });
        }
        record.fields.push(OwnedField {
            path: field.path.clone(),
            value: field.value.clone(),
            previous,
        });
    }
    Ok(Edit {
        changes,
        record: Some(record),
        pending_secrets,
        consumed_secrets: Vec::new(),
    })
}

/// Undo a connection: every owned field that still holds what we wrote goes
/// back to its previous value (plain, or fetched from the credential store)
/// or disappears, pruning emptied containers; anything the user changed since
/// is left alone. Idempotent: a field already restored is skipped.
fn restore(
    doc: &mut ConfigDoc,
    record: &Connection,
    secrets: &dyn SecretStore,
) -> Result<Edit, String> {
    let mut changes = Vec::new();
    let mut consumed_secrets = Vec::new();
    for field in &record.fields {
        let path = refs(&field.path);
        let current = doc.get_value(&path);
        if current != field.value {
            continue;
        }
        let sensitive = is_sensitive(&field.path);
        if let Some(Previous::Secret { secret_ref }) = &field.previous {
            consumed_secrets.push(secret_ref.clone());
        }
        let (restored, after_label) = match &field.previous {
            Some(Previous::Plain(value)) => (Some(value.clone()), None),
            Some(Previous::Secret { secret_ref }) => match secrets.get(secret_ref)? {
                Some(value) => (
                    Some(ConfigValue::Str(value)),
                    Some("Previous secret restored".to_string()),
                ),
                None => (
                    None,
                    Some("Previous secret unavailable; left unset".to_string()),
                ),
            },
            None => (None, None),
        };
        match &restored {
            Some(value) => doc.set_value(&path, value)?,
            None => doc.remove(&path),
        }
        changes.push(if sensitive {
            ConfigChange {
                key: path.join("."),
                before: current
                    .as_ref()
                    .map(|_| "Managed local credential".to_string()),
                after: after_label,
                sensitive: true,
            }
        } else {
            change(&path, current, restored)
        });
    }
    Ok(Edit {
        changes,
        record: None,
        pending_secrets: Vec::new(),
        consumed_secrets,
    })
}

fn change(path: &[&str], before: Option<ConfigValue>, after: Option<ConfigValue>) -> ConfigChange {
    ConfigChange {
        key: path.join("."),
        before: before.map(|value| value.display()),
        after: after.map(|value| value.display()),
        sensitive: false,
    }
}

fn preview_change(
    path: &[&str],
    before: Option<ConfigValue>,
    after: Option<ConfigValue>,
    preview: Option<&str>,
) -> ConfigChange {
    ConfigChange {
        key: path.join("."),
        before: before.map(|value| {
            if preview.is_some() && matches!(value, ConfigValue::Json(_)) {
                "Existing provider configuration".to_string()
            } else {
                value.display()
            }
        }),
        after: after.map(|value| {
            preview
                .map(str::to_string)
                .unwrap_or_else(|| value.display())
        }),
        sensitive: false,
    }
}

fn refs(path: &[String]) -> Vec<&str> {
    path.iter().map(String::as_str).collect()
}

fn selected_model(agent: Agent, doc: Option<&ConfigDoc>) -> Option<String> {
    let doc = doc?;
    match agent {
        Agent::Codex => doc.get_str(&["model"]),
        Agent::ClaudeCode => doc.get_str(&["env", "ANTHROPIC_MODEL"]),
        Agent::OpenCode => doc.get_str(&["model"]).and_then(|value| {
            value
                .strip_prefix("private-ai-gateway/")
                .map(str::to_string)
        }),
        Agent::Pi => None,
        Agent::Hermes => doc.get_str(&["model", "default"]),
    }
}

fn cli_installed(agent: Agent, home: &Path, tool_env: bool) -> bool {
    let mut paths: Vec<PathBuf> = if tool_env {
        env::var_os("PATH")
            .map(|paths| env::split_paths(&paths).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    paths.extend([
        home.join(".local/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join("Library/pnpm"),
        home.join(".bun/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    paths.extend(versioned_runtime_bins(
        &home.join(".nvm/versions/node"),
        &["bin"],
    ));
    paths.extend(versioned_runtime_bins(
        &home.join(".local/share/fnm/node-versions"),
        &["installation", "bin"],
    ));
    agent
        .cli_names()
        .iter()
        .any(|name| cli_in_paths(name, &paths))
}

fn versioned_runtime_bins(root: &Path, suffix: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .take(64)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| {
            suffix
                .iter()
                .fold(entry.path(), |path, part| path.join(part))
        })
        .collect()
}

fn cli_in_paths(name: &str, paths: &[PathBuf]) -> bool {
    let candidates: &[String] = &if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    } else {
        vec![name.to_string()]
    };
    paths.iter().any(|dir| {
        candidates
            .iter()
            .any(|candidate| dir.join(candidate).is_file())
    })
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf, String> {
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .ok_or_else(|| "Cannot determine the home directory".to_string())
}

/// The per-user app data directory (tokens, connection record, locks),
/// resolved the same way by the desktop shell and the
/// bundled helper.
pub fn app_data_dir() -> Result<PathBuf, String> {
    if let Some(home) = env_path(HOME_OVERRIDE_ENV) {
        return Ok(home.join(".private-ai-gateway"));
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
            return Err(io::Error::new(
                io::ErrorKind::Other,
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
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;
    use serde_json::json;

    const ENDPOINT: &str = "http://127.0.0.1:4180";

    fn catalog() -> Catalog {
        Catalog::from_remote(
            &json!({
                "data": [
                    { "id": "openai/gpt-oss-20b", "name": "GPT OSS 20B", "context_length": 131072 },
                    { "id": "phala/qwen" }
                ]
            }),
            1,
        )
        .unwrap()
    }

    struct Sandbox {
        home: PathBuf,
        projector: Projector,
        secrets: Arc<MemoryStore>,
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    /// A fresh home directory under the system temp dir with a fake helper
    /// binary; tool env overrides are ignored so no real config is
    /// touched.
    fn sandbox(name: &str) -> Sandbox {
        let home = env::temp_dir().join(format!("pag-agents-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let helper = home
            .join("Private AI Gateway.app")
            .join(helper_binary_name());
        write(&helper, "#!/bin/sh\n");
        let secrets = Arc::new(MemoryStore::default());
        let projector = Projector::at(
            home.clone(),
            home.join("state"),
            helper,
            ENDPOINT,
            false,
            secrets.clone(),
        );
        Sandbox {
            home,
            projector,
            secrets,
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn claude_options() -> ConnectOptions {
        ConnectOptions {
            default_model: Some("openai/gpt-oss-20b".to_string()),
        }
    }

    fn connect(sandbox: &Sandbox) -> AgentStatus {
        let catalog = catalog();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, true, Some(&catalog), &claude_options())
            .unwrap();
        sandbox
            .projector
            .apply(
                Agent::ClaudeCode,
                true,
                &preview.revision,
                Some(&catalog),
                &claude_options(),
            )
            .unwrap()
    }

    fn disconnect(sandbox: &Sandbox, agent: Agent) -> AgentStatus {
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(agent, false, None, &options)
            .unwrap();
        sandbox
            .projector
            .apply(agent, false, &preview.revision, None, &options)
            .unwrap()
    }

    fn doc(sandbox: &Sandbox, agent: Agent) -> ConfigDoc {
        let text = sandbox.projector.read_config(agent).unwrap();
        sandbox
            .projector
            .parse_config(agent, text.as_deref())
            .unwrap()
    }

    #[test]
    fn codex_and_opencode_use_official_custom_provider_configs() {
        let sandbox = sandbox("providers");
        let catalog = catalog();
        let options = claude_options();

        let preview = sandbox
            .projector
            .preview(Agent::Codex, true, Some(&catalog), &options)
            .unwrap();
        let status = sandbox
            .projector
            .apply(
                Agent::Codex,
                true,
                &preview.revision,
                Some(&catalog),
                &options,
            )
            .unwrap();
        assert!(status.connected);
        let codex = doc(&sandbox, Agent::Codex);
        assert_eq!(
            codex.get_str(&["model_provider"]).as_deref(),
            Some("private_ai_gateway")
        );
        assert_eq!(
            codex
                .get_str(&["model_providers", "private_ai_gateway", "wire_api"])
                .as_deref(),
            Some("responses")
        );
        assert_eq!(
            codex
                .get_str(&["model_providers", "private_ai_gateway", "base_url"])
                .as_deref(),
            Some("http://127.0.0.1:4180/v1")
        );
        disconnect(&sandbox, Agent::Codex);

        let preview = sandbox
            .projector
            .preview(Agent::OpenCode, true, Some(&catalog), &options)
            .unwrap();
        assert!(preview.changes.iter().any(|change| {
            change.key == "provider.private-ai-gateway"
                && change.after.as_deref() == Some("Generated catalog (2 models)")
        }));
        let status = sandbox
            .projector
            .apply(
                Agent::OpenCode,
                true,
                &preview.revision,
                Some(&catalog),
                &options,
            )
            .unwrap();
        assert!(status.connected);
        let opencode = doc(&sandbox, Agent::OpenCode);
        assert_eq!(
            opencode
                .get_str(&["provider", "private-ai-gateway", "npm"])
                .as_deref(),
            Some("@ai-sdk/openai-compatible")
        );
        assert_eq!(
            opencode
                .get_str(&["provider", "private-ai-gateway", "options", "baseURL"])
                .as_deref(),
            Some("http://127.0.0.1:4180/v1")
        );
        disconnect(&sandbox, Agent::OpenCode);
    }

    #[test]
    fn pi_and_hermes_use_verified_model_discovery() {
        let sandbox = sandbox("discovery-providers");
        let catalog = catalog();
        let options = ConnectOptions::default();

        let preview = sandbox
            .projector
            .preview(Agent::Pi, true, Some(&catalog), &options)
            .unwrap();
        assert!(preview.changes.iter().any(|change| {
            change.key == "providers.private-ai-gateway"
                && change.after.as_deref() == Some("Generated catalog (2 models)")
        }));
        sandbox
            .projector
            .apply(Agent::Pi, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        let pi = doc(&sandbox, Agent::Pi);
        let provider = pi.get_value(&["providers", "private-ai-gateway"]).unwrap();
        let ConfigValue::Json(provider) = provider else {
            panic!("Pi provider must be a generated JSON catalog");
        };
        assert_eq!(provider["models"].as_array().unwrap().len(), 2);
        assert_eq!(provider["models"][0]["id"], "openai/gpt-oss-20b");
        assert!(provider["apiKey"]
            .as_str()
            .unwrap()
            .contains("--agent-token pi"));
        disconnect(&sandbox, Agent::Pi);

        let path = sandbox.home.join(".hermes").join("config.yaml");
        write(&path, "# keep this comment\ntheme: dark\n");
        let preview = sandbox
            .projector
            .preview(Agent::Hermes, true, Some(&catalog), &options)
            .unwrap();
        sandbox
            .projector
            .apply(
                Agent::Hermes,
                true,
                &preview.revision,
                Some(&catalog),
                &options,
            )
            .unwrap();
        let hermes = doc(&sandbox, Agent::Hermes);
        assert_eq!(
            hermes.get_value(&["providers", "private-ai-gateway", "discover_models"]),
            Some(ConfigValue::Bool(true))
        );
        assert_eq!(
            hermes.get_str(&["model", "provider"]).as_deref(),
            Some("custom:private-ai-gateway")
        );
        disconnect(&sandbox, Agent::Hermes);
        let restored = fs::read_to_string(path).unwrap();
        assert!(restored.contains("# keep this comment"));
        assert!(restored.contains("theme: dark"));
        assert!(!restored.contains("private-ai-gateway"));

        let fresh = self::sandbox("fresh-hermes");
        let preview = fresh
            .projector
            .preview(
                Agent::Hermes,
                true,
                Some(&catalog),
                &ConnectOptions::default(),
            )
            .unwrap();
        fresh
            .projector
            .apply(
                Agent::Hermes,
                true,
                &preview.revision,
                Some(&catalog),
                &ConnectOptions::default(),
            )
            .unwrap();
        let hermes = doc(&fresh, Agent::Hermes);
        assert_eq!(
            hermes
                .get_str(&["providers", "private-ai-gateway", "transport"])
                .as_deref(),
            Some("chat_completions")
        );
        assert!(hermes
            .get_str(&["providers", "private-ai-gateway", "key_cmd"])
            .is_some_and(|command| command.contains("--agent-token hermes")));
    }

    #[test]
    fn installation_detection_uses_executables_not_config_directories() {
        let sandbox = sandbox("installed");
        fs::create_dir_all(sandbox.home.join(".pi/agent")).unwrap();
        let statuses = sandbox.projector.scan(None).unwrap().0;
        assert!(
            !statuses
                .iter()
                .find(|status| status.id == "pi")
                .unwrap()
                .installed
        );

        let executable = sandbox.home.join(".local/bin").join(if cfg!(windows) {
            "pi.exe"
        } else {
            "pi"
        });
        write(&executable, "#!/bin/sh\n");
        let statuses = sandbox.projector.scan(None).unwrap().0;
        assert!(
            statuses
                .iter()
                .find(|status| status.id == "pi")
                .unwrap()
                .installed
        );

        let codex = sandbox
            .home
            .join(".nvm/versions/node/v22.19.0/bin")
            .join(if cfg!(windows) { "codex.cmd" } else { "codex" });
        write(&codex, "#!/bin/sh\n");
        let statuses = sandbox.projector.scan(None).unwrap().0;
        assert!(
            statuses
                .iter()
                .find(|status| status.id == "codex")
                .unwrap()
                .installed
        );
    }

    #[test]
    fn claude_takes_over_credentials_via_the_keyring_and_restores_them() {
        let sandbox = sandbox("claude");
        let path = sandbox.home.join(".claude").join("settings.json");
        write(
            &path,
            r#"{"model": "opus", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-old-secret"}}"#,
        );
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, true, Some(&catalog()), &claude_options())
            .unwrap();
        let token_change = preview
            .changes
            .iter()
            .find(|change| change.key == "env.ANTHROPIC_AUTH_TOKEN")
            .unwrap();
        assert_eq!(token_change.before.as_deref(), Some("Existing secret"));
        assert_eq!(token_change.after, None);
        assert!(token_change.sensitive);
        let preview_json = serde_json::to_string(&preview).unwrap();
        assert!(!preview_json.contains("sk-old-secret"));
        assert!(sandbox.secrets.is_empty(), "preview parks nothing");

        let status = connect(&sandbox);
        assert!(status.connected);
        let doc = doc(&sandbox, Agent::ClaudeCode);
        assert_eq!(doc.get_str(&["env", "ANTHROPIC_AUTH_TOKEN"]), None);
        assert_eq!(
            doc.get_str(&["env", "ANTHROPIC_BASE_URL"]).as_deref(),
            Some(ENDPOINT)
        );
        assert_eq!(
            doc.get_str(&["env", "ANTHROPIC_MODEL"]).as_deref(),
            Some("openai/gpt-oss-20b")
        );
        assert!(doc
            .get_str(&["apiKeyHelper"])
            .unwrap()
            .contains("--agent-token claude-code"));
        assert_eq!(doc.get_str(&["model"]).as_deref(), Some("opus"));
        assert!(
            sandbox.secrets.holds("sk-old-secret"),
            "old secret parked in the store"
        );
        let manifest = fs::read_to_string(sandbox.projector.store_path()).unwrap();
        assert!(!manifest.contains("sk-old-secret"));
        assert!(manifest.contains("secret_ref"));

        let smaller =
            Catalog::from_remote(&json!({ "data": [{ "id": "phala/qwen" }] }), 2).unwrap();
        let status = &sandbox.projector.scan(Some(&smaller)).unwrap().0[1];
        assert!(status.connected);
        assert!(status
            .attention
            .as_deref()
            .unwrap()
            .contains("no longer served"));

        disconnect(&sandbox, Agent::ClaudeCode);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"ANTHROPIC_AUTH_TOKEN\": \"sk-old-secret\""));
        assert!(!text.contains("apiKeyHelper"));
        assert!(sandbox.secrets.is_empty(), "restore entry released");
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
        assert!(sandbox.projector.load_store().unwrap().is_empty());
    }

    #[test]
    fn agents_require_a_verified_catalog_model() {
        let sandbox = sandbox("claude-gate");
        assert!(sandbox
            .projector
            .preview(
                Agent::ClaudeCode,
                true,
                Some(&catalog()),
                &ConnectOptions::default()
            )
            .is_ok());
        assert!(sandbox
            .projector
            .preview(
                Agent::ClaudeCode,
                true,
                Some(&catalog()),
                &ConnectOptions {
                    default_model: Some("claude-sonnet-4-6".to_string()),
                },
            )
            .unwrap_err()
            .contains("not in the verified model list"));
        // Without the bundled helper, agents cannot authenticate.
        fs::remove_file(&sandbox.projector.helper_exe).unwrap();
        let status = &sandbox.projector.scan(Some(&catalog())).unwrap().0[1];
        assert!(status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("helper")));
        assert!(sandbox
            .projector
            .preview(Agent::ClaudeCode, true, Some(&catalog()), &claude_options())
            .unwrap_err()
            .contains("helper"));
    }

    #[test]
    fn apply_refuses_a_stale_revision() {
        let sandbox = sandbox("revision");
        let path = sandbox.home.join(".claude").join("settings.json");
        write(&path, r#"{"model": "opus"}"#);
        let catalog = catalog();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, true, Some(&catalog), &claude_options())
            .unwrap();
        write(&path, r#"{"model": "sonnet"}"#);
        let error = sandbox
            .projector
            .apply(
                Agent::ClaudeCode,
                true,
                &preview.revision,
                Some(&catalog),
                &claude_options(),
            )
            .unwrap_err();
        assert!(error.contains("changed since the preview"));
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "sonnet"}"#);
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn connect_rolls_everything_back_when_the_record_cannot_be_saved() {
        use std::os::unix::fs::PermissionsExt;
        let mut sandbox = sandbox("rollback");
        let blocked = sandbox.home.join("blocked");
        fs::create_dir_all(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).unwrap();
        sandbox.projector.data_dir = blocked.clone();
        let path = sandbox.home.join(".claude").join("settings.json");
        write(&path, r#"{"env": {"ANTHROPIC_API_KEY": "sk-user"}}"#);
        let catalog = catalog();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, true, Some(&catalog), &claude_options())
            .unwrap();
        let error = sandbox
            .projector
            .apply(
                Agent::ClaudeCode,
                true,
                &preview.revision,
                Some(&catalog),
                &claude_options(),
            )
            .unwrap_err();
        assert!(
            error.contains("nothing was changed") || error.contains("lock"),
            "{error}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"env": {"ANTHROPIC_API_KEY": "sk-user"}}"#
        );
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
        assert!(sandbox.secrets.is_empty(), "parked secret rolled back");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_disconnect_leaves_a_retryable_tombstone_and_never_reuses_the_token() {
        let sandbox = sandbox("tombstone");
        let path = sandbox.home.join(".claude").join("settings.json");
        write(&path, r#"{"model": "opus"}"#);
        connect(&sandbox);
        let token = sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .unwrap();
        assert!(!sandbox.projector.scan(None).unwrap().1.is_empty());

        // Make the config unwritable (a symlink target is refused) so the
        // restore step fails after the record was tombstoned.
        let dir = path.parent().unwrap().to_path_buf();
        let original = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(dir.join("elsewhere.json"), &original).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere.json"), &path).unwrap();
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, false, None, &options)
            .unwrap();
        assert!(sandbox
            .projector
            .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
            .is_err());
        assert!(
            sandbox.projector.scan(None).unwrap().1.is_empty(),
            "access stays revoked"
        );
        let status = &sandbox.projector.scan(None).unwrap().0[1];
        assert!(status.attention.as_deref().unwrap().contains("retried"));
        // Reconnecting is refused while the tombstone exists.
        assert!(sandbox
            .projector
            .apply(
                Agent::ClaudeCode,
                true,
                "any",
                Some(&catalog()),
                &claude_options()
            )
            .is_err());

        // Repair the config and retry: idempotent cleanup completes.
        fs::remove_file(&path).unwrap();
        fs::rename(dir.join("elsewhere.json"), &path).unwrap();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, false, None, &options)
            .unwrap();
        sandbox
            .projector
            .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
            .unwrap();
        assert!(sandbox.projector.load_store().unwrap().is_empty());
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
        assert!(!fs::read_to_string(&path).unwrap().contains("apiKeyHelper"));

        // A new connection issues a fresh token, never the old value.
        connect(&sandbox);
        assert_ne!(
            sandbox
                .projector
                .tokens
                .read("claude-code")
                .unwrap()
                .unwrap(),
            token
        );
    }

    #[test]
    fn disconnect_all_restores_every_agent() {
        let sandbox = sandbox("emergency");
        write(
            &sandbox.home.join(".claude").join("settings.json"),
            r#"{"model": "opus"}"#,
        );
        connect(&sandbox);
        let codex = sandbox.home.join(".codex").join("config.toml");
        write(&codex, "model_provider = \"private_ai_gateway\"\n");
        sandbox.projector.tokens.ensure("codex").unwrap();
        let mut store = sandbox.projector.load_store().unwrap();
        store.insert(
            "codex".into(),
            Connection {
                fields: vec![OwnedField {
                    path: owned(&["model_provider"]),
                    value: Some(ConfigValue::Str("private_ai_gateway".into())),
                    previous: None,
                }],
                disabled: true,
                cleanup_pending: false,
            },
        );
        sandbox.projector.save_store(&store).unwrap();

        assert!(sandbox.projector.disconnect_all().unwrap().is_empty());
        assert!(sandbox.projector.load_store().unwrap().is_empty());
        assert_eq!(fs::read_to_string(&codex).unwrap(), "");
        assert!(
            !fs::read_to_string(sandbox.home.join(".claude").join("settings.json"))
                .unwrap()
                .contains("apiKeyHelper")
        );
        assert!(sandbox
            .projector
            .tokens
            .load(&["codex", "claude-code"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn atomic_writes_refuse_symlinks_and_changed_files() {
        let dir = env::temp_dir().join(format!("pag-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("config.json");
        write_atomic(&target, "{}", Some(None)).unwrap();
        assert!(write_atomic(&target, "{\"a\":1}", Some(Some("changed"))).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
        write_atomic(&target, "{\"a\":1}", Some(Some("{}"))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let link = dir.join("link.json");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(write_atomic(&link, "{}", None).is_err());
            assert_eq!(fs::read_to_string(&target).unwrap(), "{\"a\":1}");
        }
        assert!(fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Config drift or corruption deauthorizes the token on the next load,
    /// keeps the record visible, and stays recoverable via Disconnect.
    #[test]
    fn drifted_or_broken_configs_deauthorize_tokens_but_stay_recoverable() {
        let sandbox = sandbox("drift");
        let path = sandbox.home.join(".claude").join("settings.json");
        write(
            &path,
            r#"{"model": "opus", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-old-secret"}}"#,
        );
        connect(&sandbox);
        assert!(!sandbox.projector.scan(None).unwrap().1.is_empty());

        // The user edits an owned field outside the app (a restart is just a
        // fresh scan, which is what the shell does at startup).
        let mut doc = doc(&sandbox, Agent::ClaudeCode);
        doc.set_str(&["env", "ANTHROPIC_MODEL"], "somewhere/else")
            .unwrap();
        write(&path, &doc.render().unwrap());
        assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
        let status = &sandbox.projector.scan(None).unwrap().0[1];
        assert!(status.recorded && !status.authorized && !status.connected);
        assert!(status
            .attention
            .as_deref()
            .unwrap()
            .contains("no longer matches"));

        // Corrupt the file entirely: still recorded, error reported, token
        // still unauthorized, and Disconnect cleans up without touching it.
        write(&path, "{ not json");
        assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
        let status = &sandbox.projector.scan(None).unwrap().0[1];
        assert!(status.recorded && !status.authorized);
        assert!(status.error.is_some());
        disconnect(&sandbox, Agent::ClaudeCode);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        assert!(sandbox.projector.load_store().unwrap().is_empty());
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
        // The parked previous secret was not consumed: it stays retrievable.
        assert!(sandbox.secrets.holds("sk-old-secret"));
    }

    /// Restore-all revokes every token and tombstones every record before
    /// any config is read; an unreadable config fails only its own cleanup
    /// and never leaves the agent authorized.
    #[test]
    fn restore_all_revokes_before_reading_any_config() {
        let sandbox = sandbox("tombstone-first");
        write(
            &sandbox.home.join(".claude").join("settings.json"),
            r#"{"model": "opus"}"#,
        );
        connect(&sandbox);
        // Replace the config with a directory: reading it fails outright.
        let path = sandbox.home.join(".claude").join("settings.json");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let failures = sandbox.projector.disconnect_all().unwrap();
        assert!(
            failures.is_empty(),
            "unreadable config is skipped, not fatal: {failures:?}"
        );
        assert!(sandbox.projector.load_store().unwrap().is_empty());
        assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
    }

    /// The apiKeyHelper command is parsed by a POSIX `sh` on every platform,
    /// so quoting is uniform `shlex`: paths with spaces, `$`, backticks, and
    /// single and double quotes round-trip exactly. The `shlex::split`
    /// contract holds everywhere; where an `sh` exists (Unix, Git Bash on
    /// CI) the same command line is round-tripped through `sh -c` too.
    #[test]
    fn helper_command_quotes_hostile_paths_for_the_shell() {
        for hostile in [
            "/Applications/Private AI Gateway.app/Contents/MacOS/helper",
            "/tmp/it's here/$HOME`echo`;rm -rf/helper",
            "/tmp/quote\"double\"/helper",
        ] {
            let command = helper_command(Path::new(hostile), "claude-code").unwrap();
            let suffix = " --agent-token claude-code";
            assert!(command.ends_with(suffix));
            let quoted = &command[..command.len() - suffix.len()];
            assert_eq!(shlex::split(quoted), Some(vec![hostile.to_string()]));
            match std::process::Command::new("sh")
                .args(["-c", &format!("printf %s {quoted}")])
                .output()
            {
                Ok(output) => {
                    assert_eq!(
                        String::from_utf8_lossy(&output.stdout),
                        hostile,
                        "{command}"
                    )
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("cannot run sh: {error}"),
            }
        }
    }

    /// Deleting the token file is the revocation itself: even when the very
    /// first manifest save fails, a restart (a fresh Projector) authorizes
    /// nothing, the record stays visible for a retry, and a new connection
    /// rotates to a fresh token. Covers single Disconnect and Restore all.
    #[cfg(unix)]
    #[test]
    fn revocation_is_durable_even_when_the_first_manifest_save_fails() {
        use std::os::unix::fs::PermissionsExt;
        for all in [false, true] {
            let sandbox = sandbox(if all {
                "revoke-first-all"
            } else {
                "revoke-first"
            });
            write(
                &sandbox.home.join(".claude").join("settings.json"),
                r#"{"model": "opus"}"#,
            );
            connect(&sandbox);
            let old = sandbox
                .projector
                .tokens
                .read("claude-code")
                .unwrap()
                .unwrap();
            // Make the record unsaveable: the tombstone write fails after the
            // token file is already gone.
            let data_dir = sandbox.home.join("state");
            fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o500)).unwrap();
            let result = if all {
                sandbox.projector.disconnect_all().map(|_| ())
            } else {
                let options = ConnectOptions::default();
                let preview = sandbox
                    .projector
                    .preview(Agent::ClaudeCode, false, None, &options)
                    .unwrap();
                sandbox
                    .projector
                    .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
                    .map(|_| ())
            };
            fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(result.is_err());
            assert!(
                sandbox
                    .projector
                    .tokens
                    .read("claude-code")
                    .unwrap()
                    .is_none(),
                "the capability itself is gone"
            );
            // A restart is a fresh scan: nothing is authorized, the record is
            // still shown with an attention line.
            let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
            assert!(tokens.is_empty());
            assert!(statuses[1].recorded);
            assert!(statuses[1]
                .attention
                .as_deref()
                .unwrap()
                .contains("revoked"));
            // The retry completes; a new connection never reuses the token.
            disconnect(&sandbox, Agent::ClaudeCode);
            assert!(sandbox.projector.load_store().unwrap().is_empty());
            connect(&sandbox);
            assert_ne!(
                sandbox
                    .projector
                    .tokens
                    .read("claude-code")
                    .unwrap()
                    .unwrap(),
                old
            );
        }
    }

    /// Revocation is remove-plus-parent-sync, strictly before any manifest
    /// write: with a failing sync injected, disconnect fails closed — the
    /// token entry is already gone, the manifest was never touched, a scan
    /// authorizes nothing — and the retry with a working sync completes.
    #[test]
    fn disconnect_fails_closed_when_revocation_cannot_be_persisted() {
        let mut sandbox = sandbox("sync-fail");
        write(
            &sandbox.home.join(".claude").join("settings.json"),
            r#"{"model": "opus"}"#,
        );
        connect(&sandbox);
        let manifest_before = fs::read(sandbox.projector.store_path()).unwrap();
        sandbox
            .projector
            .tokens
            .set_sync_parent(|_| Err(io::Error::other("injected sync failure")));

        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, false, None, &options)
            .unwrap();
        let error = sandbox
            .projector
            .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
            .unwrap_err();
        assert!(error.contains("could not be persisted"), "{error}");
        // The removal happened before the failed sync stopped everything…
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
        // …and the manifest write never ran: not even the tombstone landed.
        assert_eq!(
            fs::read(sandbox.projector.store_path()).unwrap(),
            manifest_before
        );
        // Fail closed either way: a scan authorizes nothing, the record stays
        // visible for a retry.
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        assert!(tokens.is_empty());
        assert!(statuses[1].recorded && statuses[1].attention.is_some());

        sandbox.projector.tokens.set_sync_parent(tokens::sync_dir);
        disconnect(&sandbox, Agent::ClaudeCode);
        assert!(sandbox.projector.load_store().unwrap().is_empty());
    }

    /// Install detection is informational only: with an empty home (and no
    /// CLI consulted), connect still previews and creates the official
    /// settings file from scratch.
    #[test]
    fn connect_creates_the_official_config_from_scratch() {
        let sandbox = sandbox("fresh-home");
        let (statuses, _) = sandbox.projector.scan(None).unwrap();
        assert!(!statuses[1].installed);
        let status = connect(&sandbox);
        assert!(status.connected);
        let text = fs::read_to_string(sandbox.home.join(".claude").join("settings.json")).unwrap();
        assert!(text.contains("ANTHROPIC_BASE_URL"));
        assert!(text.contains("apiKeyHelper"));
    }

    /// H1 at the proxy layer: a scan is what publishes authority. A token
    /// obtained while connected stops opening the proxy as soon as a scan
    /// runs after the config drifted or broke — the request is refused at
    /// auth and nothing reaches the sidecar.
    #[tokio::test]
    async fn a_scan_after_config_drift_revokes_the_old_token_at_the_proxy() {
        use crate::proxy::{router, ProxyState, Session};
        use axum::{routing::get, routing::post, Json, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let sandbox = sandbox("proxy-drift");
        let path = sandbox.home.join(".claude").join("settings.json");
        write(&path, r#"{"model": "opus"}"#);
        connect(&sandbox);
        let token = sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .unwrap();

        // A counting sidecar and a verified session for it.
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let sidecar = Router::new()
            .route(
                "/v1/messages",
                post(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Json(serde_json::json!({"ok": true})) }
                }),
            )
            .route(
                "/v1/models",
                get(|| async {
                    Json(serde_json::json!({ "data": [{ "id": "openai/gpt-oss-20b" }] }))
                }),
            );
        let sidecar_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", sidecar_listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(sidecar_listener, sidecar).await.unwrap() });

        let (sender, _events) = tokio::sync::mpsc::channel(8);
        let state = ProxyState::new(sender);
        state.set_api_key(Some("sk-live".into()));
        state.publish(Session {
            generation: 1,
            epoch: 1,
            session_id: Some("test-session".to_string()),
            base_url: Some(base_url.clone()),
            verified: false,
            catalog: None,
        });
        let catalog = state.fetch_catalog(1, 1).await.unwrap();
        state.publish(Session {
            generation: 1,
            epoch: 1,
            session_id: Some("test-session".to_string()),
            base_url: Some(base_url),
            verified: true,
            catalog: Some(catalog),
        });
        state.set_tokens(sandbox.projector.scan(None).unwrap().1);
        let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", proxy_listener.local_addr().unwrap());
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(proxy_listener, app).await.unwrap() });

        let client = reqwest::Client::new();
        let send = |token: String| {
            client
                .post(format!("{proxy_url}/v1/messages"))
                .bearer_auth(token)
                .json(&serde_json::json!({"model": "openai/gpt-oss-20b"}))
                .send()
        };
        assert_eq!(send(token.clone()).await.unwrap().status().as_u16(), 200);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // The config drifts outside the app; the next scan republishes the
        // token set and the old token stops working immediately.
        write(&path, "{ not json");
        state.set_tokens(sandbox.projector.scan(None).unwrap().1);
        assert_eq!(send(token).await.unwrap().status().as_u16(), 401);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "nothing reached the sidecar"
        );
    }
}
