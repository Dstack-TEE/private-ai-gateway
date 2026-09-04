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

use super::errors::{
    chat_gateway_error, client_safe_error_text, error_type, is_actionable_client_error,
    is_quota_exhausted_error, looks_identifying, map_upstream_status, responses_error_event,
    responses_gateway_code, upstream_message, Surface,
};
use super::request_transform::{effective_responses_tools, Endpoint, ResponsesToolMap};
use super::types::ProviderFormat;
use crate::error_payload::envelope;

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
// Deliberately *not* included: `provider` (an aggregator that publishes which
// backend served a request emits this; we do not) and `system_fingerprint` (its
// whole purpose is to identify the serving backend configuration).

/// Every field the OpenAI chat-completion surface emits, by level. Cross-checked
/// against the documented OpenAI response and the common OpenAI-compatible
/// supersets of it. Extended only by a deliberate decision to support a new field,
/// never by what an upstream happens to send.
///
/// `reasoning*`/`thinking_blocks`/`reasoning_items` are reasoning extensions;
/// `audio`/`images` are multimodal outputs; `usage.cost`/`cost_details` are
/// injected by this gateway; `usage.reasoning_tokens` is a count some servers
/// report at the top of `usage`. The nested `*_tokens_details` objects are kept
/// whole so no token-breakdown sub-field is dropped.
///
/// `native_finish_reason` is intentionally absent: it duplicates `finish_reason`
/// with the upstream's raw value, and the standard schemas do not list it. A
/// catch-all "provider-specific fields" object (as some compatibility layers
/// expose) is intentionally absent for the same reason it exists — it is exactly
/// where an upstream's identifying fields would ride.
const CHAT_TOP: &[&str] = &[
    "id",
    "object",
    "created",
    "model",
    "choices",
    "usage",
    "service_tier",
    // A streaming chunk may carry an in-band error (top level and per choice). It
    // must survive canonicalization: the client
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
    // Prompt-cache counters, both public wire dialects (read/creation and
    // hit/miss). Stripping them hid real cache activity from
    // streaming clients — and from billing's cache_hit flag.
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
    "prompt_cache_hit_tokens",
    "prompt_cache_miss_tokens",
    "reasoning_tokens",
    "server_tool_use",
    "server_tool_use_details",
    "cost",
    "cost_details",
];
// ── In-band error kinds (allowlist) ──────────────────────────────────────────
//
// An upstream error's `type`/`code` is a claim about whose problem it is, and
// the answer is not always the caller's: an account- or credential-level kind
// (`insufficient_quota`, `authentication_error`) describes the gateway's own
// standing with the upstream, and relaying it misattributes the failure to the
// caller. The HTTP layer draws the same line by status
// (`is_actionable_client_error`); an in-band error carries no status, so it is
// drawn over kinds — as an allowlist, for the same reason the field schema is
// one: a denylist relays every kind not seen before.
//
// A kind is relayed only when the caller can act on it: fix the request, or
// retry. Everything else folds into the gateway's own vocabulary via
// `suppressed_error_status`. One table for both surfaces; they barely overlap
// and a stray value from the other surface is harmless.

/// In-band error kinds a client can act on, and so worth relaying.
const RELAYABLE_ERROR_KINDS: &[&str] = &[
    // Malformed, unsupported, or aimed at something that is not there.
    "invalid_request_error",
    "not_found_error",
    "model_not_found",
    "invalid_value",
    "invalid_type",
    "unsupported_value",
    "unsupported_parameter",
    "missing_required_parameter",
    "unknown_parameter",
    // Does not fit the model.
    "context_length_exceeded",
    "string_above_max_length",
    "request_too_large",
    // The request's own content was refused or could not be read.
    "invalid_prompt",
    "content_filter",
    "content_policy_violation",
    "invalid_image",
    "invalid_image_format",
    "invalid_image_url",
    "invalid_base64_image",
    "invalid_image_mode",
    "image_parse_error",
    "image_too_large",
    "image_too_small",
    "image_file_too_large",
    "image_file_not_found",
    "image_content_policy_violation",
    "empty_image_file",
    "unsupported_image_media_type",
    "failed_to_download_image",
    // Not the caller's fault, but retryable — and it names the service's
    // load, not an account.
    "overloaded_error",
    "vector_store_timeout",
];

/// Whether an in-band error may be relayed at all.
///
/// The kind decides it, except when the error body says the provider is
/// refusing because our account with it is unpaid. Some providers report that
/// under a kind this allowlist recognizes — the same `invalid_request_error`
/// they use for real request faults — and its message names a balance rather
/// than a host, so the identifying-marker scrub has nothing to catch and would
/// relay it verbatim. The HTTP path classifies the identical body; both must
/// reach the same answer or one provider error is suppressed on one path and
/// leaked on the other.
fn is_relayable_error(error: &serde_json::Map<String, Value>) -> bool {
    !is_quota_exhausted_error(error) && is_relayable_kind(effective_error_kind(error))
}

/// The status a suppressed in-band error folds to.
///
/// An account refusal is ours whatever kind the provider filed it under, so it
/// takes the status an upstream 402 takes, directly. Deferring to
/// `suppressed_status` would let the `type` slot's fallback reclassify it — a
/// provider that files this under `invalid_request_error` would have the
/// caller told their request was invalid, when no request could have succeeded.
fn suppressed_status_for(
    object: &serde_json::Map<String, Value>,
    type_slot: Option<&Value>,
) -> u16 {
    if is_quota_exhausted_error(object) {
        return map_upstream_status(402);
    }
    suppressed_status(effective_error_kind(object), type_slot)
}

/// Whether an in-band error's kind is safe to relay. A string kind takes the
/// allowlist. A numeric kind is an HTTP status by another name and takes the
/// HTTP rule verbatim, so the in-band and buffered paths cannot disagree about
/// the same status. An absent or null kind claims nothing about whose fault it
/// is, and leaves the scrubbed message as the only thing relayed.
fn is_relayable_kind(kind: Option<&Value>) -> bool {
    match kind {
        None | Some(Value::Null) => true,
        Some(Value::String(kind)) => RELAYABLE_ERROR_KINDS.contains(&kind.as_str()),
        Some(kind @ Value::Number(_)) => {
            numeric_status(kind).is_some_and(is_actionable_client_error)
        }
        _ => false,
    }
}

