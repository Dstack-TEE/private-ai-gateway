//! Request transforms: shape a downstream request body into each candidate
//! upstream's request format.
//!
//! The design uses a config-driven engine: a provider config is
//! an ordered list of `(input_key, [ParamConfig])` entries. For each input key
//! present in the request, the engine computes a value (optional transform, then
//! a `"gateway-default"` sentinel substitution, then numeric clamping) and writes
//! it to the output under the config's output param (dot-paths supported). A
//! required entry with a default backfills when the input key is absent.
//!
//! Strongly typed endpoint structs may replace `serde_json::Value` later; for now
//! the dynamic shape keeps behavior aligned with the source.

use serde_json::{json, Value};

use super::types::{Engine, ProviderFormat, RouteCandidate};

const PDF_MIME: &str = "application/pdf";
const TXT_MIME: &str = "text/plain";
const SYSTEM_MESSAGE_ROLES: [&str; 2] = ["system", "developer"];

/// A request endpoint. Selects which per-format config table applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    ChatComplete,
    Complete,
    Embed,
    Messages,
    CreateModelResponse,
}

impl Endpoint {
    fn label(self) -> &'static str {
        match self {
            Endpoint::ChatComplete => "chatComplete",
            Endpoint::Complete => "complete",
            Endpoint::Embed => "embed",
            Endpoint::Messages => "messages",
            Endpoint::CreateModelResponse => "createModelResponse",
        }
    }
}

/// Error from shaping a request.
#[derive(Debug)]
pub enum TransformError {
    Unsupported {
        format: ProviderFormat,
        endpoint: Endpoint,
    },
    /// A transform rejected the request body (e.g. unparseable tool-call
    /// arguments). Surfaced as a gateway-attributed failure (error_source "gateway").
    InvalidRequest(String),
}

impl std::fmt::Display for TransformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransformError::Unsupported { format, endpoint } => {
                let format = match format {
                    ProviderFormat::Openai => "openai",
                    ProviderFormat::Anthropic => "anthropic",
                };
                write!(f, "{} is not supported by {format}", endpoint.label())
            }
            TransformError::InvalidRequest(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TransformError {}

type TransformFn = fn(&Value) -> Result<Option<Value>, TransformError>;

#[derive(Clone)]
struct ParamConfig {
    param: &'static str,
    default: Option<Value>,
    min: Option<i64>,
    max: Option<i64>,
    required: bool,
    transform: Option<TransformFn>,
}

fn pc(param: &'static str) -> ParamConfig {
    ParamConfig {
        param,
        default: None,
        min: None,
        max: None,
        required: false,
        transform: None,
    }
}

impl ParamConfig {
    fn with_default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }
    fn with_min(mut self, min: i64) -> Self {
        self.min = Some(min);
        self
    }
    fn with_max(mut self, max: i64) -> Self {
        self.max = Some(max);
        self
    }
    fn required(mut self) -> Self {
        self.required = true;
        self
    }
    fn with_transform(mut self, transform: TransformFn) -> Self {
        self.transform = Some(transform);
        self
    }
}

