//! Crush merges `/etc/crush/crush.json`, the user's `crush.json` and `crushrc`,
//! then its data file (`~/.local/share/crush/crush.json`), where it saves the
//! models picked in its UI; project files come last. The provider and the
//! default model go into that data file, so no lower user layer can override
//! them. Crush expands `$(command)` in `api_key`, which runs the credential
//! command.
use super::*;

const PROVIDER: &str = "private-ai-proxy";

pub(super) fn config_path(home: &Path, tool_env: bool) -> PathBuf {
    let var = |name: &str| tool_env.then(|| env_path(name)).flatten();
    if let Some(dir) = var("CRUSH_GLOBAL_DATA") {
        return dir.join("crush.json");
    }
    let base = var("XDG_DATA_HOME").unwrap_or_else(|| {
        if cfg!(windows) {
            var("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"))
        } else {
            home.join(".local").join("share")
        }
    });
    base.join("crush").join("crush.json")
}

pub(super) fn fields(
    inputs: &Inputs<'_>,
    base: &str,
    default_model: Option<&str>,
) -> Result<Vec<Field>, AgentError> {
    let catalog = inputs.catalog.ok_or(AgentError::InvalidState)?;
    let command = inputs
        .credential_command(Agent::Crush)
        .map_err(AgentError::ConfigurationConflict)?;
    let models: Vec<_> = catalog
        .models
        .iter()
        .map(|model| {
            let mut entry = serde_json::json!({
                "id": model.id(),
                "name": model.display_name(),
                "can_reason": model
                    .string_array("supported_features")
                    .iter()
                    .any(|feature| feature == "reasoning"),
                "supports_attachments": model
                    .string_array("input_modalities")
                    .iter()
                    .any(|mode| mode == "image"),
            });
            if let Some(context) = model.remote.context_length {
                entry["context_window"] = context.into();
            }
            if let Some(output) = model.remote.max_output_length {
                entry["default_max_tokens"] = output.into();
            }
            for (source, target) in [
                ("prompt", "cost_per_1m_in"),
                ("completion", "cost_per_1m_out"),
                ("input_cache_read", "cost_per_1m_in_cached"),
                ("input_cache_write", "cost_per_1m_out_cached"),
            ] {
                if let Some(price) = model
                    .price_per_million(source)
                    .and_then(serde_json::Number::from_f64)
                {
                    entry[target] = serde_json::Value::Number(price);
                }
            }
            entry
        })
        .collect();
    let mut fields = vec![generated_catalog(
        &["providers", PROVIDER],
        serde_json::json!({
            "name": PRODUCT_NAME,
            "type": "openai-compat",
            "base_url": format!("{base}/v1"),
            "api_key": format!("$({command})"),
            "discover_models": false,
            "models": models,
        }),
        catalog.models.len(),
    )];
    if let Some(model) = default_model {
        fields.push(set(&["models", "large", "provider"], PROVIDER));
        fields.push(set(&["models", "large", "model"], model));
    }
    Ok(fields)
}

/// Lower layers deep-merge beneath the data file, so any definition of the
/// provider there would add to or disable it. `crushrc` is a shell script
/// that is never run here; it is only searched for the provider id. Project
/// files need the CLI's working directory and are not inspected.
pub(super) fn validate(home: &Path, tool_env: bool) -> Result<(), AgentError> {
    let var = |name: &str| tool_env.then(|| env_path(name)).flatten();
    let user = var("CRUSH_GLOBAL_CONFIG").unwrap_or_else(|| {
        var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home.join(".config"))
            .join("crush")
    });
    let mut layers = vec![user.join("crush.json")];
    if !cfg!(windows) {
        layers.insert(0, PathBuf::from("/etc/crush/crush.json"));
    }
    for path in &layers {
        let Some(text) = read_layer(path)? else {
            continue;
        };
        let layer = parse_jsonc(&text).map_err(|reason| {
            AgentError::InvalidConfiguration(format!(
                "Cannot inspect the Crush config at {}; {reason}",
                path.display()
            ))
        })?;
        if layer.pointer("/providers/private-ai-proxy").is_some() {
            return Err(conflict(path));
        }
    }
    let script = user.join("crushrc");
    if read_layer(&script)?.is_some_and(|text| text.contains(PROVIDER)) {
        return Err(conflict(&script));
    }
    Ok(())
}

fn read_layer(path: &Path) -> Result<Option<String>, AgentError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        // The Mac App Store build cannot read outside its Home grant.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(_) => Err(AgentError::ConfigurationRead),
    }
}

fn conflict(path: &Path) -> AgentError {
    AgentError::ConfigurationConflict(format!(
        "{} defines the private-ai-proxy provider, which Crush merges into the gateway provider; remove it there",
        path.display()
    ))
}

pub(super) fn selected_model(doc: &ConfigDoc) -> Option<String> {
    (doc.get_str(&["models", "large", "provider"]).as_deref() == Some(PROVIDER))
        .then(|| doc.get_str(&["models", "large", "model"]))
        .flatten()
}