/// A numeric kind read as an HTTP status.
fn numeric_status(kind: &Value) -> Option<u16> {
    kind.as_u64().and_then(|status| u16::try_from(status).ok())
}

/// The message to send: the upstream's string scrubbed, or the generic line
/// when there is none — a structured or missing message still owes the client
/// one.
fn client_error_message(message: Option<&Value>) -> String {
    message
        .and_then(Value::as_str)
        .map_or_else(|| upstream_message(502).to_string(), client_safe_error_text)
}

/// The kind that decides relay-or-suppress for an error object: a specific
/// `code` outranks the often-generic `type` — upstreams pair a precise code
/// (`invalid_image`) with a catch-all type (`server_error`), and judging both
/// would suppress exactly the errors the allowlist exists to relay.
fn effective_error_kind(object: &serde_json::Map<String, Value>) -> Option<&Value> {
    object
        .get("code")
        .filter(|code| !code.is_null())
        .or_else(|| object.get("type"))
}

/// The `param` value to send: a string naming the request parameter at fault,
/// per the surface contracts. A structured value or an identifying string
/// (URL, host, opaque id) becomes null rather than reach the client.
fn client_error_param(param: Option<&Value>) -> Option<String> {
    let text = param?.as_str()?;
    (!looks_identifying(text)).then(|| text.to_string())
}

/// The chat `type` for a relayable effective kind whose own `type` slot did
/// not provide one: a `*_error` kind already names the class of failure
/// (`overloaded_error`, `not_found_error`); a granular code names a request
/// problem; a numeric status maps through the surface vocabulary; no kind at
/// all is the generic upstream type, as the bare-string wrap.
fn chat_error_type(kind: Option<&Value>) -> String {
    match kind {
        Some(Value::String(kind)) if kind.ends_with("_error") => kind.clone(),
        Some(Value::String(_)) => error_type(Surface::Openai, 400).to_string(),
        Some(kind @ Value::Number(_)) => {
            error_type(Surface::Openai, numeric_status(kind).unwrap_or(502)).to_string()
        }
        _ => error_type(Surface::Openai, 502).to_string(),
    }
}

/// The HTTP status a relayable kind corresponds to. The chat surface renders
/// it as the numeric `code`; the other surfaces map it into their own type
/// vocabulary via `error_type`.
fn kind_http_status(kind: Option<&Value>) -> u16 {
    match kind {
        Some(kind @ Value::Number(_)) => numeric_status(kind).unwrap_or(400),
        Some(Value::String(kind)) => match kind.as_str() {
            "not_found_error" | "model_not_found" => 404,
            "request_too_large" => 413,
            "overloaded_error" => 503,
            "vector_store_timeout" => 504,
            _ => 400,
        },
        _ => 400,
    }
}

/// The subset of the relayable kinds that is official Responses `code`
/// vocabulary. A relayable kind from another surface's vocabulary
/// (`overloaded_error`, `context_length_exceeded`) is not — it renders as
/// null/generic, the relayed message carrying the detail.
const RESPONSES_ERROR_CODES: &[&str] = &[
    "invalid_prompt",
    "vector_store_timeout",
    "invalid_image",
    "invalid_image_format",
    "invalid_base64_image",
    "invalid_image_url",
    "invalid_image_mode",
    "image_parse_error",
    "image_too_large",
    "image_too_small",
    "image_file_too_large",
    "image_file_not_found",
    "image_content_policy_violation",
    "empty_image_file",
    "unsupported_image_media_type",
    "failed_to_download_image",
];

fn responses_error_code(kind: Option<&Value>) -> Option<&str> {
    kind.and_then(Value::as_str)
        .filter(|kind| RESPONSES_ERROR_CODES.contains(kind))
}

/// The subset of the relayable kinds that is official Anthropic error-type
/// vocabulary; anything else maps through its status (`vector_store_timeout`
/// → `timeout_error`, a granular image code → `invalid_request_error`).
const ANTHROPIC_ERROR_TYPES: &[&str] = &[
    "invalid_request_error",
    "not_found_error",
    "request_too_large",
    "overloaded_error",
];

fn messages_error_type(kind: Option<&Value>) -> String {
    // An explicit `"type": null` claims nothing, exactly as an absent one does.
    match kind.filter(|kind| !kind.is_null()) {
        None => error_type(Surface::Anthropic, 502).to_string(),
        Some(Value::String(kind)) if ANTHROPIC_ERROR_TYPES.contains(&kind.as_str()) => kind.clone(),
        _ => error_type(Surface::Anthropic, kind_http_status(kind)).to_string(),
    }
}

/// The status a suppressed error folds to, given the kind that decided the
/// suppression and the `type` slot as a fallback classifier.
///
/// The allowlist will never be complete: an upstream can coin a granular code
/// this gateway has not seen, and suppressing it is the safe default. But
/// folding it to 502 also *reclassifies* it — it tells the client the provider
/// broke and the request is worth retrying, when a `type` the allowlist does
/// recognize already said the request itself is at fault. That turns a caller's
/// permanently-invalid request into a retry loop. So when the effective kind
/// decides nothing (the 502 default) and the `type` slot names a class this
/// gateway knows, that class decides the status instead. Only the status
/// changes: the kind and the message stay the gateway's own either way, so no
/// upstream vocabulary or prose rides along.
fn suppressed_status(kind: Option<&Value>, error_type_slot: Option<&Value>) -> u16 {
    match suppressed_error_status(kind) {
        // An explicit `"type": null` claims nothing, exactly as an absent one
        // does; without the filter it reaches the relayable test, which passes
        // a null kind, and lands on the request-fault default.
        502 => match error_type_slot.filter(|slot| !slot.is_null()) {
            Some(slot) if is_relayable_kind(Some(slot)) => kind_http_status(Some(slot)),
            _ => 502,
        },
        decided => decided,
    }
}

/// The status a suppressed kind folds to, mirroring the statuses
/// `map_upstream_status` preserves: rate-limit → 429, unavailable → 503,
/// timeout → 504, everything else → 502. Retry semantics survive, but in the
/// gateway's own words. Takes the single *effective* kind, never both slots —
/// a first-match-wins order over two slots would depend on argument order.
fn suppressed_error_status(kind: Option<&Value>) -> u16 {
    match kind {
        Some(Value::String(kind)) => match kind.as_str() {
            "rate_limit_error" | "rate_limit_exceeded" => 429,
            "timeout_error" => 504,
            _ => 502,
        },
        // A numeric kind takes the HTTP mapping itself; the actionable
        // statuses it preserves never reach here (they are relayable).
        Some(kind @ Value::Number(_)) => map_upstream_status(numeric_status(kind).unwrap_or(502)),
        _ => 502,
    }
}