type ProviderConfig = Vec<(&'static str, Vec<ParamConfig>)>;

// ── Public API ───────────────────────────────────────────────────────────────

/// Shape `params` into the request body for `format`/`endpoint` (+ optional
/// engine shaping for self-hosted OpenAI-compatible upstreams).
pub fn transform_to_provider_request(
    format: ProviderFormat,
    params: &Value,
    endpoint: Endpoint,
    engine: Option<Engine>,
) -> Result<Value, TransformError> {
    let mut params = params.clone();
    inject_stream_options(&mut params);
    let config = select_config(format, endpoint, engine)?;
    transform_using_provider_config(&config, &params)
}

/// Shape one body per candidate, preserving failover order. Each entry is the
/// candidate's `route_id` and its transformed body.
pub fn build_candidates(
    params: &Value,
    endpoint: Endpoint,
    candidates: &[RouteCandidate],
) -> Result<Vec<(String, Value)>, TransformError> {
    candidates
        .iter()
        .map(|candidate| {
            let body = transform_to_provider_request(
                candidate.format,
                params,
                endpoint,
                candidate.engine,
            )?;
            Ok((candidate.route_id.clone(), body))
        })
        .collect()
}

// ── Engine ───────────────────────────────────────────────────────────────────

fn select_config(
    format: ProviderFormat,
    endpoint: Endpoint,
    engine: Option<Engine>,
) -> Result<ProviderConfig, TransformError> {
    use Endpoint::*;
    use ProviderFormat::*;
    let config = match (format, endpoint) {
        (Openai, ChatComplete) => openai_chat_complete_config(engine),
        (Openai, Complete) => openai_complete_config(),
        (Openai, Embed) => openai_embed_config(),
        (Openai, Messages) => openai_to_anthropic_messages_config(),
        (Openai, CreateModelResponse) => openai_create_model_response_config(),
        (Anthropic, Complete) => anthropic_complete_config(),
        (Anthropic, ChatComplete) => anthropic_chat_complete_config(),
        (Anthropic, Messages) => anthropic_messages_config(),
        (Anthropic, Embed) | (Anthropic, CreateModelResponse) => {
            return Err(TransformError::Unsupported { format, endpoint })
        }
    };
    Ok(config)
}

fn transform_using_provider_config(
    config: &ProviderConfig,
    params: &Value,
) -> Result<Value, TransformError> {
    let mut out = json!({});
    for (input_key, param_configs) in config {
        for cfg in param_configs {
            if params.get(input_key).is_some() {
                if let Some(value) = get_value(input_key, params, cfg)? {
                    set_nested_property(&mut out, cfg.param, value);
                }
            } else if cfg.required {
                if let Some(default) = &cfg.default {
                    set_nested_property(&mut out, cfg.param, default.clone());
                }
            }
        }
    }
    Ok(out)
}

fn get_value(
    input_key: &str,
    params: &Value,
    cfg: &ParamConfig,
) -> Result<Option<Value>, TransformError> {
    let mut value: Option<Value> = match cfg.transform {
        Some(transform) => transform(params)?,
        None => params.get(input_key).cloned(),
    };

    // "gateway-default" sentinel: substitute the configured default.
    if let Some(Value::String(s)) = &value {
        if s == "gateway-default" {
            if let Some(default) = &cfg.default {
                value = Some(default.clone());
            }
        }
    }

    // Numeric clamping (min checked first; min and max are mutually exclusive).
    if let Some(Value::Number(n)) = &value {
        if let Some(f) = n.as_f64() {
            let clamped = match (cfg.min, cfg.max) {
                (Some(min), _) if f < min as f64 => Some(Value::from(min)),
                (_, Some(max)) if f > max as f64 => Some(Value::from(max)),
                _ => None,
            };
            if let Some(clamped) = clamped {
                value = Some(clamped);
            }
        }
    }

    Ok(value)
}

fn set_nested_property(obj: &mut Value, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = obj;
    for part in &parts[..parts.len() - 1] {
        if !current.is_object() {
            *current = json!({});
        }
        current = current
            .as_object_mut()
            .unwrap()
            .entry(part.to_string())
            .or_insert_with(|| json!({}));
    }
    if !current.is_object() {
        *current = json!({});
    }
    current
        .as_object_mut()
        .unwrap()
        .insert(parts[parts.len() - 1].to_string(), value);
}

// Mutate `params` to request usage on streaming: only
// when `stream === true` and `stream_options.include_usage` is not already true.
fn inject_stream_options(params: &mut Value) {
    if params.get("stream") != Some(&Value::Bool(true)) {
        return;
    }
    let already = params
        .get("stream_options")
        .and_then(|so| so.get("include_usage"))
        == Some(&Value::Bool(true));
    if already {
        return;
    }
    if let Some(obj) = params.as_object_mut() {
        let stream_options = obj
            .entry("stream_options".to_string())
            .or_insert_with(|| json!({}));
        if !stream_options.is_object() {
            *stream_options = json!({});
        }
        stream_options
            .as_object_mut()
            .unwrap()
            .insert("include_usage".to_string(), Value::Bool(true));
    }
}

// ── Shared helpers ───────────────────────────────────────────────────────────

fn is_system_role(role: &str) -> bool {
    SYSTEM_MESSAGE_ROLES.contains(&role)
}

// JavaScript truthiness for the conditionals ported below.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn str_or_empty(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}

// ── OpenAI Chat Completions → Anthropic Messages transforms ──────────────────

fn transform_assistant_message(msg: &Value) -> Result<Value, TransformError> {
    let mut content: Vec<Value> = Vec::new();
    let input_content = msg.get("content_blocks").or_else(|| msg.get("content"));
    match input_content {
        Some(Value::String(s)) if !s.is_empty() => {
            content.push(json!({ "type": "text", "text": s }));
        }
        Some(Value::Array(arr)) if !arr.is_empty() => {
            for item in arr {
                if item.get("type").and_then(Value::as_str) != Some("tool_use") {
                    content.push(item.clone());
                }
            }
        }
        _ => {}
    }
    if let Some(tool_calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            let name = call.get("function").and_then(|f| f.get("name")).cloned();
            let id = call.get("id").cloned();
            let arguments = call
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str);
            // Non-empty arguments are JSON-parsed; a parse failure rejects the
            // request rather than forwarding a different input.
            let input = match arguments {
                Some(s) if !s.is_empty() => serde_json::from_str(s).map_err(|e| {
                    TransformError::InvalidRequest(format!("invalid tool call arguments: {e}"))
                })?,
                _ => json!({}),
            };
            let mut block = json!({ "type": "tool_use" });
            let map = block.as_object_mut().unwrap();
            map.insert("name".into(), name.unwrap_or(Value::Null));
            map.insert("id".into(), id.unwrap_or(Value::Null));
            map.insert("input".into(), input);
            if let Some(cache_control) = call.get("cache_control") {
                map.insert("cache_control".into(), cache_control.clone());
            }
            content.push(block);
        }
    }
    Ok(json!({ "role": msg.get("role").cloned().unwrap_or(Value::Null), "content": content }))
}

