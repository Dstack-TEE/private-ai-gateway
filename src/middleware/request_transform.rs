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

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::aggregator::service::{
    CHAT_COMPLETIONS_PATH, COMPLETIONS_PATH, EMBEDDINGS_PATH, MESSAGES_PATH, RESPONSES_PATH,
};

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
    /// The path this endpoint is served on downstream and forwarded to upstream.
    pub fn path(self) -> &'static str {
        match self {
            Endpoint::ChatComplete => CHAT_COMPLETIONS_PATH,
            Endpoint::Complete => COMPLETIONS_PATH,
            Endpoint::Embed => EMBEDDINGS_PATH,
            Endpoint::Messages => MESSAGES_PATH,
            Endpoint::CreateModelResponse => RESPONSES_PATH,
        }
    }

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
#[derive(Debug, Clone)]
pub enum TransformError {
    Unsupported {
        format: ProviderFormat,
        endpoint: Endpoint,
    },
    /// A transform rejected the request body (e.g. unparseable tool-call
    /// arguments). Surfaced as a gateway-attributed failure (error_source "gateway").
    InvalidRequest {
        /// Returned to the client verbatim, so it never names a route, provider
        /// or upstream.
        message: String,
        /// The route that could not be shaped. Logged as the `route` field,
        /// never returned.
        route_id: Option<String>,
    },
}

impl TransformError {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        TransformError::InvalidRequest {
            message: message.into(),
            route_id: None,
        }
    }

    /// The route that could not be shaped, for the log line that accompanies
    /// the refusal.
    pub fn route_id(&self) -> Option<&str> {
        match self {
            TransformError::InvalidRequest { route_id, .. } => route_id.as_deref(),
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
    if format == ProviderFormat::Openai && endpoint == Endpoint::Messages {
        let chat = messages_to_chat_params(params)?;
        return transform_to_provider_request(format, &chat, Endpoint::ChatComplete, engine);
    }
    let mut params = params.clone();
    inject_stream_options(&mut params);
    let config = select_config(format, endpoint, engine)?;
    transform_using_provider_config(&config, &params)
}

/// The client's original Responses request, and why its chat conversion
/// failed when it did.
#[derive(Clone, Copy)]
pub struct ResponsesCandidateInput<'a> {
    pub original: &'a Value,
    pub bridge_error: Option<&'a TransformError>,
}

