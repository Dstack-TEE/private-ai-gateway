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
    let catalog = catalog.for_agent_surface(agent.surface());
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
                fields.push(set(&["model"], model));
            }
            fields
        }
        Agent::ClaudeCode => {
            let mut fields = vec![
                set(&["env", "ANTHROPIC_BASE_URL"], inputs.endpoint),
                set(&["env", "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1"),
                generated_catalog(
                    &["modelPicker"],
                    serde_json::json!({
                        "replaceBuiltInOptions": true,
                        "options": catalog.models.iter().map(|model| {
                            serde_json::json!({
                                "model": model.id(), "label": model.display_name(),
                            })
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
                opencode_provider(catalog, &api, inputs.token_path)
                    .map_err(AgentError::ConfigurationConflict)?,
                catalog.models.len(),
            )];
            if let Some(model) = default_model {
                fields.push(set(&["model"], format!("{provider}/{model}")));
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
            let mut fields = vec![
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
            ];
            if let Some(model) = default_model {
                fields.push(set(&["providers", provider, "default_model"], model));
                fields.push(set(&["model", "default"], model));
            }
            fields
        }
    })
}

/// The OpenAI-compatible API below the Local API endpoint.
pub(super) fn api_url(endpoint: &str) -> Result<String, String> {
    desktop_core::endpoint(endpoint, &["v1"]).map(String::from)
}

pub(super) fn opencode_provider(
    catalog: &Catalog,
    api: &str,
    token_path: &Path,
) -> Result<serde_json::Value, String> {
    let models = catalog
        .models
        .iter()
        .map(|model| {
            let mut config = serde_json::json!({"name": model.display_name()});
            if let (Some(context), Some(output)) =
                (model.remote.context_length, model.remote.max_output_length)
            {
                config["limit"] = serde_json::json!({"context": context, "output": output});
            }
            (model.id().to_string(), config)
        })
        .collect();
    Ok(serde_json::json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": PRODUCT_NAME,
        "options": {
            "baseURL": api,
            "apiKey": opencode_file_reference(token_path)?,
        },
        "models": serde_json::Value::Object(models),
    }))
}

/// OpenCode's `{file:<path>}` substitution, which no library writes. It ends
/// at the first `}`, so a path with a brace cannot be written safely.
fn opencode_file_reference(token_path: &Path) -> Result<String, String> {
    let path = token_path
        .to_str()
        .ok_or("The Agent token path is not valid Unicode")?;
    if path.contains(['{', '}']) {
        return Err("OpenCode cannot read an Agent token path that contains { or }".into());
    }
    Ok(format!("{{file:{path}}}"))
}

pub(super) fn pi_provider(
    catalog: &Catalog,
    api: &str,
    credential_command: &str,
) -> Result<serde_json::Value, String> {
    let models = model_rows(
        catalog,
        &ModelRows {
            positive_limits: false,
            any_input: true,
            reasoning: true,
            cost: Cost::Complete,
        },
    );
    Ok(serde_json::json!({
        "baseUrl": api,
        "api": "openai-completions",
        "apiKey": format!("!{credential_command}"),
        "models": models,
    }))
}

/// What an agent's OpenAI-completions model rows hold besides `id` and `name`.
pub(super) struct ModelRows {
    /// Leave out a context or output limit of zero.
    pub(super) positive_limits: bool,
    /// Write `input` whenever the model lists modalities, even when neither
    /// is text or image.
    pub(super) any_input: bool,
    pub(super) reasoning: bool,
    pub(super) cost: Cost,
}

pub(super) enum Cost {
    Omitted,
    /// Only the prices the service gives.
    Listed,
    /// All four rates, zero where the service gives none.
    Complete,
}

/// One model row per catalog model, in catalog order.
pub(super) fn model_rows(catalog: &Catalog, rows: &ModelRows) -> Vec<serde_json::Value> {
    use serde_json::{json, Map, Value};
    let limit = |value: Option<u64>| value.filter(|value| !rows.positive_limits || *value > 0);
    catalog
        .models
        .iter()
        .map(|model| {
            let mut row = json!({"id": model.id(), "name": model.display_name()});
            if let Some(value) = limit(model.remote.context_length) {
                row["contextWindow"] = json!(value);
            }
            if let Some(value) = limit(model.remote.max_output_length) {
                row["maxTokens"] = json!(value);
            }
            let listed = model.string_array("input_modalities");
            let input: Vec<_> = listed
                .iter()
                .filter(|value| matches!(value.as_str(), "text" | "image"))
                .collect();
            if !input.is_empty() || (rows.any_input && !listed.is_empty()) {
                row["input"] = json!(input);
            }
            if rows.reasoning
                && model
                    .string_array("supported_features")
                    .iter()
                    .any(|value| value == "reasoning")
            {
                row["reasoning"] = json!(true);
            }
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
                row["cost"] = Value::Object(cost);
            }
            row
        })
        .collect()
}

