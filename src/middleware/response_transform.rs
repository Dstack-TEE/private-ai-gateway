//! Response transforms that operate on a decoded 2xx body: the buffered
//! format conversions (Anthropic <-> OpenAI, etc.), and the shape operations
//! shared with the streaming path — `rewrite_identity` (stamp our id/model),
//! `exclude_reasoning`, and `canonicalize` (reduce to the documented output
//! schema and sanitize an in-band error). The streaming SSE machinery that
//! applies these per chunk lives in `stream_transform`; non-2xx responses are
//! normalized by `errors` before reaching here.
//!
//! Cost injection is a separate metering pass.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::request_transform::Endpoint;
use super::types::ProviderFormat;

const STRICT_OPENAI_COMPLIANCE: bool = true;

/// Transform a 2xx upstream body for `format`/`endpoint` into the client surface,
/// applying OpenAI compatibility normalization before any format conversion.
pub fn transform_response(format: ProviderFormat, endpoint: Endpoint, mut body: Value) -> Value {
    if format == ProviderFormat::Openai {
        normalize_reasoning_usage(&mut body);
    }
    use Endpoint::*;
    use ProviderFormat::*;
    match (format, endpoint) {
        (Anthropic, ChatComplete) => anthropic_chat_to_openai(body, STRICT_OPENAI_COMPLIANCE),
        (Anthropic, Complete) => anthropic_complete_to_openai(body),
        (Openai, Messages) => openai_to_anthropic_messages(body),
        // Native passthrough: openai chat/complete/embed, anthropic messages,
        // responses (createModelResponse).
        _ => body,
    }
}

/// Normalize the legacy reasoning-token usage alias into the OpenAI field.
///
/// Some OpenAI-compatible providers emit `usage.reasoning_tokens`, while clients
/// read `usage.completion_tokens_details.reasoning_tokens`. Promote the legacy
/// alias only when the canonical value is absent or null; a populated canonical
/// value remains authoritative. Preserve the alias and sibling detail fields.
pub(super) fn normalize_reasoning_usage(body: &mut Value) {
    if let Some(usage) = body.get_mut("usage") {
        let _ = normalize_reasoning_usage_value(usage);
    }
}

pub(super) fn normalize_reasoning_usage_value(usage: &mut Value) -> Option<bool> {
    let usage = usage.as_object_mut()?;
    let reasoning_tokens = usage
        .get("reasoning_tokens")
        .filter(|value| !value.is_null())
        .cloned()?;
    let details = usage
        .entry("completion_tokens_details")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if details.is_null() {
        *details = Value::Object(serde_json::Map::new());
    }
    let details = details.as_object_mut()?;
    if details.get("reasoning_tokens").is_none_or(Value::is_null) {
        details.insert("reasoning_tokens".into(), reasoning_tokens);
        return Some(true);
    }
    Some(false)
}

/// Remove reasoning traces from an OpenAI Chat Completions response while
/// leaving usage (including `completion_tokens_details.reasoning_tokens`)
/// untouched.
///
/// Must cover every field the canonical schema treats as reasoning output — the
/// allowlist keeps them all, so any this misses would survive an opt-out.
pub fn exclude_reasoning(body: &mut Value) {
    for choice in body
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        for container in ["message", "delta"] {
            if let Some(object) = choice.get_mut(container).and_then(Value::as_object_mut) {
                for key in [
                    "reasoning",
                    "reasoning_content",
                    "reasoning_details",
                    "thinking_blocks",
                    "reasoning_items",
                ] {
                    object.remove(key);
                }
            }
        }
    }
}

/// What the client is told about who served the request: our own request id and
/// the model the client asked for. Carried per request because both values are
/// per request; the streaming transform holds it behind an `Arc`.
#[derive(Debug, Clone)]
pub struct ResponseIdentity {
    pub request_id: String,
    /// `None` when the request named no model (embeddings on some surfaces), in
    /// which case whatever the upstream reported is left alone.
    pub user_model: Option<String>,
}

