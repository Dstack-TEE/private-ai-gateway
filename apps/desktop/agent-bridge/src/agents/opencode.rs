//! The OpenCode provider entry, in OpenCode 1's `provider` shape and OpenCode
//! 2's `providers` shape.

use super::*;

pub(super) fn opencode_provider(
    catalog: &Catalog,
    api: &str,
    token_path: &Path,
) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let models = catalog
        .models
        .iter()
        .map(|model| {
            let facts = OpenCodeModel::of(model);
            let mut config = json!({"name": model.display_name()});
            // Without its own variants OpenCode offers low, medium and high
            // for every custom model, its controls for a model whose efforts
            // the service does not state.
            if let Some(efforts) = &facts.efforts {
                config["variants"] = efforts
                    .iter()
                    .map(|effort| (effort.clone(), json!({"reasoningEffort": effort})))
                    .collect::<serde_json::Map<_, _>>()
                    .into();
            }
            if let Some(tools) = facts.tools {
                config["tool_call"] = json!(tools);
            }
            if let Some((input, output)) = &facts.modalities {
                config["modalities"] = json!({"input": input, "output": output});
            }
            if let Some(limit) = facts.limit {
                config["limit"] = limit;
            }
            if let Some(cost) = &facts.cost {
                let mut v1 = json!({"input": cost.input, "output": cost.output});
                if let Some(read) = cost.cache_read {
                    v1["cache_read"] = json!(read);
                }
                if let Some(write) = cost.cache_write {
                    v1["cache_write"] = json!(write);
                }
                config["cost"] = v1;
            }
            (model.id().to_string(), config)
        })
        .collect();
    Ok(json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": PRODUCT_NAME,
        "options": {
            "baseURL": api,
            "apiKey": opencode_file_reference(token_path)?,
        },
        "models": serde_json::Value::Object(models),
    }))
}

/// The OpenCode 2 `providers` entry for a projected OpenCode 1 `provider`
/// entry, its models written from the catalog in v2's own fields.
pub(super) fn opencode_provider_v2(
    provider: &serde_json::Value,
    catalog: &Catalog,
) -> serde_json::Value {
    use serde_json::json;
    let mut native = serde_json::Map::new();
    if let Some(package) = provider.get("npm") {
        native.insert("package".into(), package.clone());
    }
    if let Some(name) = provider.get("name") {
        native.insert("name".into(), name.clone());
    }
    if let Some(options) = provider
        .get("options")
        .and_then(serde_json::Value::as_object)
    {
        let mut settings = serde_json::Map::new();
        for key in ["baseURL", "apiKey", "headers", "body"] {
            if let Some(value) = options.get(key) {
                settings.insert(key.into(), value.clone());
            }
        }
        native.insert("settings".into(), serde_json::Value::Object(settings));
    }
    if let Some(models) = provider
        .get("models")
        .and_then(serde_json::Value::as_object)
    {
        let models = models
            .keys()
            .filter_map(|id| catalog.get(id))
            .map(|model| {
                let facts = OpenCodeModel::of(model);
                // V2 builds the variants for the provider package from
                // `reasoning`; without it, every model gets low, medium and
                // high.
                let mut reasoning = json!({"supported": model.reasons()});
                if let Some(efforts) = facts.efforts.as_ref().filter(|efforts| !efforts.is_empty())
                {
                    reasoning["effort"] = json!(efforts);
                }
                let mut config = json!({
                    "name": model.display_name(),
                    "capabilities": {"reasoning": reasoning},
                });
                if let Some(tools) = facts.tools {
                    config["capabilities"]["tools"] = json!(tools);
                }
                if let Some((input, output)) = &facts.modalities {
                    config["capabilities"]["input"] = json!(input);
                    config["capabilities"]["output"] = json!(output);
                }
                if let Some(limit) = facts.limit {
                    config["limit"] = limit;
                }
                if let Some(cost) = &facts.cost {
                    let mut v2 = json!({"input": cost.input, "output": cost.output});
                    let mut cache = serde_json::Map::new();
                    if let Some(read) = cost.cache_read {
                        cache.insert("read".into(), json!(read));
                    }
                    if let Some(write) = cost.cache_write {
                        cache.insert("write".into(), json!(write));
                    }
                    if !cache.is_empty() {
                        v2["cache"] = serde_json::Value::Object(cache);
                    }
                    config["cost"] = json!([v2]);
                }
                (model.id().to_string(), config)
            })
            .collect();
        native.insert("models".into(), serde_json::Value::Object(models));
    }
    serde_json::Value::Object(native)
}

/// What OpenCode 1 and 2 both describe for a model, in their own fields.
struct OpenCodeModel {
    tools: Option<bool>,
    /// Input and output modalities, when the service lists them.
    modalities: Option<(Vec<String>, Vec<String>)>,
    /// The service's efforts, as it orders them for selectors, when it
    /// states them.
    efforts: Option<Vec<String>>,
    limit: Option<serde_json::Value>,
    cost: Option<OpenCodeCost>,
}

struct OpenCodeCost {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

impl OpenCodeModel {
    fn of(model: &CatalogModel) -> Self {
        // OpenCode's own modality names. Without a list, OpenCode 1 strips
        // images and OpenCode 2 sends them to every model.
        const MODALITIES: &[&str] = &["text", "audio", "image", "video", "pdf"];
        let known = |field: &str| {
            model
                .string_array(field)
                .into_iter()
                .filter(|value| MODALITIES.contains(&value.as_str()))
                .collect::<Vec<_>>()
        };
        // Text when the service lists only the inputs, as for any chat model.
        let modalities = model
            .remote
            .extra
            .contains_key("input_modalities")
            .then(|| {
                let output = if model.remote.extra.contains_key("output_modalities") {
                    known("output_modalities")
                } else {
                    vec!["text".to_string()]
                };
                (known("input_modalities"), output)
            });
        // In the service's order, which it gives for selectors.
        let efforts = model.reasoning().map(|contract| contract.efforts);
        let price = |name| model.price_per_million(name);
        Self {
            tools: model.supports_tools(),
            modalities,
            efforts,
            limit: model
                .remote
                .context_length
                .zip(model.remote.max_output_length)
                .map(|(context, output)| {
                    serde_json::json!({"context": context, "output": output.min(context)})
                }),
            cost: (price("prompt").is_some() || price("completion").is_some()).then(|| {
                OpenCodeCost {
                    input: price("prompt").unwrap_or(0.0),
                    output: price("completion").unwrap_or(0.0),
                    cache_read: price("input_cache_read"),
                    cache_write: price("input_cache_write"),
                }
            }),
        }
    }
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