/// The legacy `/v1/completions` (text completion) surface. OpenAI-standard and
/// small; cross-checked against the documented text-completion response.
/// Reuses `CHAT_USAGE` and the chat `sanitize_error` (identical shapes).
/// `system_fingerprint` and
/// `provider` are dropped for the same reason as on the chat surface.
const COMPLETION_TOP: &[&str] = &[
    "id", "object", "created", "model", "choices", "usage", "error",
];
const COMPLETION_CHOICE: &[&str] = &["text", "index", "logprobs", "finish_reason", "error"];

/// The `/v1/responses` (Responses API) surface. A larger shape that echoes request
/// parameters back and carries client-supplied `metadata`; the field set is the
/// documented Responses API response object. `output[]` items are content and left
/// intact. `system_fingerprint`/`provider` and any other unlisted upstream field
/// are dropped. `usage` has its own shape.
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
    // A real, non-identifying OpenAI field (`auto`/`default`/`flex`); kept for
    // parity with `CHAT_TOP`.
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
/// completions, responses). The Anthropic Messages surface has no output
/// allowlist — its shape is the format conversion's business — but its in-band
/// `error` event is normalized; embeddings are not yet scoped.
/// `request_id` is the gateway's own id, stamped into a rebuilt Messages error
/// event — the gateway's error tail carries it, and a rebuilt upstream error
/// must be indistinguishable from it (and stay correlatable with the receipt).
pub fn canonicalize(body: &mut Value, endpoint: Endpoint, request_id: Option<&str>) {
    match endpoint {
        Endpoint::ChatComplete => canonicalize_chat_completion(body),
        Endpoint::Complete => canonicalize_text_completion(body),
        Endpoint::CreateModelResponse => canonicalize_responses(body),
        Endpoint::Messages => canonicalize_messages(body, request_id),
        Endpoint::Embed => {}
    }
}

/// The Messages surface is left as the upstream (or the format conversion)
/// shaped it, except for the in-band `error` event, which carries
/// upstream-controlled kind and prose. It is rebuilt in the shape
/// `stream_error_tail` emits for this protocol, dropping everything beside
/// `type`/`message` by construction; the rebuilt event keeps `type: "error"`
/// and a non-null `error` member, which the meter and the framing observer
/// classify a failed stream by.
fn canonicalize_messages(body: &mut Value, request_id: Option<&str>) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    if object.get("type").and_then(Value::as_str) != Some("error") {
        return;
    }
    // The same kind resolution and the same suppressed-status rules as every
    // other surface: read code-first, and let the account check outrank the
    // kind. Reading the `type` slot alone would relay an unpaid provider as a
    // rate limit here while the other paths call it an upstream error, and
    // would file a caller's invalid request under a retryable provider fault.
    let error = object.get("error").and_then(Value::as_object);
    let kind = error.and_then(effective_error_kind);
    let (kind, message) = if error.is_some_and(is_relayable_error) {
        (
            messages_error_type(kind),
            client_error_message(error.and_then(|error| error.get("message"))),
        )
    } else {
        let status = error.map_or(502, |error| suppressed_status_for(error, error.get("type")));
        (
            error_type(Surface::Anthropic, status).to_string(),
            upstream_message(status).to_string(),
        )
    };
    if let Value::Object(event) = envelope(Surface::Anthropic, &kind, &message, request_id) {
        *object = event;
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
/// - `error`: the one non-lifecycle envelope carrying upstream-controlled
///   prose and kind — rebuilt as the gateway's own error event.
/// - Any other typed event (deltas, item/part events): a control/content
///   envelope carrying no response object and nothing upstream-authored beyond
///   content — pass through verbatim. There is no schema to filter an envelope
///   against, and applying the final-object allowlist would empty it.
fn canonicalize_responses(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        // `response.error` must be matched before the lifecycle prefix: it
        // carries the error at the top level with no `response` member, so the
        // lifecycle arm would pass it through verbatim. Rebuilt as the
        // canonical `error` event, which metering also classifies by.
        Some("error" | "response.error") => sanitize_responses_error_event(object),
        Some(t) if t.starts_with("response.") => {
            if let Some(inner) = object.get_mut("response").and_then(Value::as_object_mut) {
                canonicalize_responses_object(inner);
            }
        }
        Some(_) => {}
        None => canonicalize_responses_object(object),
    }
}

/// Rebuild a Responses in-band `error` event as the gateway's own.
///
/// This is the one non-`response.*` event with a gateway-side canonical shape:
/// `stream_error_tail` already emits it when the gateway ends a broken
/// Responses stream itself, and a client should not be able to tell the two
/// apart. Two upstream shapes arrive — the documented flat event, and an
/// `error` object nested under the envelope — and both collapse into that one
/// shape. `sequence_number` is preserved: the protocol numbers its events, and
/// the framing observer continues that numbering when it appends a tail of its
/// own. The rebuilt event keeps `type: "error"`, which the meter and the
/// framing observer classify a failed stream by.
fn sanitize_responses_error_event(object: &mut serde_json::Map<String, Value>) {
    let nested = object.get("error");
    // Prefer the nested object's fields over the envelope's. The kind is the
    // `code`, falling back to the nested `type`; the top-level `type` is the
    // event name (`error`), never the kind.
    let field = |key: &str| {
        nested
            .and_then(|error| error.get(key))
            .or_else(|| object.get(key))
    };
    let kind = field("code")
        .filter(|code| !code.is_null())
        .or_else(|| nested.and_then(|error| error.get("type")));
    let sequence_number = object.get("sequence_number").and_then(Value::as_u64);
    // The account check reads whichever object carries the error's own fields:
    // the nested one when the provider wrapped it, the envelope otherwise.
    let carrier = nested.and_then(Value::as_object).unwrap_or(&*object);
    let quota_exhausted = is_quota_exhausted_error(carrier);
    let event = if !quota_exhausted && is_relayable_kind(kind) {
        let message = client_error_message(field("message"));
        // `code` and `param` are `string | null` on this event. Only the
        // surface's own code vocabulary is kept; any other kind — numeric, or
        // another surface's word — nulls out, the relayed message carrying the
        // detail. `param` survives only as a safe parameter name.
        // Same fallback as the buffered sibling, so the two agree on a kind
        // this surface's enum does not name.
        let code = responses_error_code(kind)
            .or_else(|| kind.map(|_| responses_gateway_code(kind_http_status(kind))));
        let param = client_error_param(field("param"));
        responses_error_event(code, &message, param.as_deref(), sequence_number)
    } else {
        let status = if quota_exhausted {
            map_upstream_status(402)
        } else {
            suppressed_status(kind, nested.and_then(|error| error.get("type")))
        };
        responses_error_event(
            Some(responses_gateway_code(status)),
            upstream_message(status),
            None,
            sequence_number,
        )
    };
    if let Value::Object(event) = event {
        *object = event;
    }
}

