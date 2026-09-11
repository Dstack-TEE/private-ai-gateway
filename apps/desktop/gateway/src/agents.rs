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
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use std::process::Command;

mod oh_my_pi;
mod openclaw;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub id: String,
    pub name: String,
    pub config_path: String,
    pub installed: bool,
    /// A connected link, including one suspended until protection resumes.
    pub connected: bool,
    /// A connection record exists (whatever the config now says).
    pub recorded: bool,
    /// The proxy would authorize this agent's token right now: recorded,
    /// enabled, config readable and its routing/authentication still managed.
    pub authorized: bool,
    /// Something the user must act on (removed model, incomplete disconnect).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_action: Option<AgentRepairAction>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentRepairAction {
    Reconnect,
    Disconnect,
}

/// One config field a connection changes. Sensitive fields never show their
/// values; `None` means absent.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConfigChange {
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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
    OpenClaw,
    OhMyPi,
}

impl Agent {
    pub fn surface(self) -> Surface {
        match self {
            Self::Codex => Surface::Responses,
            Self::ClaudeCode => Surface::Messages,
            Self::OpenCode | Self::Pi | Self::Hermes | Self::OpenClaw | Self::OhMyPi => {
                Surface::ChatCompletions
            }
        }
    }

    pub const ALL: [Agent; 7] = [
        Agent::Codex,
        Agent::ClaudeCode,
        Agent::OpenCode,
        Agent::Pi,
        Agent::Hermes,
        Agent::OpenClaw,
        Agent::OhMyPi,
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
            Agent::OpenClaw => "openclaw",
            Agent::OhMyPi => "oh-my-pi",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::ClaudeCode => "Claude Code",
            Agent::OpenCode => "OpenCode",
            Agent::Pi => "Pi",
            Agent::Hermes => "Hermes",
            Agent::OpenClaw => "OpenClaw",
            Agent::OhMyPi => "Oh My Pi",
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
            Agent::OpenClaw => &["openclaw"],
            Agent::OhMyPi => &["omp"],
        }
    }

    fn format(self) -> Format {
        match self {
            Agent::Codex => Format::Toml,
            Agent::ClaudeCode | Agent::OpenCode | Agent::Pi => Format::Json,
            Agent::Hermes => Format::Yaml,
            Agent::OpenClaw => Format::Json5,
            Agent::OhMyPi => Format::Yaml,
        }
    }

    /// The live user-level config file. With `tool_env` each tool's own
    /// location override is honored.
    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf {
        let override_dir = |name: &str| tool_env.then(|| env_path(name)).flatten();
        match self {
            Agent::OpenClaw => openclaw::config_path(home, tool_env),
            Agent::OhMyPi => oh_my_pi::config_path(home, tool_env),
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
            Agent::Pi => override_dir("PI_CODING_AGENT_DIR")
                .map(|path| match path.strip_prefix("~") {
                    Ok(suffix) => home.join(suffix),
                    Err(_) => path,
                })
                .unwrap_or_else(|| home.join(".pi").join("agent"))
                .join("models.json"),
            Agent::Hermes => override_dir("HERMES_HOME")
                .and_then(|path| {
                    path.to_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(|| hermes_native_dir(home, tool_env))
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
            Agent::OpenClaw => "OpenClaw uses a native-host provider and an executable SecretRef for its local gateway token. Restart OpenClaw after applying.",
            Agent::OhMyPi => "Oh My Pi uses its own local token and native models YAML. Choose a model in omp and restart after applying. CLI/profile/dotenv overrides must use the same native directory; native defaults and auth storage are not changed.",
            Agent::Codex => {
                "Codex will use its official custom model provider with the Responses API, the \
                 selected model from the verified catalog, command-backed authentication, and \
                 model metadata exported by the installed Codex version. Restart Codex after \
                 applying."
            }
            Agent::OpenCode => {
                "OpenCode will use an app-owned provider catalog generated from the verified \
                 service and a file-backed machine-local token. Restart OpenCode after applying."
            }
            Agent::ClaudeCode => {
                if cfg!(windows) {
                    return "Claude Code uses apiKeyHelper with a local token. Windows shell compatibility has not been verified with the real Claude CLI. Shell credentials and managed settings may override this projection. Anthropic does not officially support non-Claude models.";
                }
                "Claude Code will authenticate through apiKeyHelper with a machine-local token \
                 and discover models from the verified service. Credentials set in this settings file are taken over and restored on \
                 disconnect; a token exported in your shell would still take priority, so unset \
                 ANTHROPIC_AUTH_TOKEN and ANTHROPIC_API_KEY there. A claude.ai login is not \
                 used through the gateway. Anthropic does not officially support non-Claude models."
            }
            Agent::Pi => {
                "Pi will load an app-owned provider catalog generated from the verified service. Choose a model in Pi after applying. Restart Pi after applying."
            }
            Agent::Hermes => {
                "Hermes uses a machine-local token command. Start a new session without --api-key or --base-url overrides; existing native credentials and fallbacks are never erased."
            }
        }
    }
}

fn hermes_native_dir(home: &Path, tool_env: bool) -> PathBuf {
    if cfg!(windows) {
        tool_env
            .then(|| env_path("LOCALAPPDATA"))
            .flatten()
            .and_then(|path| {
                path.to_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("hermes")
    } else {
        home.join(".hermes")
    }
}

/// Everything a projection is computed from.
struct Inputs<'a> {
    endpoint: &'a str,
    helper_exe: &'a Path,
    token_path: &'a Path,
    codex_catalog_path: &'a Path,
    catalog: Option<&'a Catalog>,
    options: &'a ConnectOptions,
}

/// The fields this app owns for the agent and the values a connection writes.
fn fields(agent: Agent, inputs: &Inputs<'_>) -> Result<Vec<Field>, String> {
    let catalog = inputs
        .catalog
        .ok_or_else(|| "The verified model list is not available".to_string())?;
    let catalog = catalog.for_surface(agent.surface());
    if catalog.models.is_empty() {
        return Err(format!(
            "No models with confirmed {} support are available for {}",
            agent.surface().path(),
            agent.name()
        ));
    }
    let inputs = &Inputs {
        catalog: Some(&catalog),
        ..*inputs
    };
    let catalog = &catalog;
    let default_model = inputs
        .options
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if default_model.is_some_and(|model| catalog.get(model).is_none()) {
        return Err(format!(
            "`{}` is not in the verified model list compatible with {} ({})",
            default_model.unwrap_or_default(),
            agent.name(),
            agent.surface().path()
        ));
    }
    if agent == Agent::Codex && default_model.is_none() {
        return Err("Choose a verified default model for Codex".to_string());
    }
    let base = inputs.endpoint.trim_end_matches('/');
    Ok(match agent {
        Agent::OpenClaw => openclaw::fields(inputs)?,
        Agent::OhMyPi => oh_my_pi::fields(inputs)?,
        Agent::Codex => {
            let mut fields = vec![
                set(&["model_provider"], "private_ai_proxy"),
                absent(&["model_providers", "private_ai_proxy", "env_key"]),
                absent(&[
                    "model_providers",
                    "private_ai_proxy",
                    "experimental_bearer_token",
                ]),
                absent(&[
                    "model_providers",
                    "private_ai_proxy",
                    "requires_openai_auth",
                ]),
                set(
                    &["model_providers", "private_ai_proxy", "name"],
                    PRODUCT_NAME,
                ),
                set(
                    &["model_providers", "private_ai_proxy", "base_url"],
                    format!("{base}/v1"),
                ),
                set(
                    &["model_providers", "private_ai_proxy", "wire_api"],
                    "responses",
                ),
                set(
                    &["model_catalog_json"],
                    inputs.codex_catalog_path.display().to_string(),
                ),
                set(
                    &["model_providers", "private_ai_proxy", "auth", "command"],
                    inputs.helper_exe.display().to_string(),
                ),
                list(
                    &["model_providers", "private_ai_proxy", "auth", "args"],
                    &["--agent-token", "codex"],
                ),
                number(
                    &["model_providers", "private_ai_proxy", "auth", "timeout_ms"],
                    5_000,
                ),
                number(
                    &[
                        "model_providers",
                        "private_ai_proxy",
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
            let provider = "private-ai-proxy";
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
            &["providers", "private-ai-proxy"],
            pi_provider(catalog, base, inputs.helper_exe)?,
            catalog.models.len(),
        )],
        Agent::Hermes => {
            let provider = "private-ai-proxy";
            let mut fields = vec![
                set(&["providers", provider, "name"], PRODUCT_NAME),
                set(&["providers", provider, "api"], format!("{base}/v1")),
                set(&["providers", provider, "transport"], "chat_completions"),
                boolean(&["providers", provider, "discover_models"], true),
                set(
                    &["providers", provider, "key_cmd"],
                    credential_helper_command(inputs.helper_exe, Agent::Hermes)?,
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
            if let (Some(context), Some(output)) =
                (model.remote.context_length, model.remote.max_output_length)
            {
                config.insert(
                    "limit".to_string(),
                    serde_json::json!({
                        "context": context, "output": output,
                    }),
                );
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
                let input: Vec<_> = input
                    .iter()
                    .filter(|mode| matches!(mode.as_str(), "text" | "image"))
                    .collect();
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
                // Pi requires all four fields for new models; match its zero defaults.
                for key in ["input", "output", "cacheRead", "cacheWrite"] {
                    cost.entry(key.to_string()).or_insert(serde_json::json!(0));
                }
                value.insert("cost".to_string(), serde_json::Value::Object(cost));
            }
            serde_json::Value::Object(value)
        })
        .collect();
    Ok(serde_json::json!({
        "baseUrl": format!("{base}/v1"),
        "api": "openai-completions",
        "apiKey": format!("!{}", credential_helper_command(helper_exe, Agent::Pi)?),
        "models": models,
    }))
}

fn codex_catalog(
    catalog: &Catalog,
    bundled: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bundled_models = bundled
        .get("models")
        .and_then(serde_json::Value::as_array)
        .filter(|models| !models.is_empty())
        .ok_or_else(|| "Codex returned an empty bundled model catalog".to_string())?;
    let fallback = bundled_models
        .iter()
        .find(|model| {
            model
                .get("supported_in_api")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
                && model.get("visibility").and_then(serde_json::Value::as_str) == Some("list")
        })
        .unwrap_or(&bundled_models[0]);
    let models = catalog
        .models
        .iter()
        .filter(|model| model.supports(Surface::Responses))
        .enumerate()
        .map(|(index, model)| {
            let leaf = model.id().rsplit('/').next().unwrap_or(model.id());
            let matched_template = bundled_models
                .iter()
                .find(|candidate| {
                    candidate
                        .get("slug")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|slug| slug == model.id() || slug == leaf)
                });
            let template = matched_template.unwrap_or(fallback);
            let has_instructions = template
                .get("model_messages")
                .and_then(|messages| messages.get("instructions_template"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|instructions| !instructions.trim().is_empty())
                || template
                    .get("base_instructions")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|instructions| !instructions.trim().is_empty());
            if !has_instructions {
                return Err("Codex returned model metadata without its built-in instructions"
                    .to_string());
            }
            let mut value = template
                .as_object()
                .cloned()
                .ok_or_else(|| "Codex returned a malformed bundled model catalog".to_string())?;
            let capabilities = model.string_array("supported_features");
            let reasoning = capabilities.iter().any(|value| value == "reasoning");
            let verbosity = capabilities.iter().any(|value| value == "verbosity");
            let image = model
                .string_array("input_modalities")
                .iter()
                .any(|value| value == "image");
            let context = model.remote.context_length.map(serde_json::Value::from).unwrap_or_default();
            value.insert("slug".to_string(), serde_json::Value::String(model.id().to_string()));
            value.insert(
                "display_name".to_string(),
                serde_json::Value::String(model.display_name().to_string()),
            );
            value.insert(
                "description".to_string(),
                model
                    .string_field("description")
                    .map(serde_json::Value::String)
                    .unwrap_or_default(),
            );
            value.insert(
                "default_reasoning_level".to_string(),
                if reasoning {
                    serde_json::Value::String("medium".to_string())
                } else {
                    serde_json::Value::Null
                },
            );
            value.insert(
                "supported_reasoning_levels".to_string(),
                if reasoning {
                    serde_json::json!([
                        { "effort": "low", "description": "Faster responses with lighter reasoning" },
                        { "effort": "medium", "description": "Balanced reasoning for everyday coding work" },
                        { "effort": "high", "description": "Deeper reasoning for complex tasks" }
                    ])
                } else {
                    serde_json::json!([])
                },
            );
            value.insert("visibility".to_string(), serde_json::Value::String("list".to_string()));
            value.insert("supported_in_api".to_string(), serde_json::Value::Bool(true));
            value.insert("priority".to_string(), serde_json::Value::from(index + 1));
            value.insert("additional_speed_tiers".to_string(), serde_json::json!([]));
            value.insert("service_tiers".to_string(), serde_json::json!([]));
            value.insert("default_reasoning_summary".to_string(), serde_json::Value::String(if reasoning { "auto" } else { "none" }.to_string()));
            value.insert("support_verbosity".to_string(), serde_json::Value::Bool(verbosity));
            value.insert(
                "default_verbosity".to_string(),
                if verbosity {
                    serde_json::Value::String("medium".to_string())
                } else {
                    serde_json::Value::Null
                },
            );
            value.insert("supports_image_detail_original".to_string(), serde_json::Value::Bool(image));
            value.insert("context_window".to_string(), context.clone());
            value.insert("max_context_window".to_string(), context);
            value.insert("comp_hash".to_string(), serde_json::Value::Null);
            value.insert(
                "input_modalities".to_string(),
                if image { serde_json::json!(["text", "image"]) } else { serde_json::json!(["text"]) },
            );
            value.insert(
                "supports_search_tool".to_string(),
                serde_json::Value::Bool(capabilities.iter().any(|value| value == "web_search")),
            );
            value.insert("use_responses_lite".to_string(), serde_json::Value::Bool(false));
            if matched_template.is_none() {
                value.insert("tool_mode".to_string(), serde_json::Value::Null);
                value.insert("apply_patch_tool_type".to_string(), serde_json::Value::Null);
                value.insert("multi_agent_version".to_string(), serde_json::Value::Null);
                value.insert(
                    "web_search_tool_type".to_string(),
                    serde_json::Value::String("text".to_string()),
                );
                value.insert(
                    "shell_type".to_string(),
                    serde_json::Value::String("unified_exec".to_string()),
                );
            }
            Ok(serde_json::Value::Object(value))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(serde_json::json!({ "models": models }))
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

/// POSIX command syntax. The quoting test validates `sh`, not the Windows
/// Claude CLI's choice of shell; that compatibility remains unverified.
fn helper_command(exe: &Path, agent: &str) -> Result<String, String> {
    let path = exe
        .to_str()
        .ok_or_else(|| "The app path is not valid Unicode".to_string())?;
    let quoted = shlex::try_quote(path)
        .map_err(|_| "The app path cannot be quoted for the shell".to_string())?;
    Ok(format!("{quoted} --agent-token {agent}"))
}

#[cfg(not(windows))]
fn credential_helper_command(exe: &Path, agent: Agent) -> Result<String, String> {
    helper_command(exe, agent.id())
}

#[cfg(windows)]
fn credential_helper_command(exe: &Path, agent: Agent) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let path = exe
        .to_str()
        .ok_or_else(|| "The app path is not valid Unicode".to_string())?;
    if path.contains('\0') {
        return Err("The app path contains a null character".to_string());
    }
    // Hermes uses cmd.exe; Pi tries Bash then falls back to cmd.exe. Only the
    // fixed switches and base64 reach either shell; the helper path stays data.
    let encoded_path = STANDARD.encode(
        path.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let command = format!(
        "$ErrorActionPreference = 'Stop'; & ([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{encoded_path}'))) --agent-token {}; exit $LASTEXITCODE",
        agent.id()
    );
    let bytes: Vec<u8> = command.encode_utf16().flat_map(u16::to_le_bytes).collect();
    Ok(format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        STANDARD.encode(bytes)
    ))
}

fn stale_helper(agent: Agent, record: &Connection, exe: &Path) -> bool {
    let (path, expected) = match agent {
        Agent::Codex => (
            &["model_providers", "private_ai_proxy", "auth", "command"][..],
            exe.to_str().map(str::to_string),
        ),
        Agent::ClaudeCode => (&["apiKeyHelper"][..], helper_command(exe, agent.id()).ok()),
        Agent::Pi => (
            &["providers", "private-ai-proxy"][..],
            credential_helper_command(exe, agent)
                .ok()
                .map(|command| format!("!{command}")),
        ),
        Agent::Hermes => (
            &["providers", "private-ai-proxy", "key_cmd"][..],
            credential_helper_command(exe, agent).ok(),
        ),
        Agent::OpenCode => return false,
        Agent::OpenClaw => return false, // Validated with the token path by Projector.
        Agent::OhMyPi => return oh_my_pi::stale_helper(record, exe),
    };
    // Only inspect the helper-bearing field we recorded, not provider metadata.
    record.fields.iter().any(|field| {
        if field.path != owned(path) {
            return false;
        }
        let command = match &field.value {
            Some(ConfigValue::Str(command)) if agent != Agent::Pi => Some(command.as_str()),
            Some(ConfigValue::Json(provider)) if agent == Agent::Pi => {
                provider.get("apiKey").and_then(serde_json::Value::as_str)
            }
            _ => None,
        };
        expected
            .as_deref()
            .is_none_or(|expected| command != Some(expected))
    })
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
    /// Last catalog reconciled, including failed attempts, to avoid repeated writes.
    #[serde(default)]
    catalog_revision: Option<String>,
    #[serde(default)]
    config_path: Option<PathBuf>,
    fields: Vec<OwnedField>,
    /// User intent survives protection sessions; suspended links own no config.
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
    fn validate_recovery(&self) -> Result<(), String> {
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

    fn action_path(
        &self,
        agent: Agent,
        record: Option<&Connection>,
        connect: bool,
    ) -> Result<PathBuf, String> {
        if !connect {
            if let Some(record) = record.filter(|record| !record.fields.is_empty()) {
                record.validate_recovery()?;
                return record.restore_path().map(Path::to_path_buf);
            }
        }
        if agent == Agent::OhMyPi {
            oh_my_pi::validate_host(&self.home, self.tool_env)?;
        }
        let configured = agent.config_path(&self.home, self.tool_env);
        if !configured.is_absolute() {
            return Err("Set the agent config location to an absolute path; desktop and CLI working directories may differ".to_string());
        }
        let current = std::path::absolute(configured)
            .map_err(|_| "Cannot resolve an absolute agent config path".to_string())?;
        if current.to_str().is_none() {
            return Err("The agent config path must be valid Unicode".to_string());
        }
        if let Some(record) = record.filter(|record| !record.fields.is_empty()) {
            record.validate_recovery()?;
            let previous = record.restore_path()?;
            if previous != current {
                return Err("The agent config location changed. Disconnect to restore the recorded file before connecting at the new location".to_string());
            }
        }
        Ok(current)
    }

    /// Keep Codex's startup metadata in sync with the verified service
    /// catalog. The installed Codex binary supplies its exact catalog schema
    /// and complete built-in instructions; only public model metadata from
    /// the verified service is overlaid. No credentials are written here.
    pub fn sync_codex_catalog(&self, catalog: &Catalog) -> Result<(), String> {
        let bundled = self.codex_bundled_catalog()?;
        let text = serde_json::to_string_pretty(&codex_catalog(catalog, &bundled)?)
            .map_err(|error| format!("Cannot encode the Codex model catalog: {error}"))?;
        tokens::create_private_dir(&self.data_dir)
            .map_err(|error| format!("Cannot create the app data directory: {error}"))?;
        if fs::read_to_string(self.codex_catalog_path()).is_ok_and(|current| current == text) {
            return Ok(());
        }
        write_atomic(&self.codex_catalog_path(), &text, None)
            .map_err(|error| format!("Cannot write the Codex model catalog: {error}"))
    }

    #[cfg(not(test))]
    fn codex_bundled_catalog(&self) -> Result<serde_json::Value, String> {
        let search_paths = cli_paths(&self.home, self.tool_env);
        let executable = find_cli_in_paths(Agent::Codex, &search_paths).ok_or_else(|| {
            "Codex CLI was not found; install or update Codex before connecting".to_string()
        })?;
        let output = command_output(
            &executable,
            &["debug", "models", "--bundled"],
            &search_paths,
        )
        .map_err(|error| format!("Cannot read model metadata from Codex: {error}"))?;
        if !output.status.success() {
            return Err(
                "Codex could not export its bundled model metadata; update Codex and try again"
                    .to_string(),
            );
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| "Codex returned malformed bundled model metadata".to_string())
    }

    #[cfg(test)]
    fn codex_bundled_catalog(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "models": [{
                "slug": "gpt-test",
                "display_name": "GPT Test",
                "description": "Bundled test model",
                "model_messages": { "instructions_template": "Complete official Codex instructions" },
                "base_instructions": "Complete official Codex instructions",
                "supported_in_api": true,
                "visibility": "list",
                "shell_type": "unified_exec",
                "tool_mode": "code_mode_only",
                "apply_patch_tool_type": "freeform",
                "multi_agent_version": "v2",
                "web_search_tool_type": "text_and_image",
                "truncation_policy": { "mode": "tokens", "limit": 10_000 },
                "input_modalities": ["text"]
            }]
        }))
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
            _ if !connect && path.is_err() => Edit {
                changes: Vec::new(),
                record: None,
                pending_secrets: Vec::new(),
                consumed_secrets: Vec::new(),
            },
            Ok(_) if connect && catalog.is_none() => Edit {
                changes: Vec::new(),
                record: None,
                pending_secrets: Vec::new(),
                consumed_secrets: Vec::new(),
            },
            Ok(mut doc) => match self.edit(agent, connect, &mut doc, &store, catalog, options) {
                Ok(edit) => edit,
                Err(error) if !connect && store.contains_key(agent.id()) => {
                    restore_problem = Some(error);
                    Edit {
                        changes: Vec::new(),
                        record: None,
                        pending_secrets: Vec::new(),
                        consumed_secrets: Vec::new(),
                    }
                }
                Err(error) => return Err(error),
            },
            // A broken config does not prevent revocation. Applying the
            // disconnect keeps its restoration journal until repair succeeds.
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
            let path = self.action_path(agent, store.get(agent.id()), connect);
            let (text, read_error) = self.config_text_at(agent, &path);
            if revision(
                agent,
                connect,
                path.as_deref().ok(),
                text.as_deref(),
                store.get(agent.id()),
                catalog,
                options,
            ) != revision_seen
            {
                return Err(format!(
                    "The {} config changed since the preview; review the changes again",
                    agent.name()
                ));
            }
            if connect {
                let path = path?;
                if let Some(error) = read_error {
                    return Err(error);
                }
                if catalog.is_some() {
                    self.require_helper()?;
                    self.connect(agent, &mut store, text, &path, catalog, options)?;
                } else {
                    if store.contains_key(agent.id()) {
                        self.suspend(agent, &mut store)?;
                    }
                    let saved_options = if options.default_model.is_none() {
                        store
                            .get(agent.id())
                            .map(|record| record.options.clone())
                            .unwrap_or_default()
                    } else {
                        options.clone()
                    };
                    store.insert(
                        agent.id().to_string(),
                        Connection {
                            config_path: Some(path),
                            suspended: true,
                            options: saved_options,
                            ..Connection::default()
                        },
                    );
                    self.save_store(&store)?;
                }
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

    /// Reconcile saved links with protection. All edits use the same lock and
    /// restoration journal as explicit connect/disconnect operations.
    pub fn reconcile(&self, catalog: Option<&Catalog>) -> Result<Vec<(String, String)>, String> {
        lock::with_apply_lock(&self.data_dir, || {
            let mut store = self.load_store()?;
            let mut failures = Vec::new();
            for agent in Agent::ALL {
                let Some(record) = store.get(agent.id()).cloned() else {
                    continue;
                };
                let status = self.status(agent, &store, catalog);
                let result = if record.cleanup_pending {
                    self.cleanup(agent, &mut store)
                } else if catalog.is_none()
                    || !status.installed
                    || status.error.is_some()
                    || (!record.suspended && !status.authorized)
                {
                    if catalog.is_some()
                        && status.installed
                        && !record.suspended
                        && !status.authorized
                    {
                        if let Some(record) = store.get_mut(agent.id()) {
                            record.attention = status.attention.clone().or_else(|| Some("Configuration changed outside the app; reconnect to apply it again".to_string()));
                        }
                    }
                    self.suspend(agent, &mut store)
                } else if record.suspended && record.attention.is_none() {
                    self.suspend(agent, &mut store).and_then(|()| {
                        self.require_helper()?;
                        let text = self.read_config(agent)?;
                        let path = self.action_path(agent, store.get(agent.id()), true)?;
                        self.connect(agent, &mut store, text, &path, catalog, &record.options)
                    })
                } else if !record.suspended
                    && catalog.is_some_and(|catalog| {
                        record.catalog_revision.as_deref() != Some(catalog.revision.as_str())
                    })
                {
                    (|| {
                        self.require_helper()?;
                        let text = self.read_config(agent)?;
                        let doc = self.parse_config(agent, text.as_deref())?;
                        let options = ConnectOptions {
                            default_model: selected_model(agent, Some(&doc))
                                .or_else(|| record.options.default_model.clone()),
                        };
                        let path = self.action_path(agent, store.get(agent.id()), true)?;
                        self.connect(agent, &mut store, text, &path, catalog, &options)
                    })()
                } else {
                    Ok(())
                };
                if let Err(error) = result {
                    if let Some(record) = store.get_mut(agent.id()) {
                        record.attention = Some(error.clone());
                        record.catalog_revision = catalog.map(|catalog| catalog.revision.clone());
                    }
                    self.save_store(&store)?;
                    failures.push((agent.id().to_string(), error));
                }
            }
            Ok(failures)
        })
    }

    fn suspend(&self, agent: Agent, store: &mut Store) -> Result<(), String> {
        let Some(record) = store.get_mut(agent.id()) else {
            return Ok(());
        };
        if record.suspended && record.fields.is_empty() {
            return Ok(());
        }
        self.tokens.revoke(agent.id())?;
        record.suspended = true;
        self.save_store(store)?;
        let record = store
            .get(agent.id())
            .cloned()
            .ok_or("Missing agent restore record")?;
        record.validate_recovery()?;
        if record.fields.is_empty() {
            return Ok(());
        }
        let path = record.restore_path()?;
        let text = self.read_config_at(agent, path)?;
        let mut doc = ConfigDoc::parse(agent.format(), text.as_deref().unwrap_or_default())
            .map_err(|reason| {
                format!(
                    "Cannot restore {} at {}: {reason}",
                    agent.name(),
                    path.display()
                )
            })?;
        let default_model =
            selected_model(agent, Some(&doc)).or_else(|| record.options.default_model.clone());
        let edit = if text.is_some() {
            restore(&mut doc, &record, self.secrets.as_ref())?
        } else {
            // A removed config is an uninstall/user action, not a request to
            // recreate fields the gateway previously removed.
            Edit {
                changes: Vec::new(),
                record: None,
                pending_secrets: Vec::new(),
                consumed_secrets: record
                    .fields
                    .iter()
                    .filter_map(|field| match &field.previous {
                        Some(Previous::Secret { secret_ref }) => Some(secret_ref.clone()),
                        _ => None,
                    })
                    .collect(),
            }
        };
        if !edit.changes.is_empty() {
            write_atomic(path, &doc.render()?, Some(text.as_deref()))
                .map_err(|error| format!("Cannot restore {}: {error}", agent.name()))?;
        }
        for entry in &edit.consumed_secrets {
            self.secrets.delete(entry)?;
        }
        if let Some(record) = store.get_mut(agent.id()) {
            record.fields.clear();
            record.options.default_model = default_model;
        }
        self.save_store(store)
    }

    /// Token, parked secrets, config, and record land together or are rolled
    /// back together.
    fn connect(
        &self,
        agent: Agent,
        store: &mut Store,
        text: Option<String>,
        path: &Path,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<(), String> {
        let doc = self.parse_config(agent, text.as_deref())?;
        let options = connection_options(agent, &doc, store.get(agent.id()), catalog, options);
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
        let edit = self.edit(agent, true, &mut doc, store, catalog, &options)?;
        if agent == Agent::Codex {
            self.sync_codex_catalog(
                catalog.ok_or_else(|| "The verified model list is not available".to_string())?,
            )?;
        }
        let mut guard = Rollback::default();
        let result = (|| -> Result<(), String> {
            // A fresh token on every new connection; a leftover file from an
            // incomplete disconnect is never reused.
            if store
                .get(agent.id())
                .is_some_and(|record| !record.suspended)
            {
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
                write_atomic(path, &doc.render()?, Some(text.as_deref())).map_err(|error| {
                    format!("Cannot write the {} config: {error}", agent.name())
                })?;
                guard.config = Some((path.to_path_buf(), text.clone()));
            }
            if let Some(mut record) = edit.record {
                record.config_path = Some(path.to_path_buf());
                record.options = options.clone();
                record.catalog_revision = catalog.map(|catalog| catalog.revision.clone());
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

    /// Keep the journal on failure so cleanup can be retried without losing
    /// the original settings or parked credentials.
    fn cleanup(&self, agent: Agent, store: &mut Store) -> Result<(), String> {
        self.suspend(agent, store)?;
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
        if fs::metadata(&self.helper_exe)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
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
        if agent == Agent::OpenClaw {
            openclaw::validate_host(&self.home, self.tool_env)?;
            openclaw::validate_helper(&self.helper_exe, &self.tokens.path(agent.id()))?;
            openclaw::validate_config(doc, store.get(agent.id()))?;
            openclaw::validate_selection(doc, options)?;
        }
        let options = connection_options(agent, doc, store.get(agent.id()), catalog, options);
        self.validate_native_config(agent, doc, store.get(agent.id()), &options, catalog)?;
        let codex_catalog_path = self.codex_catalog_path();
        let inputs = Inputs {
            endpoint: &self.endpoint,
            helper_exe: &self.helper_exe,
            token_path: &self.tokens.path(agent.id()),
            codex_catalog_path: &codex_catalog_path,
            catalog,
            options: &options,
        };
        let fields = fields(agent, &inputs)?;
        let edit = project(doc, &fields, store.get(agent.id()), agent)?;
        if agent == Agent::OpenCode {
            self.check_opencode_merge(doc, fields.iter().any(|field| field.path == ["model"]))?;
        }
        Ok(edit)
    }

    fn validate_native_config(
        &self,
        agent: Agent,
        doc: &ConfigDoc,
        prior: Option<&Connection>,
        options: &ConnectOptions,
        catalog: Option<&Catalog>,
    ) -> Result<(), String> {
        match agent {
            Agent::OhMyPi => oh_my_pi::validate_config(doc, prior),
            Agent::Codex if doc.contains(&["model_providers", "private_ai_proxy", "aws"]) => {
                Err("Codex's gateway provider has AWS authentication, which conflicts with command authentication. Remove that conflict in Codex; it will not be overwritten".to_string())
            }
            Agent::Pi => {
                let path = Agent::Pi.config_path(&self.home, self.tool_env).with_file_name("auth.json");
                let auth = read_auth_document(&path)?;
                if auth.get("private-ai-proxy").is_some() {
                    return Err("Pi has a stored credential for private-ai-proxy that takes priority over the helper. Resolve it in Pi before connecting; auth.json is left unchanged".to_string());
                }
                Ok(())
            }
            Agent::Hermes => {
                let scope = &["providers", "private-ai-proxy"];
                if doc.contains(scope) {
                    let owned = prior.is_some_and(|record| {
                        let fields: Vec<_> = record.fields.iter().filter(|field| field.path.starts_with(&owned(scope))).collect();
                        !fields.is_empty() && fields.iter().all(|field| doc.get_value(&refs(&field.path)) == field.value)
                    });
                    if !owned {
                        return Err("The Hermes private-ai-proxy provider already exists outside this connection; it will not be overwritten".to_string());
                    }
                }
                if doc.contains(&["providers", "private-ai-proxy", "enabled"])
                    && doc.get_value(&["providers", "private-ai-proxy", "enabled"]) != Some(ConfigValue::Bool(true)) {
                    return Err("The Hermes gateway provider must have enabled: true or omit that field; resolve it in Hermes before connecting".to_string());
                }
                if doc.contains(&["providers", "private-ai-proxy", "api_mode"])
                    && doc.get_str(&["providers", "private-ai-proxy", "api_mode"]).as_deref() != Some("chat_completions") {
                    return Err("Hermes api_mode overrides the gateway's chat_completions transport; resolve that conflict in Hermes".to_string());
                }
                for key in ["api_key", "key_env", "api_key_env"] {
                    if doc.contains(&["providers", "private-ai-proxy", key]) || doc.contains(&["model", key]) {
                        return Err("Hermes has an explicit credential source that may override or seed a pool ahead of the helper; remove that conflict in Hermes".to_string());
                    }
                }
                if doc.contains(&["fallback_model"]) {
                    return Err("Hermes fallback_model is outside this verified connection; disable it in Hermes before connecting. It will not be erased".to_string());
                }
                let existing = doc.get_str(&["model", "default"]);
                let selected = options.default_model.as_deref().map(str::trim).filter(|s| !s.is_empty())
                    .or(existing.as_deref());
                if selected.is_none_or(|id| id.is_empty() || catalog.is_some_and(|catalog| catalog.get(id).is_none_or(|model| !model.supports(agent.surface())))) {
                    return Err("Choose a verified default model for Hermes; the existing default cannot be used for this connection".to_string());
                }
                let path = Agent::Hermes.config_path(&self.home, self.tool_env);
                let directory = path.parent().ok_or("Invalid Hermes config directory")?;
                let native = hermes_native_dir(&self.home, self.tool_env);
                let resolved = directory.canonicalize().unwrap_or_else(|_| directory.to_path_buf());
                let resolved_native = native.canonicalize().unwrap_or_else(|_| native.clone());
                let root = if resolved.starts_with(&resolved_native) { native }
                    else if directory.parent().and_then(Path::file_name).is_some_and(|name| name == "profiles") {
                        directory.parent().and_then(Path::parent).ok_or("Invalid Hermes profile directory")?.to_path_buf()
                    } else { directory.to_path_buf() };
                // Hermes falls back to the root auth store for named profiles.
                let name = doc.get_str(&["providers", "private-ai-proxy", "name"])
                    .unwrap_or_else(|| PRODUCT_NAME.to_string());
                for path in [directory.join("auth.json"), root.join("auth.json")] {
                    let auth = read_auth_document(&path)?;
                    if let Some(pool) = auth.get("credential_pool") {
                        let pool = pool.as_object().ok_or("Cannot verify Hermes credential_pool; resolve its shape in Hermes")?;
                        for key in ["private-ai-proxy".to_string(), format!("custom:{}", name.trim().to_lowercase().replace(' ', "-"))] {
                            if pool.get(&key).is_some_and(|entries| entries.as_array().is_none_or(|entries| !entries.is_empty())) {
                                return Err("Hermes has a gateway credential pool that takes priority over key_cmd; resolve it in Hermes. Native auth files are left unchanged".to_string());
                            }
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// OpenCode v1.18.29 deep-merges these process-level sources in order.
    /// Keep the original write/restore path: selecting JSONC instead would
    /// strand old connection journals and our JSON writer would lose comments.
    /// Project/managed/remote sources need CLI context; references stay opaque
    /// here so inspection never reads an API key file or executes anything.
    fn check_opencode_merge(&self, doc: &ConfigDoc, owns_model: bool) -> Result<(), String> {
        let ConfigDoc::Json(projected) = doc else {
            return Err("OpenCode requires a JSON projection".to_string());
        };
        let global = self
            .tool_env
            .then(|| env_path("XDG_CONFIG_HOME"))
            .flatten()
            .unwrap_or_else(|| self.home.join(".config"))
            .join("opencode");
        let target = Agent::OpenCode.config_path(&self.home, self.tool_env);
        let mut paths = vec![
            global.join("config.json"),
            global.join("opencode.json"),
            global.join("opencode.jsonc"),
        ];
        if self.tool_env {
            if let Some(path) = env_path("OPENCODE_CONFIG") {
                paths.push(path);
            }
            if let Some(dir) = env_path("OPENCODE_CONFIG_DIR") {
                paths.extend([dir.join("opencode.json"), dir.join("opencode.jsonc")]);
            }
        }
        let mut merged = serde_json::json!({});
        let mut sources = Vec::new();
        for path in paths {
            let layer = if path == target {
                projected.clone()
            } else {
                let text = match fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(format!(
                        "Cannot verify OpenCode config merge: {} is unreadable. Access is disabled; \
                         fix that file or Disconnect to restore the original config.", path.display(),
                    )),
                };
                parse_jsonc(&text).map_err(|reason| {
                    format!(
                        "Cannot verify OpenCode config merge: {} is {reason}. Access is disabled; \
                     fix that file or Disconnect to restore the original config.",
                        path.display(),
                    )
                })?
            };
            sources.push(path.display().to_string());
            merge_opencode_config(&mut merged, layer);
        }
        if self.tool_env {
            if let Some(text) =
                env::var_os("OPENCODE_CONFIG_CONTENT").filter(|text| !text.is_empty())
            {
                let layer = text
                    .to_str()
                    .ok_or_else(|| "not valid Unicode".to_string())
                    .and_then(parse_jsonc)
                    .map_err(|reason| {
                        format!(
                        "Cannot verify OpenCode config merge: OPENCODE_CONFIG_CONTENT is {reason}. \
                         Access is disabled; fix that override or Disconnect.",
                    )
                    })?;
                sources.push("OPENCODE_CONFIG_CONTENT".to_string());
                merge_opencode_config(&mut merged, layer);
            }
        }
        if merged.get("disabled_providers").is_some_and(|values| {
            values.as_array().is_none_or(|values| {
                values
                    .iter()
                    .any(|value| !value.is_string() || value.as_str() == Some("private-ai-proxy"))
            })
        }) || merged.get("enabled_providers").is_some_and(|values| {
            values.as_array().is_none_or(|values| {
                values.iter().any(|value| !value.is_string())
                    || !values
                        .iter()
                        .any(|value| value.as_str() == Some("private-ai-proxy"))
            })
        }) {
            return Err("OpenCode's enabled_providers/disabled_providers exclude the gateway or are invalid. Resolve those filters in OpenCode; they will not be overwritten".to_string());
        }
        for pointer in ["/provider/private-ai-proxy", "/model"] {
            if pointer == "/model" && !owns_model {
                continue;
            }
            if merged.pointer(pointer) != projected.pointer(pointer) {
                return Err(format!(
                    "OpenCode's merged config changes the gateway-owned field {pointer}. \
                     Access is disabled. Review {} without changing unrelated providers, \
                     or Disconnect to restore the original config.",
                    sources.join(", "),
                ));
            }
        }
        Ok(())
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
            repair_action: None,
        };
        if agent == Agent::OpenClaw && !installed && record.is_none() {
            return status;
        }
        if let Err(error) = self.require_helper() {
            status.error = Some(error.clone());
            if agent == Agent::OpenClaw {
                status.attention = Some(error);
                return status;
            }
        }
        if agent == Agent::OpenClaw && (installed || record.is_some()) {
            let valid = openclaw::validate_host(&self.home, self.tool_env).and_then(|()| {
                openclaw::validate_helper(&self.helper_exe, &self.tokens.path(agent.id()))
            });
            if let Err(attention) = valid {
                status.attention = Some(attention);
                return status;
            }
        }
        let current_path = self.action_path(agent, record, true);
        if let Err(attention) = &current_path {
            if let Some(path) = record.and_then(|record| record.config_path.as_ref()) {
                status.config_path = path.display().to_string();
            }
            status.attention = Some(attention.clone());
            return status;
        }
        // A broken config is reported, never hidden behind "not connected";
        // the record, the attention line, and Disconnect all stay available.
        let (text, read_error) = self.config_text_at(agent, &current_path);
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
        if record.suspended && !record.cleanup_pending {
            status.connected = true;
            status.attention = record.attention.clone().or_else(|| {
                (!installed).then(|| {
                    "CLI not found; configuration stays restored until the agent is available"
                        .to_string()
                })
            });
            if status.attention.is_some() && installed && status.error.is_none() {
                status.repair_action = Some(AgentRepairAction::Reconnect);
            }
            if !record.fields.is_empty() {
                status.repair_action = Some(AgentRepairAction::Disconnect);
                status.attention = Some(
                    "Configuration restoration is incomplete; retry stopping protection"
                        .to_string(),
                );
            }
            return status;
        }
        let managed = doc.as_ref().is_some_and(|doc| {
            record.fields.iter().all(|field| {
                (agent == Agent::Codex && field.path == ["model"])
                    || doc.get_value(&refs(&field.path)) == field.value
            })
        });
        let token = self.tokens.read(agent.id()).ok().flatten().is_some();
        status.connected = managed && token;
        status.authorized = !record.disabled && managed && token;
        if record.cleanup_pending {
            status.repair_action = Some(AgentRepairAction::Disconnect);
            status.attention = Some(
                "Disconnect did not complete; this agent's access is disabled until Disconnect \
                 is retried"
                    .to_string(),
            );
        } else if record.disabled {
            status.repair_action = Some(AgentRepairAction::Disconnect);
            status.attention =
                Some("This connection is disabled; Disconnect to restore your config".to_string());
        } else if !managed {
            if status.error.is_none() {
                status.repair_action = Some(AgentRepairAction::Reconnect);
            }
            status.attention = Some(
                "The gateway endpoint or authentication settings changed outside the app. \
                 Access is paused. Reconnect this agent in the app, then restart its CLI to reload the configuration."
                    .to_string(),
            );
        } else if !token {
            status.repair_action = Some(AgentRepairAction::Disconnect);
            status.attention = Some(
                "This agent's access is revoked; retry Disconnect to restore its config"
                    .to_string(),
            );
        } else if stale_helper(agent, record, &self.helper_exe) {
            status.repair_action = Some(AgentRepairAction::Reconnect);
            status.connected = false;
            status.authorized = false;
            status.attention = Some(
                "This connection uses an outdated credential helper path or command. Disconnect, \
                 then Connect again to update it."
                    .to_string(),
            );
        } else if status.connected {
            if let (Some(catalog), Some(model)) = (catalog, selected_model(agent, doc.as_ref())) {
                if catalog
                    .get(&model)
                    .is_none_or(|model| !model.supports(agent.surface()))
                {
                    status.attention = Some(format!(
                        "`{model}` is not available from the current profile. Choose an available model in the agent; the connection does not need to be recreated."
                    ));
                }
            }
        }
        if agent == Agent::OpenCode && status.authorized {
            if let Some(doc) = &doc {
                let owns_model = record.fields.iter().any(|field| field.path == ["model"]);
                if let Err(attention) = self.check_opencode_merge(doc, owns_model) {
                    status.connected = false;
                    status.authorized = false;
                    status.attention = Some(attention);
                }
            }
        }
        if status.authorized {
            if let Some(doc) = &doc {
                if let Err(attention) =
                    self.validate_native_config(agent, doc, Some(record), &record.options, catalog)
                {
                    status.connected = false;
                    status.authorized = false;
                    status.attention = Some(attention);
                }
            }
        }
        if agent == Agent::OpenClaw && status.authorized {
            let result = doc
                .as_ref()
                .ok_or_else(|| "OpenClaw config is unavailable".to_string())
                .and_then(|doc| openclaw::validate_config(doc, Some(record)));
            let stale =
                openclaw::stale_helper(record, &self.helper_exe, &self.tokens.path(agent.id()));
            if result.is_err() || stale {
                status.connected = false;
                status.authorized = false;
                status.attention = Some(result.err().unwrap_or_else(|| {
                    "The OpenClaw helper changed; Disconnect then Connect again".to_string()
                }));
            }
        }
        status
    }

    /// Lenient read for flows that must survive a broken config (status,
    /// revisions, disconnect): the error is carried, never thrown.
    fn config_text_at(
        &self,
        agent: Agent,
        path: &Result<PathBuf, String>,
    ) -> (Option<String>, Option<String>) {
        match path
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|path| self.read_config_at(agent, path))
        {
            Ok(text) => (text, None),
            Err(error) => (None, Some(error)),
        }
    }

    /// The config text, or `None` when the file does not exist yet.
    fn read_config(&self, agent: Agent) -> Result<Option<String>, String> {
        self.read_config_at(agent, &self.action_path(agent, None, true)?)
    }

    fn read_config_at(&self, agent: Agent, path: &Path) -> Result<Option<String>, String> {
        match fs::read_to_string(path) {
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
        for record in store.values() {
            record.validate_recovery()?;
        }
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

fn read_auth_document(path: &Path) -> Result<serde_json::Value, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(serde_json::json!({})),
        Err(_) => {
            return Err(format!(
                "Cannot inspect native credential conflicts at {}; the file is unreadable",
                path.display()
            ))
        }
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "Cannot inspect native credential conflicts at {}; invalid JSON",
            path.display()
        )
    })?;
    if !value.is_object() {
        return Err(format!(
            "Cannot inspect native credential conflicts at {}; expected an object",
            path.display()
        ));
    }
    Ok(value)
}

// OpenCode uses remeda mergeDeep: objects merge recursively; other values replace.
fn merge_opencode_config(target: &mut serde_json::Value, source: serde_json::Value) {
    match (target, source) {
        (serde_json::Value::Object(target), serde_json::Value::Object(source)) => {
            for (key, value) in source {
                merge_opencode_config(target.entry(key).or_insert(serde_json::Value::Null), value);
            }
        }
        (target, source) => *target = source,
    }
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
        // App-owned structured providers must never absorb an unmanaged object:
        // it may contain nested credentials that a field-name check cannot see.
        if matches!(field.value, Some(ConfigValue::Json(_)))
            && doc.contains(&path)
            && still_ours.is_none()
        {
            return Err(format!("The {} provider already exists outside this connection. Leave it unchanged and resolve the ownership conflict before connecting", agent.name()));
        }
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
                None => doc.remove(&path)?,
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
    record.validate_recovery()?;
    let routing_unchanged = record
        .fields
        .iter()
        .filter(|field| {
            matches!(
                field.path.last().map(String::as_str),
                Some(
                    "apiKeyHelper"
                        | "ANTHROPIC_BASE_URL"
                        | "base_url"
                        | "baseURL"
                        | "command"
                        | "args"
                        | "model_provider"
                )
            )
        })
        .all(|field| doc.get_value(&refs(&field.path)) == field.value);
    if !routing_unchanged
        && record.fields.iter().any(|field| {
            is_sensitive(&field.path)
                && field.value.is_none()
                && field.previous.is_some()
                && doc.get_value(&refs(&field.path)).is_none()
        })
    {
        return Err("Credential restoration is ambiguous after routing or helper edits. Access is disabled; the recovery record and parked credentials are retained".to_string());
    }
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
            None => doc.remove(&path)?,
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

fn connection_options(
    agent: Agent,
    doc: &ConfigDoc,
    prior: Option<&Connection>,
    catalog: Option<&Catalog>,
    options: &ConnectOptions,
) -> ConnectOptions {
    if agent != Agent::Codex || options.default_model.is_some() {
        return options.clone();
    }
    let current = selected_model(agent, Some(doc))
        .filter(|model| catalog.is_some_and(|catalog| catalog.get(model).is_some()));
    let saved = prior.and_then(|record| record.options.default_model.clone());
    let preferred = if prior.is_some_and(|record| record.suspended && record.attention.is_none()) {
        saved.or(current)
    } else {
        current.or(saved)
    };
    ConnectOptions {
        default_model: preferred.or_else(|| {
            catalog
                .and_then(|catalog| {
                    catalog
                        .models
                        .iter()
                        .find(|model| model.supports(agent.surface()))
                })
                .map(|model| model.id().to_string())
        }),
    }
}

fn selected_model(agent: Agent, doc: Option<&ConfigDoc>) -> Option<String> {
    let doc = doc?;
    match agent {
        Agent::Codex => doc.get_str(&["model"]),
        Agent::ClaudeCode => doc.get_str(&["env", "ANTHROPIC_MODEL"]),
        Agent::OpenCode => doc
            .get_str(&["model"])
            .and_then(|value| value.strip_prefix("private-ai-proxy/").map(str::to_string)),
        Agent::Pi => None,
        Agent::Hermes => doc.get_str(&["model", "default"]),
        Agent::OpenClaw => openclaw::selected_model(doc),
        Agent::OhMyPi => None,
    }
}

fn cli_paths(home: &Path, tool_env: bool) -> Vec<PathBuf> {
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
    paths
}

fn find_cli(agent: Agent, home: &Path, tool_env: bool) -> Option<PathBuf> {
    let mut paths = cli_paths(home, tool_env);
    if cfg!(windows) && agent == Agent::Hermes {
        if let Some(directory) = agent.config_path(home, tool_env).parent() {
            paths.push(directory.join("bin"));
        }
    }
    find_cli_in_paths(agent, &paths)
}

fn find_cli_in_paths(agent: Agent, paths: &[PathBuf]) -> Option<PathBuf> {
    agent
        .cli_names()
        .iter()
        .find_map(|name| cli_in_paths(name, paths))
}

fn cli_installed(agent: Agent, home: &Path, tool_env: bool) -> bool {
    find_cli(agent, home, tool_env).is_some()
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

fn cli_in_paths(name: &str, paths: &[PathBuf]) -> Option<PathBuf> {
    let candidates: &[String] = &if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    } else {
        vec![name.to_string()]
    };
    paths
        .iter()
        .flat_map(|dir| candidates.iter().map(move |candidate| dir.join(candidate)))
        .find(|candidate| candidate.is_file())
}

fn command_output(
    executable: &Path,
    args: &[&str],
    search_paths: &[PathBuf],
) -> io::Result<std::process::Output> {
    let mut command;
    #[cfg(windows)]
    if executable
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
    {
        command = Command::new("cmd");
        command.arg("/C").arg(executable).args(args);
    } else {
        command = Command::new(executable);
        command.args(args);
    }
    #[cfg(not(windows))]
    {
        command = Command::new(executable);
        command.args(args);
    }
    command.env("PATH", command_path(search_paths)?);
    bounded_command_output(command, std::time::Duration::from_secs(15))
}

fn bounded_command_output(
    command: Command,
    timeout: std::time::Duration,
) -> io::Result<std::process::Output> {
    // Agent transactions are synchronous and may run inside a Tokio worker.
    // A dedicated thread keeps the bounded I/O runtime out of the caller's runtime.
    std::thread::Builder::new()
        .name("agent-metadata".to_string())
        .spawn(move || {
            use std::process::Stdio;
            use tokio::io::AsyncReadExt;

            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    let mut child = tokio::process::Command::from(command)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()?;
                    let mut stdout = child.stdout.take().ok_or_else(|| io::Error::other("Missing stdout pipe"))?;
                    let mut stderr = child.stderr.take().ok_or_else(|| io::Error::other("Missing stderr pipe"))?;
                    let mut out = Vec::new();
                    let mut err = Vec::new();
                    let result = tokio::time::timeout(timeout, async {
                        tokio::try_join!(child.wait(), stdout.read_to_end(&mut out), stderr.read_to_end(&mut err))
                    }).await;
                    match result {
                        Ok(Ok((status, _, _))) => Ok(std::process::Output { status, stdout: out, stderr: err }),
                        result => {
                            // kill() also waits for exit, releasing the process before the config lock.
                            child.kill().await?;
                            match result {
                                Ok(Err(error)) => Err(error),
                                _ => Err(io::Error::new(io::ErrorKind::TimedOut, "Codex model metadata export timed out; check the Codex installation and retry")),
                            }
                        }
                    }
                })
        })?
        .join()
        .map_err(|_| io::Error::other("Model metadata worker failed"))?
}

fn command_path(search_paths: &[PathBuf]) -> io::Result<OsString> {
    let mut paths = search_paths.to_vec();
    if let Some(current) = env::var_os("PATH") {
        for path in env::split_paths(&current) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    env::join_paths(paths).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    if let Some(home) = env_path("USERPROFILE") {
        return Ok(home);
    }
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .ok_or_else(|| "Cannot determine the home directory".to_string())
}

/// The per-user app data directory (tokens, connection record, locks),
/// resolved the same way by the desktop shell and the
/// bundled helper.
pub fn app_data_dir() -> Result<PathBuf, String> {
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
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;
    use serde_json::json;

    const ENDPOINT: &str = "http://127.0.0.1:4180";

    pub(super) fn catalog() -> Catalog {
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

    pub(super) struct Sandbox {
        pub(super) home: PathBuf,
        pub(super) projector: Projector,
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
    pub(super) fn sandbox(name: &str) -> Sandbox {
        let home = env::temp_dir().join(format!("pap-agents-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let helper = home.join("Private AI Proxy.app").join(helper_binary_name());
        write(&helper, "#!/bin/sh\n");
        let data_dir = if cfg!(target_os = "macos") {
            home.join("Library")
                .join("Application Support")
                .join(APP_IDENTIFIER)
        } else {
            home.join(APP_IDENTIFIER)
        };
        let staged = data_dir.join("helpers").join(helper_binary_name());
        write(&staged, "#!/bin/sh\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&helper, &staged] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let secrets = Arc::new(MemoryStore::default());
        let projector = Projector::at(
            home.clone(),
            data_dir,
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

    pub(super) fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn claude_options() -> ConnectOptions {
        ConnectOptions {
            default_model: Some("openai/gpt-oss-20b".to_string()),
        }
    }

    #[test]
    fn openclaw_same_path_empty_helper_deauthorizes_but_does_not_block_disconnect() {
        let mut sandbox = sandbox("openclaw-empty-helper");
        let agent = Agent::OpenClaw;
        let options = claude_options();
        let catalog = catalog();
        sandbox.projector.helper_exe = sandbox
            .projector
            .data_dir
            .join("helpers")
            .join(helper_binary_name());
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .unwrap();
        sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        fs::write(&sandbox.projector.helper_exe, "").unwrap();
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        let status = statuses
            .iter()
            .find(|status| status.id == "openclaw")
            .unwrap();
        assert!(status.recorded && !status.connected && !status.authorized && tokens.is_empty());
        assert!(sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .is_err());
        disconnect(&sandbox, agent);
        assert!(sandbox.projector.load_store().unwrap().is_empty());
    }

    #[test]
    fn helper_relocation_requires_explicit_reconnect_without_scan_writes() {
        for agent in Agent::ALL {
            let mut sandbox = sandbox(&format!("helper-relocation-{}", agent.id()));
            let catalog = catalog();
            let options = claude_options();
            let path = agent.config_path(&sandbox.home, false);
            let preview = sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .unwrap();
            sandbox
                .projector
                .apply(agent, true, &preview.revision, Some(&catalog), &options)
                .unwrap();
            let current = sandbox.projector.helper_exe.clone();
            let assert_scan = |sandbox: &Sandbox, connected: bool, attention: Option<&str>| {
                let config = fs::read(&path).unwrap();
                let manifest = fs::read(sandbox.projector.store_path()).unwrap();
                let token_path = sandbox.projector.tokens.path(agent.id());
                let token = fs::read(&token_path).unwrap();
                let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
                let status = statuses
                    .iter()
                    .find(|status| status.id == agent.id())
                    .unwrap();
                assert!(status.recorded);
                assert_eq!(status.connected, connected, "{}", agent.id());
                assert_eq!(status.authorized, connected, "{}", agent.id());
                assert_eq!(tokens.is_empty(), !connected);
                if let Some(attention) = attention {
                    assert!(status.attention.as_deref().unwrap().contains(attention));
                } else {
                    assert!(status.attention.is_none());
                }
                assert_eq!(fs::read(&path).unwrap(), config);
                assert_eq!(fs::read(sandbox.projector.store_path()).unwrap(), manifest);
                assert_eq!(fs::read(token_path).unwrap(), token);
            };
            assert_scan(&sandbox, true, None);
            // A recorded Pi catalog may differ from today's generated metadata.
            if agent == Agent::Pi {
                let mut config = doc(&sandbox, agent);
                config
                    .set_str(&["providers", "private-ai-proxy", "name"], "Previous name")
                    .unwrap();
                write(&path, &config.render().unwrap());
                let mut store = sandbox.projector.load_store().unwrap();
                store.get_mut(agent.id()).unwrap().fields[0].value =
                    config.get_value(&["providers", "private-ai-proxy"]);
                sandbox.projector.save_store(&store).unwrap();
                assert_scan(&sandbox, true, None);
            }
            let stable = sandbox
                .home
                .join("stable helpers")
                .join(helper_binary_name());
            sandbox.projector.helper_exe = stable.clone();
            write(&sandbox.projector.helper_exe, "helper");
            if agent == Agent::OpenCode {
                assert_scan(&sandbox, true, None);
            } else if agent == Agent::OpenClaw {
                assert_scan(
                    &sandbox,
                    false,
                    Some(if cfg!(unix) {
                        "differs"
                    } else {
                        "helper changed"
                    }),
                );
                sandbox.projector.helper_exe = current;
                assert_scan(&sandbox, true, None);
            } else {
                assert_scan(&sandbox, false, Some("Disconnect, then Connect"));
                sandbox.projector.helper_exe = current;
                assert_scan(&sandbox, true, None);
                sandbox.projector.helper_exe = stable;
            }
            // External edits take precedence over helper relocation and survive cleanup.
            let mut config = doc(&sandbox, agent);
            let field = match agent {
                Agent::Codex => &["model_provider"][..],
                Agent::ClaudeCode => &["apiKeyHelper"][..],
                Agent::OpenCode => &["model"][..],
                Agent::Pi => &["providers", "private-ai-proxy", "apiKey"][..],
                Agent::Hermes => &["providers", "private-ai-proxy", "key_cmd"][..],
                Agent::OpenClaw => &["agents", "defaults", "model", "primary"][..],
                Agent::OhMyPi => &["providers", "private-ai-proxy", "apiKey"][..],
            };
            config.set_str(field, "external-edit").unwrap();
            write(&path, &config.render().unwrap());
            assert_scan(&sandbox, false, Some("settings changed"));
            disconnect(&sandbox, agent);
            assert_eq!(
                doc(&sandbox, agent).get_str(field).as_deref(),
                Some("external-edit"),
            );
        }
    }

    #[tokio::test]
    async fn metadata_timeout_releases_the_config_transaction() {
        const CHILD_ENV: &str = "PAP_TEST_METADATA_TIMEOUT_CHILD";
        if env::var_os(CHILD_ENV).is_some() {
            std::thread::sleep(std::time::Duration::from_secs(30));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new(env::current_exe().unwrap());
        command.args([
            "--exact",
            "agents::tests::metadata_timeout_releases_the_config_transaction",
        ]);
        command.env(CHILD_ENV, "1");
        let started = std::time::Instant::now();
        let error = lock::with_apply_lock(root.path(), || {
            let error =
                bounded_command_output(command, std::time::Duration::from_secs(1)).unwrap_err();
            Ok(error.kind())
        })
        .unwrap();
        assert_eq!(error, io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        lock::with_apply_lock(root.path(), || Ok(())).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn command_output_resolves_the_discovered_shebang_runtime() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!("pap-command-path-{}", std::process::id()));
        let runtime = root.join("bin");
        let executable = runtime.join("codex");
        let node = runtime.join("node");
        let _ = fs::remove_dir_all(&root);
        write(&executable, "#!/usr/bin/env node\n");
        write(&node, "#!/bin/sh\nprintf bundled-catalog\n");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&node, fs::Permissions::from_mode(0o700)).unwrap();

        let output = command_output(&executable, &[], std::slice::from_ref(&runtime)).unwrap();
        let _ = fs::remove_dir_all(root);
        assert!(output.status.success());
        assert_eq!(output.stdout, b"bundled-catalog");
    }

    #[test]
    fn hermes_paths_follow_platform_overrides_and_isolate_test_home() {
        const CASE_ENV: &str = "PAP_TEST_HERMES_PATH_CASE";
        const ROOT_ENV: &str = "PAP_TEST_HERMES_PATH_ROOT";
        if let Ok(case) = env::var(CASE_ENV) {
            let root = PathBuf::from(env::var_os(ROOT_ENV).unwrap());
            let home = root.join(if case == "isolated" {
                "isolated"
            } else {
                "user"
            });
            let default = if cfg!(windows) {
                home.join("AppData").join("Local").join("hermes")
            } else {
                home.join(".hermes")
            };
            let expected = match case.as_str() {
                "override" => root.join("custom-profile"),
                "local-appdata" if cfg!(windows) => root.join("local-data").join("hermes"),
                _ => default,
            };
            let projector = Projector::new(
                root.join(helper_binary_name()),
                ENDPOINT,
                Arc::new(MemoryStore::default()),
            )
            .unwrap();
            assert_eq!(projector.home, home);
            assert_eq!(
                Agent::Hermes.config_path(&projector.home, projector.tool_env),
                expected.join("config.yaml")
            );
            assert_eq!(
                Agent::Pi.config_path(&projector.home, projector.tool_env),
                if case == "isolated" {
                    home.join(".pi").join("agent").join("models.json")
                } else {
                    root.join("pi-override").join("models.json")
                }
            );
            if case == "isolated" {
                assert!(!projector.tool_env);
                assert_eq!(projector.data_dir, home.join(".private-ai-proxy"));
                for agent in Agent::ALL {
                    assert!(agent
                        .config_path(&projector.home, projector.tool_env)
                        .starts_with(&home));
                }
            }
            if cfg!(windows) {
                let executable = expected.join("bin").join("hermes.exe");
                write(&executable, "launcher fixture");
                assert_eq!(
                    find_cli(Agent::Hermes, &projector.home, projector.tool_env),
                    Some(executable)
                );
            }
            return;
        }

        // Process-local environment avoids races with the other agent tests.
        let root = tempfile::tempdir().unwrap();
        for case in [
            "default",
            "local-appdata",
            "override",
            "isolated",
            "native-home",
        ] {
            let mut command = Command::new(env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "agents::tests::hermes_paths_follow_platform_overrides_and_isolate_test_home",
                ])
                .env(CASE_ENV, case)
                .env(ROOT_ENV, root.path())
                .env("HOME", root.path().join("user"))
                .env("USERPROFILE", root.path().join("user"))
                .env("APPDATA", root.path().join("roaming"))
                .env("XDG_DATA_HOME", root.path().join("data"))
                .env("PI_CODING_AGENT_DIR", root.path().join("pi-override"))
                .env("PI_AGENT_DIR", root.path().join("unused-pi-dir"))
                .env("PATH", "")
                .env_remove(HOME_OVERRIDE_ENV)
                .env_remove("HERMES_HOME")
                .env_remove("LOCALAPPDATA");
            if matches!(case, "local-appdata" | "override" | "isolated") {
                command.env("LOCALAPPDATA", root.path().join("local-data"));
            }
            if matches!(case, "override" | "isolated") {
                command.env("HERMES_HOME", root.path().join("custom-profile"));
            }
            if case == "isolated" {
                command
                    .env(HOME_OVERRIDE_ENV, root.path().join("isolated"))
                    .env("CODEX_HOME", root.path().join("outside-codex"));
            }
            if cfg!(windows) && case == "native-home" {
                command.env("HOME", root.path().join("git-home"));
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{case}: {output:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the installed helper and Python on a disposable Windows runner"]
    fn hermes_windows_installed_command_round_trip() {
        let installed = PathBuf::from(env::var_os("PAP_TEST_HELPER_PATH").unwrap());
        assert!(installed.is_absolute() && installed.is_file());
        assert!(installed.to_str().unwrap().contains(' '));
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join(".private-ai-proxy");
        let tokens = TokenFiles::new(&data);
        let hostile = home
            .path()
            .join("quote'\u{2019}; Write-Output injected; # %PAP_CMD_PROBE% ! ^ & (meta)")
            .join(helper_binary_name());
        fs::create_dir_all(hostile.parent().unwrap()).unwrap();
        fs::copy(&installed, &hostile).unwrap();
        for executable in [&installed, &hostile] {
            let token = tokens.ensure("hermes").unwrap();
            let fields = fields(
                Agent::Hermes,
                &Inputs {
                    endpoint: ENDPOINT,
                    helper_exe: executable,
                    token_path: &tokens.path("hermes"),
                    codex_catalog_path: &data.join(CODEX_CATALOG_FILE),
                    catalog: Some(&catalog()),
                    options: &ConnectOptions::default(),
                },
            )
            .unwrap();
            let command = fields
                .into_iter()
                .find(|field| field.path == owned(&["providers", "private-ai-proxy", "key_cmd"]))
                .and_then(|field| match field.value {
                    Some(ConfigValue::Str(command)) => Some(command),
                    _ => None,
                })
                .unwrap();
            let run = || {
                Command::new("python")
                    .args([
                        "-c",
                        "import subprocess,sys; p=subprocess.run(sys.argv[1],shell=True,capture_output=True,text=True,timeout=15); sys.stdout.write(p.stdout); sys.stderr.write(p.stderr); sys.exit(p.returncode)",
                        &command,
                    ])
                    .env(HOME_OVERRIDE_ENV, home.path())
                    .env("PAP_CMD_PROBE", "expanded-by-shell")
                    .output()
                    .unwrap()
            };
            let output = run();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), token);
            fs::remove_file(tokens.path("hermes")).unwrap();
            let output = run();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            if executable == &hostile {
                fs::remove_file(executable).unwrap();
                let output = run();
                assert!(!output.status.success());
                assert!(output.stdout.is_empty());
            }
        }
        assert!(credential_helper_command(Path::new("bad\0path"), Agent::Hermes).is_err());
    }

    #[test]
    fn codex_model_changes_keep_credentials_but_endpoint_changes_revoke_them() {
        let sandbox = sandbox("codex-model-preference");
        let agent = Agent::Codex;
        let path = agent.config_path(&sandbox.home, false);
        write(
            &sandbox.home.join(".local/bin").join(if cfg!(windows) {
                "codex.exe"
            } else {
                "codex"
            }),
            "test cli",
        );
        let options = claude_options();
        let catalog = catalog();
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .unwrap();
        sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        let token = sandbox.projector.tokens.read(agent.id()).unwrap();
        let mut config = doc(&sandbox, agent);
        config.set_str(&["model"], "phala/qwen").unwrap();
        write(&path, &config.render().unwrap());
        sandbox.projector.reconcile(Some(&catalog)).unwrap();
        assert!(
            sandbox
                .projector
                .status(
                    agent,
                    &sandbox.projector.load_store().unwrap(),
                    Some(&catalog)
                )
                .authorized
        );
        assert_eq!(sandbox.projector.tokens.read(agent.id()).unwrap(), token);
        sandbox.projector.reconcile(None).unwrap();
        let mut legacy = sandbox.projector.load_store().unwrap();
        let record = legacy.get_mut(agent.id()).unwrap();
        record.options = options.clone();
        record.attention = Some("Configuration changed in an older app version".into());
        sandbox.projector.save_store(&legacy).unwrap();
        let repair = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &repair)
            .unwrap();
        sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &repair)
            .unwrap();
        sandbox.projector.reconcile(None).unwrap();
        sandbox.projector.reconcile(Some(&catalog)).unwrap();
        assert_eq!(
            doc(&sandbox, agent).get_str(&["model"]).as_deref(),
            Some("phala/qwen")
        );
        assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_some());
        let mut config = doc(&sandbox, agent);
        config
            .set_str(
                &["model_providers", "private_ai_proxy", "base_url"],
                "https://example.com/v1",
            )
            .unwrap();
        write(&path, &config.render().unwrap());
        sandbox.projector.reconcile(Some(&catalog)).unwrap();
        assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
        assert_eq!(
            doc(&sandbox, agent)
                .get_str(&["model_providers", "private_ai_proxy", "base_url"])
                .as_deref(),
            Some("https://example.com/v1")
        );
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

    pub(super) fn disconnect(sandbox: &Sandbox, agent: Agent) -> AgentStatus {
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
    fn links_survive_stop_restart_and_uninstall_without_owning_inactive_configs() {
        let sandbox = sandbox("link-lifecycle");
        let agent = Agent::ClaudeCode;
        let config = agent.config_path(&sandbox.home, false);
        let cli = sandbox.home.join(".local/bin").join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        write(&cli, "test cli");
        write(
            &config,
            r#"{"model":"original","env":{"ANTHROPIC_AUTH_TOKEN":"original-secret"}}"#,
        );
        let options = claude_options();
        let preview = sandbox
            .projector
            .preview(agent, true, None, &options)
            .unwrap();
        let status = sandbox
            .projector
            .apply(agent, true, &preview.revision, None, &options)
            .unwrap();
        assert!(status.connected && !status.authorized);
        assert_eq!(
            doc(&sandbox, agent).get_value(&["model"]),
            Some(ConfigValue::Str("original".into()))
        );

        assert!(sandbox
            .projector
            .reconcile(Some(&catalog()))
            .unwrap()
            .is_empty());
        assert!(
            sandbox
                .projector
                .scan(None)
                .unwrap()
                .0
                .iter()
                .find(|s| s.id == agent.id())
                .unwrap()
                .authorized
        );
        assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
        assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
        assert_eq!(
            doc(&sandbox, agent).get_value(&["model"]),
            Some(ConfigValue::Str("original".into()))
        );
        assert_eq!(
            doc(&sandbox, agent).get_value(&["env", "ANTHROPIC_AUTH_TOKEN"]),
            Some(ConfigValue::Str("original-secret".into()))
        );
        assert!(sandbox.projector.load_store().unwrap()[agent.id()].suspended);

        assert!(sandbox
            .projector
            .reconcile(Some(&catalog()))
            .unwrap()
            .is_empty());
        fs::remove_file(&cli).unwrap();
        fs::remove_file(&config).unwrap();
        assert!(sandbox
            .projector
            .reconcile(Some(&catalog()))
            .unwrap()
            .is_empty());
        assert!(
            !config.exists(),
            "uninstall must not recreate deleted config"
        );
        let status = sandbox
            .projector
            .scan(None)
            .unwrap()
            .0
            .into_iter()
            .find(|s| s.id == agent.id())
            .unwrap();
        assert!(status.connected && !status.authorized && status.attention.is_some());
        write(&cli, "test cli");
        assert!(sandbox
            .projector
            .reconcile(Some(&catalog()))
            .unwrap()
            .is_empty());
        assert!(
            sandbox
                .projector
                .scan(None)
                .unwrap()
                .0
                .iter()
                .find(|s| s.id == agent.id())
                .unwrap()
                .authorized
        );
        disconnect(&sandbox, agent);
        assert!(sandbox.projector.load_store().unwrap().is_empty());
    }

    #[test]
    fn suspended_restore_keeps_external_edits_and_retries_invalid_files() {
        let sandbox = sandbox("link-restore-retry");
        let agent = Agent::ClaudeCode;
        let config = agent.config_path(&sandbox.home, false);
        write(&config, r#"{"model":"original"}"#);
        connect(&sandbox);
        let managed = fs::read_to_string(&config).unwrap();
        write(&config, "invalid json");
        assert_eq!(sandbox.projector.reconcile(None).unwrap().len(), 1);
        assert!(!sandbox.projector.load_store().unwrap()[agent.id()]
            .fields
            .is_empty());
        assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
        write(&config, &managed);
        assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
        assert_eq!(
            doc(&sandbox, agent).get_value(&["model"]),
            Some(ConfigValue::Str("original".into()))
        );
        connect(&sandbox);
        let mut edited = doc(&sandbox, agent);
        edited
            .set_value(&["model"], &ConfigValue::Str("user-choice".into()))
            .unwrap();
        write(&config, &edited.render().unwrap());
        assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
        assert_eq!(
            doc(&sandbox, agent).get_value(&["model"]),
            Some(ConfigValue::Str("user-choice".into()))
        );
    }

    #[test]
    fn opencode_merge_conflicts_revoke_without_writes_and_keep_original_restore_path() {
        let sandbox = sandbox("opencode-merge");
        let path = Agent::OpenCode.config_path(&sandbox.home, false);
        let jsonc = path.with_extension("jsonc");
        let original = json!({
            "model": "other/original",
            "provider": {"other": {"name": "User provider"}}
        });
        write(&path, &original.to_string());
        let benign =
            "{/* user's comment */\"provider\":{\"other\":{\"name\":\"JSONC user provider\"}},}";
        write(&jsonc, benign);
        let catalog = catalog();
        let options = claude_options();
        let preview = sandbox
            .projector
            .preview(Agent::OpenCode, true, Some(&catalog), &options)
            .unwrap();
        assert!(
            sandbox
                .projector
                .apply(
                    Agent::OpenCode,
                    true,
                    &preview.revision,
                    Some(&catalog),
                    &options,
                )
                .unwrap()
                .authorized
        );
        assert_eq!(fs::read_to_string(&jsonc).unwrap(), benign);
        assert_eq!(
            doc(&sandbox, Agent::OpenCode)
                .get_str(&["provider", "other", "name"])
                .as_deref(),
            Some("User provider")
        );
        let preview = sandbox
            .projector
            .preview(Agent::OpenCode, true, Some(&catalog), &options)
            .unwrap();
        let config_before = fs::read(&path).unwrap();
        let record_before = fs::read(sandbox.projector.store_path()).unwrap();
        let token_path = sandbox.projector.tokens.path("opencode");
        let token_before = fs::read(&token_path).unwrap();
        for conflict in [
            "{\"model\":\"other/override\",}",
            "{/* keep */\"provider\":{\"private-ai-proxy\":{\"options\":{\"baseURL\":\"http://127.0.0.1:1/v1\"}}}}",
            "{\"provider\":{\"private-ai-proxy\":{\"options\":{\"apiKey\":\"synthetic-never-log-me\"}}}}",
            "{\"provider\":null}",
            "{/* broken",
        ] {
            write(&jsonc, conflict);
            let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
            let status = statuses.iter().find(|status| status.id == "opencode").unwrap();
            assert!(status.recorded && !status.connected && !status.authorized);
            assert!(tokens.is_empty());
            let attention = status.attention.as_deref().unwrap();
            assert!(attention.contains("opencode.jsonc"), "{attention}");
            assert!(!attention.contains("synthetic-never-log-me"));
            assert!(sandbox.projector
                .preview(Agent::OpenCode, true, Some(&catalog), &options).is_err());
            // Re-read companion files at apply, even when the main revision is unchanged.
            assert!(sandbox.projector.apply(
                Agent::OpenCode, true, &preview.revision, Some(&catalog), &options,
            ).is_err());
            assert_eq!(fs::read(&path).unwrap(), config_before);
            assert_eq!(fs::read(sandbox.projector.store_path()).unwrap(), record_before);
            assert_eq!(fs::read(&token_path).unwrap(), token_before);
            assert_eq!(fs::read_to_string(&jsonc).unwrap(), conflict);
        }
        fs::remove_file(&jsonc).unwrap();
        fs::create_dir(&jsonc).unwrap();
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        let status = statuses
            .iter()
            .find(|status| status.id == "opencode")
            .unwrap();
        assert!(!status.authorized && tokens.is_empty());
        assert!(status.attention.as_deref().unwrap().contains("unreadable"));
        fs::remove_dir(&jsonc).unwrap();
        write(&jsonc, benign);
        assert!(
            sandbox
                .projector
                .scan(None)
                .unwrap()
                .0
                .iter()
                .find(|status| status.id == "opencode")
                .unwrap()
                .authorized
        );
        write(
            &jsonc,
            "{/* keep on disconnect */\"model\":\"other/override\"}",
        );
        let jsonc_before = fs::read(&jsonc).unwrap();
        let mut edited = doc(&sandbox, Agent::OpenCode);
        edited
            .set_str(&["provider", "other", "name"], "Edited outside the app")
            .unwrap();
        write(&path, &edited.render().unwrap());
        disconnect(&sandbox, Agent::OpenCode);
        let mut restored = original;
        restored["provider"]["other"]["name"] = json!("Edited outside the app");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap(),
            restored
        );
        assert_eq!(fs::read(&jsonc).unwrap(), jsonc_before);
        assert!(sandbox.projector.load_store().unwrap().is_empty());
        assert!(!token_path.exists());
    }

    #[test]
    fn opencode_process_overrides_follow_official_merge_order() {
        const CASE: &str = "PAP_TEST_OPENCODE_MERGE_CASE";
        if let Ok(case) = env::var(CASE) {
            let mut sandbox = sandbox("opencode-env-merge");
            sandbox.projector.tool_env = true;
            let global = env_path("XDG_CONFIG_HOME").unwrap().join("opencode");
            write(
                &global.join("opencode.jsonc"),
                "{/* preserved */\"model\":\"other/model\"}",
            );
            let expected = ConfigDoc::Json(json!({
                "model": "private-ai-proxy/test",
                "provider": {"private-ai-proxy": {"name": "Gateway"}}
            }));
            if let Some(dir) = env_path("OPENCODE_CONFIG_DIR") {
                write(&dir.join("opencode.json"), "{\"model\":\"other/dir-json\"}");
                write(
                    &dir.join("opencode.jsonc"),
                    "{\"model\":\"private-ai-proxy/test\",}",
                );
            }
            let result = sandbox.projector.check_opencode_merge(&expected, true);
            assert_eq!(
                result.is_ok(),
                matches!(case.as_str(), "explicit" | "directory"),
                "{case}: {result:?}"
            );
            if case == "global" {
                // A default model is not owned when the user did not select one.
                assert!(sandbox
                    .projector
                    .check_opencode_merge(&expected, false)
                    .is_ok());
            }
            return;
        }
        let root = tempfile::tempdir().unwrap();
        for case in [
            "global",
            "explicit",
            "directory",
            "content",
            "invalid-content",
        ] {
            let dir = root.path().join(case);
            let mut command = Command::new(env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "agents::tests::opencode_process_overrides_follow_official_merge_order",
                ])
                .env(CASE, case)
                .env("XDG_CONFIG_HOME", &dir)
                .env_remove("OPENCODE_CONFIG")
                .env_remove("OPENCODE_CONFIG_DIR")
                .env_remove("OPENCODE_CONFIG_CONTENT");
            if case != "global" {
                // Even pointing back at global JSON makes it higher priority than global JSONC.
                command.env("OPENCODE_CONFIG", dir.join("opencode/opencode.json"));
            }
            if matches!(case, "directory" | "content" | "invalid-content") {
                command.env("OPENCODE_CONFIG_DIR", dir.join("extra"));
            }
            if case == "content" {
                command.env("OPENCODE_CONFIG_CONTENT", "{\"model\":\"other/content\"}");
            } else if case == "invalid-content" {
                command.env("OPENCODE_CONFIG_CONTENT", "{invalid");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn credential_restore_revokes_before_refusing_a_changed_route() {
        let sandbox = sandbox("secret-route-ownership");
        let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
        write(
            &path,
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-test-parked"}}"#,
        );
        connect(&sandbox);
        let projection = fs::read_to_string(&path).unwrap();
        let mut edited = doc(&sandbox, Agent::ClaudeCode);
        edited
            .set_str(&["env", "ANTHROPIC_BASE_URL"], "http://127.0.0.1:1")
            .unwrap();
        write(&path, &edited.render().unwrap());
        let before = fs::read(&path).unwrap();
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, false, None, &options)
            .unwrap();
        assert!(preview.changes.is_empty());
        assert!(preview.note.contains("ambiguous"));
        assert!(!serde_json::to_string(&preview)
            .unwrap()
            .contains("sk-test-parked"));
        assert!(sandbox
            .projector
            .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
            .is_err());
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
        assert!(sandbox.secrets.holds("sk-test-parked"));
        assert!(sandbox
            .projector
            .load_store()
            .unwrap()
            .contains_key("claude-code"));
        assert_eq!(fs::read(&path).unwrap(), before);
        write(&path, &projection);
        disconnect(&sandbox, Agent::ClaudeCode);
        assert_eq!(
            doc(&sandbox, Agent::ClaudeCode)
                .get_str(&["env", "ANTHROPIC_AUTH_TOKEN"])
                .as_deref(),
            Some("sk-test-parked")
        );
    }

    #[test]
    fn recorded_paths_restore_original_files_after_location_changes() {
        for agent in [Agent::ClaudeCode, Agent::OpenCode] {
            let mut sandbox = sandbox(&format!("recorded-path-{}", agent.id()));
            let catalog = catalog();
            let options = claude_options();
            let path = agent.config_path(&sandbox.home, false);
            let original = if agent == Agent::ClaudeCode {
                r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-test-original"},"model":"user-model"}"#
            } else {
                r#"{"model":"other/user-model","provider":{"other":{"name":"User provider"}}}"#
            };
            write(&path, original);
            let preview = sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .unwrap();
            sandbox
                .projector
                .apply(agent, true, &preview.revision, Some(&catalog), &options)
                .unwrap();
            assert_eq!(
                sandbox.projector.load_store().unwrap()[agent.id()]
                    .config_path
                    .as_deref(),
                Some(path.as_path())
            );
            let projected = fs::read_to_string(&path).unwrap();
            sandbox.projector.home = sandbox.home.join("different-profile");
            let foreign = agent.config_path(&sandbox.projector.home, false);
            write(&foreign, &projected);
            let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
            let status = statuses
                .iter()
                .find(|status| status.id == agent.id())
                .unwrap();
            assert!(
                status.recorded && !status.connected && !status.authorized && tokens.is_empty()
            );
            assert!(status
                .attention
                .as_deref()
                .unwrap()
                .contains("location changed"));
            assert!(sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .is_err());
            fs::remove_file(&sandbox.projector.helper_exe).unwrap();
            disconnect(&sandbox, agent);
            assert_eq!(fs::read_to_string(foreign).unwrap(), projected);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&fs::read_to_string(path).unwrap())
                    .unwrap(),
                serde_json::from_str::<serde_json::Value>(original).unwrap()
            );
        }
    }

    #[test]
    fn legacy_recovery_never_guesses_paths_or_displays_structured_secrets() {
        for structured in [false, true] {
            let sandbox = sandbox(if structured {
                "legacy-structured"
            } else {
                "legacy-path"
            });
            connect(&sandbox);
            let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
            let before = fs::read(&path).unwrap();
            let mut store = sandbox.projector.load_store().unwrap();
            let record = store.get_mut("claude-code").unwrap();
            record.config_path = None;
            if structured {
                record.fields[0].previous = Some(Previous::Plain(ConfigValue::Json(
                    json!({"apiKey":"sk-test-hidden"}),
                )));
            }
            write(
                &sandbox.projector.store_path(),
                &serde_json::to_string(&store).unwrap(),
            );
            let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
            assert!(!statuses[1].authorized && tokens.is_empty());
            let options = ConnectOptions::default();
            let preview = sandbox
                .projector
                .preview(Agent::ClaudeCode, false, None, &options)
                .unwrap();
            assert!(preview.changes.is_empty());
            assert!(!serde_json::to_string(&preview)
                .unwrap()
                .contains("sk-test-hidden"));
            assert!(sandbox
                .projector
                .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
                .is_err());
            assert!(sandbox
                .projector
                .tokens
                .read("claude-code")
                .unwrap()
                .is_none());
            assert!(sandbox
                .projector
                .load_store()
                .unwrap()
                .contains_key("claude-code"));
            assert_eq!(fs::read(path).unwrap(), before);
        }
    }

    #[test]
    fn unmanaged_provider_objects_are_not_captured_as_plain_backups() {
        for agent in [Agent::OpenCode, Agent::Pi] {
            for value in [
                json!(null),
                json!({"apiKey":"sk-test-hidden","options":{"headers":{"Authorization":"sk-test-hidden"}}}),
            ] {
                let sandbox = sandbox(&format!("namespace-{}", agent.id()));
                let path = agent.config_path(&sandbox.home, false);
                let key = if agent == Agent::OpenCode {
                    "provider"
                } else {
                    "providers"
                };
                let text = json!({key:{"private-ai-proxy":value}}).to_string();
                write(&path, &text);
                let error = sandbox
                    .projector
                    .preview(agent, true, Some(&catalog()), &claude_options())
                    .unwrap_err();
                assert!(!error.contains("sk-test-hidden"));
                assert_eq!(fs::read_to_string(&path).unwrap(), text);
                assert!(!sandbox.projector.store_path().exists());
                assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
            }
        }
    }

    #[test]
    fn native_auth_and_routing_conflicts_are_read_only_and_deauthorize() {
        for (agent, case) in [
            (Agent::Codex, "aws"),
            (Agent::Pi, "stored-key"),
            (Agent::OpenCode, "disabled"),
            (Agent::OpenCode, "allowlist"),
            (Agent::Hermes, "api_mode"),
            (Agent::Hermes, "disabled"),
            (Agent::Hermes, "explicit-key"),
            (Agent::Hermes, "pool"),
            (Agent::Hermes, "fallback"),
        ] {
            let sandbox = sandbox(&format!("conflict-{}-{case}", agent.id()));
            let catalog = catalog();
            let options = claude_options();
            let preview = sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .unwrap();
            sandbox
                .projector
                .apply(agent, true, &preview.revision, Some(&catalog), &options)
                .unwrap();
            let preview = sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .unwrap();
            let path = agent.config_path(&sandbox.home, false);
            let auth_path = path.with_file_name("auth.json");
            let mut edited = doc(&sandbox, agent);
            match (agent, case) {
                (Agent::Codex, _) => edited
                    .set_str(
                        &["model_providers", "private_ai_proxy", "aws", "region"],
                        "test-region",
                    )
                    .unwrap(),
                (Agent::Pi, _) => write(
                    &auth_path,
                    r#"{"private-ai-proxy":{"type":"api_key","key":"sk-test-hidden"}}"#,
                ),
                (Agent::OpenCode, "disabled") => edited
                    .set_value(
                        &["disabled_providers"],
                        &ConfigValue::List(vec!["private-ai-proxy".into()]),
                    )
                    .unwrap(),
                (Agent::OpenCode, _) => edited
                    .set_value(
                        &["enabled_providers"],
                        &ConfigValue::List(vec!["other".into()]),
                    )
                    .unwrap(),
                (Agent::Hermes, "api_mode") => edited
                    .set_str(
                        &["providers", "private-ai-proxy", "api_mode"],
                        "codex_responses",
                    )
                    .unwrap(),
                (Agent::Hermes, "disabled") => edited
                    .set_value(
                        &["providers", "private-ai-proxy", "enabled"],
                        &ConfigValue::Bool(false),
                    )
                    .unwrap(),
                (Agent::Hermes, "explicit-key") => edited
                    .set_str(&["model", "api_key"], "sk-test-hidden")
                    .unwrap(),
                (Agent::Hermes, "pool") => write(
                    &auth_path,
                    r#"{"credential_pool":{"private-ai-proxy":[{"access_token":"sk-test-hidden"}]}}"#,
                ),
                (Agent::Hermes, _) => edited
                    .set_str(&["fallback_model", "provider"], "other")
                    .unwrap(),
                _ => unreachable!(),
            }
            write(&path, &edited.render().unwrap());
            let config_before = fs::read(&path).unwrap();
            let auth_before = fs::read(&auth_path).ok();
            let record_before = fs::read(sandbox.projector.store_path()).unwrap();
            let token_path = sandbox.projector.tokens.path(agent.id());
            let token_before = fs::read(&token_path).unwrap();
            let (statuses, tokens) = sandbox.projector.scan(Some(&catalog)).unwrap();
            let status = statuses
                .iter()
                .find(|status| status.id == agent.id())
                .unwrap();
            assert!(
                status.recorded && !status.authorized && !status.connected && tokens.is_empty(),
                "{agent:?}/{case}"
            );
            assert!(!status
                .attention
                .as_deref()
                .unwrap()
                .contains("sk-test-hidden"));
            assert!(sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .is_err());
            assert!(sandbox
                .projector
                .apply(agent, true, &preview.revision, Some(&catalog), &options)
                .is_err());
            assert_eq!(fs::read(&path).unwrap(), config_before);
            assert_eq!(fs::read(&auth_path).ok(), auth_before);
            assert_eq!(
                fs::read(sandbox.projector.store_path()).unwrap(),
                record_before
            );
            assert_eq!(fs::read(&token_path).unwrap(), token_before);
            disconnect(&sandbox, agent);
            assert_eq!(fs::read(&auth_path).ok(), auth_before);
        }
    }

    #[test]
    fn opencode_limits_and_hermes_defaults_do_not_invent_metadata() {
        let catalog = Catalog::from_remote(
            &json!({"data":[
                {"id":"context-only","context_length":123},
                {"id":"output-only","max_output_length":45},
                {"id":"complete","context_length":123,"max_output_length":45}
            ]}),
            1,
        )
        .unwrap();
        let provider = opencode_provider(&catalog, ENDPOINT, Path::new("token"));
        assert!(provider["models"]["context-only"].get("limit").is_none());
        assert!(provider["models"]["output-only"].get("limit").is_none());
        assert_eq!(
            provider["models"]["complete"]["limit"],
            json!({"context":123,"output":45})
        );
        let sandbox = sandbox("hermes-default-conflict");
        let path = Agent::Hermes.config_path(&sandbox.home, false);
        write(&path, "model:\n  default: unrelated-model\n");
        assert!(sandbox
            .projector
            .preview(
                Agent::Hermes,
                true,
                Some(&catalog),
                &ConnectOptions::default()
            )
            .is_err());
        assert!(sandbox
            .projector
            .preview(
                Agent::Hermes,
                true,
                Some(&catalog),
                &ConnectOptions {
                    default_model: Some("complete".into())
                }
            )
            .is_ok());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "model:\n  default: unrelated-model\n"
        );
    }

    #[test]
    fn codex_and_opencode_use_official_custom_provider_configs() {
        let sandbox = sandbox("providers");
        let catalog = catalog();
        let options = claude_options();
        let path = Agent::Codex.config_path(&sandbox.home, false);
        write(
            &path,
            "[model_providers.private_ai_proxy]\n\
                      env_key = 'OLD_KEY'\n\
                      experimental_bearer_token = 'old-synthetic-token'\n\
                      requires_openai_auth = true\n",
        );

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
        for key in [
            "env_key",
            "experimental_bearer_token",
            "requires_openai_auth",
        ] {
            assert_eq!(
                codex.get_value(&["model_providers", "private_ai_proxy", key]),
                None,
            );
        }
        assert!(!fs::read_to_string(sandbox.projector.store_path())
            .unwrap()
            .contains("old-synthetic-token"));
        assert_eq!(
            codex.get_str(&["model_provider"]).as_deref(),
            Some("private_ai_proxy")
        );
        assert_eq!(
            codex
                .get_str(&["model_providers", "private_ai_proxy", "wire_api"])
                .as_deref(),
            Some("responses")
        );
        assert_eq!(
            codex
                .get_str(&["model_providers", "private_ai_proxy", "base_url"])
                .as_deref(),
            Some("http://127.0.0.1:4180/v1")
        );
        assert_eq!(
            codex.get_str(&["model_catalog_json"]).as_deref(),
            Some(
                sandbox
                    .projector
                    .codex_catalog_path()
                    .to_string_lossy()
                    .as_ref()
            )
        );
        let generated: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(sandbox.projector.codex_catalog_path()).unwrap(),
        )
        .unwrap();
        assert_eq!(generated["models"][0]["slug"], "openai/gpt-oss-20b");
        assert_eq!(generated["models"][0]["context_window"], 131072);
        assert_eq!(
            generated["models"][1]["context_window"],
            serde_json::Value::Null
        );
        assert_eq!(
            generated["models"][0]["model_messages"]["instructions_template"],
            "Complete official Codex instructions"
        );
        assert_eq!(
            generated["models"][0]["base_instructions"],
            "Complete official Codex instructions"
        );
        assert_eq!(generated["models"][0]["support_verbosity"], false);
        assert_eq!(
            generated["models"][0]["default_verbosity"],
            serde_json::Value::Null
        );
        assert_eq!(generated["models"][0]["tool_mode"], serde_json::Value::Null);
        assert_eq!(
            generated["models"][0]["apply_patch_tool_type"],
            serde_json::Value::Null
        );
        assert_eq!(
            generated["models"][0]["multi_agent_version"],
            serde_json::Value::Null
        );
        assert_eq!(generated["models"][0]["web_search_tool_type"], "text");
        assert_eq!(generated["models"][0]["shell_type"], "unified_exec");
        disconnect(&sandbox, Agent::Codex);
        let restored = doc(&sandbox, Agent::Codex);
        for (key, value) in [
            ("env_key", ConfigValue::Str("OLD_KEY".into())),
            (
                "experimental_bearer_token",
                ConfigValue::Str("old-synthetic-token".into()),
            ),
            ("requires_openai_auth", ConfigValue::Bool(true)),
        ] {
            assert_eq!(
                restored.get_value(&["model_providers", "private_ai_proxy", key]),
                Some(value),
            );
        }

        let preview = sandbox
            .projector
            .preview(Agent::OpenCode, true, Some(&catalog), &options)
            .unwrap();
        assert!(preview.changes.iter().any(|change| {
            change.key == "provider.private-ai-proxy"
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
                .get_str(&["provider", "private-ai-proxy", "npm"])
                .as_deref(),
            Some("@ai-sdk/openai-compatible")
        );
        assert_eq!(
            opencode
                .get_str(&["provider", "private-ai-proxy", "options", "baseURL"])
                .as_deref(),
            Some("http://127.0.0.1:4180/v1")
        );
        disconnect(&sandbox, Agent::OpenCode);
    }

    #[test]
    fn inventory_refresh_updates_active_catalog_without_rotating_credentials() {
        let sandbox = sandbox("inventory-refresh");
        let agent = Agent::Pi;
        let path = agent.config_path(&sandbox.home, false);
        write(&path, r#"{"custom":true}"#);
        write(
            &sandbox
                .home
                .join(".local/bin")
                .join(if cfg!(windows) { "pi.exe" } else { "pi" }),
            "test cli",
        );
        let mut catalog = catalog();
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .unwrap();
        sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        let token = sandbox.projector.tokens.read(agent.id()).unwrap();
        catalog
            .apply_endpoint_inventory(
                "https://tee.redpill.ai",
                &crate::catalog::EndpointInventory::bundled().unwrap(),
            )
            .unwrap();
        assert!(sandbox
            .projector
            .reconcile(Some(&catalog))
            .unwrap()
            .is_empty());
        assert_eq!(sandbox.projector.tokens.read(agent.id()).unwrap(), token);
        let ConfigValue::Json(provider) = doc(&sandbox, agent)
            .get_value(&["providers", "private-ai-proxy"])
            .unwrap()
        else {
            panic!("missing Pi catalog");
        };
        assert_eq!(provider["models"].as_array().unwrap().len(), 1);
        let record = fs::read(sandbox.projector.store_path()).unwrap();
        assert!(sandbox
            .projector
            .reconcile(Some(&catalog))
            .unwrap()
            .is_empty());
        assert_eq!(fs::read(sandbox.projector.store_path()).unwrap(), record);
        disconnect(&sandbox, agent);
        assert!(doc(&sandbox, agent)
            .get_value(&["providers", "private-ai-proxy"])
            .is_none());
        assert_eq!(
            doc(&sandbox, agent).get_value(&["custom"]),
            Some(ConfigValue::Bool(true))
        );
    }

    #[test]
    fn projections_and_codex_defaults_use_the_same_endpoint_filter() {
        let sandbox = sandbox("endpoint-filter");
        let mut catalog = catalog();
        catalog.models[0].supported_surfaces = Some(vec![Surface::ChatCompletions]);
        catalog.models[1].supported_surfaces = Some(vec![Surface::Responses]);
        let options = ConnectOptions::default();
        for agent in [Agent::Codex, Agent::Pi] {
            let preview = sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .unwrap();
            sandbox
                .projector
                .apply(agent, true, &preview.revision, Some(&catalog), &options)
                .unwrap();
        }
        assert_eq!(
            doc(&sandbox, Agent::Codex).get_str(&["model"]).as_deref(),
            Some("phala/qwen")
        );
        let bundled: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(sandbox.projector.codex_catalog_path()).unwrap(),
        )
        .unwrap();
        assert_eq!(bundled["models"].as_array().unwrap().len(), 1);
        assert_eq!(bundled["models"][0]["slug"], "phala/qwen");
        let ConfigValue::Json(provider) = doc(&sandbox, Agent::Pi)
            .get_value(&["providers", "private-ai-proxy"])
            .unwrap()
        else {
            panic!("missing Pi provider");
        };
        assert_eq!(provider["models"].as_array().unwrap().len(), 1);
        assert_eq!(provider["models"][0]["id"], "openai/gpt-oss-20b");
        assert!(sandbox
            .projector
            .preview(Agent::ClaudeCode, true, Some(&catalog), &options)
            .unwrap_err()
            .contains("No models with confirmed"));
        assert!(sandbox
            .projector
            .preview(Agent::Codex, true, Some(&catalog), &claude_options())
            .is_err());

        // Losing support must not silently replace the saved Codex selection.
        catalog.models[0].supported_surfaces = Some(vec![Surface::Responses]);
        catalog.models[1].supported_surfaces = Some(vec![]);
        assert!(sandbox
            .projector
            .preview(Agent::Codex, true, Some(&catalog), &options)
            .is_err());
    }

    #[test]
    fn pi_and_hermes_use_verified_model_discovery() {
        let sandbox = sandbox("discovery-providers");
        let catalog = Catalog::from_remote(
            &json!({"data": [
                {"id": "openai/gpt-oss-20b", "input_modalities": ["text", "image", "audio"],
                 "pricing": {"prompt": "0.000001"}},
                {"id": "phala/qwen"}
            ]}),
            1,
        )
        .unwrap();
        let options = ConnectOptions::default();

        let preview = sandbox
            .projector
            .preview(Agent::Pi, true, Some(&catalog), &options)
            .unwrap();
        assert!(preview.changes.iter().any(|change| {
            change.key == "providers.private-ai-proxy"
                && change.after.as_deref() == Some("Generated catalog (2 models)")
        }));
        sandbox
            .projector
            .apply(Agent::Pi, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        let pi = doc(&sandbox, Agent::Pi);
        let provider = pi.get_value(&["providers", "private-ai-proxy"]).unwrap();
        let ConfigValue::Json(provider) = provider else {
            panic!("Pi provider must be a generated JSON catalog");
        };
        assert_eq!(provider["api"], "openai-completions");
        assert_eq!(provider["models"].as_array().unwrap().len(), 2);
        assert_eq!(provider["models"][0]["id"], "openai/gpt-oss-20b");
        assert_eq!(provider["models"][0]["input"], json!(["text", "image"]));
        assert_eq!(
            provider["models"][0]["cost"],
            json!({"input": 1.0, "output": 0, "cacheRead": 0, "cacheWrite": 0}),
        );
        assert!(provider["models"][1].get("cost").is_none());
        assert_eq!(
            provider["apiKey"],
            format!(
                "!{}",
                credential_helper_command(&sandbox.projector.helper_exe, Agent::Pi).unwrap()
            )
        );
        #[cfg(windows)]
        {
            let pi_token = sandbox.projector.tokens.read("pi").unwrap().unwrap();
            let config_path = Agent::Pi.config_path(&sandbox.home, sandbox.projector.tool_env);
            let mut legacy = provider.clone();
            legacy["apiKey"] = json!(format!(
                "!{}",
                helper_command(&sandbox.projector.helper_exe, "pi").unwrap()
            ));
            let value = ConfigValue::Json(legacy);
            let mut config = doc(&sandbox, Agent::Pi);
            config
                .set_value(&["providers", "private-ai-proxy"], &value)
                .unwrap();
            write(&config_path, &config.render().unwrap());
            let mut store = sandbox.projector.load_store().unwrap();
            store
                .get_mut("pi")
                .unwrap()
                .fields
                .iter_mut()
                .find(|field| field.path == owned(&["providers", "private-ai-proxy"]))
                .unwrap()
                .value = Some(value);
            sandbox.projector.save_store(&store).unwrap();
            let config_before = fs::read(&config_path).unwrap();
            let record_before = fs::read(sandbox.projector.store_path()).unwrap();
            let token_before = fs::read(sandbox.projector.tokens.path("pi")).unwrap();
            let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
            let pi = statuses.iter().find(|status| status.id == "pi").unwrap();
            assert!(pi.recorded && !pi.connected && !pi.authorized);
            assert!(pi
                .attention
                .as_deref()
                .unwrap()
                .contains("Disconnect, then Connect"));
            assert_eq!(tokens.agent_for(&pi_token), None);
            assert_eq!(fs::read(&config_path).unwrap(), config_before);
            assert_eq!(
                fs::read(sandbox.projector.store_path()).unwrap(),
                record_before
            );
            assert_eq!(
                fs::read(sandbox.projector.tokens.path("pi")).unwrap(),
                token_before
            );

            config
                .set_str(
                    &["providers", "private-ai-proxy", "apiKey"],
                    "!user-command",
                )
                .unwrap();
            write(&config_path, &config.render().unwrap());
            let edited = fs::read(&config_path).unwrap();
            let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
            let pi = statuses.iter().find(|status| status.id == "pi").unwrap();
            assert!(pi.recorded && !pi.connected && !pi.authorized);
            assert!(pi.attention.is_some());
            assert!(matches!(
                pi.repair_action,
                Some(AgentRepairAction::Reconnect)
            ));
            assert_eq!(tokens.agent_for(&pi_token), None);
            assert_eq!(fs::read(&config_path).unwrap(), edited);
            assert_eq!(
                fs::read(sandbox.projector.store_path()).unwrap(),
                record_before
            );
            assert_eq!(
                fs::read(sandbox.projector.tokens.path("pi")).unwrap(),
                token_before
            );
        }
        disconnect(&sandbox, Agent::Pi);

        let options = claude_options();
        let path = Agent::Hermes.config_path(&sandbox.home, sandbox.projector.tool_env);
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
            hermes.get_value(&["providers", "private-ai-proxy", "discover_models"]),
            Some(ConfigValue::Bool(true))
        );
        assert_eq!(
            hermes.get_str(&["model", "provider"]).as_deref(),
            Some("custom:private-ai-proxy")
        );
        disconnect(&sandbox, Agent::Hermes);
        let restored = fs::read_to_string(path).unwrap();
        assert!(restored.contains("# keep this comment"));
        assert!(restored.contains("theme: dark"));
        assert!(!restored.contains("private-ai-proxy"));

        let fresh = self::sandbox("fresh-hermes");
        assert!(fresh
            .projector
            .preview(
                Agent::Hermes,
                true,
                Some(&catalog),
                &ConnectOptions::default()
            )
            .is_err());
        let preview = fresh
            .projector
            .preview(Agent::Hermes, true, Some(&catalog), &options)
            .unwrap();
        fresh
            .projector
            .apply(
                Agent::Hermes,
                true,
                &preview.revision,
                Some(&catalog),
                &options,
            )
            .unwrap();
        let hermes = doc(&fresh, Agent::Hermes);
        assert_eq!(
            hermes
                .get_str(&["providers", "private-ai-proxy", "transport"])
                .as_deref(),
            Some("chat_completions")
        );
        assert_eq!(
            hermes.get_str(&["providers", "private-ai-proxy", "key_cmd"]),
            Some(credential_helper_command(&fresh.projector.helper_exe, Agent::Hermes).unwrap())
        );
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

        let executable =
            sandbox
                .home
                .join(".local/bin")
                .join(if cfg!(windows) { "pi.exe" } else { "pi" });
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
            .contains("not available"));

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
        let error = sandbox
            .projector
            .apply(
                Agent::ClaudeCode,
                false,
                &preview.revision,
                Some(&catalog),
                &claude_options(),
            )
            .unwrap_err();
        assert!(error.contains("changed since the preview"));
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "opus"}"#);
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
        write(&codex, "model_provider = \"private_ai_proxy\"\n");
        sandbox.projector.tokens.ensure("codex").unwrap();
        let mut store = sandbox.projector.load_store().unwrap();
        store.insert(
            "codex".into(),
            Connection {
                config_path: Some(codex.clone()),
                fields: vec![OwnedField {
                    path: owned(&["model_provider"]),
                    value: Some(ConfigValue::Str("private_ai_proxy".into())),
                    previous: None,
                }],
                disabled: true,
                cleanup_pending: false,
                ..Connection::default()
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
        let dir = env::temp_dir().join(format!("pap-atomic-{}", std::process::id()));
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
            .contains("settings changed"));

        // Corrupt the file entirely: still recorded, error reported, token
        // still unauthorized, and Disconnect retains its recovery journal.
        write(&path, "{ not json");
        assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
        let status = &sandbox.projector.scan(None).unwrap().0[1];
        assert!(status.recorded && !status.authorized);
        assert!(status.error.is_some());
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, false, None, &ConnectOptions::default())
            .unwrap();
        assert!(sandbox
            .projector
            .apply(
                Agent::ClaudeCode,
                false,
                &preview.revision,
                None,
                &ConnectOptions::default()
            )
            .is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        assert!(sandbox.projector.load_store().unwrap()["claude-code"].cleanup_pending);
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
        assert_eq!(failures.len(), 1);
        assert!(sandbox.projector.load_store().unwrap()["claude-code"].cleanup_pending);
        assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
    }

    /// POSIX quoting round-trips through shlex and, where available, sh.
    /// This does not prove which shell the Windows Claude CLI selects.
    #[test]
    fn helper_command_quotes_hostile_paths_for_the_shell() {
        for hostile in [
            "/Applications/Private AI Proxy.app/Contents/MacOS/helper",
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
        #[cfg(windows)]
        {
            use base64::{engine::general_purpose::STANDARD, Engine};

            // Pi's Bash and cmd.exe fallback see only fixed switches and a
            // base64 word. Neither shell interprets the Windows helper path.
            let hostile = Path::new(r"C:\Users\O'Brien %USERPROFILE% ! &\helper.exe");
            let provider = pi_provider(&catalog(), ENDPOINT, hostile).unwrap();
            let command = provider["apiKey"]
                .as_str()
                .unwrap()
                .strip_prefix('!')
                .unwrap();
            let words: Vec<_> = command.split_ascii_whitespace().collect();
            assert_eq!(words.len(), 5);
            assert_eq!(
                &words[..4],
                &[
                    "powershell.exe",
                    "-NoProfile",
                    "-NonInteractive",
                    "-EncodedCommand"
                ]
            );
            assert_eq!(shlex::split(command).unwrap(), words);
            let bytes = STANDARD.decode(words[4]).unwrap();
            let script = String::from_utf16(
                &bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            let encoded_path = STANDARD.encode(
                hostile
                    .to_str()
                    .unwrap()
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            assert!(script.contains(&format!("FromBase64String('{encoded_path}')")));
            assert!(script.ends_with("--agent-token pi; exit $LASTEXITCODE"));
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
            let data_dir = sandbox.projector.data_dir.clone();
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
        let state = ProxyState::new(sender).unwrap();
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