fn transform_tool_message(msg: &Value) -> Value {
    let tool_use_id = match msg.get("tool_call_id") {
        Some(Value::Null) | None => Value::String(String::new()),
        Some(v) => v.clone(),
    };
    let mut block = json!({ "type": "tool_result", "tool_use_id": tool_use_id });
    if let Some(content) = msg.get("content") {
        block
            .as_object_mut()
            .unwrap()
            .insert("content".into(), content.clone());
    }
    json!({ "role": "user", "content": [block] })
}

fn append_image_content_item(item: &Value, content: &mut Vec<Value>) {
    let url = item
        .get("image_url")
        .and_then(|iu| iu.get("url"))
        .and_then(Value::as_str);
    let url = match url {
        Some(u) if !u.is_empty() => u,
        _ => return,
    };
    if !url.starts_with("data:") {
        content.push(json!({ "type": "image", "source": { "type": "url", "url": url } }));
        return;
    }
    let parts: Vec<&str> = url.split(';').collect();
    if parts.len() != 2 {
        return;
    }
    let base64_parts: Vec<&str> = parts[1].split(',').collect();
    let base64_image = base64_parts.get(1).copied().unwrap_or("");
    let media_type_parts: Vec<&str> = parts[0].split(':').collect();
    if media_type_parts.len() == 2 && !base64_image.is_empty() {
        let media_type = media_type_parts[1];
        let block_type = if media_type == PDF_MIME {
            "document"
        } else {
            "image"
        };
        let mut block = json!({
            "type": block_type,
            "source": { "type": "base64", "media_type": media_type, "data": base64_image },
        });
        if item.get("cache_control").is_some() {
            block
                .as_object_mut()
                .unwrap()
                .insert("cache_control".into(), json!({ "type": "ephemeral" }));
        }
        content.push(block);
    }
}

fn append_file_content_item(item: &Value, content: &mut Vec<Value>) {
    let file = item.get("file");
    let file_url = file.and_then(|f| f.get("file_url"));
    if file_url.map(truthy).unwrap_or(false) {
        content.push(json!({
            "type": "document",
            "source": { "type": "url", "url": file_url.unwrap().clone() },
        }));
        return;
    }
    let file_data = file.and_then(|f| f.get("file_data"));
    if file_data.map(truthy).unwrap_or(false) {
        let mime = file
            .and_then(|f| f.get("mime_type"))
            .and_then(Value::as_str)
            .unwrap_or(PDF_MIME);
        let content_type = if mime == TXT_MIME { "text" } else { "base64" };
        content.push(json!({
            "type": "document",
            "source": { "type": content_type, "data": file_data.unwrap().clone(), "media_type": mime },
        }));
    }
}

fn anthropic_messages(params: &Value) -> Result<Option<Value>, TransformError> {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(arr) = params.get("messages").and_then(Value::as_array) {
        for msg in arr {
            let role = str_or_empty(msg.get("role"));
            if is_system_role(role) {
                continue;
            }
            if role == "assistant" {
                messages.push(transform_assistant_message(msg)?);
            } else if role == "tool" {
                messages.push(transform_tool_message(msg));
            } else {
                let content = msg.get("content");
                let array_content = content.and_then(Value::as_array);
                if let Some(items) = array_content.filter(|a| !a.is_empty()) {
                    let mut out_content: Vec<Value> = Vec::new();
                    for item in items {
                        match item.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                let mut block = json!({ "type": "text" });
                                if let Some(text) = item.get("text") {
                                    block
                                        .as_object_mut()
                                        .unwrap()
                                        .insert("text".into(), text.clone());
                                }
                                if item.get("cache_control").is_some() {
                                    block.as_object_mut().unwrap().insert(
                                        "cache_control".into(),
                                        json!({ "type": "ephemeral" }),
                                    );
                                }
                                out_content.push(block);
                            }
                            Some("image_url") => append_image_content_item(item, &mut out_content),
                            Some("file") => append_file_content_item(item, &mut out_content),
                            _ => {}
                        }
                    }
                    messages.push(json!({ "role": role, "content": out_content }));
                } else {
                    let mut message = json!({ "role": role });
                    if let Some(content) = content {
                        message
                            .as_object_mut()
                            .unwrap()
                            .insert("content".into(), content.clone());
                    }
                    messages.push(message);
                }
            }
        }
    }
    Ok(Some(Value::Array(messages)))
}

fn anthropic_system(params: &Value) -> Result<Option<Value>, TransformError> {
    let mut system: Vec<Value> = Vec::new();
    if let Some(arr) = params.get("messages").and_then(Value::as_array) {
        for msg in arr {
            let role = str_or_empty(msg.get("role"));
            if !is_system_role(role) {
                continue;
            }
            let content = msg.get("content");
            let first_block_has_text = content
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|b| b.get("text"))
                .map(truthy)
                .unwrap_or(false);
            if let (Some(items), true) = (content.and_then(Value::as_array), first_block_has_text) {
                for block in items {
                    let mut entry = json!({ "type": "text" });
                    if let Some(text) = block.get("text") {
                        entry
                            .as_object_mut()
                            .unwrap()
                            .insert("text".into(), text.clone());
                    }
                    if block.get("cache_control").is_some() {
                        entry
                            .as_object_mut()
                            .unwrap()
                            .insert("cache_control".into(), json!({ "type": "ephemeral" }));
                    }
                    system.push(entry);
                }
            } else if let Some(Value::String(s)) = content {
                system.push(json!({ "type": "text", "text": s }));
            }
        }
    }
    Ok(Some(Value::Array(system)))
}