fn canonicalize_responses_object(object: &mut serde_json::Map<String, Value>) {
    retain_allowed(object, RESPONSES_TOP);
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        retain_allowed(usage, RESPONSES_USAGE);
    }
    // `output[]` items are content (message/reasoning/function_call, …) and left
    // intact; the identifying fields ride at the top level, dropped above.
    sanitize_responses_error_field(object.get_mut("error"));
}

/// The response object's `error` field is `{code, message}` — string code,
/// string message — or null. Anything non-null is *rebuilt* into exactly that
/// shape, never filtered in place: filtering would let a relayable-but-bare
/// error keep no code, a numeric code stay numeric, or a non-string `message`
/// bypass the text scrub. Same relay policy as `sanitize_error`: a relayable
/// kind keeps its name and its scrubbed message; a suppressed kind becomes the
/// gateway's own vocabulary; no kind at all relays the scrubbed message under
/// the generic code.
fn sanitize_responses_error_field(error: Option<&mut Value>) {
    let Some(error) = error else { return };
    let (code, message) = match &*error {
        // A null error is the healthy value; there is nothing to rebuild.
        Value::Null => return,
        // A bare-string error (some providers send one) becomes the object.
        Value::String(text) => (
            responses_gateway_code(502).to_string(),
            client_safe_error_text(text),
        ),
        Value::Object(object) => {
            let kind = effective_error_kind(object);
            if !is_quota_exhausted_error(object) && is_relayable_kind(kind) {
                // Only the surface's own code vocabulary is kept; any other
                // kind gets the generic code, with the scrubbed message still
                // carrying the detail.
                // A relayable kind outside this surface's enum still has a
                // status; render the gateway code for *that*, not the generic
                // server error — `server_error` beside "the request was
                // rejected as invalid" tells the client to retry a request
                // that cannot succeed.
                let code = responses_error_code(kind)
                    .unwrap_or_else(|| responses_gateway_code(kind_http_status(kind)))
                    .to_string();
                let message = client_error_message(object.get("message"));
                (code, message)
            } else {
                let status = suppressed_status_for(object, object.get("type"));
                (
                    responses_gateway_code(status).to_string(),
                    upstream_message(status).to_string(),
                )
            }
        }
        // Any other non-null shape (an array, a number, a boolean) claims
        // nothing parseable and its interior is upstream-controlled — fold it
        // rather than relay it.
        _ => (
            responses_gateway_code(502).to_string(),
            upstream_message(502).to_string(),
        ),
    };
    *error = json!({ "code": code, "message": message });
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

/// Make an in-band error client-safe. An object whose kind is not relayable is
/// replaced wholesale by the gateway's own error — scrubbing is not enough,
/// because an account-status message carries no structural marker for the
/// scrubber to catch. A relayable object is *rebuilt* as the full
/// `{message, type, param, code}` object (a client indexing those keys must
/// never miss one) with its message scrubbed; a bare-string error becomes the
/// same object; any other non-null shape folds into the generic error.
fn sanitize_error(error: Option<&mut Value>) {
    let Some(error) = error else { return };
    if error.is_null() {
        return;
    }
    // A bare-string error (some providers send one) becomes the standard
    // object — a client parsing that shape must never meet a string.
    if let Value::String(text) = error {
        let mut generic = chat_gateway_error(502);
        generic["message"] = Value::String(client_safe_error_text(text));
        *error = generic;
        return;
    }
    let Some(object) = error.as_object() else {
        // Any other non-null shape (an array, a number, a boolean) claims
        // nothing parseable and its interior is upstream-controlled — fold it
        // rather than relay it.
        *error = chat_gateway_error(502);
        return;
    };
    if !is_relayable_error(object) {
        let status = suppressed_status_for(object, object.get("type"));
        *error = chat_gateway_error(status);
        return;
    }
    // Relayable: rebuild rather than filter, so every key is present. `code`
    // is always the numeric status on this surface — aggregator-style clients
    // classify retries by it, and a vocabulary string would degrade to a
    // generic 500 in their parsers. It renders from the same effective kind
    // the relay decision used, whichever slot carried it; only a kindless
    // error claims no status and keeps a null code.
    let code = match effective_error_kind(object).filter(|kind| !kind.is_null()) {
        Some(kind) => json!(kind_http_status(Some(kind))),
        None => Value::Null,
    };
    let error_kind = match object.get("type").and_then(Value::as_str) {
        Some(kind) if RELAYABLE_ERROR_KINDS.contains(&kind) => kind.to_string(),
        _ => chat_error_type(effective_error_kind(object)),
    };
    let message = match object.get("message") {
        Some(Value::String(text)) => client_safe_error_text(text),
        // A structured value is an upstream detail carrier, not a message, and
        // a null or absent one still owes the client a message.
        _ => client_safe_error_text(""),
    };
    let param = client_error_param(object.get("param"));
    *error = json!({
        "message": message,
        "type": error_kind,
        "param": param,
        "code": code,
    });
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

// ── OpenAI chat.completion → Responses API object ────────────────────────────

/// The request fields a Responses object echoes back, taken from the client's
/// Responses request. `store` is always false: the gateway keeps nothing.
pub fn responses_echo(params: &Value) -> Value {
    let mut echo = serde_json::Map::new();
    for key in [
        "instructions",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "max_output_tokens",
        "text",
        "reasoning",
        "metadata",
        "truncation",
        "user",
    ] {
        if let Some(value) = params.get(key) {
            echo.insert(key.into(), value.clone());
        }
    }
    if let Ok(tools) = effective_responses_tools(params) {
        if !tools.is_empty() {
            echo.insert("tools".into(), Value::Array(tools));
        }
    }
    echo.insert("store".into(), Value::Bool(false));
    Value::Object(echo)
}

/// Rebuild a chat completion as the Responses object answering the client:
/// the first choice's reasoning, text (or refusal) and tool calls become
/// output items in that order, `finish_reason` becomes `status`, and the
/// usage buckets take their Responses names. `echo` supplies the request
/// fields the object repeats.
pub fn openai_chat_to_responses(response: Value, echo: &Value) -> Value {
    let tool_map = ResponsesToolMap::from_echo(echo);
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .cloned();
    let message = choice
        .as_ref()
        .and_then(|choice| choice.get("message"))
        .cloned()
        .unwrap_or(Value::Null);
    let id = match response.get("id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!("resp_{}", now_millis()),
    };

    let mut output: Vec<Value> = Vec::new();
    let mut invalid_function_arguments = false;
    let mut invalid_tool_call_identity = false;
    if let Some(text) = reasoning_text(&message) {
        let item_id = item_id("rs", &id, output.len());
        output.push(reasoning_item(&item_id, text, "completed"));
    }
    if choice.is_some() {
        let text = message.get("content").and_then(Value::as_str).unwrap_or("");
        let refusal = message
            .get("refusal")
            .and_then(Value::as_str)
            .filter(|refusal| !refusal.is_empty());
        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let part = match refusal {
            Some(refusal) => Some(refusal_part(refusal)),
            // A turn with neither text nor tool calls is still an (empty) message.
            None if !text.is_empty() || tool_calls.is_empty() => Some(output_text_part(text)),
            None => None,
        };
        if let Some(part) = part {
            let item_id = item_id("msg", &id, output.len());
            output.push(message_item(&item_id, part, "completed"));
        }
        for call in &tool_calls {
            let function = call.get("function");
            let call_id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let arguments = function
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if call_id.trim().is_empty() || name.trim().is_empty() {
                invalid_tool_call_identity = true;
                continue;
            }
            if tool_map.is_custom(name) {
                output.push(custom_tool_call_item(
                    call_id,
                    name,
                    &custom_tool_input(arguments),
                    "completed",
                ));
            } else {
                let Some(arguments) = normalize_function_call_arguments(arguments) else {
                    invalid_function_arguments = true;
                    continue;
                };
                let namespace = tool_map.namespace(name);
                output.push(function_call_item(
                    call_id,
                    namespace.map_or(name, |tool| tool.name.as_str()),
                    namespace.map(|tool| tool.namespace.as_str()),
                    &arguments,
                    "completed",
                ));
            }
        }
    }

    let upstream_error = response
        .get("error")
        .filter(|error| !error.is_null())
        .cloned();
    let (status, incomplete_details, error) = match upstream_error {
        Some(error) => ("failed", Value::Null, error),
        None => {
            let terminal = responses_terminal(
                choice
                    .as_ref()
                    .and_then(|choice| choice.get("finish_reason"))
                    .and_then(Value::as_str),
            );
            match terminal {
                None => ("failed", Value::Null, invalid_finish_reason_error()),
                Some(("completed", _)) if invalid_tool_call_identity => {
                    ("failed", Value::Null, invalid_tool_call_identity_error())
                }
                Some(("completed", _)) if invalid_function_arguments => (
                    "failed",
                    Value::Null,
                    invalid_function_call_arguments_error(),
                ),
                Some((status, details)) => (status, details, Value::Null),
            }
        }
    };
    responses_object(
        echo,
        ResponsesHead {
            id: &id,
            created_at: now_secs(),
            model: response.get("model").cloned().unwrap_or(Value::Null),
            status,
            incomplete_details,
            error,
        },
        output,
        Some(responses_usage(response.get("usage"))),
    )
}

/// The per-response fields of a Responses object, beside the echoed request
/// fields and the output.
pub(super) struct ResponsesHead<'a> {
    pub id: &'a str,
    pub created_at: u64,
    pub model: Value,
    pub status: &'a str,
    pub incomplete_details: Value,
    pub error: Value,
}

/// Assemble a Responses object: the echoed request fields, the head, the
/// output items, and usage where the lifecycle has produced one (`null` on the
/// snapshots a stream opens with).
pub(super) fn responses_object(
    echo: &Value,
    head: ResponsesHead<'_>,
    output: Vec<Value>,
    usage: Option<Value>,
) -> Value {
    let mut object = echo.as_object().cloned().unwrap_or_default();
    object.extend(
        [
            ("id", json!(head.id)),
            ("object", json!("response")),
            ("created_at", json!(head.created_at)),
            ("model", head.model),
            ("status", json!(head.status)),
            ("incomplete_details", head.incomplete_details),
            ("error", head.error),
            ("output", Value::Array(output)),
            ("usage", usage.unwrap_or(Value::Null)),
        ]
        .map(|(key, value)| (key.to_string(), value)),
    );
    Value::Object(object)
}

/// An output item id: the kind, the response id, and the item's output index,
/// so a response with two messages names them apart.
pub(super) fn item_id(kind: &str, response_id: &str, output_index: usize) -> String {
    format!("{kind}_{response_id}_{output_index}")
}

/// The reasoning a chat message or delta carries, under either wire spelling.
pub(super) fn reasoning_text(message: &Value) -> Option<&str> {
    ["reasoning_content", "reasoning"]
        .into_iter()
        .find_map(|key| message.get(key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

pub(super) fn reasoning_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "reasoning",
        "summary": [],
        "content": [{ "type": "reasoning_text", "text": text }],
        "status": status,
    })
}

pub(super) fn message_item(id: &str, part: Value, status: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": status,
        "content": [part],
    })
}