/// Rewrite the response identity — the upstream's `id` (its *format* varies
/// between servers, so it distinguishes one backend from another) and its `model`
/// (a server may report its own internal name where the client asked for the
/// catalog slug) — to values the client should see.
///
/// This is a *value* rewrite, distinct from `canonicalize`, which drops whole
/// fields. Two streaming shapes nest the identity one level down and are keyed off
/// the event `type`:
/// - `message_start`: Anthropic *streaming*. The identity is in `message`, where
///   the `OpenaiToAnthropicMessages` conversion (which runs first) puts the
///   upstream's values.
/// - `response.*`: Responses API *streaming*. Lifecycle events
///   (`response.created`/`response.completed`/…) nest the full response object —
///   and its `id`/`model` — under `response`; delta events carry neither.
///
/// Everything else takes the top-level `id`/`model`: every buffered body (OpenAI
/// chat/completion/responses, and the Anthropic message — which is
/// `type: "message"`), and any typed event carrying identity at the top level.
/// `rewrite_id_and_model` only *replaces* `id`/`model` where they already exist,
/// so an event without them (an Anthropic `content_block_delta`, a Responses delta
/// or `error` event) is untouched — and a `tool_use`/`item_id` or `content_block`
/// id, which is content identity rather than response identity, is never touched.
///
/// Only ever *replaces* `id`/`model`, never adds them: an Anthropic
/// `content_block_delta` legitimately carries neither, and inventing them would
/// corrupt the event.
pub fn rewrite_identity(body: &mut Value, identity: &ResponseIdentity) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        // Anthropic streaming: identity is in `message_start`'s `message`.
        Some("message_start") => {
            if let Some(message) = object.get_mut("message").and_then(Value::as_object_mut) {
                rewrite_id_and_model(message, identity);
            }
        }
        // Responses API streaming: lifecycle events nest identity in `response`.
        Some(t) if t.starts_with("response.") => {
            if let Some(inner) = object.get_mut("response").and_then(Value::as_object_mut) {
                rewrite_id_and_model(inner, identity);
            }
        }
        // Everything else — the buffered body (OpenAI chat/completion/responses, or
        // the Anthropic message, which is `type: "message"`) and any typed event
        // that carries its identity at the top level. `rewrite_id_and_model` only
        // replaces `id`/`model` where they already exist, so an event without them
        // (an Anthropic `content_block_delta`, a Responses delta) is left untouched.
        _ => rewrite_id_and_model(object, identity),
    }
}

/// Replace `id`/`model` where they already exist, on whichever object carries
/// the response identity.
fn rewrite_id_and_model(object: &mut serde_json::Map<String, Value>, identity: &ResponseIdentity) {
    if object.contains_key("id") {
        object.insert("id".to_string(), Value::String(identity.request_id.clone()));
    }
    if let (true, Some(model)) = (object.contains_key("model"), identity.user_model.as_ref()) {
        object.insert("model".to_string(), Value::String(model.clone()));
    }
}

// ── Canonical output schema (allowlist) ──────────────────────────────────────
//
// The client sees one shape regardless of which upstream served the request:
// only the fields this gateway documents as its output. A denylist can only
// remove what has been seen before and leaks anything new — an engine's next
// nonstandard field, a provider's verification blob, a trace event. An allowlist
// leaks nothing by construction: a field the schema does not name never reaches
// the client, whether or not we have seen it.
//
// This is why `rewrite_identity` no longer strips engine fields —
// `matched_stop` and its kind are simply not on the allowlist. Identity that is
// a *value*, not a field (`id`/`model`), is still rewritten there; the allowlist
// only decides which fields survive.
//
// Deliberately *not* included: `provider` (an aggregator like OpenRouter
// publishes which provider served a request; we hide it) and `system_fingerprint`
// (its whole purpose is to identify the serving backend configuration).

/// Every field the OpenAI chat-completion surface emits, by level. Cross-checked
/// against OpenRouter's documented response and litellm's `Message`/`Choices`/
/// `Usage` types. Extended only by a deliberate decision to support a new field,
/// never by what an upstream happens to send.
///
/// `reasoning*`/`thinking_blocks`/`reasoning_items` are reasoning extensions;
/// `audio`/`images` are multimodal outputs; `usage.cost`/`cost_details` are
/// injected by this gateway; `usage.reasoning_tokens` is a count some servers
/// report at the top of `usage`. The nested `*_tokens_details` objects are kept
/// whole so no token-breakdown sub-field is dropped.
///
/// `native_finish_reason` is intentionally absent: it duplicates `finish_reason`
/// with the upstream's raw value, and neither OpenRouter's schema nor litellm's
/// `Choices` type lists it. `provider_specific_fields` (litellm's catch-all for
/// upstream extras) is intentionally absent for the same reason it exists — it is
/// exactly where an upstream's identifying fields would ride.
const CHAT_TOP: &[&str] = &[
    "id",
    "object",
    "created",
    "model",
    "choices",
    "usage",
    "service_tier",
    // A streaming chunk may carry an in-band error (OpenRouter's schema lists it,
    // top level and per choice). It must survive canonicalization: the client
    // needs to see the error, and the downstream metering stage detects a failed
    // 200 stream by this field — dropping it would meter a failure as success.
    "error",
];
const CHAT_CHOICE: &[&str] = &[
    "index",
    "message",
    "delta",
    "finish_reason",
    "logprobs",
    "error",
];
const CHAT_MESSAGE: &[&str] = &[
    "role",
    "content",
    "name",
    "refusal",
    "tool_calls",
    "function_call",
    "annotations",
    "audio",
    "images",
    "reasoning",
    "reasoning_content",
    "reasoning_details",
    "thinking_blocks",
    "reasoning_items",
];
const CHAT_USAGE: &[&str] = &[
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "prompt_tokens_details",
    "completion_tokens_details",
    "reasoning_tokens",
    "server_tool_use",
    "server_tool_use_details",
    "cost",
    "cost_details",
];
/// The standard OpenAI error object. Its interior must be filtered like every
/// other level: OpenRouter nests `error.metadata.provider_name` and
/// `error.metadata.raw` (the raw upstream error, often a host/URL) inside it, and
/// keeping the whole object would relay exactly the upstream identity the rest of
/// this hides. The buffered error path rebuilds a fresh object for the same
/// reason; this keeps only these fields and scrubs the message.
const CHAT_ERROR: &[&str] = &["message", "type", "param", "code"];

