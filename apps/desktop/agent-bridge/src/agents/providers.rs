use super::*;

/// Everything a projection is computed from.
pub(super) struct Inputs<'a> {
    pub(super) file_credentials: bool,
    pub(super) endpoint: &'a str,
    pub(super) helper_exe: &'a Path,
    pub(super) token_path: &'a Path,
    pub(super) codex_catalog_path: &'a Path,
    pub(super) catalog: Option<&'a Catalog>,
    pub(super) options: &'a ConnectOptions,
}

impl Inputs<'_> {
    pub(super) fn credential_command(&self, agent: Agent) -> Result<String, String> {
        agent_credential_command(
            self.helper_exe,
            agent,
            self.file_credentials.then_some(self.token_path),
        )
    }
}

/// The fields this app owns for the agent and the values a connection writes.
pub(super) fn fields(agent: Agent, inputs: &Inputs<'_>) -> Result<Vec<Field>, AgentError> {
    let catalog = inputs.catalog.ok_or(AgentError::InvalidState)?;
    let catalog = agent.catalog(catalog);
    if catalog.models.is_empty() {
        return Err(AgentError::NoCompatibleModels);
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
    if inputs.options.default_model.is_some() && default_model.is_none() {
        return Err(AgentError::IncompatibleModel);
    }
    if default_model.is_some_and(|model| catalog.get(model).is_none()) {
        return Err(AgentError::IncompatibleModel);
    }
    if agent == Agent::Codex && default_model.is_none() {
        return Err(AgentError::IncompatibleModel);
    }
    let api = api_url(inputs.endpoint).map_err(|_| AgentError::InvalidState)?;
    Ok(match agent {
        Agent::OpenClaw => openclaw::fields(inputs).map_err(AgentError::ConfigurationConflict)?,
        Agent::OhMyPi => oh_my_pi::fields(inputs).map_err(AgentError::ConfigurationConflict)?,
        Agent::Dsh => dsh::fields(inputs)?,
        Agent::Codex => {
            let provider =
                |key: &[&'static str]| [&["model_providers", "private_ai_proxy"], key].concat();
            let mut fields = vec![
                set(&["model_provider"], "private_ai_proxy"),
                absent(&provider(&["env_key"])),
                absent(&provider(&["experimental_bearer_token"])),
                absent(&provider(&["requires_openai_auth"])),
                set(&provider(&["name"]), PRODUCT_NAME),
                set(&provider(&["base_url"]), api.as_str()),
                set(&provider(&["wire_api"]), "responses"),
                set(
                    &["model_catalog_json"],
                    inputs.codex_catalog_path.display().to_string(),
                ),
                set(
                    &provider(&["auth", "command"]),
                    if inputs.file_credentials {
                        "/bin/cat".into()
                    } else {
                        inputs.helper_exe.display().to_string()
                    },
                ),
                list(
                    &provider(&["auth", "args"]),
                    &if inputs.file_credentials {
                        vec![inputs.token_path.to_str().ok_or(AgentError::InvalidState)?]
                    } else {
                        vec!["--agent-token", "codex"]
                    },
                ),
                number(&provider(&["auth", "timeout_ms"]), 5_000),
                number(&provider(&["auth", "refresh_interval_ms"]), 0),
            ];
            if let Some(model) = default_model {
                fields.push(set(model_selection_paths(Agent::Codex)[0].path, model));
            }
            fields
        }
        Agent::ClaudeCode => {
            let mut fields = vec![
                set(&["env", "ANTHROPIC_BASE_URL"], inputs.endpoint),
                set(&["env", "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1"),
                set(&["env", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1"),
                generated_catalog(
                    &["modelPicker"],
                    serde_json::json!({
                        "replaceBuiltInOptions": true,
                        "options": catalog.models.iter().map(|model| {
                            let mut option = serde_json::json!({
                                "model": model.id(), "label": model.display_name(),
                            });
                            if let Some(description) = model.string_field("description") {
                                option["description"] = description
                                    .split_whitespace()
                                    .collect::<Vec<_>>()
                                    .join(" ")
                                    .into();
                            }
                            option
                        }).collect::<Vec<_>>()
                    }),
                    catalog.models.len(),
                ),
                set(
                    &["apiKeyHelper"],
                    inputs
                        .credential_command(Agent::ClaudeCode)
                        .map_err(AgentError::ConfigurationConflict)?,
                ),
                absent(&["env", "ANTHROPIC_AUTH_TOKEN"]),
                absent(&["env", "ANTHROPIC_API_KEY"]),
                absent(&["env", "ANTHROPIC_MODEL"]),
            ];
            fields.extend(
                CLAUDE_PROVIDER_SWITCHES
                    .iter()
                    .map(|key| absent(&["env", key])),
            );
            // The service cannot review auto mode's actions; Claude Code's
            // own classifier requests can.
            fields.push(set(&["env", "CLAUDE_CODE_AUTO_MODE_SERVER"], "0"));
            if let Some(model) = default_model {
                fields.push(set(model_selection_paths(Agent::ClaudeCode)[0].path, model));
                fields.extend(claude_model_env(
                    catalog.get(model).ok_or(AgentError::IncompatibleModel)?,
                ));
            }
            fields
        }
        Agent::OpenCode => {
            let provider = "private-ai-proxy";
            let mut fields = vec![generated_catalog(
                &["provider", provider],
                opencode_provider(catalog, &api, inputs.token_path)
                    .map_err(AgentError::ConfigurationConflict)?,
                catalog.models.len(),
            )];
            if let Some(model) = default_model {
                fields.push(set(
                    model_selection_paths(Agent::OpenCode)[0].path,
                    format!("{provider}/{model}"),
                ));
            }
            fields
        }
        Agent::Pi => vec![generated_catalog(
            &["providers", "private-ai-proxy"],
            pi_provider(
                catalog,
                &api,
                &inputs
                    .credential_command(Agent::Pi)
                    .map_err(AgentError::ConfigurationConflict)?,
            )
            .map_err(AgentError::ConfigurationConflict)?,
            catalog.models.len(),
        )],
        Agent::Hermes => {
            let provider = "private-ai-proxy";
            // Hermes keeps discovering models when `models` is a mapping, and
            // reads these before its live probe and its name-matched tables.
            let models: serde_json::Map<_, _> = catalog
                .models
                .iter()
                .map(|model| {
                    let mut entry = serde_json::json!({
                        "supports_vision": model
                            .string_array("input_modalities")
                            .iter()
                            .any(|value| value == "image"),
                    });
                    if let Some(context) = model.remote.context_length.filter(|value| *value > 0) {
                        entry["context_length"] = context.into();
                    }
                    (model.id().to_string(), entry)
                })
                .collect();
            let mut fields = vec![
                generated_catalog(
                    &["providers", provider, "models"],
                    serde_json::Value::Object(models),
                    catalog.models.len(),
                ),
                set(&["providers", provider, "name"], PRODUCT_NAME),
                set(&["providers", provider, "api"], api.as_str()),
                set(&["providers", provider, "transport"], "chat_completions"),
                boolean(&["providers", provider, "discover_models"], true),
                set(
                    &["providers", provider, "key_cmd"],
                    inputs
                        .credential_command(Agent::Hermes)
                        .map_err(AgentError::ConfigurationConflict)?,
                ),
                set(&["model", "provider"], format!("custom:{provider}")),
                absent(&["model", "base_url"]),
                absent(&["model", "api_mode"]),
            ];
            if let Some(model) = default_model {
                fields.push(set(model_selection_paths(Agent::Hermes)[1].path, model));
                fields.push(set(model_selection_paths(Agent::Hermes)[0].path, model));
            }
            fields
        }
    })
}

/// The OpenAI-compatible API below the Local API endpoint.
pub(super) fn api_url(endpoint: &str) -> Result<String, String> {
    desktop_core::endpoint(endpoint, &["v1"]).map(String::from)
}

pub(super) fn pi_provider(
    catalog: &Catalog,
    api: &str,
    credential_command: &str,
) -> Result<serde_json::Value, String> {
    let models = model_rows(
        catalog,
        &ModelRows {
            // Pi refuses the whole file for a zero limit.
            positive_limits: true,
            max_tokens: true,
            any_input: true,
            input: TEXT_AND_IMAGE,
            cost: Cost::Complete,
        },
        pi_reasoning,
    );
    Ok(serde_json::json!({
        "baseUrl": api,
        "api": "openai-completions",
        "apiKey": format!("!{credential_command}"),
        "models": models,
    }))
}

/// `reasoning` and, for a stated contract, Pi's `thinkingLevelMap` as Pi's
/// own catalog maps efforts: every level is named, a string sends that effort
/// and `null` hides the level.
pub(super) fn pi_reasoning(
    model: &CatalogModel,
    row: &mut serde_json::Map<String, serde_json::Value>,
) {
    use serde_json::{json, Map};
    if !model.reasons() {
        return;
    }
    row.insert("reasoning".into(), json!(true));
    let Some(contract) = model.reasoning() else {
        return;
    };
    let levels = EffortLevels::of(&contract);
    let mut map = Map::new();
    map.insert("off".into(), json!(levels.off.then_some("none")));
    for level in EFFORT_LEVELS {
        map.insert(
            (*level).into(),
            json!(levels.levels.contains(level).then_some(*level)),
        );
    }
    row.insert("thinkingLevelMap".into(), map.into());
}

/// What an agent's OpenAI-completions model rows hold besides `id` and `name`.
pub(super) struct ModelRows {
    /// Leave out a context or output limit of zero.
    pub(super) positive_limits: bool,
    /// Write `maxTokens`, the output limit within the context window.
    pub(super) max_tokens: bool,
    /// Write `input` whenever the model lists modalities, even when none is
    /// one the agent takes.
    pub(super) any_input: bool,
    /// The input modalities the agent takes.
    pub(super) input: &'static [&'static str],
    pub(super) cost: Cost,
}

pub(super) const TEXT_AND_IMAGE: &[&str] = &["text", "image"];

pub(super) enum Cost {
    Omitted,
    /// Only the prices the service gives.
    Listed,
    /// All four rates, zero where the service gives none.
    Complete,
}

/// The effort level names Pi, Oh My Pi, OpenClaw and dsh share, lightest
/// first; each calls the effort that turns reasoning off `off`.
pub(super) const EFFORT_LEVELS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// A reasoning contract in those level names.
pub(super) struct EffortLevels {
    /// The efforts offered, lightest first.
    pub(super) levels: Vec<&'static str>,
    /// `off` sends the effort `none`.
    pub(super) off: bool,
    pub(super) default: Option<&'static str>,
}

impl EffortLevels {
    pub(super) fn of(contract: &ReasoningContract) -> Self {
        let offered = |level: &&str| contract.efforts.iter().any(|effort| effort == level);
        Self {
            levels: EFFORT_LEVELS.iter().copied().filter(offered).collect(),
            off: contract.offers_off(),
            default: EFFORT_LEVELS
                .iter()
                .copied()
                .find(|level| contract.default_effort.as_deref() == Some(level)),
        }
    }
}

/// One model row per catalog model, in catalog order. `reasoning` adds the
/// agent's own reasoning fields to a row, before its prices.
pub(super) fn model_rows(
    catalog: &Catalog,
    rows: &ModelRows,
    reasoning: impl Fn(&CatalogModel, &mut serde_json::Map<String, serde_json::Value>),
) -> Vec<serde_json::Value> {
    use serde_json::{json, Map, Value};
    let limit = |value: Option<u64>| value.filter(|value| !rows.positive_limits || *value > 0);
    catalog
        .models
        .iter()
        .map(|model| {
            let mut row = Map::new();
            row.insert("id".into(), json!(model.id()));
            row.insert("name".into(), json!(model.display_name()));
            let context = limit(model.remote.context_length);
            if let Some(value) = context {
                row.insert("contextWindow".into(), json!(value));
            }
            if rows.max_tokens {
                // An output limit beyond the window could never be met.
                let output = limit(model.remote.max_output_length)
                    .map(|output| context.map_or(output, |context| output.min(context)));
                if let Some(value) = output {
                    row.insert("maxTokens".into(), json!(value));
                }
            }
            let listed = model.string_array("input_modalities");
            let input: Vec<_> = listed
                .iter()
                .filter(|value| rows.input.contains(&value.as_str()))
                .collect();
            if !input.is_empty() || (rows.any_input && !listed.is_empty()) {
                row.insert("input".into(), json!(input));
            }
            reasoning(model, &mut row);
            let mut cost = Map::new();
            if !matches!(rows.cost, Cost::Omitted) {
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
                        cost.insert(target.into(), Value::Number(price));
                    }
                }
            }
            if !cost.is_empty() {
                if matches!(rows.cost, Cost::Complete) {
                    for key in ["input", "output", "cacheRead", "cacheWrite"] {
                        cost.entry(key).or_insert(json!(0));
                    }
                }
                row.insert("cost".into(), Value::Object(cost));
            }
            Value::Object(row)
        })
        .collect()
}
