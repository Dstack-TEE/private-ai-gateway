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
    let base = inputs.endpoint.trim_end_matches('/');
    Ok(match agent {
        Agent::OpenClaw => openclaw::fields(inputs).map_err(AgentError::ConfigurationConflict)?,
        Agent::OhMyPi => oh_my_pi::fields(inputs).map_err(AgentError::ConfigurationConflict)?,
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
                    if inputs.file_credentials {
                        "/bin/cat".into()
                    } else {
                        inputs.helper_exe.display().to_string()
                    },
                ),
                list(
                    &["model_providers", "private_ai_proxy", "auth", "args"],
                    &if inputs.file_credentials {
                        vec![inputs.token_path.to_str().ok_or(AgentError::InvalidState)?]
                    } else {
                        vec!["--agent-token", "codex"]
                    },
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
                generated_catalog(
                    &["modelPicker"],
                    serde_json::json!({
                        "replaceBuiltInOptions": true,
                        "options": catalog.models.iter().map(|model| {
                            serde_json::json!({ "model": model.id() })
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
            pi_provider(
                catalog,
                base,
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
                set(&["providers", provider, "api"], format!("{base}/v1")),
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

pub(super) fn opencode_provider(
    catalog: &Catalog,
    base: &str,
    token_path: &Path,
) -> serde_json::Value {
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

pub(super) fn pi_provider(
    catalog: &Catalog,
    base: &str,
    credential_command: &str,
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
        "apiKey": format!("!{credential_command}"),
        "models": models,
    }))
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
            value.insert("auto_compact_token_limit".to_string(), serde_json::Value::Null);
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
            value.insert("default_service_tier".to_string(), serde_json::Value::Null);
            value.insert("upgrade".to_string(), serde_json::Value::Null);
            value.insert("availability_nux".to_string(), serde_json::Value::Null);
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
            value.insert("comp_hash".to_string(), serde_json::Value::Null);
            value.insert(
                "input_modalities".to_string(),
                if image { serde_json::json!(["text", "image"]) } else { serde_json::json!(["text"]) },
            );
            value.insert(
                "supports_search_tool".to_string(),
                serde_json::Value::Bool(capabilities.iter().any(|value| value == "web_search")),
            );
            // PAP provides streaming HTTP Responses, not Codex-specific transports.
            value.insert("use_responses_lite".to_string(), serde_json::Value::Bool(false));
            value.insert("prefer_websockets".to_string(), serde_json::Value::Bool(false));
            value.insert("supports_experimental_context".to_string(), serde_json::Value::Bool(false));
            if matched_template.is_none() {
                value.insert("experimental_supported_tools".to_string(), serde_json::json!([]));
                value.insert("multi_agent_reasoning_effort".to_string(), serde_json::Value::Null);
                value.insert("auto_review_model_override".to_string(), serde_json::Value::Null);
                value.insert("model_specialty".to_string(), serde_json::Value::Null);
                value.insert("node_repl_auto_review_required".to_string(), serde_json::Value::Bool(false));
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
    // model_catalog_json replaces the entire upstream catalog. Preserve every
    // bundled entry, overlay exact slugs once, and append provider-specific IDs.
    let entries = bundled["models"]
        .as_array_mut()
        .ok_or_else(|| "The app-owned Codex model catalog is malformed".to_string())?;
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