/// Shape one body per candidate, preserving failover order. Each entry is the
/// candidate's `route_id`, its transformed body, and the endpoint the body is
/// shaped for — the upstream path it must be forwarded to.
///
/// `responses` is the client's original Responses request. A candidate listing
/// `/v1/responses` in `supportedEndpoints` gets that original shaped for the
/// Responses endpoint; every other candidate gets `params`, the chat body.
///
/// A candidate that cannot shape THIS request is skipped (logged at debug,
/// never surfaced to the client), and the remaining candidates keep the
/// request alive: with capability filtering upstream, one mis-described
/// backend must cost an attempt, not the whole request. Only when every
/// candidate fails does the first error surface — that is a request nothing
/// could serve.
pub fn build_candidates(
    params: &Value,
    endpoint: Endpoint,
    candidates: &[RouteCandidate],
    requested_reasoning: Option<&ReasoningConfig>,
    responses: Option<ResponsesCandidateInput<'_>>,
) -> Result<Vec<(String, Value, Endpoint)>, TransformError> {
    let mut shaped = Vec::new();
    let mut first_error: Option<TransformError> = None;
    // Converted once; every Chat candidate of a Messages request shares it.
    let messages_chat = (endpoint == Endpoint::Messages
        && candidates
            .iter()
            .any(|candidate| candidate.format == ProviderFormat::Openai))
    .then(|| messages_to_chat_params(params));
    for candidate in candidates {
        let bridge = match &messages_chat {
            Some(chat) => Bridge::Messages(chat),
            None => responses.map_or(Bridge::None, Bridge::Responses),
        };
        match shape_candidate(params, endpoint, candidate, requested_reasoning, bridge) {
            Ok((upstream_endpoint, body)) => {
                shaped.push((candidate.route_id.clone(), body, upstream_endpoint))
            }
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

/// The Chat bridge a request may take to a Chat-only candidate.
#[derive(Clone, Copy)]
enum Bridge<'a> {
    None,
    /// `params` is already the chat conversion of this Responses request.
    Responses(ResponsesCandidateInput<'a>),
    /// The chat conversion of a Messages request, or why it has none.
    Messages(&'a Result<Value, TransformError>),
}

/// One candidate's upstream endpoint and body. Both bridged surfaces reach a
/// Chat-only candidate as a chat request, so reasoning is encoded in that
/// candidate's dialect exactly as for a client's own chat request.
fn shape_candidate(
    params: &Value,
    endpoint: Endpoint,
    candidate: &RouteCandidate,
    requested_reasoning: Option<&ReasoningConfig>,
    bridge: Bridge<'_>,
) -> Result<(Endpoint, Value), TransformError> {
    let shape = |params: &Value, endpoint| {
        let params = candidate_params(params, endpoint, candidate, requested_reasoning)?;
        transform_to_provider_request(candidate.format, &params, endpoint, candidate.engine)
    };
    match bridge {
        Bridge::Responses(responses) if candidate.supports_endpoint(RESPONSES_PATH) => {
            let body = transform_to_provider_request(
                candidate.format,
                responses.original,
                Endpoint::CreateModelResponse,
                candidate.engine,
            )?;
            Ok((Endpoint::CreateModelResponse, body))
        }
        Bridge::Responses(responses) => match responses.bridge_error {
            Some(err) => Err(err.clone()),
            None => Ok((
                Endpoint::ChatComplete,
                shape(params, Endpoint::ChatComplete)?,
            )),
        },
        // The upstream router serves a chat-format route's /v1/messages on
        // its chat path, so the endpoint stays the client's.
        Bridge::Messages(chat) if candidate.format == ProviderFormat::Openai => {
            let chat = chat.as_ref().map_err(TransformError::clone)?;
            Ok((endpoint, shape(chat, Endpoint::ChatComplete)?))
        }
        Bridge::None | Bridge::Messages(_) => Ok((endpoint, shape(params, endpoint)?)),
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
    let omit_output_limit = should_omit_structured_output_limit(candidate, &params);
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
    if omit_output_limit {
        object.remove("max_tokens");
        object.remove("max_completion_tokens");
    }
    let Some(effective) = effective else {
        return Ok(params);
    };
    validate_effective(&effective).map_err(|err| TransformError::InvalidRequest {
        message: format!("invalid effective reasoning: {err}"),
        route_id: Some(candidate.route_id.clone()),
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

fn has_callable_tools(params: &Value, key: &str) -> bool {
    params.get(key).is_some_and(|value| match value.as_array() {
        Some(items) => !items.is_empty(),
        None => !value.is_null(),
    })
}

fn should_omit_structured_output_limit(candidate: &RouteCandidate, params: &Value) -> bool {
    if candidate.format != ProviderFormat::Openai
        || candidate
            .reasoning_policy
            .as_ref()
            .and_then(|policy| policy.omit_max_tokens)
            != Some(true)
    {
        return false;
    }
    let structured = params
        .get("response_format")
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "json_object" | "json_schema"));
    structured && !has_callable_tools(params, "tools") && !has_callable_tools(params, "functions")
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
        route_id: Some(candidate.route_id.clone()),
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
        (Openai, CreateModelResponse) => openai_create_model_response_config(),
        (Anthropic, Complete) => anthropic_complete_config(),
        (Anthropic, ChatComplete) => anthropic_chat_complete_config(),
        (Anthropic, Messages) => anthropic_messages_config(),
        // OpenAI Messages is converted to a chat request before shaping.
        (Openai, Messages) | (Anthropic, Embed) | (Anthropic, CreateModelResponse) => {
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

// ── Anthropic Messages → OpenAI Chat Completions ─────────────────────────────
//
// Like other Messages-to-Chat bridges, the conversion carries what Chat can
// express and drops the rest: serving hints (`cache_control`,
// `context_management`, `service_tier`, ...), fields it does not read
// (`citations`, `signature`, `is_error`, ...), and content, tools or top-level
// capabilities a Chat upstream cannot serve (`mcp_servers`, Anthropic-defined
// tools, document sources it cannot read). Only a malformed request is
// refused. Which route serves a request that needs more is routing's choice.

/// Rewrite an Anthropic Messages request as the Chat Completions request that
/// serves it.
/// Reasoning (`thinking`, `output_config.effort`) is not part of the body: it
/// is encoded per candidate, in that route's dialect, from
/// [`messages_reasoning`].
pub fn messages_to_chat_params(params: &Value) -> Result<Value, TransformError> {
    let request = params
        .as_object()
        .ok_or_else(|| TransformError::invalid_request("request body must be a JSON object"))?;

    let system = match request.get("system") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => text_blocks(blocks)?,
        Some(_) => {
            return Err(TransformError::invalid_request(
                "system must be a string or an array of text blocks",
            ))
        }
    };
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(json!({ "role": "system", "content": system }));
    }
    let turns = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| TransformError::invalid_request("messages must be an array"))?;
    for turn in turns {
        push_messages_turn(turn, &mut messages)?;
    }

    let mut chat = Map::new();
    chat.insert("messages".into(), Value::Array(messages));
    for key in [
        "model",
        "max_tokens",
        "temperature",
        "top_p",
        "top_k",
        "stream",
    ] {
        if let Some(value) = request.get(key) {
            chat.insert(key.into(), value.clone());
        }
    }
    if let Some(stop) = request
        .get("stop_sequences")
        .filter(|stop| stop.as_array().is_some_and(|stop| !stop.is_empty()))
    {
        chat.insert("stop".into(), stop.clone());
    }
    let tools = messages_tools(request.get("tools"))?;
    // Chat rejects tool controls without tools, and they control nothing.
    if let Some(choice) = request
        .get("tool_choice")
        .filter(|choice| !choice.is_null() && !tools.is_empty())
    {
        if let Some(chat_choice) = messages_tool_choice(choice, &tools)? {
            chat.insert("tool_choice".into(), chat_choice);
        }
        if choice.get("disable_parallel_tool_use") == Some(&Value::Bool(true)) {
            chat.insert("parallel_tool_calls".into(), Value::Bool(false));
        }
    }
    if !tools.is_empty() {
        chat.insert("tools".into(), Value::Array(tools));
    }
    if let Some(user) = request
        .get("metadata")
        .and_then(|metadata| metadata.get("user_id"))
        .and_then(Value::as_str)
        .filter(|user| !user.is_empty())
    {
        chat.insert("user".into(), json!(user));
    }
    if let Some(format) = request
        .get("output_config")
        .and_then(|config| config.get("format"))
        .filter(|format| !format.is_null())
    {
        if let Some(format) = messages_response_format(format)? {
            chat.insert("response_format".into(), format);
        }
    }
    Ok(Value::Object(chat))
}

/// The reasoning a Messages request asks for, as route-neutral config, and
/// whether the client asked to see it. Only enabled or adaptive thinking whose
/// `display` returns text (`summarized`, the default before `display` existed)
/// gets thinking blocks; `omitted` and `updates` hide the reasoning text.
///
/// `output_config.effort` states the level directly. Otherwise a thinking
/// budget is bucketed as claude-code-router and new-api bucket it (≤1024 low,
/// ≤8192 medium, above that high): most Chat reasoning dialects carry only an
/// effort. Adaptive thinking, or enabled thinking without a budget, means
/// Anthropic's default, high. A shape this gateway cannot read asks for
/// nothing it could honour.
pub fn messages_reasoning(params: &Value) -> (Option<ReasoningConfig>, bool) {
    // An effort this gateway does not know states no level.
    let effort = params
        .get("output_config")
        .and_then(|config| config.get("effort"))
        .and_then(|effort| serde_json::from_value::<ReasoningEffort>(effort.clone()).ok());
    let effort_config = |effort| ReasoningConfig {
        effort: Some(effort),
        ..Default::default()
    };
    let Some(thinking) = params
        .get("thinking")
        .filter(|thinking| !thinking.is_null())
    else {
        return (effort.map(effort_config), false);
    };
    let visible = !matches!(
        thinking.get("display").and_then(Value::as_str),
        Some("omitted" | "updates")
    );
    let budget = thinking
        .get("budget_tokens")
        .and_then(Value::as_u64)
        .filter(|budget| *budget > 0);
    match thinking.get("type").and_then(Value::as_str) {
        Some("disabled") => (
            Some(ReasoningConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            false,
        ),
        Some("enabled" | "adaptive") => {
            let effort = effort.unwrap_or(match budget {
                Some(..=1024) => ReasoningEffort::Low,
                Some(1025..=8192) => ReasoningEffort::Medium,
                _ => ReasoningEffort::High,
            });
            (Some(effort_config(effort)), visible)
        }
        _ => (effort.map(effort_config), false),
    }
}

fn block_type(block: &Value) -> Result<&str, TransformError> {
    block
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| TransformError::invalid_request("content block requires a type"))
}

fn block_text(block: &Value) -> Result<&str, TransformError> {
    block
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| TransformError::invalid_request("text block requires text"))
}

/// Separate text blocks read as paragraphs once joined into one Chat string.
fn join_text<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    texts
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The text of the system prompt's blocks; it has no other kind Chat reads.
fn text_blocks(blocks: &[Value]) -> Result<String, TransformError> {
    let mut texts = Vec::new();
    for block in blocks {
        if block_type(block)? == "text" {
            texts.push(block_text(block)?);
        }
    }
    Ok(join_text(texts))
}

/// Append the Chat messages for one Messages turn.
fn push_messages_turn(turn: &Value, messages: &mut Vec<Value>) -> Result<(), TransformError> {
    let role = turn
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| matches!(*role, "user" | "assistant"))
        .ok_or_else(|| TransformError::invalid_request("message role must be user or assistant"))?;
    match turn.get("content") {
        Some(Value::String(text)) => messages.push(json!({ "role": role, "content": text })),
        Some(Value::Array(blocks)) if role == "user" => push_user_turn(blocks, messages)?,
        Some(Value::Array(blocks)) => messages.push(assistant_turn(blocks)?),
        _ => {
            return Err(TransformError::invalid_request(
                "message content must be a string or an array",
            ))
        }
    }
    Ok(())
}

/// A user turn's tool results become `tool` messages ahead of the rest of the
/// turn, because Chat requires them to answer the assistant turn directly.
/// Chat tool messages carry text only, so images a tool returned lead the
/// user message that follows, where the model reads them as the results.
fn push_user_turn(blocks: &[Value], messages: &mut Vec<Value>) -> Result<(), TransformError> {
    let mut content = Vec::new();
    let mut parts = Vec::new();
    let mut answered_tools = false;
    for block in blocks {
        match block_type(block)? {
            "text" => parts.push(json!({ "type": "text", "text": block_text(block)? })),
            "image" => parts.extend(image_part(block)?),
            "document" => parts.extend(document_parts(block)),
            "tool_result" => {
                messages.push(tool_result_message(block, &mut content)?);
                answered_tools = true;
            }
            _ => {}
        }
    }
    content.append(&mut parts);
    if !content.is_empty() || !answered_tools {
        messages.push(json!({ "role": "user", "content": chat_content(content) }));
    }
    Ok(())
}

/// An assistant turn is one Chat message: its text, its tool calls, and its
/// thinking as `reasoning_content`. Chat cannot say where text fell relative
/// to the calls, and replaying the turn does not need it to.
fn assistant_turn(blocks: &[Value]) -> Result<Value, TransformError> {
    let mut texts = Vec::new();
    let mut reasoning = Vec::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block_type(block)? {
            "text" => texts.push(block_text(block)?),
            "thinking" => reasoning.push(
                block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            // Encrypted reasoning only Anthropic can read; nothing to replay.
            "redacted_thinking" => {}
            "tool_use" => {
                let input = block
                    .get("input")
                    .filter(|input| input.is_object())
                    .ok_or_else(|| {
                        TransformError::invalid_request("tool_use input must be a JSON object")
                    })?;
                tool_calls.push(json!({
                    "id": required_string(block, "id", "tool_use block")?,
                    "type": "function",
                    "function": {
                        "name": required_string(block, "name", "tool_use block")?,
                        "arguments": input.to_string(),
                    },
                }));
            }
            _ => {}
        }
    }
    let mut message = json!({ "role": "assistant", "content": join_text(texts) });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    let reasoning = join_text(reasoning);
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    Ok(message)
}

/// Chat content for converted parts: a plain string when every part is text,
/// the shape every Chat upstream accepts.
fn chat_content(parts: Vec<Value>) -> Value {
    if parts
        .iter()
        .all(|part| part.get("type").and_then(Value::as_str) == Some("text"))
    {
        return json!(join_text(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
        ));
    }
    Value::Array(parts)
}

fn image_part(block: &Value) -> Result<Option<Value>, TransformError> {
    let source = block.get("source").unwrap_or(&Value::Null);
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => format!(
            "data:{};base64,{}",
            required_string(source, "media_type", "image source")?,
            required_string(source, "data", "image source")?
        ),
        Some("url") => required_string(source, "url", "image source")?.to_string(),
        _ => return Ok(None),
    };
    Ok(Some(
        json!({ "type": "image_url", "image_url": { "url": url } }),
    ))
}

/// The parts a document becomes: its `title` and `context`, which Anthropic
/// shows the model beside it, then a PDF as a Chat file part or plain text as
/// text. A source Chat cannot carry (URLs, custom content) is dropped.
fn document_parts(block: &Value) -> Vec<Value> {
    let text_of = |key| block.get(key).and_then(Value::as_str).unwrap_or_default();
    let source = block.get("source").unwrap_or(&Value::Null);
    let data = source
        .get("data")
        .and_then(Value::as_str)
        .filter(|data| !data.is_empty());
    let document = match (
        source.get("type").and_then(Value::as_str),
        source.get("media_type").and_then(Value::as_str),
        data,
    ) {
        (Some("base64"), Some(PDF_MIME), Some(data)) => json!({
            "type": "file",
            "file": { "file_data": format!("data:{PDF_MIME};base64,{data}") },
        }),
        (Some("text"), Some(TXT_MIME) | None, Some(data)) => {
            json!({ "type": "text", "text": data })
        }
        _ => return Vec::new(),
    };
    let label = join_text([text_of("title"), text_of("context")]);
    let mut parts = Vec::new();
    if !label.is_empty() {
        parts.push(json!({ "type": "text", "text": label }));
    }
    parts.push(document);
    parts
}

/// Where a tool's images went, for a model reading the Chat tool message.
const TOOL_IMAGES_NOTE: &str = "[The tool returned images; they follow in the next user message.]";

/// The `tool` message answering one tool call. Images the tool returned go to
/// `images` for the user message that follows; a note keeps them attached.
fn tool_result_message(block: &Value, images: &mut Vec<Value>) -> Result<Value, TransformError> {
    let tool_use_id = required_string(block, "tool_use_id", "tool_result block")?;
    let images_before = images.len();
    let mut texts = Vec::new();
    match block.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => texts.push(text.as_str()),
        Some(Value::Array(blocks)) => {
            for content in blocks {
                match block_type(content)? {
                    "text" => texts.push(block_text(content)?),
                    "image" => images.extend(image_part(content)?),
                    _ => {}
                }
            }
        }
        Some(_) => {
            return Err(TransformError::invalid_request(
                "tool_result content must be a string or an array",
            ))
        }
    }
    if images.len() > images_before {
        texts.push(TOOL_IMAGES_NOTE);
    }
    Ok(json!({ "role": "tool", "tool_call_id": tool_use_id, "content": join_text(texts) }))
}

/// Client tools become Chat functions. Anthropic's typed tools run on its
/// servers or assume a harness Chat cannot address.
fn messages_tools(tools: Option<&Value>) -> Result<Vec<Value>, TransformError> {
    let tools = match tools {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(tools)) => tools,
        Some(_) => return Err(TransformError::invalid_request("tools must be an array")),
    };
    let mut functions = Vec::new();
    for tool in tools {
        // Anthropic-defined tools run on its servers or assume a harness
        // Chat cannot address; a model offered them as bare functions would
        // call tools that do nothing.
        if !matches!(tool.get("type"), None | Some(Value::Null))
            && tool.get("type") != Some(&json!("custom"))
        {
            continue;
        }
        let schema = tool
            .get("input_schema")
            .filter(|schema| schema.is_object())
            .ok_or_else(|| {
                TransformError::invalid_request("tool requires an input_schema object")
            })?;
        let mut function = json!({
            "name": required_string(tool, "name", "tool")?,
            "parameters": schema,
        });
        if let Some(description) = tool.get("description").and_then(Value::as_str) {
            function["description"] = json!(description);
        }
        if let Some(strict) = tool.get("strict").and_then(Value::as_bool) {
            function["strict"] = json!(strict);
        }
        functions.push(json!({ "type": "function", "function": function }));
    }
    Ok(functions)
}