/// The legacy `/v1/completions` (text completion) surface. OpenAI-standard and
/// small; cross-checked against litellm's `TextCompletionResponse`/`TextChoices`.
/// Reuses `CHAT_USAGE`/`CHAT_ERROR` (identical shapes). `system_fingerprint` and
/// `provider` are dropped for the same reason as on the chat surface.
const COMPLETION_TOP: &[&str] = &[
    "id", "object", "created", "model", "choices", "usage", "error",
];
const COMPLETION_CHOICE: &[&str] = &["text", "index", "logprobs", "finish_reason", "error"];

/// The `/v1/responses` (Responses API) surface. A larger shape that echoes request
/// parameters back and carries client-supplied `metadata`; the field set is
/// litellm's `ResponsesAPIResponse` (OpenRouter does not document this endpoint).
/// `output[]` items are content and left intact. `system_fingerprint`/`provider`
/// and any other unlisted upstream field are dropped. `usage` has its own shape.
const RESPONSES_TOP: &[&str] = &[
    "id",
    "created_at",
    "error",
    "incomplete_details",
    "instructions",
    "metadata",
    "model",
    "object",
    "output",
    "parallel_tool_calls",
    "temperature",
    "tool_choice",
    "tools",
    "top_p",
    "max_output_tokens",
    "previous_response_id",
    "reasoning",
    "status",
    "text",
    "truncation",
    "usage",
    "user",
    "store",
    // Not in litellm's model but a real, non-identifying OpenAI field
    // (`auto`/`default`/`flex`); kept for parity with `CHAT_TOP`.
    "service_tier",
];
const RESPONSES_USAGE: &[&str] = &[
    "input_tokens",
    "input_tokens_details",
    "output_tokens",
    "output_tokens_details",
    "total_tokens",
    "cost",
    "cost_details",
];

/// Reduce a response to the gateway's documented output schema for `endpoint`,
/// dropping every field the schema does not name. Runs last, after
/// `rewrite_identity` and cost injection, on both a buffered body and a single
/// streaming chunk.
///
/// Covers the three OpenAI-compatible surfaces (chat completions, legacy
/// completions, responses). The Anthropic Messages surface and embeddings are not
/// canonicalized here — Messages has its own shape handled by the format
/// conversion, and embeddings are not yet scoped.
pub fn canonicalize(body: &mut Value, endpoint: Endpoint) {
    match endpoint {
        Endpoint::ChatComplete => canonicalize_chat_completion(body),
        Endpoint::Complete => canonicalize_text_completion(body),
        Endpoint::CreateModelResponse => canonicalize_responses(body),
        Endpoint::Embed | Endpoint::Messages => {}
    }
}

fn canonicalize_text_completion(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    retain_allowed(object, COMPLETION_TOP);
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        retain_allowed(usage, CHAT_USAGE);
    }
    sanitize_error(object.get_mut("error"));
    for choice in object
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if let Some(choice) = choice.as_object_mut() {
            retain_allowed(choice, COMPLETION_CHOICE);
            sanitize_error(choice.get_mut("error"));
        }
    }
}

/// The Responses API is the one surface whose streaming events do *not* share
/// the final object's shape: a stream is a sequence of typed envelopes
/// (`{"type":"response.output_text.delta","delta":…}`,
/// `{"type":"response.completed","response":{…}}`, `{"type":"error",…}`), while
/// the buffered body *is* the response object (`{"object":"response",…}`, no
/// top-level `type`). Applying the final-object allowlist to an event envelope
/// would empty it and destroy the stream — and, because metering keys on the
/// envelope's `type`/`response.status`/`error`, misreport a successful stream as
/// failed. So dispatch on the envelope `type`:
/// - No `type`: the buffered response object — canonicalize it directly.
/// - `response.*` lifecycle: the full object is nested under `response` — that is
///   the only identity-bearing part; canonicalize it and leave the envelope's
///   control fields (`type`, `sequence_number`, …) untouched.
/// - Any other typed event (deltas, item/part events, `error`): a control/content
///   envelope carrying no response object — pass through verbatim.
fn canonicalize_responses(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some(t) if t.starts_with("response.") => {
            if let Some(inner) = object.get_mut("response").and_then(Value::as_object_mut) {
                canonicalize_responses_object(inner);
            }
        }
        Some(_) => {}
        None => canonicalize_responses_object(object),
    }
}

fn canonicalize_responses_object(object: &mut serde_json::Map<String, Value>) {
    retain_allowed(object, RESPONSES_TOP);
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        retain_allowed(usage, RESPONSES_USAGE);
    }
    // `output[]` items are content (message/reasoning/function_call, …) and left
    // intact; the identifying fields ride at the top level, dropped above.
    sanitize_error(object.get_mut("error"));
}