pub(super) fn codex_catalog(catalog: &Catalog) -> Result<serde_json::Value, String> {
    let mut bundled: serde_json::Value =
        serde_json::from_str(include_str!("../../resources/codex/models.json")).map_err(|_| {
            "The app-owned Codex model catalog is invalid. Reinstall Private AI Proxy.".to_string()
        })?;
    let bundled_models = bundled
        .get("models")
        .and_then(serde_json::Value::as_array)
        .filter(|models| !models.is_empty())
        .ok_or_else(|| "The app-owned Codex model catalog is empty".to_string())?;
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
        .filter(|model| model.supports_agent(Surface::Responses))
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
            let mut value = template
                .as_object()
                .cloned()
                .ok_or_else(|| "The app-owned Codex model catalog is malformed".to_string())?;
            let capabilities = model.string_array("supported_features");
            let reasoning = capabilities.iter().any(|value| value == "reasoning");
            let verbosity = capabilities.iter().any(|value| value == "verbosity");
            let image = model
                .string_array("input_modalities")
                .iter()
                .any(|value| value == "image");
            // Keep the baseline's context defaults when the provider gives no limit.
            // Clearing them would disable Codex's derived auto-compaction threshold.
            if let Some(context) = model.remote.context_length {
                let context = i64::try_from(context).ok().filter(|limit| *limit > 0)
                    .ok_or_else(|| "The provider returned an invalid Codex context limit".to_string())?;
                value.insert("context_window".to_string(), serde_json::Value::from(context));
                value.insert("max_context_window".to_string(), serde_json::Value::from(context));
            }
            let web_search = capabilities.iter().any(|value| value == "web_search");
            value.extend(object(serde_json::json!({
                "auto_compact_token_limit": null,
                "slug": model.id(),
                "display_name": model.display_name(),
                "description": model.string_field("description"),
                "default_reasoning_level": reasoning.then_some("medium"),
                "supported_reasoning_levels": if reasoning {
                    serde_json::json!([
                        { "effort": "low", "description": "Faster responses with lighter reasoning" },
                        { "effort": "medium", "description": "Balanced reasoning for everyday coding work" },
                        { "effort": "high", "description": "Deeper reasoning for complex tasks" }
                    ])
                } else {
                    serde_json::json!([])
                },
                "visibility": "list",
                "supported_in_api": true,
                "priority": index + 1,
                "additional_speed_tiers": [],
                "service_tiers": [],
                "default_service_tier": null,
                "upgrade": null,
                "availability_nux": null,
                "default_reasoning_summary": if reasoning { "auto" } else { "none" },
                "support_verbosity": verbosity,
                "default_verbosity": verbosity.then_some("medium"),
                "supports_image_detail_original": image,
                "comp_hash": null,
                "input_modalities": if image { serde_json::json!(["text", "image"]) } else { serde_json::json!(["text"]) },
                "supports_search_tool": web_search,
                // PAP provides streaming HTTP Responses, not Codex-specific transports
                // or reasoning-effort configuration_update items.
                "use_responses_lite": false,
                "supports_experimental_context": false,
                "supports_reasoning_effort_updates": false,
            })));
            // Upstream templates still carry this, but it is not a ModelInfo field.
            value.remove("prefer_websockets");
            if matched_template.is_none() {
                value.extend(object(serde_json::json!({
                    "experimental_supported_tools": [],
                    "multi_agent_reasoning_effort": null,
                    "auto_review_model_override": null,
                    "model_specialty": null,
                    "node_repl_auto_review_required": false,
                    "tool_mode": null,
                    "apply_patch_tool_type": null,
                    "multi_agent_version": null,
                    "web_search_tool_type": "text",
                    "shell_type": "unified_exec",
                })));
            }
            Ok(serde_json::Value::Object(value))
        })
        .collect::<Result<Vec<_>, String>>()?;
    // model_catalog_json replaces the entire upstream catalog. Preserve every
    // bundled entry, overlay exact slugs once, and append provider-specific IDs.
    // Bundled entries stay for Codex internals but are hidden: the proxy cannot
    // serve them, and Codex defaults to the first picker-visible model.
    let entries = bundled["models"]
        .as_array_mut()
        .ok_or_else(|| "The app-owned Codex model catalog is malformed".to_string())?;
    for entry in entries.iter_mut() {
        entry["visibility"] = serde_json::Value::String("hide".to_string());
    }
    for model in models {
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry["slug"] == model["slug"])
        {
            *existing = model;
        } else {
            entries.push(model);
        }
    }
    Ok(bundled)
}

/// The entries of a JSON object literal, in order.
fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(object) => object,
        _ => unreachable!("only JSON object literals are overlaid"),
    }
}