/// Chat `tool_choice` for a Messages one. A choice naming a tool the bridge
/// left out is left to the model.
fn messages_tool_choice(choice: &Value, tools: &[Value]) -> Result<Option<Value>, TransformError> {
    Ok(Some(match choice.get("type").and_then(Value::as_str) {
        Some("auto") => json!("auto"),
        Some("any") => json!("required"),
        Some("none") => json!("none"),
        Some("tool") => {
            let name = required_string(choice, "name", "tool_choice")?;
            if !tools.iter().any(|tool| tool["function"]["name"] == name) {
                return Ok(None);
            }
            json!({ "type": "function", "function": { "name": name } })
        }
        _ => {
            return Err(TransformError::invalid_request(
                "tool_choice requires type auto, any, none, or tool",
            ))
        }
    }))
}

/// Chat `response_format` for `output_config.format`: Anthropic's structured
/// output constrains the response to a JSON schema, as a strict Chat
/// `json_schema` format does.
fn messages_response_format(format: &Value) -> Result<Option<Value>, TransformError> {
    let schema = format
        .get("schema")
        .filter(|schema| schema.is_object())
        .filter(|_| format.get("type").and_then(Value::as_str) == Some("json_schema"));
    let Some(schema) = schema else {
        return Ok(None);
    };
    Ok(Some(json!({
        "type": "json_schema",
        "json_schema": { "name": "output", "schema": schema, "strict": true },
    })))
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

// ── Responses API → OpenAI chat.completion request ───────────────────────────

/// Rewrite a Responses API request as the Chat Completions request that
/// serves it: `instructions` and the `input` items become `messages`; the
/// tool, output-format and reasoning controls take their chat spellings.
/// The stateful fields the gateway cannot honour are refused, not dropped:
/// it stores no response, so nothing could be continued or retrieved.
pub fn responses_to_chat_params(params: &Value) -> Result<Value, TransformError> {
    validate_responses_request(params)?;
    let input = params
        .as_object()
        .ok_or_else(|| TransformError::invalid_request("request body must be a JSON object"))?;
    // A stored prompt is the request's content: there is nothing to send a
    // Chat upstream without it. Controls Chat has no counterpart for
    // (`truncation`, `include`, output modalities, ...) are dropped.
    if input.get("prompt").is_some_and(|prompt| !prompt.is_null()) {
        return Err(TransformError::invalid_request(
            "prompt templates require an upstream that supports /v1/responses",
        ));
    }

    let effective_tools = effective_responses_tools(params)?;
    let (chat_tools, tool_map, function_names) = chat_tools(&effective_tools)?;

    let mut messages = Vec::new();
    let mut input_state = ResponsesInputState::default();
    if let Some(instructions) = input
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        messages.push(json!({ "role": "system", "content": instructions }));
    }
    match input.get("input") {
        Some(Value::String(text)) => messages.push(json!({ "role": "user", "content": text })),
        Some(Value::Array(items)) => {
            for item in items {
                push_input_item(item, &mut messages, &mut input_state)?;
            }
            input_state.finish(&mut messages)?;
        }
        _ => {
            return Err(TransformError::invalid_request(
                "input must be a string or an array of items",
            ))
        }
    }

    let mut chat = Map::new();
    chat.insert("messages".into(), Value::Array(messages));
    for (key, chat_key) in [
        ("model", "model"),
        // Gateway-local routing metadata. Provider configs omit it before the
        // request is sent upstream, but consult must see it unchanged.
        ("provider", "provider"),
        // The current spelling: gpt-5-era OpenAI models refuse `max_tokens`
        // on chat, and the self-hosted engines accept both.
        ("max_output_tokens", "max_completion_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stream", "stream"),
        ("user", "user"),
        ("prompt_cache_key", "prompt_cache_key"),
    ] {
        if let Some(value) = input.get(key) {
            chat.insert(chat_key.into(), value.clone());
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        if let Some(choice) = chat_tool_choice(choice, &function_names, &tool_map)? {
            chat.insert("tool_choice".into(), choice);
        }
    }
    // Hosted tools are omitted, so there may be no tools left for Chat's
    // tool controls to govern.
    if !chat_tools.is_empty() {
        chat.insert("tools".into(), Value::Array(chat_tools));
        if let Some(parallel) = input.get("parallel_tool_calls") {
            chat.insert("parallel_tool_calls".into(), parallel.clone());
        }
    }
    if let Some(text) = input.get("text").and_then(Value::as_object) {
        if let Some(format) = text.get("format").and_then(Value::as_object) {
            if let Some(response_format) = chat_response_format(format) {
                chat.insert("response_format".into(), response_format);
            }
        }
        if let Some(verbosity) = text.get("verbosity") {
            chat.insert("verbosity".into(), verbosity.clone());
        }
    }
    if let Some(effort) = input
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .filter(|effort| !effort.is_null())
    {
        chat.insert("reasoning_effort".into(), effort.clone());
    }
    Ok(Value::Object(chat))
}

/// Reject stateful controls the gateway cannot honor even when the selected
/// upstream implements Responses directly. Response ids are gateway-owned,
/// so forwarding continuation or storage controls would expose a contract the
/// client cannot reliably use on the next request.
pub fn validate_responses_request(params: &Value) -> Result<(), TransformError> {
    let input = params
        .as_object()
        .ok_or_else(|| TransformError::invalid_request("request body must be a JSON object"))?;
    if input
        .get("previous_response_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return Err(TransformError::invalid_request(
            "previous_response_id is not supported: the gateway stores no responses",
        ));
    }
    if input
        .get("conversation")
        .is_some_and(|value| !value.is_null())
    {
        return Err(TransformError::invalid_request(
            "conversation is not supported: the gateway stores no responses",
        ));
    }
    for key in ["store", "background"] {
        if input.get(key) == Some(&Value::Bool(true)) {
            return Err(TransformError::invalid_request(format!(
                "{key}: true is not supported: the gateway stores no responses"
            )));
        }
    }
    Ok(())
}