fn anthropic_tools(params: &Value) -> Result<Option<Value>, TransformError> {
    let mut tools: Vec<Value> = Vec::new();
    if let Some(arr) = params.get("tools").and_then(Value::as_array) {
        for tool in arr {
            if let Some(function) = tool.get("function") {
                let parameters = function.get("parameters");
                let schema = json!({
                    "type": parameters.and_then(|p| p.get("type")).cloned().unwrap_or_else(|| json!("object")),
                    "properties": parameters.and_then(|p| p.get("properties")).cloned().unwrap_or_else(|| json!({})),
                    "required": parameters.and_then(|p| p.get("required")).cloned().unwrap_or_else(|| json!([])),
                    "$defs": parameters.and_then(|p| p.get("$defs")).cloned().unwrap_or_else(|| json!({})),
                });
                let mut entry = json!({
                    "name": function.get("name").cloned().unwrap_or(Value::Null),
                    "description": str_or_empty(function.get("description")),
                    "input_schema": schema,
                });
                if tool.get("cache_control").is_some() {
                    entry
                        .as_object_mut()
                        .unwrap()
                        .insert("cache_control".into(), json!({ "type": "ephemeral" }));
                }
                tools.push(entry);
            } else if let Some(tool_type) = tool.get("type").and_then(Value::as_str) {
                let tool_options = tool.get(tool_type);
                let mut entry = serde_json::Map::new();
                if let Some(Value::Object(options)) = tool_options {
                    for (k, v) in options {
                        entry.insert(k.clone(), v.clone());
                    }
                }
                entry.insert("name".into(), json!(tool_type));
                if let Some(name) = tool_options.and_then(|o| o.get("name")) {
                    entry.insert("type".into(), name.clone());
                }
                if tool.get("cache_control").is_some() {
                    entry.insert("cache_control".into(), json!({ "type": "ephemeral" }));
                }
                tools.push(Value::Object(entry));
            }
        }
    }
    Ok(Some(Value::Array(tools)))
}

fn anthropic_tool_choice(params: &Value) -> Result<Option<Value>, TransformError> {
    if let Some(tool_choice) = params.get("tool_choice") {
        match tool_choice {
            Value::String(s) if s == "required" => return Ok(Some(json!({ "type": "any" }))),
            Value::String(s) if s == "auto" => return Ok(Some(json!({ "type": "auto" }))),
            Value::Object(_) => {
                let name = tool_choice
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                return Ok(Some(json!({ "type": "tool", "name": name })));
            }
            _ => {}
        }
    }
    Ok(Some(Value::Null))
}

// ── Anthropic legacy completion transforms ───────────────────────────────────

fn anthropic_complete_prompt(params: &Value) -> Result<Option<Value>, TransformError> {
    let prompt = str_or_empty(params.get("prompt"));
    Ok(Some(Value::String(format!(
        "\n\nHuman: {prompt}\n\nAssistant:"
    ))))
}

fn anthropic_complete_stop(params: &Value) -> Result<Option<Value>, TransformError> {
    Ok(match params.get("stop") {
        Some(Value::Null) => Some(json!([])),
        Some(v) => Some(v.clone()),
        None => None,
    })
}

// ── Anthropic Messages → OpenAI Chat Completions transforms ──────────────────

