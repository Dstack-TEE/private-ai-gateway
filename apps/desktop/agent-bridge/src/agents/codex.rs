//! The Codex model catalog `model_catalog_json` points at.

use super::*;

/// Codex's `codex-rs/models-manager/prompt.md` at the bundled catalog's tag.
const CODEX_BASE_INSTRUCTIONS: &str = include_str!("../../resources/codex/prompt.md");

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
        .filter(|model| Agent::Codex.can_use(catalog, model))
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
            let reasoning = model.reasons();
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
            value.extend(object(serde_json::json!({
                "auto_compact_token_limit": null,
                "slug": model.id(),
                "display_name": model.display_name(),
                "description": model.string_field("description"),
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
                // Chat engines behind the service take only auto, low and high.
                "supports_image_detail_original": false,
                "comp_hash": null,
                "input_modalities": if image { serde_json::json!(["text", "image"]) } else { serde_json::json!(["text"]) },
                // Codex's own tool search, which the service declares as a
                // function to models that take tools.
                "supports_search_tool": model.supports_tools() == Some(true),
                // PAP provides streaming HTTP Responses, not Codex-specific transports
                // or reasoning-effort configuration_update items.
                "use_responses_lite": false,
                "supports_experimental_context": false,
                "supports_reasoning_effort_updates": false,
            })));
            // Only efforts the service offers: Codex sends the chosen one as
            // is. Without one, Codex sends none and the service's default
            // applies. A model whose efforts the service does not state gets
            // none, as Codex gives a model it does not know, not the
            // template's GPT levels.
            let contract = model.reasoning().unwrap_or_default();
            value.insert(
                "supported_reasoning_levels".into(),
                codex_reasoning_levels(&contract.efforts, bundled_models),
            );
            value.insert(
                "default_reasoning_level".into(),
                contract.default_effort.into(),
            );
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
                    // The service declares Codex's freeform tools as functions.
                    "apply_patch_tool_type": "freeform",
                    "multi_agent_version": null,
                    "web_search_tool_type": "text",
                    "shell_type": "unified_exec",
                    // The prompt Codex uses for a model it does not know, not
                    // the template's, written for that OpenAI model.
                    "model_messages": {"instructions_template": CODEX_BASE_INSTRUCTIONS},
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

/// Effort names lightest first, as Codex ranks them: it reads the last
/// preset as the model's deepest effort.
const CODEX_EFFORT_ORDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Codex reasoning presets for the service's efforts, lightest first, with
/// the descriptions Codex's own catalog gives them.
fn codex_reasoning_levels(efforts: &[String], bundled: &[serde_json::Value]) -> serde_json::Value {
    let description = |effort: &str| {
        bundled
            .iter()
            .filter_map(|model| model.get("supported_reasoning_levels")?.as_array())
            .flatten()
            .find(|preset| preset.get("effort").and_then(serde_json::Value::as_str) == Some(effort))
            .and_then(|preset| preset.get("description").cloned())
            .unwrap_or_else(|| effort.into())
    };
    let mut efforts = efforts.to_vec();
    efforts.sort_by_key(|effort| {
        CODEX_EFFORT_ORDER
            .iter()
            .position(|known| known == effort)
            .unwrap_or(CODEX_EFFORT_ORDER.len())
    });
    serde_json::Value::Array(
        efforts
            .iter()
            .map(|effort| serde_json::json!({"effort": effort, "description": description(effort)}))
            .collect(),
    )
}

/// The entries of a JSON object literal, in order.
fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(object) => object,
        _ => unreachable!("only JSON object literals are overlaid"),
    }
}