/// What the input items seen so far imply for the ones after them: the calls
/// awaiting outputs, and reasoning awaiting its assistant turn.
#[derive(Default)]
struct ResponsesInputState {
    calls: BTreeMap<String, ResponsesCallKind>,
    outputs: HashSet<String>,
    pending_reasoning: Option<String>,
    /// Images tool outputs returned, for the user message after the outputs.
    pending_media: Vec<Value>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResponsesCallKind {
    Function,
    Custom,
}

impl ResponsesCallKind {
    fn output_type(self) -> &'static str {
        match self {
            Self::Function => "function_call_output",
            Self::Custom => "custom_tool_call_output",
        }
    }
}

impl ResponsesInputState {
    /// Chat tool messages carry text only, so images the tool outputs
    /// returned follow them in a user message.
    fn flush_media(&mut self, messages: &mut Vec<Value>) {
        if !self.pending_media.is_empty() {
            let content = std::mem::take(&mut self.pending_media);
            messages.push(json!({ "role": "user", "content": content }));
        }
    }

    fn finish(&mut self, messages: &mut Vec<Value>) -> Result<(), TransformError> {
        self.flush_media(messages);
        if let Some(call_id) = self
            .calls
            .keys()
            .find(|call_id| !self.outputs.contains(*call_id))
        {
            return Err(TransformError::invalid_request(format!(
                "tool call {call_id} is missing its output"
            )));
        }
        Ok(())
    }
}

/// Append the chat message(s) for one Responses input item: a `message` (or
/// the role-only shorthand), a `function_call` the model made, its
/// `function_call_output`, or a `reasoning` item. Clear reasoning text is
/// attached to the following assistant turn; encrypted-only reasoning cannot
/// be replayed through Chat Completions and is omitted.
fn push_input_item(
    item: &Value,
    messages: &mut Vec<Value>,
    state: &mut ResponsesInputState,
) -> Result<(), TransformError> {
    let item_type = item.get("type").and_then(Value::as_str);
    if !matches!(
        item_type,
        Some("function_call_output" | "custom_tool_call_output")
    ) {
        state.flush_media(messages);
    }
    match item_type {
        Some("message") | None => {
            let role = item
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| TransformError::invalid_request("input message requires a role"))?;
            if !matches!(role, "user" | "assistant" | "system" | "developer") {
                return Err(TransformError::invalid_request(format!(
                    "input message role {role} is not supported"
                )));
            }
            let content = chat_message_content(role, item.get("content"))?;
            let mut message = json!({ "role": role, "content": content });
            if role == "assistant" {
                if let Some(reasoning) = state.pending_reasoning.take() {
                    message["reasoning_content"] = json!(reasoning);
                }
            } else {
                state.pending_reasoning = None;
            }
            messages.push(message);
        }
        Some("function_call") => {
            let call_id = required_string(item, "call_id", "function_call")?;
            let name = response_call_name(item, "function_call")?;
            // Replayed as the model wrote them, malformed or not: the
            // client already had its say on them in the call's output.
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .filter(|arguments| !arguments.trim().is_empty())
                .unwrap_or("{}");
            push_chat_call(
                call_id,
                &name,
                arguments,
                ResponsesCallKind::Function,
                messages,
                state,
            )?;
        }
        Some("custom_tool_call") => {
            let call_id = required_string(item, "call_id", "custom_tool_call")?;
            let name = required_string(item, "name", "custom_tool_call")?;
            let input = item.get("input").and_then(Value::as_str).ok_or_else(|| {
                TransformError::invalid_request("custom_tool_call requires a string input")
            })?;
            let arguments = json!({ "input": input }).to_string();
            push_chat_call(
                call_id,
                name,
                &arguments,
                ResponsesCallKind::Custom,
                messages,
                state,
            )?;
        }
        Some(item_type @ ("function_call_output" | "custom_tool_call_output")) => {
            let call_id = required_string(item, "call_id", item_type)?;
            let Some(call_kind) = state.calls.get(call_id).copied() else {
                return Err(TransformError::invalid_request(format!(
                    "tool output references unknown call {call_id}"
                )));
            };
            if item_type != call_kind.output_type() {
                return Err(TransformError::invalid_request(format!(
                    "tool call {call_id} requires {}",
                    call_kind.output_type()
                )));
            }
            if !state.outputs.insert(call_id.to_string()) {
                return Err(TransformError::invalid_request(format!(
                    "duplicate tool output for call {call_id}"
                )));
            }
            let mut media = Vec::new();
            let content = tool_output_text(item.get("output"), &mut media)?;
            state.pending_media.append(&mut media);
            messages.push(json!({ "role": "tool", "tool_call_id": call_id, "content": content }));
        }
        Some("reasoning") => {
            let content = text_of(item.get("content"));
            let reasoning = if content.is_empty() {
                text_of(item.get("summary"))
            } else {
                content
            };
            if !reasoning.is_empty() {
                state
                    .pending_reasoning
                    .get_or_insert_with(String::new)
                    .push_str(&reasoning);
            }
        }
        // Items a Chat upstream has no counterpart for (hosted tool calls,
        // `additional_tools` carriers read by `effective_responses_tools`).
        Some(_) => {}
    }
    Ok(())
}