pub(super) fn output_text_part(text: &str) -> Value {
    json!({ "type": "output_text", "text": text, "annotations": [] })
}

pub(super) fn refusal_part(refusal: &str) -> Value {
    json!({ "type": "refusal", "refusal": refusal })
}

pub(super) fn function_call_item(
    call_id: &str,
    name: &str,
    namespace: Option<&str>,
    arguments: &str,
    status: &str,
) -> Value {
    let mut item = json!({
        "id": format!("fc_{call_id}"),
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
        "status": status,
    });
    if let Some(namespace) = namespace {
        item["namespace"] = json!(namespace);
    }
    item
}

pub(super) fn custom_tool_call_item(call_id: &str, name: &str, input: &str, status: &str) -> Value {
    json!({
        "id": format!("ctc_{call_id}"),
        "type": "custom_tool_call",
        "call_id": call_id,
        "name": name,
        "input": input,
        "status": status,
    })
}

pub(super) fn custom_tool_input(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(object)) if object.is_empty() => String::new(),
        Ok(Value::Object(object)) => object
            .get("input")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| trimmed.to_string()),
        _ => trimmed.to_string(),
    }
}

/// Normalize Chat function arguments for a Responses function-call item.
/// Empty arguments mean an empty object; every other value must be a JSON
/// object so a later Responses history replay remains valid.
pub(super) fn normalize_function_call_arguments(arguments: &str) -> Option<String> {
    if arguments.trim().is_empty() {
        return Some("{}".to_string());
    }
    matches!(
        serde_json::from_str::<Value>(arguments),
        Ok(Value::Object(_))
    )
    .then(|| arguments.to_string())
}