fn oai_transform_messages(params: &Value) -> Result<Option<Value>, TransformError> {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = params.get("system") {
        match system {
            Value::String(s) if !s.is_empty() => {
                messages.push(json!({ "role": "system", "content": s }));
            }
            Value::Array(blocks) => {
                let text: Vec<&str> = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect();
                let joined = text.join("\n");
                if !joined.is_empty() {
                    messages.push(json!({ "role": "system", "content": joined }));
                }
            }
            _ => {}
        }
    }

    let Some(anthropic_messages) = params.get("messages").and_then(Value::as_array) else {
        return Ok(Some(Value::Array(messages)));
    };

    for msg in anthropic_messages {
        let role = msg.get("role").cloned().unwrap_or(Value::Null);
        match msg.get("content") {
            Some(Value::String(s)) => {
                messages.push(json!({ "role": role, "content": s }));
            }
            Some(Value::Array(blocks)) => {
                let mut content: Vec<Value> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                let mut tool_results: Vec<(Value, Option<Value>)> = Vec::new();

                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if block.get("text").is_some() {
                                content.push(json!({ "type": "text", "text": str_or_empty(block.get("text")) }));
                            }
                        }
                        Some("image") => {
                            if let Some(source) = block.get("source").filter(|s| truthy(s)) {
                                match source.get("type").and_then(Value::as_str) {
                                    Some("base64") => {
                                        let url = format!(
                                            "data:{};base64,{}",
                                            str_or_empty(source.get("media_type")),
                                            str_or_empty(source.get("data"))
                                        );
                                        content.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                                    }
                                    Some("url") => {
                                        content.push(json!({
                                            "type": "image_url",
                                            "image_url": { "url": str_or_empty(source.get("url")) },
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some("tool_use") => {
                            if block.get("id").is_some() && block.get("name").is_some() {
                                let input =
                                    block.get("input").cloned().unwrap_or_else(|| json!({}));
                                let arguments = serde_json::to_string(&input)
                                    .unwrap_or_else(|_| "{}".to_string());
                                tool_calls.push(json!({
                                    "id": str_or_empty(block.get("id")),
                                    "type": "function",
                                    "function": { "name": str_or_empty(block.get("name")), "arguments": arguments },
                                }));
                            }
                        }
                        Some("tool_result") => {
                            if block.get("tool_use_id").is_some() {
                                let content_value = match block.get("content") {
                                    Some(Value::String(s)) => Some(Value::String(s.clone())),
                                    Some(v) => Some(Value::String(
                                        serde_json::to_string(v).unwrap_or_default(),
                                    )),
                                    None => None,
                                };
                                tool_results.push((
                                    json!(str_or_empty(block.get("tool_use_id"))),
                                    content_value,
                                ));
                            }
                        }
                        Some("document") => {
                            if let Some(source) = block.get("source").filter(|s| truthy(s)) {
                                match source.get("type").and_then(Value::as_str) {
                                    Some("url") => {
                                        content.push(json!({
                                            "type": "file",
                                            "file": {
                                                "file_url": source.get("url").cloned().unwrap_or(Value::Null),
                                                "mime_type": source.get("media_type").cloned().unwrap_or(Value::Null),
                                            },
                                        }));
                                    }
                                    Some("base64") | Some("text") => {
                                        content.push(json!({
                                            "type": "file",
                                            "file": {
                                                "file_data": source.get("data").cloned().unwrap_or(Value::Null),
                                                "mime_type": source.get("media_type").cloned().unwrap_or(Value::Null),
                                            },
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }

                if !content.is_empty() || !tool_calls.is_empty() {
                    let mut message = serde_json::Map::new();
                    message.insert("role".into(), role.clone());
                    if !content.is_empty() {
                        if content.len() == 1
                            && content[0].get("type").and_then(Value::as_str) == Some("text")
                        {
                            message.insert(
                                "content".into(),
                                content[0].get("text").cloned().unwrap_or_else(|| json!("")),
                            );
                        } else {
                            message.insert("content".into(), Value::Array(content.clone()));
                        }
                    }
                    if !tool_calls.is_empty() {
                        message.insert("tool_calls".into(), Value::Array(tool_calls.clone()));
                        let has_content = message.get("content").map(truthy).unwrap_or(false);
                        if !has_content {
                            message.insert("content".into(), json!(""));
                        }
                    }
                    messages.push(Value::Object(message));
                }

                for (tool_use_id, content_value) in tool_results {
                    let mut tool_message = json!({ "role": "tool", "tool_call_id": tool_use_id });
                    if let Some(content_value) = content_value {
                        tool_message
                            .as_object_mut()
                            .unwrap()
                            .insert("content".into(), content_value);
                    }
                    messages.push(tool_message);
                }
            }
            _ => {}
        }
    }

    Ok(Some(Value::Array(messages)))
}

fn oai_transform_tools(params: &Value) -> Result<Option<Value>, TransformError> {
    let tools = match params.get("tools").and_then(Value::as_array) {
        Some(tools) if !tools.is_empty() => tools,
        _ => return Ok(None),
    };
    let out: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let has_type = tool.get("type").map(truthy).unwrap_or(false);
            // A null `input_schema` is falsy in the source, so treat it like an
            // absent schema rather than emitting `parameters: null`.
            let input_schema = tool.get("input_schema");
            let schema_present = input_schema.map(truthy).unwrap_or(false);
            if has_type && !schema_present {
                let tool_type = str_or_empty(tool.get("type"));
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(tool_type);
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{tool_type} tool"));
                json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": { "type": "object", "properties": {}, "required": [] },
                    },
                })
            } else {
                let parameters = if schema_present {
                    input_schema.unwrap().clone()
                } else {
                    json!({ "type": "object", "properties": {}, "required": [] })
                };
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").cloned().unwrap_or(Value::Null),
                        "description": str_or_empty(tool.get("description")),
                        "parameters": parameters,
                    },
                })
            }
        })
        .collect();
    Ok(Some(Value::Array(out)))
}

fn oai_transform_tool_choice(params: &Value) -> Result<Option<Value>, TransformError> {
    let Some(tool_choice) = params.get("tool_choice").filter(|tc| truthy(tc)) else {
        return Ok(None);
    };
    Ok(match tool_choice.get("type").and_then(Value::as_str) {
        Some("auto") => Some(json!("auto")),
        Some("any") => Some(json!("required")),
        Some("tool") => match tool_choice
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            Some(name) => Some(json!({ "type": "function", "function": { "name": name } })),
            None => Some(json!("required")),
        },
        _ => None,
    })
}

fn oai_transform_stop_sequences(params: &Value) -> Result<Option<Value>, TransformError> {
    Ok(
        match params.get("stop_sequences").and_then(Value::as_array) {
            Some(arr) if !arr.is_empty() => Some(Value::Array(arr.clone())),
            _ => None,
        },
    )
}

fn oai_user_from_metadata(params: &Value) -> Result<Option<Value>, TransformError> {
    Ok(params
        .get("metadata")
        .and_then(|m| m.get("user_id"))
        .cloned())
}

// ── Engine reasoning-effort remap ────────────────────────────────────────────

fn map_sglang_reasoning_effort(params: &Value) -> Result<Option<Value>, TransformError> {
    Ok(match params.get("reasoning_effort") {
        Some(Value::String(effort)) => {
            let mapped = match effort.as_str() {
                "minimal" => "low",
                "xhigh" => "max",
                other => other,
            };
            Some(Value::String(mapped.to_string()))
        }
        other => other.cloned(),
    })
}

// ── Config tables ────────────────────────────────────────────────────────────

fn openai_chat_complete_config(engine: Option<Engine>) -> ProviderConfig {
    let mut config: ProviderConfig = vec![
        (
            "model",
            vec![pc("model").with_default(json!("gpt-3.5-turbo")).required()],
        ),
        ("messages", vec![pc("messages").with_default(json!(""))]),
        ("functions", vec![pc("functions")]),
        ("function_call", vec![pc("function_call")]),
        (
            "max_tokens",
            vec![pc("max_tokens").with_default(json!(100)).with_min(0)],
        ),
        (
            "temperature",
            vec![pc("temperature")
                .with_default(json!(1))
                .with_min(0)
                .with_max(2)],
        ),
        (
            "top_p",
            vec![pc("top_p").with_default(json!(1)).with_min(0).with_max(1)],
        ),
        ("n", vec![pc("n").with_default(json!(1))]),
        ("stream", vec![pc("stream").with_default(json!(false))]),
        ("stop", vec![pc("stop")]),
        (
            "presence_penalty",
            vec![pc("presence_penalty").with_min(-2).with_max(2)],
        ),
        (
            "frequency_penalty",
            vec![pc("frequency_penalty").with_min(-2).with_max(2)],
        ),
        ("logit_bias", vec![pc("logit_bias")]),
        ("user", vec![pc("user")]),
        ("seed", vec![pc("seed")]),
        ("tools", vec![pc("tools")]),
        ("tool_choice", vec![pc("tool_choice")]),
        ("response_format", vec![pc("response_format")]),
        ("logprobs", vec![pc("logprobs").with_default(json!(false))]),
        ("top_logprobs", vec![pc("top_logprobs")]),
        ("stream_options", vec![pc("stream_options")]),
        ("service_tier", vec![pc("service_tier")]),
        ("parallel_tool_calls", vec![pc("parallel_tool_calls")]),
        ("max_completion_tokens", vec![pc("max_completion_tokens")]),
        ("store", vec![pc("store")]),
        ("metadata", vec![pc("metadata")]),
        ("modalities", vec![pc("modalities")]),
        ("audio", vec![pc("audio")]),
        ("prediction", vec![pc("prediction")]),
        ("reasoning_effort", vec![pc("reasoning_effort")]),
        ("web_search_options", vec![pc("web_search_options")]),
        ("prompt_cache_key", vec![pc("prompt_cache_key")]),
        ("safety_identifier", vec![pc("safety_identifier")]),
        ("verbosity", vec![pc("verbosity")]),
    ];

    if let Some(engine) = engine {
        config.push(("top_k", vec![pc("top_k")]));
        config.push(("min_p", vec![pc("min_p")]));
        config.push(("repetition_penalty", vec![pc("repetition_penalty")]));
        config.push(("chat_template_kwargs", vec![pc("chat_template_kwargs")]));
        if engine == Engine::Sglang {
            for entry in config.iter_mut() {
                if entry.0 == "reasoning_effort" {
                    entry.1 =
                        vec![pc("reasoning_effort").with_transform(map_sglang_reasoning_effort)];
                }
            }
        }
    }

    config
}

fn openai_complete_config() -> ProviderConfig {
    vec![
        (
            "model",
            vec![pc("model")
                .with_default(json!("text-davinci-003"))
                .required()],
        ),
        ("prompt", vec![pc("prompt").with_default(json!(""))]),
        (
            "max_tokens",
            vec![pc("max_tokens").with_default(json!(100)).with_min(0)],
        ),
        (
            "temperature",
            vec![pc("temperature")
                .with_default(json!(1))
                .with_min(0)
                .with_max(2)],
        ),
        (
            "top_p",
            vec![pc("top_p").with_default(json!(1)).with_min(0).with_max(1)],
        ),
        ("n", vec![pc("n").with_default(json!(1))]),
        ("stream", vec![pc("stream").with_default(json!(false))]),
        ("stream_options", vec![pc("stream_options")]),
        ("logprobs", vec![pc("logprobs").with_max(5)]),
        ("echo", vec![pc("echo").with_default(json!(false))]),
        ("stop", vec![pc("stop")]),
        (
            "presence_penalty",
            vec![pc("presence_penalty").with_min(-2).with_max(2)],
        ),
        (
            "frequency_penalty",
            vec![pc("frequency_penalty").with_min(-2).with_max(2)],
        ),
        ("best_of", vec![pc("best_of")]),
        ("logit_bias", vec![pc("logit_bias")]),
        ("user", vec![pc("user")]),
        ("seed", vec![pc("seed")]),
        ("suffix", vec![pc("suffix")]),
        ("include_usage", vec![pc("include_usage")]),
    ]
}

fn openai_embed_config() -> ProviderConfig {
    vec![
        (
            "model",
            vec![pc("model")
                .with_default(json!("text-embedding-ada-002"))
                .required()],
        ),
        ("input", vec![pc("input").required()]),
        ("encoding_format", vec![pc("encoding_format")]),
        ("dimensions", vec![pc("dimensions")]),
        ("user", vec![pc("user")]),
    ]
}

fn openai_create_model_response_config() -> ProviderConfig {
    vec![
        ("input", vec![pc("input").required()]),
        ("model", vec![pc("model").required()]),
        ("background", vec![pc("background")]),
        ("include", vec![pc("include")]),
        ("instructions", vec![pc("instructions")]),
        ("max_output_tokens", vec![pc("max_output_tokens")]),
        ("metadata", vec![pc("metadata")]),
        ("modalities", vec![pc("modalities")]),
        ("parallel_tool_calls", vec![pc("parallel_tool_calls")]),
        ("previous_response_id", vec![pc("previous_response_id")]),
        ("prompt", vec![pc("prompt")]),
        ("prompt_cache_key", vec![pc("prompt_cache_key")]),
        ("reasoning", vec![pc("reasoning")]),
        ("store", vec![pc("store")]),
        ("stream", vec![pc("stream")]),
        ("temperature", vec![pc("temperature")]),
        ("text", vec![pc("text")]),
        ("tool_choice", vec![pc("tool_choice")]),
        ("tools", vec![pc("tools")]),
        ("top_p", vec![pc("top_p")]),
        ("truncation", vec![pc("truncation")]),
        ("user", vec![pc("user")]),
        ("verbosity", vec![pc("verbosity")]),
    ]
}

fn openai_to_anthropic_messages_config() -> ProviderConfig {
    vec![
        ("model", vec![pc("model").required()]),
        (
            "messages",
            vec![pc("messages")
                .required()
                .with_transform(oai_transform_messages)],
        ),
        ("max_tokens", vec![pc("max_tokens").required()]),
        (
            "temperature",
            vec![pc("temperature").with_min(0).with_max(2)],
        ),
        ("top_p", vec![pc("top_p").with_min(0).with_max(1)]),
        ("top_k", vec![pc("top_k")]),
        ("stream", vec![pc("stream").with_default(json!(false))]),
        ("stream_options", vec![pc("stream_options")]),
        (
            "stop_sequences",
            vec![pc("stop").with_transform(oai_transform_stop_sequences)],
        ),
        (
            "tools",
            vec![pc("tools").with_transform(oai_transform_tools)],
        ),
        (
            "tool_choice",
            vec![pc("tool_choice").with_transform(oai_transform_tool_choice)],
        ),
        (
            "metadata",
            vec![pc("user").with_transform(oai_user_from_metadata)],
        ),
    ]
}

fn anthropic_chat_complete_config() -> ProviderConfig {
    vec![
        (
            "model",
            vec![pc("model").with_default(json!("claude-2.1")).required()],
        ),
        (
            "messages",
            vec![
                pc("messages").required().with_transform(anthropic_messages),
                pc("system").with_transform(anthropic_system),
            ],
        ),
        ("tools", vec![pc("tools").with_transform(anthropic_tools)]),
        (
            "tool_choice",
            vec![pc("tool_choice").with_transform(anthropic_tool_choice)],
        ),
        ("max_tokens", vec![pc("max_tokens").required()]),
        ("max_completion_tokens", vec![pc("max_tokens")]),
        (
            "temperature",
            vec![pc("temperature")
                .with_default(json!(1))
                .with_min(0)
                .with_max(1)],
        ),
        (
            "top_p",
            vec![pc("top_p").with_default(json!(-1)).with_min(-1)],
        ),
        ("top_k", vec![pc("top_k").with_default(json!(-1))]),
        ("stop", vec![pc("stop_sequences")]),
        ("stream", vec![pc("stream").with_default(json!(false))]),
        ("user", vec![pc("metadata.user_id")]),
        ("thinking", vec![pc("thinking")]),
    ]
}

fn anthropic_complete_config() -> ProviderConfig {
    vec![
        (
            "model",
            vec![pc("model")
                .with_default(json!("claude-instant-1"))
                .required()],
        ),
        (
            "prompt",
            vec![pc("prompt")
                .required()
                .with_transform(anthropic_complete_prompt)],
        ),
        ("max_tokens", vec![pc("max_tokens_to_sample").required()]),
        (
            "temperature",
            vec![pc("temperature")
                .with_default(json!(1))
                .with_min(0)
                .with_max(1)],
        ),
        (
            "top_p",
            vec![pc("top_p").with_default(json!(-1)).with_min(-1)],
        ),
        ("top_k", vec![pc("top_k").with_default(json!(-1))]),
        (
            "stop",
            vec![pc("stop_sequences").with_transform(anthropic_complete_stop)],
        ),
        ("stream", vec![pc("stream").with_default(json!(false))]),
        ("user", vec![pc("metadata.user_id")]),
    ]
}

fn anthropic_messages_config() -> ProviderConfig {
    vec![
        ("model", vec![pc("model").required()]),
        ("messages", vec![pc("messages").required()]),
        ("max_tokens", vec![pc("max_tokens").required()]),
        ("container", vec![pc("container")]),
        ("mcp_servers", vec![pc("mcp_servers")]),
        ("metadata", vec![pc("metadata")]),
        ("service_tier", vec![pc("service_tier")]),
        ("stop_sequences", vec![pc("stop_sequences")]),
        ("stream", vec![pc("stream")]),
        ("system", vec![pc("system")]),
        ("temperature", vec![pc("temperature")]),
        ("thinking", vec![pc("thinking")]),
        ("tool_choice", vec![pc("tool_choice")]),
        ("tools", vec![pc("tools")]),
        ("top_k", vec![pc("top_k")]),
        ("top_p", vec![pc("top_p")]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::types::{Engine, ProviderFormat, RouteCandidate};

    fn chat(format: ProviderFormat, params: Value, engine: Option<Engine>) -> Value {
        transform_to_provider_request(format, &params, Endpoint::ChatComplete, engine).unwrap()
    }

    #[test]
    fn engine_adds_extra_params_only_when_engine_set() {
        let with_engine = chat(
            ProviderFormat::Openai,
            json!({ "model": "m", "messages": [], "top_k": 5, "min_p": 0.1 }),
            Some(Engine::Vllm),
        );
        assert_eq!(with_engine["top_k"], json!(5));
        assert_eq!(with_engine["min_p"], json!(0.1));

        let managed = chat(
            ProviderFormat::Openai,
            json!({ "model": "m", "messages": [], "top_k": 5 }),
            None,
        );
        // Managed OpenAI chatComplete has no top_k param, so it is dropped.
        assert!(managed.get("top_k").is_none());
    }

    #[test]
    fn stream_options_injected_for_chat_and_completions_but_not_responses() {
        let chat_out = chat(
            ProviderFormat::Openai,
            json!({ "model": "m", "messages": [], "stream": true }),
            None,
        );
        assert_eq!(chat_out["stream_options"], json!({ "include_usage": true }));

        // Legacy /v1/completions must also carry include_usage to upstream, or
        // usage-only streaming providers (e.g. vLLM) never emit a usage chunk
        // and the meter records 0 tokens.
        let complete_out = transform_to_provider_request(
            ProviderFormat::Openai,
            &json!({ "model": "m", "prompt": "hi", "stream": true }),
            Endpoint::Complete,
            None,
        )
        .unwrap();
        assert_eq!(
            complete_out["stream_options"],
            json!({ "include_usage": true })
        );

        let completions = transform_to_provider_request(
            ProviderFormat::Openai,
            &json!({ "model": "m", "prompt": "hi", "stream": true, "include_usage": true }),
            Endpoint::Complete,
            None,
        )
        .unwrap();
        assert_eq!(
            completions["stream_options"],
            json!({ "include_usage": true })
        );
        assert_eq!(completions["include_usage"], json!(true));

        let responses = transform_to_provider_request(
            ProviderFormat::Openai,
            &json!({ "model": "gpt-4o", "input": "hi", "stream": true }),
            Endpoint::CreateModelResponse,
            None,
        )
        .unwrap();
        assert!(responses.get("stream_options").is_none());
    }

    #[test]
    fn build_candidates_emits_one_body_per_route_even_when_identical() {
        // This path consumes typed per-route bodies; it emits one body
        // per route and does not collapse identical candidates into a shared body.
        let params = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 8 });
        let candidates = vec![
            RouteCandidate {
                route_id: "openai:a".into(),
                format: ProviderFormat::Openai,
                engine: None,
            },
            RouteCandidate {
                route_id: "openai:b".into(),
                format: ProviderFormat::Openai,
                engine: None,
            },
        ];
        let bodies = build_candidates(&params, Endpoint::ChatComplete, &candidates).unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0].0, "openai:a");
        assert_eq!(bodies[1].0, "openai:b");
        assert_eq!(bodies[0].1, bodies[1].1);
    }

    #[test]
    fn unsupported_format_endpoint_errors() {
        let err = transform_to_provider_request(
            ProviderFormat::Anthropic,
            &json!({ "model": "m", "input": "x" }),
            Endpoint::Embed,
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn build_candidates_shapes_per_route() {
        let params = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 8 });
        let candidates = vec![
            RouteCandidate {
                route_id: "openai:m".into(),
                format: ProviderFormat::Openai,
                engine: None,
            },
            RouteCandidate {
                route_id: "anthropic:m".into(),
                format: ProviderFormat::Anthropic,
                engine: None,
            },
        ];
        let bodies = build_candidates(&params, Endpoint::ChatComplete, &candidates).unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0].0, "openai:m");
        // OpenAI passthrough keeps messages as-is.
        assert_eq!(bodies[0].1["messages"], params["messages"]);
        assert_eq!(bodies[1].0, "anthropic:m");
        // Anthropic shaping converts to Anthropic messages and max_tokens stays.
        assert_eq!(bodies[1].1["max_tokens"], json!(8));
        assert_eq!(bodies[1].1["messages"][0]["role"], json!("user"));
    }
}