fn retain_allowed(object: &mut serde_json::Map<String, Value>, allowed: &[&str]) {
    object.retain(|key, _| allowed.contains(&key.as_str()));
}

fn canonicalize_chat_completion(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    retain_allowed(object, CHAT_TOP);
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        retain_allowed(usage, CHAT_USAGE);
    }
    // The allowlist keeps the `error` field (so the client sees an in-band error
    // and metering classifies the stream), but the error object's interior is
    // upstream-controlled — filter it to the standard fields and scrub its
    // message, so no `metadata.provider_name`/`raw` or unknown sub-field leaks.
    sanitize_error(object.get_mut("error"));
    for choice in object
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let Some(choice) = choice.as_object_mut() else {
            continue;
        };
        retain_allowed(choice, CHAT_CHOICE);
        sanitize_error(choice.get_mut("error"));
        // A buffered choice carries `message`; a streaming choice carries `delta`.
        for container in ["message", "delta"] {
            if let Some(part) = choice.get_mut(container).and_then(Value::as_object_mut) {
                retain_allowed(part, CHAT_MESSAGE);
            }
        }
    }
}

/// Make an in-band error client-safe. An object error is filtered to the standard
/// fields (dropping `metadata` and any other upstream-controlled sub-field) and
/// its message scrubbed; a bare-string error is scrubbed. Both shapes match what
/// the buffered path accepts; any other shape is left untouched.
fn sanitize_error(error: Option<&mut Value>) {
    match error {
        Some(Value::String(text)) => {
            *text = crate::middleware::errors::client_safe_error_text(text);
        }
        Some(Value::Object(object)) => {
            retain_allowed(object, CHAT_ERROR);
            if let Some(Value::String(message)) = object.get_mut("message") {
                *message = crate::middleware::errors::client_safe_error_text(message);
            }
        }
        _ => {}
    }
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// Read a token count, accepting integer- or float-encoded numbers.
pub(super) fn i64_field(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

// Anthropic stop_reason -> OpenAI finish_reason (strict compliance maps it).
pub(super) fn transform_finish_reason(stop_reason: Option<&str>, strict: bool) -> String {
    let Some(reason) = stop_reason else {
        return "stop".to_string();
    };
    if !strict {
        return reason.to_string();
    }
    match reason {
        "stop_sequence" | "end_turn" | "pause_turn" => "stop",
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        _ => "stop",
    }
    .to_string()
}

// OpenAI finish_reason -> Anthropic stop_reason (downstream surface).
pub(super) fn map_finish_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        // stop, content_filter, missing, and anything else collapse to end_turn.
        _ => "end_turn",
    }
}

fn anthropic_chat_to_openai(response: Value, strict: bool) -> Value {
    let empty = Vec::new();
    let content_items = response
        .get("content")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let mut content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for item in content_items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    content.push_str(text);
                }
            }
            Some("tool_use") => {
                let arguments = serde_json::to_string(item.get("input").unwrap_or(&Value::Null))
                    .unwrap_or_else(|_| "null".to_string());
                tool_calls.push(json!({
                    "id": item.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": item.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": arguments,
                    },
                }));
            }
            _ => {}
        }
    }

    let usage_src = response.get("usage").unwrap_or(&Value::Null);
    let input = i64_field(usage_src, "input_tokens");
    let output = i64_field(usage_src, "output_tokens");
    let cache_creation = i64_field(usage_src, "cache_creation_input_tokens");
    let cache_read = i64_field(usage_src, "cache_read_input_tokens");
    let mut usage = json!({
        "prompt_tokens": input + cache_creation + cache_read,
        "completion_tokens": output,
        "total_tokens": input + output + cache_creation + cache_read,
    });
    // Echo the cache buckets only when present in the source: spread the raw
    // fields and drop undefined ones.
    if cache_creation != 0 || cache_read != 0 {
        let map = usage.as_object_mut().unwrap();
        if let Some(value) = usage_src.get("cache_read_input_tokens") {
            map.insert("cache_read_input_tokens".into(), value.clone());
        }
        if let Some(value) = usage_src.get("cache_creation_input_tokens") {
            map.insert("cache_creation_input_tokens".into(), value.clone());
        }
    }

    let mut message = json!({ "role": "assistant", "content": content });
    if !tool_calls.is_empty() {
        message
            .as_object_mut()
            .unwrap()
            .insert("tool_calls".into(), Value::Array(tool_calls));
    }
    // When not strict, the raw Anthropic blocks (minus tool_use) are attached as
    // a content_blocks extension. Strict mode (the default) omits them.
    if !strict {
        let blocks: Vec<Value> = content_items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) != Some("tool_use"))
            .cloned()
            .collect();
        message
            .as_object_mut()
            .unwrap()
            .insert("content_blocks".into(), Value::Array(blocks));
    }

    let stop_reason = response.get("stop_reason").and_then(Value::as_str);
    json!({
        "id": response.get("id").cloned().unwrap_or(Value::Null),
        "object": "chat.completion",
        "created": now_secs(),
        "model": response.get("model").cloned().unwrap_or(Value::Null),
        "provider": "anthropic",
        "choices": [{
            "message": message,
            "index": 0,
            "logprobs": Value::Null,
            "finish_reason": transform_finish_reason(stop_reason, strict),
        }],
        "usage": usage,
    })
}

