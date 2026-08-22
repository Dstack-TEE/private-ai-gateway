//! Request transforms: shape a downstream request body into each candidate
//! upstream's request format.
//!
//! The design uses a config-driven engine: each ordered `ParamConfig` names its
//! input key and output parameter. For every present input, the engine applies
//! an optional transform, substitutes the `"gateway-default"` sentinel, clamps
//! numeric values, and writes the result to the output (including dot-paths).
//! Required entries with defaults backfill absent inputs.
//!
//! Strongly typed endpoint structs may replace `serde_json::Value` later; for now
//! the dynamic shape keeps behavior aligned with the source.

use serde_json::{json, Map, Value};

use super::reasoning::validate_effective;
use super::types::{
    Engine, ProviderFormat, ReasoningConfig, ReasoningEffort, ReasoningFormat, RouteCandidate,
};

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
    InvalidRequest {
        /// Returned to the client. Must never name a route, provider or upstream:
        /// this string reaches the caller verbatim, and `route <provider>:<model>`
        /// used to be part of it.
        message: String,
        /// Operator-only context — which route could not be shaped. Logged, never
        /// returned.
        detail: Option<String>,
    },
}

impl TransformError {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        TransformError::InvalidRequest {
            message: message.into(),
            detail: None,
        }
    }

    /// Operator-only context, for the log line that accompanies the refusal.
    pub fn detail(&self) -> Option<&str> {
        match self {
            TransformError::InvalidRequest { detail, .. } => detail.as_deref(),
            TransformError::Unsupported { .. } => None,
        }
    }

    /// A body we cannot shape for the chosen route is the request's problem and
    /// the caller can act on it; a route offered for an endpoint its format
    /// cannot serve is ours.
    pub fn client_status(&self) -> u16 {
        match self {
            TransformError::InvalidRequest { .. } => 400,
            TransformError::Unsupported { .. } => 500,
        }
    }
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
            TransformError::InvalidRequest { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TransformError {}

type TransformFn = fn(&Value) -> Result<Option<Value>, TransformError>;

#[derive(Clone)]
struct ParamConfig {
    input: &'static str,
    param: &'static str,
    default: Option<Value>,
    min: Option<i64>,
    max: Option<i64>,
    required: bool,
    transform: Option<TransformFn>,
}

fn pc(input: &'static str) -> ParamConfig {
    ParamConfig {
        input,
        param: input,
        default: None,
        min: None,
        max: None,
        required: false,
        transform: None,
    }
}

impl ParamConfig {
    fn to(mut self, param: &'static str) -> Self {
        self.param = param;
        self
    }
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

type ProviderConfig = Vec<ParamConfig>;

macro_rules! p {
    ($input:literal $(=> $output:literal)? $(, $method:ident $(($arg:expr))? )* $(,)?) => {
        pc($input)$(.to($output))?$(.$method($($arg)?))*
    };
}

macro_rules! pass {
    ($($key:literal),* $(,)?) => {
        vec![$(pc($key)),*]
    };
}

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
///
/// A candidate that cannot shape THIS request is skipped (logged at debug,
/// never surfaced to the client), and the remaining candidates keep the
/// request alive: with capability filtering upstream, one mis-described
/// backend must cost an attempt, not the whole request. Only when every
/// candidate fails does the first error surface — that is a request nothing
/// could serve, and the 400 it produces is the same one the all-or-nothing
/// version produced.
pub fn build_candidates(
    params: &Value,
    endpoint: Endpoint,
    candidates: &[RouteCandidate],
    requested_reasoning: Option<&ReasoningConfig>,
) -> Result<Vec<(String, Value)>, TransformError> {
    let mut shaped: Vec<(String, Value)> = Vec::new();
    let mut first_error: Option<TransformError> = None;
    for candidate in candidates {
        let body = candidate_params(params, endpoint, candidate, requested_reasoning).and_then(
            |candidate_params| {
                transform_to_provider_request(
                    candidate.format,
                    &candidate_params,
                    endpoint,
                    candidate.engine,
                )
            },
        );
        match body {
            Ok(body) => shaped.push((candidate.route_id.clone(), body)),
            Err(err) => {
                tracing::debug!(
                    route_id = %candidate.route_id,
                    error = %err,
                    "candidate cannot shape this request; skipping"
                );
                first_error.get_or_insert(err);
            }
        }
    }
    match (shaped.is_empty(), first_error) {
        (true, Some(err)) => Err(err),
        _ => Ok(shaped),
    }
}

fn candidate_params(
    params: &Value,
    endpoint: Endpoint,
    candidate: &RouteCandidate,
    requested_reasoning: Option<&ReasoningConfig>,
) -> Result<Value, TransformError> {
    let mut params = params.clone();
    if endpoint != Endpoint::ChatComplete {
        return Ok(params);
    }
    // Read request context before the mutable borrow below.
    let derived = chat_template_reasoning_intent(&params, candidate, requested_reasoning);
    let requested_reasoning = requested_reasoning.or(derived.as_ref());
    let effective = resolve_effective_reasoning(candidate, &params, requested_reasoning);
    let object = params
        .as_object_mut()
        .ok_or_else(|| TransformError::invalid_request("request body must be a JSON object"))?;
    for key in ["reasoning", "reasoning_effort", "include_reasoning"] {
        object.remove(key);
    }
    // On the OpenAI wire `thinking` is the thinking_type dialect's key, written
    // below from the effective reasoning and never taken from the caller. On
    // the Anthropic wire it is the caller's own control and passes through.
    if candidate.format == ProviderFormat::Openai {
        object.remove("thinking");
    }
    let Some(effective) = effective else {
        return Ok(params);
    };
    validate_effective(&effective).map_err(|err| TransformError::InvalidRequest {
        message: format!("invalid effective reasoning: {err}"),
        detail: Some(format!("route {}", candidate.route_id)),
    })?;
    sync_chat_template_reasoning(object, &effective);
    let reasoning_format = candidate.reasoning_format.unwrap_or_else(|| {
        if candidate.engine.is_some() {
            ReasoningFormat::ReasoningEffort
        } else {
            ReasoningFormat::Reasoning
        }
    });
    match (candidate.format, reasoning_format) {
        (ProviderFormat::Openai, ReasoningFormat::Reasoning) => {
            object.insert("reasoning".to_string(), reasoning_object(&effective));
        }
        (ProviderFormat::Openai, ReasoningFormat::ReasoningEffort) => {
            if effective.max_tokens.is_some() {
                return invalid_reasoning(candidate, "cannot represent max_tokens");
            }
            let effort = effective.effort.unwrap_or_else(|| match effective.enabled {
                Some(true) => ReasoningEffort::Medium,
                Some(false) => ReasoningEffort::None,
                None => unreachable!("validated reasoning has an effort, budget, or enabled flag"),
            });
            object.insert(
                "reasoning_effort".to_string(),
                Value::String(effort.as_str().to_string()),
            );
        }
        (ProviderFormat::Openai, ReasoningFormat::ChatTemplateThinking) => {
            set_chat_template_reasoning(object, &effective, candidate, "thinking")?;
        }
        (ProviderFormat::Openai, ReasoningFormat::ChatTemplateEnableThinking) => {
            set_chat_template_reasoning(object, &effective, candidate, "enable_thinking")?;
        }
        (ProviderFormat::Openai, ReasoningFormat::ThinkingType) => {
            if effective.max_tokens.is_some() {
                return invalid_reasoning(candidate, "cannot represent max_tokens");
            }
            let Some(enabled) = reasoning_enabled(&effective) else {
                return invalid_reasoning(candidate, "reasoning configuration is empty");
            };
            object.insert(
                "thinking".to_string(),
                json!({ "type": if enabled { "enabled" } else { "disabled" } }),
            );
            // The level only means something next to an enabled switch.
            if let Some(effort) = effective.effort.filter(|_| enabled) {
                object.insert(
                    "reasoning_effort".to_string(),
                    Value::String(effort.as_str().to_string()),
                );
            }
        }
        (ProviderFormat::Anthropic, _) => return invalid_reasoning(candidate, "has no adapter"),
    }
    Ok(params)
}

/// Compute the effective reasoning config from the deployment's policy and the
/// request context. Three cases:
///
/// 1. `response_format` present, no `tools`: structured output. Apply override
///    or default (always from policy — the caller's reasoning is ignored
///    because OR injects a default that cannot be distinguished from intent).
///
/// 2. `tools` present (with or without `response_format`): tool calls. Apply
///    threshold only — disable reasoning when max_tokens is small enough that
///    reasoning might eat the budget. Above the threshold, keep the caller's
///    reasoning. Tool call JSON is small, but complex prompts can produce
///    hundreds of reasoning tokens, so the threshold protects small budgets.
///
/// 3. Neither: normal chat. Keep the caller's reasoning.
fn resolve_effective_reasoning(
    candidate: &RouteCandidate,
    params: &Value,
    requested: Option<&ReasoningConfig>,
) -> Option<ReasoningConfig> {
    let policy = candidate.reasoning_policy.as_ref();
    let has_rf = params.get("response_format").is_some_and(|v| !v.is_null());
    let has_tools = params.get("tools").is_some_and(|v| !v.is_null());
    let max_tokens = params
        .get("max_completion_tokens")
        .and_then(Value::as_u64)
        .or_else(|| params.get("max_tokens").and_then(Value::as_u64));

    if has_rf && !has_tools {
        // Structured output: override or default (always from policy when
        // configured — the caller's reasoning is ignored because OR injects a
        // default that cannot be distinguished from intent). Fall back to
        // caller when no policy is configured.
        policy
            .and_then(|p| p.override_policy.clone().or(p.default_policy.clone()))
            .or(requested.cloned())
    } else if has_tools {
        // Tools: threshold only. Disable below threshold, keep caller above.
        if let Some(p) = policy {
            if let (Some(threshold), Some(mt)) = (p.threshold, max_tokens) {
                if mt <= threshold {
                    return Some(ReasoningConfig {
                        effort: Some(ReasoningEffort::None),
                        ..Default::default()
                    });
                }
            }
        }
        requested.cloned()
    } else {
        // Normal chat: caller's reasoning.
        requested.cloned()
    }
}

/// Read a switched-off `chat_template_kwargs` thinking flag as a request to
/// disable reasoning.
///
/// The flag is the vLLM chat-template idiom, and for some callers it is the
/// only way to say "no thinking" at all: a client whose model catalog exposes
/// no `none` reasoning effort has nothing else to send, so it sets
/// `{"thinking": false, "enable_thinking": false}` and no `reasoning` field.
/// Left untranslated that is an opaque body key. It survives to the upstream
/// only for self-hosted engines (see the passthrough allowlist), and only a
/// real vLLM/SGLang server acts on it — a managed API behind the same route
/// receives it and ignores it, so the model keeps thinking and the caller is
/// silently not honoured. Reading it here turns the switch into ordinary
/// reasoning intent, so it is subject to policy and gets encoded in whatever
/// dialect the route actually speaks.
///
/// Three deliberate limits:
///
/// - An explicit `reasoning`/`reasoning_effort` always wins. It is the standard
///   field and the more precise statement of intent; the switch only fills the
///   silence.
/// - Only `false` is read. Encoding "on" would have to invent an effort
///   (`enabled: true` maps to `medium`), which is a stronger claim than the
///   caller made and would change what a working request asks for.
/// - Only OpenAI-format routes that declare a `reasoning_format` take part. A
///   route whose dialect nobody configured cannot be assumed to want a
///   synthesized parameter, and the Anthropic surface treats any reasoning as a
///   hard error. Since this intent is inferred rather than stated, it must
///   never be the reason a request fails: where it cannot be encoded, it is
///   simply not derived, and the switch stays the passthrough it is today.
fn chat_template_reasoning_intent(
    params: &Value,
    candidate: &RouteCandidate,
    requested: Option<&ReasoningConfig>,
) -> Option<ReasoningConfig> {
    if requested.is_some()
        || candidate.format != ProviderFormat::Openai
        || candidate.reasoning_format.is_none()
    {
        return None;
    }
    let kwargs = params.get("chat_template_kwargs")?.as_object()?;
    let mut disabled = false;
    for key in ["thinking", "enable_thinking"] {
        match kwargs.get(key).and_then(Value::as_bool) {
            // Any switch left on ends the derivation, which covers both rules
            // above at once: a plain "on" is never translated, and a caller who
            // sets the two aliases against each other has stated no intent to
            // read, so the body is left alone rather than a side picked.
            Some(true) => return None,
            Some(false) => disabled = true,
            None => {}
        }
    }
    disabled.then(|| ReasoningConfig {
        enabled: Some(false),
        ..Default::default()
    })
}

fn sync_chat_template_reasoning(object: &mut Map<String, Value>, reasoning: &ReasoningConfig) {
    let Some(Value::Object(kwargs)) = object.get_mut("chat_template_kwargs") else {
        return;
    };
    let Some(enabled) = reasoning_enabled(reasoning) else {
        return;
    };
    for key in ["thinking", "enable_thinking"] {
        if let Some(value) = kwargs.get_mut(key) {
            *value = Value::Bool(enabled);
        }
    }
}

fn reasoning_enabled(reasoning: &ReasoningConfig) -> Option<bool> {
    reasoning
        .enabled
        .or_else(|| {
            reasoning
                .effort
                .map(|effort| effort != ReasoningEffort::None)
        })
        .or(reasoning.max_tokens.map(|_| true))
}

fn set_chat_template_reasoning(
    object: &mut Map<String, Value>,
    reasoning: &ReasoningConfig,
    candidate: &RouteCandidate,
    key: &str,
) -> Result<(), TransformError> {
    if reasoning.max_tokens.is_some() {
        return invalid_reasoning(candidate, "cannot represent max_tokens");
    }
    let Some(enabled) = reasoning_enabled(reasoning) else {
        return invalid_reasoning(candidate, "reasoning configuration is empty");
    };
    let kwargs = object
        .entry("chat_template_kwargs")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| TransformError::invalid_request("chat_template_kwargs must be an object"))?;
    kwargs.insert(key.to_string(), Value::Bool(enabled));
    Ok(())
}

/// The route id names the provider (`<provider>:<model>`), so it goes
/// to the log and not to the client — the caller only needs to know that the
/// model they asked for cannot express the reasoning they requested.
fn invalid_reasoning<T>(candidate: &RouteCandidate, message: &str) -> Result<T, TransformError> {
    Err(TransformError::InvalidRequest {
        message: format!("the requested model {message}"),
        detail: Some(format!("route {}", candidate.route_id)),
    })
}

fn reasoning_object(config: &ReasoningConfig) -> Value {
    let mut object = serde_json::Map::new();
    if let Some(effort) = config.effort {
        object.insert("effort".into(), effort.as_str().into());
    }
    if let Some(max_tokens) = config.max_tokens {
        object.insert("max_tokens".into(), max_tokens.into());
    }
    if let Some(enabled) = config.enabled {
        object.insert("enabled".into(), enabled.into());
    }
    Value::Object(object)
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
    for cfg in config {
        if params.get(cfg.input).is_some() {
            if let Some(value) = get_value(params, cfg)? {
                set_nested_property(&mut out, cfg.param, value);
            }
        } else if cfg.required {
            if let Some(default) = &cfg.default {
                set_nested_property(&mut out, cfg.param, default.clone());
            }
        }
    }
    Ok(out)
}

fn get_value(params: &Value, cfg: &ParamConfig) -> Result<Option<Value>, TransformError> {
    let mut value: Option<Value> = match cfg.transform {
        Some(transform) => transform(params)?,
        None => params.get(cfg.input).cloned(),
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
                    TransformError::invalid_request(format!("invalid tool call arguments: {e}"))
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

// ── Config tables ────────────────────────────────────────────────────────────

fn openai_chat_complete_config(engine: Option<Engine>) -> ProviderConfig {
    let mut config = pass!(
        "functions",
        "function_call",
        "stop",
        "logit_bias",
        "user",
        "seed",
        "tools",
        "tool_choice",
        "response_format",
        "top_logprobs",
        "stream_options",
        "service_tier",
        "parallel_tool_calls",
        "max_completion_tokens",
        "store",
        "metadata",
        "modalities",
        "audio",
        "prediction",
        "reasoning",
        "reasoning_effort",
        "thinking",
        "web_search_options",
        "prompt_cache_key",
        "safety_identifier",
        "verbosity",
    );
    config.extend([
        p!("model", with_default(json!("gpt-3.5-turbo")), required),
        p!("messages", with_default(json!(""))),
        p!("max_tokens", with_default(json!(100)), with_min(0)),
        p!(
            "temperature",
            with_default(json!(1)),
            with_min(0),
            with_max(2)
        ),
        p!("top_p", with_default(json!(1)), with_min(0), with_max(1)),
        p!("n", with_default(json!(1))),
        p!("stream", with_default(json!(false))),
        p!("presence_penalty", with_min(-2), with_max(2)),
        p!("frequency_penalty", with_min(-2), with_max(2)),
        p!("logprobs", with_default(json!(false))),
    ]);
    // Self-hosted engines take the full canonical effort vocabulary, so the
    // effort value rides the shared pass-through above unchanged.
    if engine.is_some() {
        config.extend(pass!(
            "top_k",
            "min_p",
            "repetition_penalty",
            "chat_template_kwargs",
        ));
    }
    config
}

fn openai_complete_config() -> ProviderConfig {
    let mut config = pass!(
        "stream_options",
        "stop",
        "best_of",
        "logit_bias",
        "user",
        "seed",
        "suffix",
    );
    config.extend([
        p!("model", with_default(json!("text-davinci-003")), required),
        p!("prompt", with_default(json!(""))),
        p!("max_tokens", with_default(json!(100)), with_min(0)),
        p!(
            "temperature",
            with_default(json!(1)),
            with_min(0),
            with_max(2)
        ),
        p!("top_p", with_default(json!(1)), with_min(0), with_max(1)),
        p!("n", with_default(json!(1))),
        p!("stream", with_default(json!(false))),
        p!("logprobs", with_max(5)),
        p!("echo", with_default(json!(false))),
        p!("presence_penalty", with_min(-2), with_max(2)),
        p!("frequency_penalty", with_min(-2), with_max(2)),
    ]);
    config
}

fn openai_embed_config() -> ProviderConfig {
    let mut config = pass!("encoding_format", "dimensions", "user");
    config.extend([
        p!(
            "model",
            with_default(json!("text-embedding-ada-002")),
            required
        ),
        p!("input", required),
    ]);
    config
}

fn openai_create_model_response_config() -> ProviderConfig {
    let mut config = pass!(
        "background",
        "include",
        "instructions",
        "max_output_tokens",
        "metadata",
        "modalities",
        "parallel_tool_calls",
        "previous_response_id",
        "prompt",
        "prompt_cache_key",
        "reasoning",
        "store",
        "stream",
        "temperature",
        "text",
        "tool_choice",
        "tools",
        "top_p",
        "truncation",
        "user",
        "verbosity",
    );
    config.extend([p!("input", required), p!("model", required)]);
    config
}

fn openai_to_anthropic_messages_config() -> ProviderConfig {
    vec![
        p!("model", required),
        p!("messages", required, with_transform(oai_transform_messages)),
        p!("max_tokens", required),
        p!("temperature", with_min(0), with_max(2)),
        p!("top_p", with_min(0), with_max(1)),
        p!("top_k"),
        p!("stream", with_default(json!(false))),
        p!("stream_options"),
        p!("stop_sequences" => "stop", with_transform(oai_transform_stop_sequences)),
        p!("tools", with_transform(oai_transform_tools)),
        p!("tool_choice", with_transform(oai_transform_tool_choice)),
        p!("metadata" => "user", with_transform(oai_user_from_metadata)),
    ]
}

fn anthropic_chat_complete_config() -> ProviderConfig {
    vec![
        p!("model", with_default(json!("claude-2.1")), required),
        p!("messages", required, with_transform(anthropic_messages)),
        p!("messages" => "system", with_transform(anthropic_system)),
        p!("tools", with_transform(anthropic_tools)),
        p!("tool_choice", with_transform(anthropic_tool_choice)),
        p!("max_tokens", required),
        p!("max_completion_tokens" => "max_tokens"),
        p!(
            "temperature",
            with_default(json!(1)),
            with_min(0),
            with_max(1)
        ),
        p!("top_p", with_default(json!(-1)), with_min(-1)),
        p!("top_k", with_default(json!(-1))),
        p!("stop" => "stop_sequences"),
        p!("stream", with_default(json!(false))),
        p!("user" => "metadata.user_id"),
        p!("thinking"),
    ]
}

fn anthropic_complete_config() -> ProviderConfig {
    vec![
        p!("model", with_default(json!("claude-instant-1")), required),
        p!(
            "prompt",
            required,
            with_transform(anthropic_complete_prompt)
        ),
        p!("max_tokens" => "max_tokens_to_sample", required),
        p!(
            "temperature",
            with_default(json!(1)),
            with_min(0),
            with_max(1)
        ),
        p!("top_p", with_default(json!(-1)), with_min(-1)),
        p!("top_k", with_default(json!(-1))),
        p!("stop" => "stop_sequences", with_transform(anthropic_complete_stop)),
        p!("stream", with_default(json!(false))),
        p!("user" => "metadata.user_id"),
    ]
}

fn anthropic_messages_config() -> ProviderConfig {
    let mut config = pass!(
        "container",
        "mcp_servers",
        "metadata",
        "service_tier",
        "stop_sequences",
        "stream",
        "system",
        "temperature",
        "thinking",
        "tool_choice",
        "tools",
        "top_k",
        "top_p",
    );
    config.extend([
        p!("model", required),
        p!("messages", required),
        p!("max_tokens", required),
    ]);
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::types::{Engine, ProviderFormat, ReasoningPolicy, RouteCandidate};

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

        let structured = chat(
            ProviderFormat::Openai,
            json!({
                "model": "m",
                "messages": [],
                "response_format": { "type": "json_object" }
            }),
            Some(Engine::Sglang),
        );
        assert!(structured.get("reasoning_effort").is_none());
    }

    #[test]
    fn stream_options_injected_for_chat_but_not_responses() {
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
    fn build_candidates_skips_unshapeable_and_keeps_rest() {
        let params = json!({ "model": "m", "input": "x" });
        let shapeable = |id: &str| RouteCandidate {
            route_id: id.into(),
            format: ProviderFormat::Openai,
            engine: None,
            reasoning_format: None,
            reasoning_policy: None,
        };
        // (Anthropic, Embed) has no transform: that candidate is skipped and
        // the shapeable one still carries the request.
        let unshapeable = RouteCandidate {
            route_id: "anthropic:a".into(),
            format: ProviderFormat::Anthropic,
            engine: None,
            reasoning_format: None,
            reasoning_policy: None,
        };
        let bodies = build_candidates(
            &params,
            Endpoint::Embed,
            &[unshapeable.clone(), shapeable("openai:b")],
            None,
        )
        .unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].0, "openai:b");

        // Only when EVERY candidate fails does the error surface — the same
        // 400 the all-or-nothing version produced.
        let err = build_candidates(&params, Endpoint::Embed, &[unshapeable], None);
        assert!(err.is_err());
    }

    #[test]
    fn build_candidates_uses_effective_or_requested_reasoning() {
        let params = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 8, "response_format": { "type": "json_object" } });
        let reasoning = |effort| {
            Some(ReasoningConfig {
                effort: Some(effort),
                ..Default::default()
            })
        };
        let candidates = vec![
            RouteCandidate {
                route_id: "openai:a".into(),
                format: ProviderFormat::Openai,
                engine: None,
                reasoning_format: Some(ReasoningFormat::Reasoning),
                reasoning_policy: Some(ReasoningPolicy {
                    override_policy: reasoning(ReasoningEffort::High),
                    ..Default::default()
                }),
            },
            RouteCandidate {
                route_id: "openai:b".into(),
                format: ProviderFormat::Openai,
                engine: Some(Engine::Sglang),
                reasoning_format: None,
                reasoning_policy: Some(ReasoningPolicy {
                    override_policy: reasoning(ReasoningEffort::Minimal),
                    ..Default::default()
                }),
            },
            RouteCandidate {
                route_id: "openai:c".into(),
                format: ProviderFormat::Openai,
                engine: None,
                reasoning_format: None,
                reasoning_policy: None,
            },
        ];
        let requested = reasoning(ReasoningEffort::Medium).unwrap();
        let bodies = build_candidates(
            &params,
            Endpoint::ChatComplete,
            &candidates,
            Some(&requested),
        )
        .unwrap();
        assert_eq!(bodies.len(), 3);
        assert_eq!(bodies[0].1["reasoning"]["effort"], "high");
        assert!(bodies[0].1.get("reasoning_effort").is_none());
        assert_eq!(bodies[1].1["reasoning_effort"], "minimal");
        assert!(bodies[1].1.get("reasoning").is_none());
        // Candidate c has no policy — falls back to caller's requested reasoning.
        assert_eq!(bodies[2].1["reasoning"]["effort"], "medium");
        assert!(bodies[2].1.get("reasoning_effort").is_none());
    }

    #[test]
    fn selected_reasoning_reconciles_chat_template_aliases() {
        for (engine, requested, controlled, original, expected) in [
            (Engine::Vllm, ReasoningEffort::None, None, true, false),
            (
                Engine::Sglang,
                ReasoningEffort::High,
                Some(ReasoningEffort::None),
                true,
                false,
            ),
            (
                Engine::Vllm,
                ReasoningEffort::None,
                Some(ReasoningEffort::High),
                false,
                true,
            ),
        ] {
            let reasoning = |effort| ReasoningConfig {
                effort: Some(effort),
                ..Default::default()
            };
            let params = json!({
                "model": "m",
                "messages": [],
                "response_format": { "type": "json_object" },
                "chat_template_kwargs": {
                    "thinking": original,
                    "enable_thinking": original,
                    "tokenize": false
                }
            });
            let candidate = RouteCandidate {
                route_id: "self-hosted:m".into(),
                format: ProviderFormat::Openai,
                engine: Some(engine),
                reasoning_format: None,
                reasoning_policy: controlled.map(|c| ReasoningPolicy {
                    override_policy: Some(reasoning(c)),
                    ..Default::default()
                }),
            };

            let bodies = build_candidates(
                &params,
                Endpoint::ChatComplete,
                &[candidate],
                Some(&reasoning(requested)),
            )
            .unwrap();
            let kwargs = &bodies[0].1["chat_template_kwargs"];
            assert_eq!(kwargs["thinking"], expected);
            assert_eq!(kwargs["enable_thinking"], expected);
            assert_eq!(kwargs["tokenize"], false);
        }
    }

    /// The case that motivated `chat_template_reasoning_intent`: a caller
    /// disables thinking the only way its catalog lets it, and the route is a
    /// managed API that ignores the switch. Before, nothing was encoded and the
    /// upstream kept thinking; now the switch is re-expressed in the dialect
    /// the route declared.
    #[test]
    fn chat_template_switch_off_is_read_as_reasoning_intent() {
        for reasoning_format in ["reasoning_effort", "reasoning"] {
            let request = json!({
                "model": "m",
                "messages": [],
                "max_tokens": 65536,
                "chat_template_kwargs": { "thinking": false, "enable_thinking": false }
            });
            let (params, requested, _) =
                crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
            assert!(requested.is_none(), "the switch is not a reasoning field");
            let candidate: RouteCandidate = serde_json::from_value(json!({
                "routeId": "managed:m",
                "format": "openai",
                "engine": "sglang",
                "reasoningFormat": reasoning_format,
            }))
            .unwrap();

            let bodies = build_candidates(
                &params,
                Endpoint::ChatComplete,
                &[candidate],
                requested.as_ref(),
            )
            .unwrap();
            let body = &bodies[0].1;
            if reasoning_format == "reasoning_effort" {
                assert_eq!(body["reasoning_effort"], "none");
            } else {
                assert_eq!(body["reasoning"]["enabled"], false);
            }
            // The switch still rides along for an upstream that does honor it.
            assert_eq!(body["chat_template_kwargs"]["thinking"], false);
        }
    }

    /// Most requests that hit this also carry `tools`, which is a separate
    /// branch of `resolve_effective_reasoning` — the derived intent has to
    /// survive it. Above the policy threshold it is what disables reasoning; at
    /// or below, the threshold already forces `none` and the two agree.
    #[test]
    fn chat_template_switch_survives_the_tools_branch() {
        for (max_tokens, tool_choice) in [
            (65536, json!("none")),
            (65536, json!("auto")),
            (65536, json!("required")),
            (
                65536,
                json!({ "type": "function", "function": { "name": "get_current_weather" } }),
            ),
            (256, json!("auto")),
        ] {
            let request = json!({
                "model": "m",
                "messages": [],
                "max_tokens": max_tokens,
                "tools": [{
                    "type": "function",
                    "function": { "name": "get_current_weather", "parameters": {} }
                }],
                "tool_choice": tool_choice,
                "chat_template_kwargs": { "thinking": false, "enable_thinking": false }
            });
            let (params, requested, _) =
                crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
            let candidate: RouteCandidate = serde_json::from_value(json!({
                "routeId": "managed:m",
                "format": "openai",
                "engine": "sglang",
                "reasoningFormat": "reasoning_effort",
                "reasoningPolicy": { "threshold": 2048 }
            }))
            .unwrap();

            let bodies = build_candidates(
                &params,
                Endpoint::ChatComplete,
                &[candidate],
                requested.as_ref(),
            )
            .unwrap();
            let body = &bodies[0].1;
            assert_eq!(
                body["reasoning_effort"], "none",
                "max_tokens {max_tokens}, tool_choice {tool_choice}"
            );
            // The tool call itself must be untouched by the reasoning shaping.
            assert_eq!(body["tool_choice"], tool_choice);
            assert!(body["tools"].is_array());
        }
    }

    /// The three limits the derivation deliberately keeps: an explicit
    /// reasoning field wins, "on" is never synthesized, and a route that
    /// declares no dialect is left exactly as it is today — the last one is
    /// what keeps the Anthropic surface (where any reasoning is a hard error)
    /// and managed OpenAI routes from newly rejecting a request that works.
    #[test]
    fn chat_template_switch_is_not_read_outside_its_limits() {
        let candidate = |format: &str, reasoning_format: Option<&str>| -> RouteCandidate {
            let mut fields = json!({
                "routeId": "managed:m",
                "format": format,
                "engine": "sglang",
            });
            if let Some(dialect) = reasoning_format {
                fields["reasoningFormat"] = dialect.into();
            }
            serde_json::from_value(fields).unwrap()
        };
        let base =
            |kwargs: Value| json!({ "model": "m", "messages": [], "chat_template_kwargs": kwargs });

        // Explicit reasoning wins over the switch.
        let mut request = base(json!({ "thinking": false }));
        request["reasoning_effort"] = "high".into();
        let (params, requested, _) =
            crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
        let bodies = build_candidates(
            &params,
            Endpoint::ChatComplete,
            &[candidate("openai", Some("reasoning_effort"))],
            requested.as_ref(),
        )
        .unwrap();
        assert_eq!(bodies[0].1["reasoning_effort"], "high");

        // Nothing is synthesized for a switch that is on, for a contradictory
        // pair, for a route that declares no dialect, or for the Anthropic
        // surface — the last two would otherwise be a shape the upstream
        // rejects and an outright 400, from an intent nobody stated.
        let off = json!({ "thinking": false, "enable_thinking": false });
        for (kwargs, format, dialect) in [
            (
                json!({ "thinking": true, "enable_thinking": true }),
                "openai",
                Some("reasoning_effort"),
            ),
            (
                json!({ "thinking": true, "enable_thinking": false }),
                "openai",
                Some("reasoning_effort"),
            ),
            (off.clone(), "openai", None),
            (off.clone(), "anthropic", Some("reasoning_effort")),
        ] {
            let request = base(kwargs.clone());
            let (params, requested, _) =
                crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
            let bodies = build_candidates(
                &params,
                Endpoint::ChatComplete,
                &[candidate(format, dialect)],
                requested.as_ref(),
            )
            .expect("a derived intent must never be the reason a request fails");
            let body = &bodies[0].1;
            assert!(
                body.get("reasoning_effort").is_none() && body.get("reasoning").is_none(),
                "{kwargs} on {format}/{dialect:?} should stay a passthrough, got {body}"
            );
            if format == "openai" {
                assert_eq!(body["chat_template_kwargs"], kwargs);
            }
        }
    }

    #[test]
    fn chat_template_reasoning_format_inserts_native_switch() {
        for (format, key) in [
            ("chat_template_thinking", "thinking"),
            ("chat_template_enable_thinking", "enable_thinking"),
        ] {
            let request = json!({
                "model": "m",
                "messages": [],
                "response_format": { "type": "json_object" },
                "reasoning_effort": "none"
            });
            let (params, _requested, _) =
                crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
            let candidate: RouteCandidate = serde_json::from_value(json!({
                "routeId": "self-hosted:m",
                "format": "openai",
                "engine": "vllm",
                "reasoningFormat": format,
                "reasoningPolicy": {
                    "override": { "effort": "none" }
                }
            }))
            .unwrap();

            let bodies =
                build_candidates(&params, Endpoint::ChatComplete, &[candidate], None).unwrap();
            let body = &bodies[0].1;
            assert_eq!(body["chat_template_kwargs"][key], false);
            assert!(body.get("reasoning_effort").is_none());
            assert!(body.get("reasoning").is_none());
        }
    }

    /// DeepSeek's dialect: `thinking.type` is the switch, `reasoning_effort`
    /// the level. The switch is always written; the level only next to an
    /// enabled switch; a budget cannot be expressed at all.
    #[test]
    fn thinking_type_reasoning_format_writes_switch_and_level() {
        let candidate: RouteCandidate = serde_json::from_value(json!({
            "routeId": "vendor:m",
            "format": "openai",
            "reasoningFormat": "thinking_type",
        }))
        .unwrap();
        let shape = |request: Value| {
            let (params, requested, _) =
                crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
            build_candidates(
                &params,
                Endpoint::ChatComplete,
                std::slice::from_ref(&candidate),
                requested.as_ref(),
            )
            .map(|bodies| bodies[0].1.clone())
        };
        let base = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] });
        let with = |field: &str, value: Value| {
            let mut request = base.clone();
            request[field] = value;
            request
        };

        for (request, thinking, effort) in [
            (with("reasoning_effort", "none".into()), "disabled", None),
            (
                with("reasoning", json!({ "enabled": false })),
                "disabled",
                None,
            ),
            (
                with("reasoning", json!({ "effort": "high" })),
                "enabled",
                Some("high"),
            ),
            (
                with("reasoning", json!({ "enabled": true })),
                "enabled",
                None,
            ),
            // A caller's chat-template switch is read as intent on a route
            // that declares a dialect, and lands in this dialect.
            (
                with(
                    "chat_template_kwargs",
                    json!({ "thinking": false, "enable_thinking": false }),
                ),
                "disabled",
                None,
            ),
        ] {
            let body = shape(request.clone()).unwrap();
            assert_eq!(body["thinking"], json!({ "type": thinking }), "{request}");
            assert_eq!(
                body.get("reasoning_effort").and_then(Value::as_str),
                effort,
                "{request}"
            );
            assert!(body.get("reasoning").is_none(), "{request}");
        }

        // A budget has no encoding on this wire.
        assert!(shape(with("reasoning", json!({ "max_tokens": 512 }))).is_err());

        // With nothing to say the switch stays absent, and a caller-supplied
        // `thinking` does not stand in for it: on the OpenAI wire the gateway
        // owns that key. On the Anthropic wire it is the caller's own control
        // and still passes through.
        let callers = with(
            "thinking",
            json!({ "type": "enabled", "budget_tokens": 1024 }),
        );
        let body = shape(callers.clone()).unwrap();
        assert!(body.get("thinking").is_none());
        let anthropic: RouteCandidate = serde_json::from_value(json!({
            "routeId": "anthropic:m",
            "format": "anthropic",
        }))
        .unwrap();
        let bodies =
            build_candidates(&callers, Endpoint::ChatComplete, &[anthropic], None).unwrap();
        assert_eq!(bodies[0].1["thinking"]["budget_tokens"], 1024);
    }

    #[test]
    fn explicit_candidate_reasoning_format_maps_enabled_to_openai_parameter() {
        let request = json!({
            "model": "gpt-5",
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "enabled": true }
        });
        let (params, requested, _) =
            crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
        let candidate: RouteCandidate = serde_json::from_value(json!({
            "routeId": "openai:gpt-5",
            "format": "openai",
            "reasoningFormat": "reasoning_effort"
        }))
        .unwrap();

        let bodies = build_candidates(
            &params,
            Endpoint::ChatComplete,
            &[candidate],
            requested.as_ref(),
        )
        .unwrap();

        assert_eq!(bodies[0].1["reasoning_effort"], "medium");
        assert!(bodies[0].1.get("reasoning").is_none());
    }

    #[test]
    fn managed_candidate_without_dialect_preserves_reasoning_budget() {
        let request = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "max_tokens": 1000 }
        });
        let (params, requested, _) =
            crate::middleware::reasoning::normalize_chat_request(&request).unwrap();
        let candidate: RouteCandidate = serde_json::from_value(json!({
            "routeId": "compatible:m",
            "format": "openai"
        }))
        .unwrap();

        let bodies = build_candidates(
            &params,
            Endpoint::ChatComplete,
            &[candidate],
            requested.as_ref(),
        )
        .unwrap();

        assert_eq!(bodies[0].1["reasoning"]["max_tokens"], 1000);
        assert_eq!(bodies[0].1["reasoning"]["enabled"], true);
        assert!(bodies[0].1.get("reasoning_effort").is_none());

        let effort_candidate: RouteCandidate = serde_json::from_value(json!({
            "routeId": "self-hosted:m",
            "format": "openai",
            "engine": "sglang"
        }))
        .unwrap();
        let error = build_candidates(
            &params,
            Endpoint::ChatComplete,
            &[effort_candidate],
            requested.as_ref(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot represent max_tokens"));
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
                reasoning_format: None,
                reasoning_policy: None,
            },
            RouteCandidate {
                route_id: "anthropic:m".into(),
                format: ProviderFormat::Anthropic,
                engine: None,
                reasoning_format: None,
                reasoning_policy: None,
            },
        ];
        let bodies = build_candidates(&params, Endpoint::ChatComplete, &candidates, None).unwrap();
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