fn push_chat_call(
    call_id: &str,
    name: &str,
    arguments: &str,
    kind: ResponsesCallKind,
    messages: &mut Vec<Value>,
    state: &mut ResponsesInputState,
) -> Result<(), TransformError> {
    if state.calls.insert(call_id.to_string(), kind).is_some() {
        return Err(TransformError::invalid_request(format!(
            "duplicate tool call id {call_id}"
        )));
    }
    let call = json!({
        "id": call_id,
        "type": "function",
        "function": { "name": name, "arguments": arguments },
    });
    // A call joins the assistant message before it — the turn's text or
    // earlier calls — so the turn replays as the one message the model made.
    let turn = state
        .pending_reasoning
        .is_none()
        .then(|| messages.last_mut())
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"));
    if let Some(turn) = turn {
        match turn.get_mut("tool_calls").and_then(Value::as_array_mut) {
            Some(calls) => calls.push(call),
            None => turn["tool_calls"] = json!([call]),
        }
    } else {
        let mut message = json!({ "role": "assistant", "tool_calls": [call] });
        if let Some(reasoning) = state.pending_reasoning.take() {
            message["reasoning_content"] = json!(reasoning);
        }
        messages.push(message);
    }
    Ok(())
}

fn required_string<'a>(
    object: &'a Value,
    key: &str,
    item_type: &str,
) -> Result<&'a str, TransformError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            TransformError::invalid_request(format!("{item_type} requires a non-empty {key}"))
        })
}

fn response_call_name(item: &Value, item_type: &str) -> Result<String, TransformError> {
    let name = required_string(item, "name", item_type)?;
    Ok(match item.get("namespace").and_then(Value::as_str) {
        Some(namespace) if !namespace.is_empty() => flatten_namespace_tool_name(namespace, name),
        _ => name.to_string(),
    })
}

/// Chat `content` for a Responses message: a string stays one; content parts
/// take their chat spellings. Assistant history is flattened only when every
/// part is text, the shape every chat upstream accepts.
fn chat_message_content(role: &str, content: Option<&Value>) -> Result<Value, TransformError> {
    let parts = match content {
        Some(Value::String(text)) => return Ok(json!(text)),
        Some(Value::Array(parts)) => parts,
        _ => {
            return Err(TransformError::invalid_request(
                "input message requires content",
            ))
        }
    };
    if role == "assistant" {
        return Ok(json!(text_parts(parts, "assistant content")?));
    }
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let part_type = part
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| TransformError::invalid_request("input content part requires a type"))?;
        match part_type {
            "input_text" | "output_text" | "text" => {
                out.push(json!({ "type": "text", "text": str_or_empty(part.get("text")) }));
            }
            // Chat has no counterpart for image file ids or file URLs.
            "input_image" => {
                if let Some(url) = part.get("image_url").and_then(Value::as_str) {
                    let mut image_url = json!({ "url": url });
                    if let Some(detail) = part.get("detail") {
                        image_url["detail"] = detail.clone();
                    }
                    out.push(json!({ "type": "image_url", "image_url": image_url }));
                }
            }
            "input_file" => {
                if part.get("file_url").is_some_and(|value| !value.is_null()) {
                    continue;
                }
                let file_id = optional_non_empty_string(part, "file_id", "input_file")?;
                let file_data = optional_non_empty_string(part, "file_data", "input_file")?;
                if file_id.is_some() == file_data.is_some() {
                    return Err(TransformError::invalid_request(
                        "input_file requires exactly one of file_id or file_data",
                    ));
                }
                let mut file = Map::new();
                if let Some(file_id) = file_id {
                    file.insert("file_id".into(), json!(file_id));
                }
                if let Some(file_data) = file_data {
                    file.insert("file_data".into(), json!(file_data));
                }
                if let Some(filename) = optional_non_empty_string(part, "filename", "input_file")? {
                    file.insert("filename".into(), json!(filename));
                }
                out.push(json!({ "type": "file", "file": file }));
            }
            _ => {}
        }
    }
    // Every part was one Chat cannot carry: an empty message, not an empty list.
    Ok(if out.is_empty() {
        json!("")
    } else {
        Value::Array(out)
    })
}

/// A tool output's text; images it returned go to `media`, with a note that
/// keeps them attached to this output.
fn tool_output_text(
    output: Option<&Value>,
    media: &mut Vec<Value>,
) -> Result<String, TransformError> {
    let parts = match output {
        Some(Value::String(text)) => return Ok(text.clone()),
        Some(Value::Array(parts)) => parts,
        Some(other) => return Ok(other.to_string()),
        None => {
            return Err(TransformError::invalid_request(
                "tool output requires an output value",
            ))
        }
    };
    let mut text = text_parts(parts, "tool output content")?;
    media.extend(parts.iter().filter_map(|part| {
        let url = part
            .get("image_url")
            .and_then(Value::as_str)
            .filter(|_| part.get("type").and_then(Value::as_str) == Some("input_image"))?;
        Some(json!({ "type": "image_url", "image_url": { "url": url } }))
    }));
    if !media.is_empty() {
        text = join_text([text.as_str(), TOOL_IMAGES_NOTE]);
    }
    Ok(text)
}

/// The text of parts that Chat can only carry as text.
fn text_parts(parts: &[Value], context: &str) -> Result<String, TransformError> {
    let mut text = String::new();
    for part in parts {
        let part_type = part.get("type").and_then(Value::as_str);
        if !matches!(part_type, Some("input_text" | "output_text" | "text")) {
            continue;
        }
        let part_text = part.get("text").and_then(Value::as_str).ok_or_else(|| {
            TransformError::invalid_request(format!("{context} text part requires text"))
        })?;
        text.push_str(part_text);
    }
    Ok(text)
}

fn optional_non_empty_string<'a>(
    object: &'a Value,
    key: &str,
    item_type: &str,
) -> Result<Option<&'a str>, TransformError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(TransformError::invalid_request(format!(
            "{item_type} {key} must be a non-empty string"
        ))),
    }
}

/// The text of a Responses `output` or content: a string as is, content parts
/// joined, anything else serialized — a tool result is whatever the tool said.
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