fn anthropic_complete_to_openai(response: Value) -> Value {
    json!({
        "id": response.get("log_id").cloned().unwrap_or(Value::Null),
        "object": "text_completion",
        "created": now_secs(),
        "model": response.get("model").cloned().unwrap_or(Value::Null),
        "provider": "anthropic",
        "choices": [{
            "text": response.get("completion").cloned().unwrap_or(Value::Null),
            "index": 0,
            "logprobs": Value::Null,
            "finish_reason": response.get("stop_reason").cloned().unwrap_or(Value::Null),
        }],
    })
}

fn openai_to_anthropic_messages(response: Value) -> Value {
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or(Value::Null);
    let message = choice.get("message").cloned().unwrap_or(Value::Null);

    let mut content: Vec<Value> = Vec::new();
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            let arguments = call
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let input: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").cloned().unwrap_or(Value::Null),
                "name": call.get("function").and_then(|f| f.get("name")).cloned().unwrap_or(Value::Null),
                "input": input,
            }));
        }
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }

    let response_usage = response.get("usage");
    let mut usage = json!({
        "input_tokens": response_usage.map(|u| i64_field(u, "prompt_tokens")).unwrap_or(0),
        "output_tokens": response_usage.map(|u| i64_field(u, "completion_tokens")).unwrap_or(0),
    });
    if let Some(u) = response_usage {
        let map = usage.as_object_mut().unwrap();
        if let Some(cache_read) = u.get("cache_read_input_tokens") {
            map.insert("cache_read_input_tokens".into(), cache_read.clone());
        }
        if let Some(cache_creation) = u.get("cache_creation_input_tokens") {
            map.insert("cache_creation_input_tokens".into(), cache_creation.clone());
        }
    }

    let id = match response.get("id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!("msg_{}", now_millis()),
    };
    let stop_reason = map_finish_reason(choice.get("finish_reason").and_then(Value::as_str));
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": response.get("model").cloned().unwrap_or(Value::Null),
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": usage,
    })
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use serde_json::json;

    fn identity() -> ResponseIdentity {
        ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        }
    }

    /// The two steps together, as the buffered path runs them: identity rewrite
    /// then canonicalize. The upstream id/model become ours, and the engine field
    /// `matched_stop` is gone — dropped by the allowlist, not named anywhere.
    #[test]
    fn rewrites_identity_and_canonicalizes_a_chat_completion() {
        let mut body = json!({
            "id": "7bdaaade50304502b0fe7e66e9a4bec2",
            "object": "chat.completion",
            "model": "vendor-model-int",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "hi" },
                "finish_reason": "stop",
                "matched_stop": 424242
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        rewrite_identity(&mut body, &identity());
        canonicalize(&mut body, Endpoint::ChatComplete);
        assert_eq!(body["id"], "req_ours");
        assert_eq!(body["model"], "acme/model-a");
        assert!(body["choices"][0].get("matched_stop").is_none());
        // Everything on the allowlist is left alone.
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["choices"][0]["message"]["content"], "hi");
        assert_eq!(body["usage"]["completion_tokens"], 1);
    }

    /// `rewrite_identity` alone is a value rewrite only — it no longer drops
    /// fields, so an engine field survives it and is left to `canonicalize`.
    #[test]
    fn identity_rewrite_does_not_drop_fields() {
        let mut body = json!({
            "id": "x",
            "choices": [{ "index": 0, "matched_stop": 424242 }]
        });
        rewrite_identity(&mut body, &identity());
        assert_eq!(body["id"], "req_ours");
        assert_eq!(body["choices"][0]["matched_stop"], 424242);
    }

    /// The trap: Anthropic Messages carries a top-level `stop_reason` that is
    /// part of its contract. The identity rewrite never touches it, and the
    /// Messages surface is not canonicalized against the chat-completion schema.
    #[test]
    fn keeps_the_anthropic_top_level_stop_reason() {
        let mut body = json!({
            "id": "msg_upstream",
            "type": "message",
            "model": "claude-x",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "hi" }]
        });
        rewrite_identity(&mut body, &identity());
        canonicalize(&mut body, Endpoint::Messages);
        assert_eq!(body["stop_reason"], "end_turn");
        assert_eq!(body["id"], "req_ours");
    }

    /// A choice-level `stop_reason` (an engine's, not the Anthropic contract's)
    /// is dropped by the chat-completion allowlist.
    #[test]
    fn canonicalize_drops_a_choice_level_stop_reason() {
        let mut body = json!({
            "id": "x",
            "choices": [{ "index": 0, "finish_reason": "stop", "stop_reason": 424242 }]
        });
        canonicalize(&mut body, Endpoint::ChatComplete);
        assert!(body["choices"][0].get("stop_reason").is_none());
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
    }

    /// The allowlist drops fields never seen before — a provider verification
    /// blob, a trace, an engine field — without naming them, and keeps `usage.cost`
    /// (injected by us) and a streaming `delta`.
    #[test]
    fn canonicalize_drops_unknown_fields_and_keeps_the_contract() {
        let mut body = json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "m",
            "provider": "SomeProvider",
            "system_fingerprint": "fp_backend_1",
            "vendor_verification": { "sig": "x" },
            "x_vendor_trace": "x",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "hi", "vendor_ext": 1 },
                "finish_reason": null,
                "matched_stop": 1
            }],
            "usage": { "completion_tokens": 2, "cost": 0.01, "reasoning_tokens": 1, "vendor_meta": 9 }
        });
        canonicalize(&mut body, Endpoint::ChatComplete);
        let top: Vec<&String> = body.as_object().unwrap().keys().collect();
        assert!(
            !top.iter().any(|k| [
                "provider",
                "system_fingerprint",
                "vendor_verification",
                "x_vendor_trace"
            ]
            .contains(&k.as_str())),
            "leaked: {top:?}"
        );
        assert!(body["choices"][0].get("matched_stop").is_none());
        assert!(body["choices"][0]["delta"].get("vendor_ext").is_none());
        assert!(body["usage"].get("vendor_meta").is_none());
        // Contract fields survive.
        assert_eq!(body["choices"][0]["delta"]["content"], "hi");
        assert_eq!(body["usage"]["cost"], 0.01);
        assert_eq!(body["usage"]["reasoning_tokens"], 1);
    }

    /// Multimodal and reasoning output fields cross-checked against OpenRouter and
    /// litellm must survive — dropping them would break audio, image, and
    /// thinking-block responses. Locks the allowlist against a too-narrow trim.
    #[test]
    fn canonicalize_keeps_multimodal_and_reasoning_output() {
        let mut body = json!({
            "id": "x",
            "object": "chat.completion",
            "service_tier": "default",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "audio": { "id": "a", "data": "…" },
                    "images": [{ "type": "image_url", "image_url": { "url": "…" } }],
                    "reasoning": "…",
                    "thinking_blocks": [{ "type": "thinking", "thinking": "…" }],
                    "reasoning_items": [{ "type": "reasoning" }],
                    "name": "assistant-1"
                },
                "finish_reason": "stop"
            }],
            "usage": { "completion_tokens": 1, "server_tool_use": { "web_search": 1 } }
        });
        canonicalize(&mut body, Endpoint::ChatComplete);
        assert_eq!(body["service_tier"], "default");
        let msg = &body["choices"][0]["message"];
        for field in [
            "audio",
            "images",
            "reasoning",
            "thinking_blocks",
            "reasoning_items",
            "name",
        ] {
            assert!(msg.get(field).is_some(), "dropped message.{field}");
        }
        assert!(body["usage"].get("server_tool_use").is_some());
    }

    /// An in-band error frame on a chat-completion stream must survive
    /// canonicalization, or the client loses the error and the downstream meter
    /// (which keys on top-level `error`) records a failed stream as a success. A
    /// clean message is kept verbatim.
    #[test]
    fn canonicalize_keeps_an_in_band_stream_error() {
        let mut body = json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "error": { "message": "context length exceeded", "code": 400 }
        });
        canonicalize(&mut body, Endpoint::ChatComplete);
        assert_eq!(body["error"]["message"], "context length exceeded");
        assert_eq!(body["error"]["code"], 400);
    }

    /// The error object's interior is filtered like every other level: the message
    /// is scrubbed, standard fields (`code`) are kept, and upstream-identifying
    /// sub-fields — `metadata.provider_name`, `metadata.raw` — are dropped, not
    /// just the message. Checked top-level and per choice.
    #[test]
    fn canonicalize_scrubs_an_identifying_in_band_error() {
        let mut body = json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "error": {
                "message": "backend 10.0.0.7 refused the connection",
                "code": 502,
                "metadata": { "provider_name": "SomeProvider", "raw": "upstream https://api.acme.ai 502" }
            },
            "choices": [{
                "index": 0,
                "error": { "message": "no route to inference-7.internal.acme.io", "metadata": { "provider_name": "X" } }
            }]
        });
        canonicalize(&mut body, Endpoint::ChatComplete);
        assert_eq!(body["error"]["message"], "the provider returned an error");
        assert_eq!(body["error"]["code"], 502); // standard field kept
        assert!(
            body["error"].get("metadata").is_none(),
            "leaked error.metadata"
        );
        assert_eq!(
            body["choices"][0]["error"]["message"],
            "the provider returned an error"
        );
        assert!(body["choices"][0]["error"].get("metadata").is_none());
    }

    /// Some providers send a bare-string `error` rather than an object; it is
    /// scrubbed too, matching what the buffered error path accepts.
    #[test]
    fn canonicalize_scrubs_a_bare_string_error() {
        let mut body = json!({
            "id": "x",
            "error": "cannot connect to host files.example.com:443"
        });
        canonicalize(&mut body, Endpoint::ChatComplete);
        assert_eq!(body["error"], "the provider returned an error");
    }

    /// Opting out of reasoning must drop every reasoning field the allowlist
    /// keeps — including `thinking_blocks` and `reasoning_items`, which an earlier
    /// version of `exclude_reasoning` missed while canonicalize kept them.
    #[test]
    fn exclude_reasoning_covers_every_allowlisted_reasoning_field() {
        let mut body = json!({
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "hi",
                    "reasoning": "…",
                    "reasoning_content": "…",
                    "reasoning_details": [{}],
                    "thinking_blocks": [{ "type": "thinking" }],
                    "reasoning_items": [{}]
                }
            }]
        });
        exclude_reasoning(&mut body);
        let msg = body["choices"][0]["message"].as_object().unwrap();
        for field in [
            "reasoning",
            "reasoning_content",
            "reasoning_details",
            "thinking_blocks",
            "reasoning_items",
        ] {
            assert!(!msg.contains_key(field), "leaked {field} after opt-out");
        }
        assert_eq!(msg["content"], "hi");
    }

    /// A surface without a defined schema (Anthropic Messages, embeddings) is
    /// passed through untouched.
    #[test]
    fn canonicalize_is_a_noop_for_unschematized_surfaces() {
        let original = json!({ "id": "x", "anything": true, "choices": [] });
        for endpoint in [Endpoint::Messages, Endpoint::Embed] {
            let mut body = original.clone();
            canonicalize(&mut body, endpoint);
            assert_eq!(body, original, "mutated for {endpoint:?}");
        }
    }

    /// Legacy `/v1/completions`: engine and identity fields are dropped, the
    /// OpenAI-standard text choice survives.
    #[test]
    fn canonicalize_text_completion() {
        let mut body = json!({
            "id": "x",
            "object": "text_completion",
            "created": 1,
            "model": "m",
            "system_fingerprint": "fp_backend",
            "provider": "SomeProvider",
            "choices": [{
                "text": "hello",
                "index": 0,
                "finish_reason": "stop",
                "logprobs": null,
                "matched_stop": 1
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "cost": 0.01 }
        });
        canonicalize(&mut body, Endpoint::Complete);
        assert!(body.get("system_fingerprint").is_none());
        assert!(body.get("provider").is_none());
        assert!(body["choices"][0].get("matched_stop").is_none());
        assert_eq!(body["choices"][0]["text"], "hello");
        assert_eq!(body["usage"]["cost"], 0.01);
    }

    /// `/v1/responses`: identity/unknown top-level fields are dropped, but the
    /// request-echo params, client `metadata`, and `output[]` content survive, and
    /// the Responses-shaped `usage` (`input_tokens`/`output_tokens`) is kept.
    #[test]
    fn canonicalize_responses() {
        let mut body = json!({
            "id": "resp_x",
            "object": "response",
            "created_at": 1,
            "model": "m",
            "system_fingerprint": "fp_backend",
            "provider": "SomeProvider",
            "status": "completed",
            "metadata": { "client_tag": "abc" },
            "temperature": 0.7,
            "tools": [{ "type": "function" }],
            "previous_response_id": "resp_prev",
            "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "hi" }] }],
            "usage": { "input_tokens": 3, "output_tokens": 2, "total_tokens": 5, "cost": 0.02 }
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse);
        // Dropped: upstream identity and unknowns.
        assert!(body.get("system_fingerprint").is_none());
        assert!(body.get("provider").is_none());
        // Kept: request-echo, client metadata, output content, responses usage.
        assert_eq!(body["metadata"]["client_tag"], "abc");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["previous_response_id"], "resp_prev");
        assert_eq!(body["output"][0]["content"][0]["text"], "hi");
        assert_eq!(body["usage"]["input_tokens"], 3);
        assert_eq!(body["usage"]["cost"], 0.02);
    }

    /// `/v1/responses` streaming: a lifecycle event carries the response object
    /// nested under `response`. Its identity is rewritten and its interior
    /// canonicalized, while the envelope's own control fields (`type`,
    /// `sequence_number`) — which metering keys on — are left intact.
    #[test]
    fn canonicalize_responses_lifecycle_event() {
        let mut body = json!({
            "type": "response.completed",
            "sequence_number": 42,
            "response": {
                "id": "resp_upstream",
                "object": "response",
                "model": "upstream-internal",
                "system_fingerprint": "fp_backend",
                "provider": "SomeProvider",
                "status": "completed",
                "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "hi" }] }],
                "usage": { "input_tokens": 3, "output_tokens": 2 }
            }
        });
        rewrite_identity(&mut body, &identity());
        canonicalize(&mut body, Endpoint::CreateModelResponse);
        // Envelope control fields survive untouched (metering depends on them).
        assert_eq!(body["type"], "response.completed");
        assert_eq!(body["sequence_number"], 42);
        // Nested object: identity rewritten, upstream markers dropped, content kept.
        assert_eq!(body["response"]["id"], "req_ours");
        assert_eq!(body["response"]["model"], "acme/model-a");
        assert!(body["response"].get("system_fingerprint").is_none());
        assert!(body["response"].get("provider").is_none());
        assert_eq!(body["response"]["status"], "completed");
        assert_eq!(body["response"]["output"][0]["content"][0]["text"], "hi");
    }

    /// `/v1/responses` streaming: a delta and a typed `error` event are not the
    /// response object; applying the final-object allowlist would empty them.
    /// They must pass through verbatim so the client sees content and errors and
    /// metering can classify the stream.
    #[test]
    fn canonicalize_responses_passes_through_non_object_events() {
        for event in [
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 0,
                "content_index": 0,
                "delta": "hi",
                "sequence_number": 3
            }),
            json!({
                "type": "error",
                "code": "server_error",
                "message": "boom",
                "sequence_number": 9
            }),
        ] {
            let mut body = event.clone();
            rewrite_identity(&mut body, &identity());
            canonicalize(&mut body, Endpoint::CreateModelResponse);
            assert_eq!(body, event, "envelope was mutated: {event}");
        }
    }

    /// Anthropic streaming events carry neither field; inventing them would
    /// corrupt the event.
    #[test]
    fn never_adds_id_or_model_to_an_event_that_has_neither() {
        let mut body = json!({ "type": "content_block_delta", "index": 0 });
        rewrite_identity(&mut body, &identity());
        assert_eq!(body, json!({ "type": "content_block_delta", "index": 0 }));
    }

    /// Anthropic streaming hides the response identity one level down, in the
    /// `message` of a `message_start` event — where the format conversion put the
    /// upstream's own id and internal model name. The top-level-only rewrite
    /// missed it, leaking both to a `/v1/messages` client of an OpenAI upstream.
    #[test]
    fn rewrites_the_nested_identity_of_an_anthropic_message_start() {
        let mut body = json!({
            "type": "message_start",
            "message": {
                "id": "chatcmpl-7bdaaade5030",
                "type": "message",
                "role": "assistant",
                "model": "vendor-model-int",
                "content": [],
            },
        });
        rewrite_identity(&mut body, &identity());
        assert_eq!(body["message"]["id"], "req_ours");
        assert_eq!(body["message"]["model"], "acme/model-a");
        // The rest of the event is untouched.
        assert_eq!(body["message"]["role"], "assistant");
    }

    /// A `tool_use` id is content identity, not response identity: the
    /// message_start branch must not reach into other events and rewrite it.
    #[test]
    fn leaves_a_tool_use_id_in_a_content_block_alone() {
        let mut body = json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": { "type": "tool_use", "id": "toolu_abc", "name": "f" },
        });
        let before = body.clone();
        rewrite_identity(&mut body, &identity());
        assert_eq!(body, before);
    }

    /// No requested model (some embedding surfaces) leaves the upstream's alone
    /// rather than blanking it.
    #[test]
    fn leaves_model_alone_when_the_request_named_none() {
        let mut body = json!({ "id": "x", "model": "whatever" });
        rewrite_identity(
            &mut body,
            &ResponseIdentity {
                request_id: "req_ours".to_string(),
                user_model: None,
            },
        );
        assert_eq!(body["model"], "whatever");
        assert_eq!(body["id"], "req_ours");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_messages_empty_content_gets_placeholder() {
        let body = json!({
            "id": "c", "model": "gpt-4",
            "choices": [{ "message": { "role": "assistant" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 0 }
        });
        let out = transform_response(ProviderFormat::Openai, Endpoint::Messages, body);
        assert_eq!(out["content"], json!([{ "type": "text", "text": "" }]));
        assert_eq!(out["stop_reason"], json!("end_turn"));
    }

    #[test]
    fn reasoning_exclusion_preserves_usage() {
        let mut body = json!({"choices":[{"message":{"content":"ok","reasoning":"secret"}}],
            "usage":{"completion_tokens_details":{"reasoning_tokens":3}}});
        exclude_reasoning(&mut body);
        assert!(body["choices"][0]["message"].get("reasoning").is_none());
        assert_eq!(
            body["usage"]["completion_tokens_details"]["reasoning_tokens"],
            3
        );
    }

    #[test]
    fn legacy_reasoning_usage_is_normalized_without_losing_canonical_data() {
        let body = json!({
            "choices": [],
            "usage": {
                "completion_tokens": 128,
                "reasoning_tokens": 128,
                "completion_tokens_details": null
            }
        });
        let out = transform_response(ProviderFormat::Openai, Endpoint::ChatComplete, body);
        assert_eq!(
            out["usage"]["completion_tokens_details"]["reasoning_tokens"],
            128
        );
        assert_eq!(out["usage"]["reasoning_tokens"], 128);

        let canonical = transform_response(
            ProviderFormat::Openai,
            Endpoint::ChatComplete,
            json!({
                "usage": {
                    "reasoning_tokens": 128,
                    "completion_tokens_details": {
                        "accepted_prediction_tokens": 4,
                        "reasoning_tokens": 7
                    }
                }
            }),
        );
        assert_eq!(
            canonical["usage"]["completion_tokens_details"]["reasoning_tokens"],
            7
        );
        assert_eq!(
            canonical["usage"]["completion_tokens_details"]["accepted_prediction_tokens"],
            4
        );
    }
}