pub(super) fn invalid_function_call_arguments_error() -> Value {
    json!({
        "code": "server_error",
        "message": "The upstream provider returned invalid function-call arguments",
    })
}

pub(super) fn invalid_tool_call_identity_error() -> Value {
    json!({
        "code": "server_error",
        "message": "The upstream provider returned a tool call without a valid id or name",
    })
}

pub(super) fn invalid_finish_reason_error() -> Value {
    json!({
        "code": "server_error",
        "message": "The upstream provider ended without a valid finish reason",
    })
}

/// How a chat `finish_reason` ends a response: the `status`, and the
/// `incomplete_details` that say why a cut-off one stopped.
pub(super) fn responses_terminal(finish_reason: Option<&str>) -> Option<(&'static str, Value)> {
    match finish_reason {
        Some("stop" | "tool_calls") => Some(("completed", Value::Null)),
        Some("length") => Some(("incomplete", json!({ "reason": "max_output_tokens" }))),
        Some("content_filter") => Some(("incomplete", json!({ "reason": "content_filter" }))),
        _ => None,
    }
}

/// Responses `usage` from chat `usage`: the token buckets renamed, with the
/// cache-read and reasoning counts nested where the Responses schema keeps them.
pub(super) fn responses_usage(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let input = i64_field(usage, "prompt_tokens");
    let output = i64_field(usage, "completion_tokens");
    let cached = usage
        .get("prompt_tokens_details")
        .map(|details| i64_field(details, "cached_tokens"))
        .filter(|cached| *cached != 0)
        .unwrap_or_else(|| i64_field(usage, "cache_read_input_tokens"));
    let reasoning = usage
        .get("completion_tokens_details")
        .map(|details| i64_field(details, "reasoning_tokens"))
        .unwrap_or(0);
    let total = match i64_field(usage, "total_tokens") {
        0 => input + output,
        total => total,
    };
    json!({
        "input_tokens": input,
        "input_tokens_details": { "cached_tokens": cached },
        "output_tokens": output,
        "output_tokens_details": { "reasoning_tokens": reasoning },
        "total_tokens": total,
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
        canonicalize(&mut body, Endpoint::ChatComplete, None);
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
        canonicalize(&mut body, Endpoint::Messages, None);
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
        canonicalize(&mut body, Endpoint::ChatComplete, None);
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
        canonicalize(&mut body, Endpoint::ChatComplete, None);
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

    /// Multimodal and reasoning output fields must survive — dropping them would
    /// break audio, image, and thinking-block responses. Locks the allowlist
    /// against a too-narrow trim.
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
        canonicalize(&mut body, Endpoint::ChatComplete, None);
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
    /// clean message is kept verbatim, and the rebuilt object always carries
    /// the full `{message, type, param, code}` shape — a numeric code stays a
    /// number, which retry-classifying clients depend on.
    #[test]
    fn canonicalize_keeps_an_in_band_stream_error() {
        let mut body = json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "error": { "message": "context length exceeded", "code": 400 }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(
            body["error"],
            json!({
                "message": "context length exceeded",
                "type": "invalid_request_error",
                "param": null,
                "code": 400
            })
        );
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
                "code": 400,
                "metadata": { "provider_name": "SomeProvider", "raw": "upstream https://api.acme.ai 502" }
            },
            "choices": [{
                "index": 0,
                "error": { "message": "no route to inference-7.internal.acme.io", "metadata": { "provider_name": "X" } }
            }]
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["message"], "the provider returned an error");
        assert_eq!(body["error"]["code"], 400); // standard field kept

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
    /// scrubbed and wrapped into the standard error object — the surface
    /// contract is `{message, type, param, code}` and a client parsing that
    /// shape must never meet a string.
    #[test]
    fn canonicalize_scrubs_a_bare_string_error() {
        let mut body = json!({
            "id": "x",
            "error": "cannot connect to host files.example.com:443"
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(
            body["error"],
            json!({
                "message": "the provider returned an error",
                "type": "upstream_error",
                "code": 502,
                "param": null
            })
        );
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
    /// passed through untouched — for Messages the in-band `error` event is the
    /// one exception, covered separately.
    #[test]
    fn canonicalize_is_a_noop_for_unschematized_surfaces() {
        let original = json!({ "id": "x", "anything": true, "choices": [] });
        for endpoint in [Endpoint::Messages, Endpoint::Embed] {
            let mut body = original.clone();
            canonicalize(&mut body, endpoint, None);
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
        canonicalize(&mut body, Endpoint::Complete, None);
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
            "error": { "code": "server_error", "message": "backend https://api.acme.ai fell over" },
            "usage": { "input_tokens": 3, "output_tokens": 2, "total_tokens": 5, "cost": 0.02 }
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        // Dropped: upstream identity and unknowns.
        assert!(body.get("system_fingerprint").is_none());
        assert!(body.get("provider").is_none());
        // A suppressed error keeps the response object's `{code, message}` shape —
        // no chat-style `type`/`param` — in the gateway's own vocabulary.
        assert_eq!(
            body["error"],
            json!({ "code": "server_error", "message": "The upstream provider returned an error" })
        );
        // Kept: request-echo, client metadata, output content, responses usage.
        assert_eq!(body["metadata"]["client_tag"], "abc");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["previous_response_id"], "resp_prev");
        assert_eq!(body["output"][0]["content"][0]["text"], "hi");
        assert_eq!(body["usage"]["input_tokens"], 3);
        assert_eq!(body["usage"]["cost"], 0.02);

        // A bare-string error is wrapped into the `{code, message}` shape, not
        // left a string.
        let mut body = json!({
            "object": "response",
            "error": "backend at https://api.acme.ai timed out"
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(
            body["error"],
            json!({ "code": "server_error", "message": "the provider returned an error" })
        );

        // A malformed error (an array — any non-null shape that is neither
        // object nor string) folds instead of passing through.
        let mut body = json!({
            "object": "response",
            "error": [{ "provider": "SomeProvider", "raw": "https://api.acme.ai 502" }]
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(
            body["error"],
            json!({ "code": "server_error", "message": "The upstream provider returned an error" })
        );
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
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
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

    /// `/v1/responses` streaming: a typed event that carries no response object
    /// is not the response object; applying the final-object allowlist would
    /// empty it. It must pass through verbatim so the client sees content and
    /// metering can classify the stream. The `error` event is the one
    /// exception — it is the only envelope carrying upstream-controlled prose,
    /// and the gateway defines its shape (covered separately).
    #[test]
    fn canonicalize_responses_passes_through_an_unknown_typed_event() {
        let event = json!({
            "type": "response.output_text.delta",
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "delta": "hi",
            "sequence_number": 3
        });
        let mut body = event.clone();
        rewrite_identity(&mut body, &identity());
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(body, event, "envelope was mutated: {event}");
    }

    /// `/v1/responses` streaming: an in-band `error` event is rebuilt as the
    /// gateway's own. A kind naming our account with the upstream (the nested
    /// shape one provider sends) folds into the generic upstream error — the
    /// message would survive the scrubber's shape checks yet still leak who
    /// served the request and blame the caller's quota for ours. An actionable
    /// kind (the documented flat shape) is relayed. `sequence_number` survives
    /// either way; the framing observer continues the numbering from it.
    #[test]
    fn canonicalize_responses_rebuilds_an_in_band_error() {
        let mut body = json!({
            "type": "error",
            "sequence_number": 2,
            "error": {
                "type": "insufficient_quota",
                "code": "credit_balance_exhausted",
                "message": "You have no credits remaining. Add credits at https://console.acme.ai/billing.",
                "param": null
            }
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(body["type"], "error");
        assert_eq!(body["code"], "server_error");
        assert_eq!(body["message"], "The upstream provider returned an error");
        assert_eq!(body["sequence_number"], 2);
        assert!(body.get("error").is_none(), "nested error object survived");
        assert!(!body.to_string().contains("console.acme.ai"));

        // A relayable kind from another surface's vocabulary is not a Responses
        // code, but it still has a status: it renders as the enum value for
        // that status, so the one machine-readable field agrees with the
        // relayed message instead of calling a request fault a server error.
        // An official code (`invalid_image`, asserted in the param cases below)
        // is kept as-is.
        let mut body = json!({
            "type": "error",
            "code": "context_length_exceeded",
            "message": "input exceeds the model context window",
            "param": "input",
            "sequence_number": 7
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(body["code"], "invalid_prompt");
        assert_eq!(body["message"], "input exceeds the model context window");
        assert_eq!(body["param"], "input");
        assert_eq!(body["sequence_number"], 7);

        // `code` and `param` are `string | null` on the wire: a numeric code is
        // never relayed as a number, it renders as the enum value for the
        // status it names, and `param` nulls out when structured — or when it
        // is an identifying string.
        let mut body = json!({
            "type": "error",
            "code": 400,
            "message": "bad request",
            "param": { "raw": "https://api.acme.ai", "provider": "SomeProvider" },
            "sequence_number": 3
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(body["code"], "invalid_prompt");
        assert_eq!(body["message"], "bad request");
        assert_eq!(body["param"], Value::Null);
        assert_eq!(body["sequence_number"], 3);

        // A suppressed rate limit renders the surface's own official code, not
        // a chat/HTTP type word.
        let mut body = json!({
            "type": "error",
            "code": "rate_limit_exceeded",
            "message": "acme tier exhausted",
            "sequence_number": 6
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(body["code"], "rate_limit_exceeded");
        assert_eq!(
            body["message"],
            "Rate limit exceeded. Please retry after some time."
        );

        let mut body = json!({
            "type": "error",
            "code": "invalid_image",
            "message": "bad image",
            "param": "https://api.acme.ai/internal",
            "sequence_number": 5
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(body["code"], "invalid_image");
        assert_eq!(body["param"], Value::Null);

        // An error framed as `response.error` (error object at the top level,
        // no `response` member) must not fall into the lifecycle passthrough;
        // it is rebuilt as the canonical `error` event.
        let mut body = json!({
            "type": "response.error",
            "sequence_number": 4,
            "error": { "type": "insufficient_quota", "code": "credit_balance_exhausted", "message": "no credits" }
        });
        canonicalize(&mut body, Endpoint::CreateModelResponse, None);
        assert_eq!(body["type"], "error");
        assert_eq!(body["code"], "server_error");
        assert_eq!(body["message"], "The upstream provider returned an error");
        assert_eq!(body["sequence_number"], 4);
    }

    /// A chat in-band error whose kind names our account with the upstream is
    /// replaced by the gateway's own error object — but `error` stays non-null,
    /// or metering would record the failed stream as a success. A rate-limit
    /// kind keeps its retry semantics in our vocabulary, as the HTTP layer does
    /// for an upstream 429.
    #[test]
    fn canonicalize_collapses_a_chat_error_that_names_our_account() {
        let mut body = json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "error": {
                "type": "insufficient_quota",
                "code": "credit_balance_exhausted",
                "message": "You have no credits remaining."
            }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["type"], "upstream_error");
        assert_eq!(body["error"]["code"], 502); // numeric: retry classifiers read it
        assert_eq!(
            body["error"]["message"],
            "The upstream provider returned an error"
        );

        let mut body = json!({
            "error": { "type": "rate_limit_error", "message": "acme tier exhausted, upgrade your plan" }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], 429);
        assert_eq!(
            body["error"]["message"],
            "Rate limit exceeded. Please retry after some time."
        );

        // A precise code outranks a generic type: relayed on the code (as its
        // numeric status on this surface), with the non-relayable type
        // replaced — and always the full four-field object.
        let mut body = json!({
            "error": { "type": "server_error", "code": "invalid_image", "message": "image could not be decoded" }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(
            body["error"],
            json!({
                "message": "image could not be decoded",
                "type": "invalid_request_error",
                "param": null,
                "code": 400
            })
        );

        // A numeric 503 keeps its status semantics instead of folding to 502,
        // as `map_upstream_status` does on the HTTP path.
        let mut body = json!({
            "error": { "code": 503, "message": "acme scheduler drained" }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["type"], "service_unavailable");
        assert_eq!(body["error"]["code"], 503);
        assert_eq!(
            body["error"]["message"],
            "The model is currently unavailable. Please try again later."
        );

        // The `type` and the numeric code derive from the effective kind,
        // whichever slot carried it: a `*_error` kind keeps naming its own
        // class of failure and must not be relabeled a request error, and a
        // type-only kind still yields its status.
        // One kind per slot covers both axes: that the slot does not matter,
        // and that the status comes from the kind rather than a fixed default.
        for (slot, kind, expect_type, expect_code) in [
            ("code", "overloaded_error", "overloaded_error", 503),
            ("type", "not_found_error", "not_found_error", 404),
        ] {
            let mut body = json!({ "error": { slot: kind, "message": "m" } });
            canonicalize(&mut body, Endpoint::ChatComplete, None);
            assert_eq!(body["error"]["type"], expect_type, "{slot}:{kind}");
            assert_eq!(body["error"]["code"], expect_code, "{slot}:{kind}");
        }

        // An unrecognized code beside a `type` the allowlist does know keeps
        // that type's classification: the error is still fully rebuilt in our
        // own words, but the client is not told to retry a request that is
        // permanently invalid.
        let mut body = json!({
            "error": { "type": "invalid_request_error", "code": "string_below_min_length",
                       "message": "messages[0].content is too short" }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["code"], 400);
        assert_eq!(
            body["error"]["message"],
            "The request was rejected as invalid"
        );

        // An account refusal reported under a kind the allowlist recognizes is
        // still suppressed: its message names a balance rather than a host, so
        // the identifying-marker scrub has nothing to catch and would relay it
        // verbatim — telling the caller their own balance is empty. The HTTP
        // path classifies the identical body, and the two must agree.
        let mut body = json!({
            "error": { "type": "invalid_request_error",
                       "message": "Your credit balance is too low to access the API, please go to Plans & Billing." }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["type"], "upstream_error");
        assert_eq!(
            body["error"]["message"],
            "The upstream provider returned an error"
        );
        assert!(!body.to_string().contains("credit balance"));

        // A malformed error (neither object nor string) folds instead of
        // passing through — but stays non-null for metering.
        let mut body = json!({
            "error": [{ "provider": "SomeProvider", "raw": "https://api.acme.ai 502" }]
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["type"], "upstream_error");
        assert_eq!(
            body["error"]["message"],
            "The upstream provider returned an error"
        );

        // A null message still yields a string one (the shape itself is pinned
        // by the case above, which builds through the same path).
        let mut body = json!({
            "error": { "code": "invalid_image", "message": null }
        });
        canonicalize(&mut body, Endpoint::ChatComplete, None);
        assert_eq!(body["error"]["message"], "the provider returned an error");
        assert_eq!(body["error"]["code"], 400);

        // `param` survives only as a safe parameter name: a structured value
        // and an identifying string both null out, a plain name is kept.
        for (param, expect) in [
            (
                json!({ "provider": "SomeProvider", "raw": "https://api.acme.ai/internal" }),
                Value::Null,
            ),
            (json!("https://api.acme.ai/internal"), Value::Null),
            (json!("input"), json!("input")),
        ] {
            let mut body = json!({
                "error": { "code": "invalid_image", "message": "bad image", "param": param }
            });
            canonicalize(&mut body, Endpoint::ChatComplete, None);
            assert_eq!(body["error"]["param"], expect);
            assert_eq!(body["error"]["code"], 400);
        }
    }

    /// A Messages in-band `error` event is rebuilt in the tail's shape: an
    /// account-naming kind folds into `api_error` with the generic message,
    /// while an actionable kind keeps its type and its scrubbed message.
    #[test]
    fn canonicalize_messages_rebuilds_an_in_band_error_event() {
        let mut body = json!({
            "type": "error",
            "error": { "type": "billing_error", "message": "credit balance too low, top up your account" },
            "request_id": "req_upstream"
        });
        canonicalize(&mut body, Endpoint::Messages, Some("req_ours"));
        assert_eq!(
            body,
            json!({
                "type": "error",
                "error": { "type": "api_error", "message": "The upstream provider returned an error" },
                // The gateway's own id, as on the gateway's error tail — the
                // upstream's is gone with the rest of the event.
                "request_id": "req_ours"
            })
        );

        let mut body = json!({
            "type": "error",
            "error": { "type": "invalid_request_error", "message": "max_tokens is required" }
        });
        canonicalize(&mut body, Endpoint::Messages, None);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["message"], "max_tokens is required");

        // A relayable kind from another surface's vocabulary maps into this
        // surface's own types.
        let mut body = json!({
            "type": "error",
            "error": { "type": "vector_store_timeout", "message": "m" }
        });
        canonicalize(&mut body, Endpoint::Messages, None);
        assert_eq!(body["error"]["type"], "timeout_error");

        // This surface reaches the same verdict as the others on the same
        // body: an account refusal filed under a rate-limit kind is not a rate
        // limit, and a caller's invalid request is not a provider fault.
        let mut body = json!({
            "type": "error",
            "error": { "type": "rate_limit_error",
                       "message": "Your credit balance is too low to access the API." }
        });
        canonicalize(&mut body, Endpoint::Messages, None);
        assert_eq!(body["error"]["type"], "api_error");
        assert_eq!(
            body["error"]["message"],
            "The upstream provider returned an error"
        );

        let mut body = json!({
            "type": "error",
            "error": { "type": "invalid_request_error", "code": "string_below_min_length",
                       "message": "messages[0].content is too short" }
        });
        canonicalize(&mut body, Endpoint::Messages, None);
        assert_eq!(body["error"]["type"], "invalid_request_error");
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