const CHAT_TOOL_NAME_MAX_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NamespacedTool {
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ResponsesToolMap {
    custom: HashSet<String>,
    namespaces: HashMap<String, NamespacedTool>,
}

impl ResponsesToolMap {
    pub(super) fn from_echo(echo: &Value) -> Self {
        let Some(tools) = echo.get("tools").and_then(Value::as_array) else {
            return Self::default();
        };
        Self::from_tools(tools)
    }

    fn from_tools(tools: &[Value]) -> Self {
        let mut map = Self::default();
        for tool in tools {
            match tool.get("type").and_then(Value::as_str) {
                Some("custom") => {
                    if let Some(name) = tool.get("name").and_then(Value::as_str) {
                        map.custom.insert(name.to_string());
                    }
                }
                Some("namespace") => {
                    let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(children) = tool.get("tools").and_then(Value::as_array) else {
                        continue;
                    };
                    for child in children {
                        if child.get("type").and_then(Value::as_str) != Some("function") {
                            continue;
                        }
                        let Some(name) = child.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        map.namespaces.insert(
                            flatten_namespace_tool_name(namespace, name),
                            NamespacedTool {
                                namespace: namespace.to_string(),
                                name: name.to_string(),
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        map
    }

    pub(super) fn is_custom(&self, name: &str) -> bool {
        self.custom.contains(name)
    }

    pub(super) fn namespace(&self, name: &str) -> Option<&NamespacedTool> {
        self.namespaces.get(name)
    }

    fn has_namespace(&self, namespace: &str) -> bool {
        self.namespaces
            .values()
            .any(|tool| tool.namespace == namespace)
    }
}

/// Merge ordinary Responses tools with the `additional_tools` input item used
/// by newer Codex clients. Keeping this in one helper makes request lowering
/// and response tool-name restoration use the same declaration set.
pub(super) fn effective_responses_tools(params: &Value) -> Result<Vec<Value>, TransformError> {
    let mut tools = match params.get("tools") {
        None => Vec::new(),
        Some(Value::Array(tools)) => tools.clone(),
        Some(_) => return Err(TransformError::invalid_request("tools must be an array")),
    };
    if let Some(items) = params.get("input").and_then(Value::as_array) {
        for item in items {
            if item.get("type").and_then(Value::as_str) != Some("additional_tools") {
                continue;
            }
            let additional = item.get("tools").and_then(Value::as_array).ok_or_else(|| {
                TransformError::invalid_request("additional_tools requires a tools array")
            })?;
            tools.extend(additional.iter().cloned());
        }
    }
    Ok(tools)
}

fn chat_tools(
    tools: &[Value],
) -> Result<(Vec<Value>, ResponsesToolMap, HashSet<String>), TransformError> {
    let tool_map = ResponsesToolMap::from_tools(tools);
    let mut translated = Vec::new();
    let mut function_names = HashSet::new();
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => {
                let name = required_string(tool, "name", "function tool")?;
                push_chat_tool(tool, name, None, &mut translated, &mut function_names)?;
            }
            Some("custom") => {
                // A grammar-constrained tool still takes its input as text.
                let name = required_string(tool, "name", "custom tool")?;
                push_chat_tool(
                    tool,
                    name,
                    Some(json!({
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string",
                                "description": "The raw input for this tool, passed through verbatim."
                            }
                        },
                        "required": ["input"]
                    })),
                    &mut translated,
                    &mut function_names,
                )?;
            }
            Some("namespace") => {
                let namespace = required_string(tool, "name", "namespace tool")?;
                let children = tool.get("tools").and_then(Value::as_array).ok_or_else(|| {
                    TransformError::invalid_request("namespace tool requires a tools array")
                })?;
                for child in children {
                    let child_type = child.get("type").and_then(Value::as_str);
                    if child_type != Some("function") {
                        continue;
                    }
                    let name = required_string(child, "name", "namespace function tool")?;
                    let flat = flatten_namespace_tool_name(namespace, name);
                    push_chat_tool(child, &flat, None, &mut translated, &mut function_names)?;
                }
            }
            // Hosted tools (`web_search`, `tool_search`, ...) have no Chat
            // counterpart. Codex advertises them alongside client tools even
            // when a turn does not need them, so they are omitted.
            _ => {}
        }
    }
    Ok((translated, tool_map, function_names))
}

fn push_chat_tool(
    source: &Value,
    name: &str,
    parameters: Option<Value>,
    translated: &mut Vec<Value>,
    function_names: &mut HashSet<String>,
) -> Result<(), TransformError> {
    if !function_names.insert(name.to_string()) {
        return Err(TransformError::invalid_request(format!(
            "duplicate function tool name {name}"
        )));
    }
    let mut function = Map::new();
    function.insert("name".into(), json!(name));
    for key in ["description", "parameters", "strict"] {
        if let Some(value) = source.get(key) {
            function.insert(key.into(), value.clone());
        }
    }
    if let Some(parameters) = parameters {
        function.insert("parameters".into(), parameters);
        function.remove("strict");
    }
    translated.push(json!({ "type": "function", "function": function }));
    Ok(())
}

fn flatten_namespace_tool_name(namespace: &str, name: &str) -> String {
    let full = format!("{namespace}__{name}");
    if full.len() <= CHAT_TOOL_NAME_MAX_LEN {
        return full;
    }
    let digest = Sha256::digest(full.as_bytes());
    let suffix = format!("__{}", hex::encode(&digest[..4]));
    let prefix_len = CHAT_TOOL_NAME_MAX_LEN - suffix.len();
    let mut end = 0;
    for (index, character) in full.char_indices() {
        let next = index + character.len_utf8();
        if next > prefix_len {
            break;
        }
        end = next;
    }
    format!("{}{}", &full[..end], suffix)
}

/// Chat `tool_choice` for a Responses one. A choice Chat cannot express — a
/// namespace, or a tool the bridge omitted — is left to the model.
fn chat_tool_choice(
    choice: &Value,
    function_names: &HashSet<String>,
    tool_map: &ResponsesToolMap,
) -> Result<Option<Value>, TransformError> {
    match choice {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none") => {
            Ok((!function_names.is_empty()).then(|| json!(choice)))
        }
        Value::String(choice) if choice == "required" && function_names.is_empty() => Ok(None),
        Value::String(choice) if choice == "required" => Ok(Some(json!(choice))),
        Value::Object(object)
            if matches!(
                object.get("type").and_then(Value::as_str),
                Some("function" | "custom")
            ) =>
        {
            let item_type = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut name = required_string(choice, "name", "tool_choice")?.to_string();
            if item_type == "function" {
                if let Some(namespace) = object.get("namespace").and_then(Value::as_str) {
                    name = flatten_namespace_tool_name(namespace, &name);
                }
            } else if !tool_map.is_custom(&name) {
                return Err(TransformError::invalid_request(format!(
                    "tool_choice references unknown custom tool {name}"
                )));
            }
            if !function_names.contains(&name) {
                return Ok(None);
            }
            Ok(Some(json!({
                "type": "function",
                "function": { "name": name },
            })))
        }
        Value::Object(object)
            if object.get("type").and_then(Value::as_str) == Some("namespace") =>
        {
            let namespace = required_string(choice, "name", "namespace tool_choice")?;
            if !tool_map.has_namespace(namespace) {
                return Err(TransformError::invalid_request(format!(
                    "tool_choice references unknown namespace {namespace}"
                )));
            }
            Ok(None)
        }
        _ => Err(TransformError::invalid_request(
            "tool_choice must be auto, none, required, or a named function or custom tool",
        )),
    }
}

/// Chat `response_format` for a Responses `text.format`; `text` (the default)
/// and anything unknown ask for nothing.
fn chat_response_format(format: &Map<String, Value>) -> Option<Value> {
    match format.get("type").and_then(Value::as_str)? {
        "json_object" => Some(json!({ "type": "json_object" })),
        "json_schema" => {
            let mut schema = Map::new();
            for key in ["name", "description", "schema", "strict"] {
                if let Some(value) = format.get(key) {
                    schema.insert(key.into(), value.clone());
                }
            }
            Some(json!({ "type": "json_schema", "json_schema": schema }))
        }
        _ => None,
    }
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
        "cache_control",
        "container",
        "context_management",
        "inference_geo",
        "mcp_servers",
        "metadata",
        "output_config",
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
            supported_endpoints: Vec::new(),
            format: ProviderFormat::Openai,
            engine: None,
            reasoning_format: None,
            reasoning_policy: None,
        };
        // (Anthropic, Embed) has no transform: that candidate is skipped and
        // the shapeable one still carries the request.
        let unshapeable = RouteCandidate {
            route_id: "anthropic:a".into(),
            supported_endpoints: Vec::new(),
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
            None,
        )
        .unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].0, "openai:b");

        // Only when EVERY candidate fails does the error surface — the same
        // 400 the all-or-nothing version produced.
        let err = build_candidates(&params, Endpoint::Embed, &[unshapeable], None, None);
        assert!(err.is_err());
    }

    #[test]
    fn responses_bridge_drops_what_chat_cannot_carry() {
        let bridge: RouteCandidate = serde_json::from_value(json!({
            "routeId": "chat:m",
            "format": "openai"
        }))
        .unwrap();
        let native: RouteCandidate = serde_json::from_value(json!({
            "routeId": "responses:m",
            "format": "openai",
            "supportedEndpoints": ["/v1/responses"]
        }))
        .unwrap();
        let routes = |request: &Value| {
            let converted = responses_to_chat_params(request);
            let (params, endpoint) = match &converted {
                Ok(chat) => (chat.clone(), Endpoint::ChatComplete),
                Err(_) => (request.clone(), Endpoint::CreateModelResponse),
            };
            build_candidates(
                &params,
                endpoint,
                &[bridge.clone(), native.clone()],
                None,
                Some(ResponsesCandidateInput {
                    original: request,
                    bridge_error: converted.as_ref().err(),
                }),
            )
            .unwrap()
        };

        // A stored prompt is the request's content: only a native route can serve it.
        let prompt = json!({ "model": "m", "input": "hello", "prompt": { "id": "pmpt_1" } });
        let served = routes(&prompt);
        assert_eq!(served.len(), 1);
        assert_eq!(served[0].0, "responses:m");
        assert_eq!(served[0].1, prompt);

        // Controls and content Chat has no counterpart for are dropped.
        for request in [
            json!({ "model": "m", "input": "hello", "truncation": "auto" }),
            json!({ "model": "m", "input": "hello", "modalities": ["audio"] }),
            json!({ "model": "m", "input": "hello", "include": ["file_search_call.results"] }),
            json!({ "model": "m", "input": "hello", "text": { "format": { "type": "future_format" } } }),
            json!({
                "model": "m",
                "input": [{
                    "type": "message", "role": "user",
                    "content": [
                        { "type": "input_text", "text": "hello" },
                        { "type": "input_file", "file_url": "https://example.com/a.pdf" }
                    ]
                }]
            }),
        ] {
            let served = routes(&request);
            assert_eq!(served[0].0, "chat:m", "{request}");
            assert_eq!(served[1].0, "responses:m", "{request}");
            assert!(served[0].1.to_string().contains("hello"), "{request}");
        }
    }

    #[test]
    fn responses_tool_output_images_follow_the_tool_messages() {
        let chat = responses_to_chat_params(&json!({
            "model": "m",
            "input": [
                { "type": "function_call", "call_id": "c1", "name": "view", "arguments": "{" },
                { "type": "function_call", "call_id": "c2", "name": "view", "arguments": "{}" },
                {
                    "type": "function_call_output", "call_id": "c1",
                    "output": [
                        { "type": "input_text", "text": "first" },
                        { "type": "input_image", "image_url": "https://x/a.png" }
                    ]
                },
                { "type": "function_call_output", "call_id": "c2", "output": "second" },
                { "type": "message", "role": "user", "content": "go on" }
            ]
        }))
        .unwrap();
        let roles: Vec<&str> = chat["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["assistant", "tool", "tool", "user", "user"]);
        // The model's own malformed arguments replay as it wrote them.
        assert_eq!(
            chat["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{"
        );
        assert!(chat["messages"][1]["content"]
            .as_str()
            .unwrap()
            .starts_with("first"));
        assert_eq!(
            chat["messages"][3]["content"],
            json!([{ "type": "image_url", "image_url": { "url": "https://x/a.png" } }])
        );
    }

    #[test]
    fn messages_bridge_carries_model_visible_input() {
        let request = json!({
            "model": "m",
            "max_tokens": 64,
            "system": [
                { "type": "text", "text": "", "cache_control": { "type": "ephemeral" } },
                { "type": "text", "text": "system", "cache_control": { "type": "ephemeral" } }
            ],
            "messages": [
                {
                    "role": "user",
                    "content": [{
                        "type": "document",
                        "title": "notes",
                        "source": { "type": "text", "media_type": "text/plain", "data": "body" }
                    }]
                },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "thinking", "thinking": "plan", "signature": "sig" },
                        { "type": "redacted_thinking", "data": "opaque" },
                        { "type": "text", "text": "calling" },
                        { "type": "tool_use", "id": "call_1", "name": "lookup", "input": { "q": "x" } },
                        { "type": "text", "text": "after" }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "continue" },
                        {
                            "type": "tool_result", "tool_use_id": "call_1", "is_error": true,
                            "cache_control": { "type": "ephemeral" },
                            "content": [
                                { "type": "text", "text": "failed" },
                                { "type": "image", "source": { "type": "url", "url": "https://x/a.png" } }
                            ]
                        }
                    ]
                }
            ],
            "tools": [{
                "name": "lookup",
                "description": "Lookup",
                "input_schema": { "type": "object" },
                "strict": true,
                "cache_control": { "type": "ephemeral" }
            }],
            "tool_choice": { "type": "any", "disable_parallel_tool_use": true },
            "metadata": { "user_id": "user-1" },
            "output_config": { "format": { "type": "json_schema", "schema": { "type": "object" } } },
            "cache_control": { "type": "ephemeral" },
            "context_management": { "edits": [] },
            "service_tier": "auto"
        });
        let body = transform_to_provider_request(
            ProviderFormat::Openai,
            &request,
            Endpoint::Messages,
            None,
        )
        .unwrap();
        assert_eq!(
            body["messages"],
            json!([
                { "role": "system", "content": "system" },
                { "role": "user", "content": "notes\n\nbody" },
                {
                    "role": "assistant",
                    "content": "calling\n\nafter",
                    "reasoning_content": "plan",
                    "tool_calls": [{
                        "id": "call_1", "type": "function",
                        "function": { "name": "lookup", "arguments": "{\"q\":\"x\"}" }
                    }]
                },
                {
                    "role": "tool", "tool_call_id": "call_1",
                    "content": "failed\n\n[The tool returned images; they follow in the next user message.]"
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "image_url", "image_url": { "url": "https://x/a.png" } },
                        { "type": "text", "text": "continue" }
                    ]
                }
            ])
        );
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["tools"][0]["function"]["strict"], true);
        assert_eq!(body["user"], "user-1");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        for dropped in [
            "cache_control",
            "is_error",
            "context_management",
            "service_tier",
        ] {
            assert!(
                !body.to_string().contains(dropped),
                "{dropped} reached Chat"
            );
        }

        // Tool controls go only with tools.
        let body = transform_to_provider_request(
            ProviderFormat::Openai,
            &json!({
                "model": "m", "max_tokens": 8,
                "messages": [{ "role": "user", "content": "hi" }],
                "tool_choice": { "type": "auto", "disable_parallel_tool_use": true }
            }),
            Endpoint::Messages,
            None,
        )
        .unwrap();
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn messages_bridge_drops_what_chat_cannot_carry() {
        let base = |extra: Value| {
            let mut request = json!({
                "model": "m", "max_tokens": 8,
                "messages": [{ "role": "user", "content": "hi" }]
            });
            request
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            request
        };
        for request in [
            base(json!({ "mcp_servers": [] })),
            base(json!({ "container": "c" })),
            base(json!({
                "tools": [
                    { "type": "web_search_20250305", "name": "web_search" },
                    { "name": "f", "input_schema": { "type": "object" } }
                ],
                "tool_choice": { "type": "tool", "name": "web_search" }
            })),
            base(json!({ "thinking": { "type": "enabled" } })),
            base(json!({ "output_config": { "format": { "type": "future" } } })),
            base(json!({
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "hi" },
                        { "type": "document", "source": { "type": "url", "url": "https://x/a.pdf" } },
                        { "type": "search_result", "source": "s", "title": "t", "content": [] }
                    ]
                }]
            })),
        ] {
            let chat =
                messages_to_chat_params(&request).unwrap_or_else(|err| panic!("{request}: {err}"));
            assert_eq!(
                chat["messages"],
                json!([{ "role": "user", "content": "hi" }])
            );
            assert!(chat.get("tool_choice").is_none(), "{chat}");
            assert!(chat.get("response_format").is_none(), "{chat}");
            if let Some(tools) = chat.get("tools") {
                assert_eq!(tools.as_array().unwrap().len(), 1, "{chat}");
            }
        }
        // A malformed request is still refused.
        let malformed = base(json!({
            "tools": [{ "name": "f", "input_schema": { "type": "object" } }],
            "tool_choice": { "type": "tool" }
        }));
        assert!(messages_to_chat_params(&malformed).is_err());

        // Routing keeps its order; a native route gets the request untouched.
        let request = base(json!({ "mcp_servers": [], "thinking": { "type": "adaptive" } }));
        let candidates: Vec<RouteCandidate> = serde_json::from_value(json!([
            { "routeId": "openai:m", "format": "openai" },
            { "routeId": "anthropic:m", "format": "anthropic" }
        ]))
        .unwrap();
        let bodies =
            build_candidates(&request, Endpoint::Messages, &candidates, None, None).unwrap();
        let routes: Vec<&str> = bodies.iter().map(|body| body.0.as_str()).collect();
        assert_eq!(routes, ["openai:m", "anthropic:m"]);
        assert!(bodies[0].1.get("mcp_servers").is_none());
        assert_eq!(bodies[1].1["thinking"], request["thinking"]);
    }

    #[test]
    fn messages_thinking_becomes_the_candidates_reasoning_dialect() {
        for (thinking, effort, expected, visible) in [
            (
                json!({ "type": "enabled", "budget_tokens": 1024 }),
                None,
                Some("low"),
                true,
            ),
            (
                json!({ "type": "enabled", "budget_tokens": 4096 }),
                None,
                Some("medium"),
                true,
            ),
            (
                json!({ "type": "enabled", "budget_tokens": 16000 }),
                None,
                Some("high"),
                true,
            ),
            (
                json!({ "type": "enabled", "budget_tokens": 16000 }),
                Some("low"),
                Some("low"),
                true,
            ),
            (json!({ "type": "adaptive" }), None, Some("high"), true),
            (json!({ "type": "enabled" }), None, Some("high"), true),
            (json!({ "type": "future" }), None, None, false),
            (
                json!({ "type": "adaptive", "display": "omitted" }),
                None,
                Some("high"),
                false,
            ),
            (
                json!({ "type": "enabled", "budget_tokens": 1024, "display": "summarized" }),
                None,
                Some("low"),
                true,
            ),
            (
                json!({ "type": "disabled" }),
                Some("high"),
                Some("none"),
                false,
            ),
            (Value::Null, Some("medium"), Some("medium"), false),
            (Value::Null, None, None, false),
        ] {
            let mut request = json!({
                "model": "m", "max_tokens": 8,
                "messages": [{ "role": "user", "content": "hi" }],
                "thinking": thinking
            });
            if let Some(effort) = effort {
                request["output_config"] = json!({ "effort": effort });
            }
            let (reasoning, shown) = messages_reasoning(&request);
            assert_eq!(shown, visible, "{request}");
            let candidates: Vec<RouteCandidate> = serde_json::from_value(json!([
                { "routeId": "vllm:m", "format": "openai", "reasoningFormat": "reasoning_effort" },
                { "routeId": "anthropic:m", "format": "anthropic" }
            ]))
            .unwrap();
            let bodies = build_candidates(
                &request,
                Endpoint::Messages,
                &candidates,
                reasoning.as_ref(),
                None,
            )
            .unwrap();
            assert_eq!(
                bodies[0].1.get("reasoning_effort").and_then(Value::as_str),
                expected,
                "{request}"
            );
            assert!(bodies[0].1.get("thinking").is_none());
            assert!(bodies[0].1.get("output_config").is_none());
            assert_eq!(bodies[1].1.get("thinking"), request.get("thinking"));
        }
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
                supported_endpoints: Vec::new(),
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
                supported_endpoints: Vec::new(),
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
                supported_endpoints: Vec::new(),
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
            None,
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
    fn configured_structured_output_without_tools_omits_both_token_limits() {
        let candidate: RouteCandidate = serde_json::from_value(json!({
            "routeId": "managed:m",
            "format": "openai",
            "reasoningFormat": "reasoning_effort",
            "reasoningPolicy": {
                "override": { "effort": "none" },
                "omitMaxTokens": true
            }
        }))
        .unwrap();
        let requested = ReasoningConfig {
            effort: Some(ReasoningEffort::High),
            ..Default::default()
        };

        let params = json!({
            "response_format": { "type": "json_schema" },
            "tools": [],
            "max_tokens": 2000,
            "max_completion_tokens": 2048
        });
        let bodies = build_candidates(
            &params,
            Endpoint::ChatComplete,
            std::slice::from_ref(&candidate),
            Some(&requested),
            None,
        )
        .unwrap();
        let body = &bodies[0].1;
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(params["max_tokens"], 2000);

        let with_tools = json!({
            "response_format": { "type": "json_schema" },
            "tools": [{}],
            "max_tokens": 2000
        });
        let bodies = build_candidates(
            &with_tools,
            Endpoint::ChatComplete,
            &[candidate],
            Some(&requested),
            None,
        )
        .unwrap();
        assert_eq!(bodies[0].1["max_tokens"], 2000);
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
                supported_endpoints: Vec::new(),
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
                None,
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
                None,
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
                None,
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
            None,
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
                None,
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
                build_candidates(&params, Endpoint::ChatComplete, &[candidate], None, None)
                    .unwrap();
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
                None,
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
            build_candidates(&callers, Endpoint::ChatComplete, &[anthropic], None, None).unwrap();
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
            None,
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
            None,
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
            None,
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
                supported_endpoints: Vec::new(),
                format: ProviderFormat::Openai,
                engine: None,
                reasoning_format: None,
                reasoning_policy: None,
            },
            RouteCandidate {
                route_id: "anthropic:m".into(),
                supported_endpoints: Vec::new(),
                format: ProviderFormat::Anthropic,
                engine: None,
                reasoning_format: None,
                reasoning_policy: None,
            },
        ];
        let bodies =
            build_candidates(&params, Endpoint::ChatComplete, &candidates, None, None).unwrap();
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
