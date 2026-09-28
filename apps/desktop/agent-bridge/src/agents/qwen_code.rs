//! Qwen Code reads model providers from `modelProviders` in its user
//! `settings.json`, each model naming the environment variable that holds its
//! key. It resolves that variable from the process environment, then `.env`
//! files, then the `env` block of `settings.json`, so the local token is
//! written to that block. A custom provider id routes through `providerProtocol`;
//! the default model is `model.name` under the OpenAI auth type.
use super::*;

const PROVIDER: &str = "private-ai-proxy";
pub(super) const KEY_ENV: &str = "PRIVATE_AI_PROXY_API_KEY";

pub(super) fn fields(
    inputs: &Inputs<'_>,
    base: &str,
    default_model: Option<&str>,
) -> Result<Vec<Field>, AgentError> {
    let catalog = inputs.catalog.ok_or(AgentError::InvalidState)?;
    let models = catalog
        .models
        .iter()
        .map(|model| {
            let mut generation = serde_json::Map::new();
            if let Some(context) = model.remote.context_length {
                generation.insert("contextWindowSize".into(), context.into());
            }
            if model
                .string_array("input_modalities")
                .iter()
                .any(|mode| mode == "image")
            {
                generation.insert("modalities".into(), serde_json::json!({ "image": true }));
            }
            let mut entry = serde_json::json!({
                "id": model.id(),
                "name": model.display_name(),
                "envKey": KEY_ENV,
                "baseUrl": format!("{base}/v1"),
            });
            if !generation.is_empty() {
                entry["generationConfig"] = serde_json::Value::Object(generation);
            }
            entry
        })
        .collect();
    let mut fields = vec![
        generated_catalog(
            &["modelProviders", PROVIDER],
            serde_json::Value::Array(models),
            catalog.models.len(),
        ),
        set(&["providerProtocol", PROVIDER], "openai"),
        set(&["env", KEY_ENV], inputs.token()?),
    ];
    if let Some(model) = default_model {
        fields.push(set(&["security", "auth", "selectedType"], "openai"));
        fields.push(set(&["model", "name"], model));
    }
    Ok(fields)
}

/// System settings override the user file, and `modelProviders` and
/// `providerProtocol` replace rather than merge; a process or user `.env`
/// value of the key variable wins over the `env` block. Workspace settings
/// need the CLI's working directory and are not inspected.
pub(super) fn validate(home: &Path, tool_env: bool, owns_model: bool) -> Result<(), AgentError> {
    let system = tool_env
        .then(|| env_path("QWEN_CODE_SYSTEM_SETTINGS_PATH"))
        .flatten()
        .unwrap_or_else(system_settings);
    let text = match fs::read_to_string(&system) {
        Ok(text) => Some(text),
        // Outside the Home grant the Mac App Store build cannot read it.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            None
        }
        Err(_) => return Err(AgentError::ConfigurationRead),
    };
    if let Some(text) = text {
        let layer = parse_jsonc(&text).map_err(|reason| {
            AgentError::InvalidConfiguration(format!(
                "Cannot inspect Qwen Code system settings at {}; {reason}",
                system.display()
            ))
        })?;
        let key = format!("/env/{KEY_ENV}");
        let mut owned = vec!["/modelProviders", "/providerProtocol", key.as_str()];
        if owns_model {
            owned.extend([
                "/model/name",
                "/security/auth/selectedType",
                "/security/auth/enforcedType",
            ]);
        }
        if let Some(pointer) = owned
            .iter()
            .find(|pointer| layer.pointer(pointer).is_some())
        {
            return Err(AgentError::ConfigurationConflict(format!(
                "Qwen Code system settings at {} set {pointer}, which overrides the gateway provider; resolve it there",
                system.display()
            )));
        }
    }
    let qwen_home = Agent::QwenCode
        .config_path(home, tool_env)
        .parent()
        .map(Path::to_path_buf)
        .ok_or(AgentError::InvalidState)?;
    let shadowed = (tool_env && env::var_os(KEY_ENV).is_some())
        || [qwen_home.join(".env"), home.join(".env")]
            .iter()
            .any(|path| {
                fs::read_to_string(path).is_ok_and(|text| {
                    text.lines().any(|line| {
                        let line = line.trim_start();
                        let line = line.strip_prefix("export ").unwrap_or(line);
                        line.split_once('=')
                            .is_some_and(|(key, _)| key.trim() == KEY_ENV)
                    })
                })
            });
    if shadowed {
        return Err(AgentError::AuthenticationConflict(format!(
            "{KEY_ENV} is set in the environment or a user .env file, which Qwen Code reads before its settings; remove it there"
        )));
    }
    Ok(())
}

fn system_settings() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\ProgramData\qwen-code\settings.json")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/QwenCode/settings.json")
    } else {
        PathBuf::from("/etc/qwen-code/settings.json")
    }
}
