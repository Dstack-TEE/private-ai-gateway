//! Stateful SSE response transforms: convert an upstream provider's streaming
//! events into the downstream client surface, event by event, threading mutable
//! state across events (a per-stream transform state).
//!
//! Three provider conversions are supported. Same-format streaming reaches this
//! module too: every stream is sanitized here (identity rewrite + canonicalize,
//! per chunk), and reasoning is excluded when the client opts out. Cost
//! injection, TTFT, and outcome are a separate metering pass downstream (`sse`).

use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Bytes;
use futures_util::Stream;
use serde_json::{json, Value};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{ServiceError, ServiceResponseStream};

use super::request_transform::Endpoint;
use super::response_transform::{
    self, i64_field, map_finish_reason, now_millis, now_secs, transform_finish_reason,
    ResponseIdentity,
};
use super::sse::MAX_SSE_LINE_BYTES;
use super::types::ProviderFormat;

const STRICT_OPENAI_COMPLIANCE: bool = true;

/// Which streaming transform applies.
///
/// `Clone` rather than `Copy`: `SanitizeResponse` carries per-request values
/// (our request id, the client's model name), which no unit variant can.
#[derive(Debug, Clone)]
pub enum StreamTransform {
    AnthropicToOpenaiChat,
    OpenaiToAnthropicMessages,
    AnthropicCompleteToOpenai,
    ExcludeReasoning,
    /// Applied to every stream, including same-format passthrough, which is the
    /// one path that previously relayed provider bytes verbatim. Carries the
    /// endpoint so each chunk is canonicalized to that surface's output schema.
    SanitizeResponse(Arc<ResponseIdentity>, Endpoint),
}

impl StreamTransform {
    fn provider(&self) -> &'static str {
        match self {
            StreamTransform::OpenaiToAnthropicMessages => "openai",
            StreamTransform::ExcludeReasoning => "openai",
            StreamTransform::SanitizeResponse(_, _) => "openai",
            _ => "anthropic",
        }
    }

    // Parse a raw event text (lines joined by `\n`) and transform it.
    // `Err(())` means the provider sent an unparseable event; this ends the
    // stream and classifies it failed rather than skipping a truncated payload.
    // Each transform dispatches known control events by name first, then skips
    // any event whose payload is empty (per the SSE spec an empty data buffer
    // aborts dispatch — covers `: PROCESSING` heartbeats, ignored fields, and
    // name-only or empty-`data:` keep-alives).
    fn apply(
        &self,
        event: &str,
        fallback_id: &str,
        state: &mut StreamState,
    ) -> Result<Option<String>, ()> {
        match self {
            StreamTransform::ExcludeReasoning => {
                return Ok(Some(edit_event_body(
                    event,
                    response_transform::exclude_reasoning,
                )))
            }
            StreamTransform::SanitizeResponse(identity, endpoint) => {
                return Ok(Some(edit_event_body(event, |body| {
                    response_transform::rewrite_identity(body, identity);
                    response_transform::canonicalize(body, *endpoint);
                })))
            }
            _ => {}
        }
        let event = parse_event(event);
        match self {
            StreamTransform::AnthropicToOpenaiChat => {
                anthropic_chat_stream(&event, fallback_id, state, STRICT_OPENAI_COMPLIANCE)
            }
            StreamTransform::OpenaiToAnthropicMessages => {
                openai_to_anthropic_messages_stream(&event, fallback_id, state)
            }
            StreamTransform::AnthropicCompleteToOpenai => anthropic_complete_stream(&event),
            StreamTransform::ExcludeReasoning | StreamTransform::SanitizeResponse(_, _) => {
                unreachable!("handled above")
            }
        }
    }
}

/// Select the streaming transform for a committed route format + endpoint, or
/// `None` for native passthrough.
pub fn select_stream_transform(
    format: ProviderFormat,
    endpoint: Endpoint,
) -> Option<StreamTransform> {
    use Endpoint::*;
    use ProviderFormat::*;
    match (format, endpoint) {
        (Anthropic, ChatComplete) => Some(StreamTransform::AnthropicToOpenaiChat),
        (Anthropic, Complete) => Some(StreamTransform::AnthropicCompleteToOpenai),
        (Openai, Messages) => Some(StreamTransform::OpenaiToAnthropicMessages),
        _ => None,
    }
}

/// Mutable per-stream state threaded across events. Fields are split by
/// direction; a given stream only touches the ones for its transform.
#[derive(Default)]
struct StreamState {
    // Anthropic -> OpenAI chat.
    tool_index: Option<i64>,
    usage: Option<Value>,
    model: Option<String>,
    // OpenAI -> Anthropic messages.
    id: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    has_started: bool,
    content_block_started: bool,
    current_content_index: i64,
    tool_calls_started: BTreeSet<i64>,
    finish_reason: Option<String>,
    /// Set once the closing events have been emitted, so they cannot be sent twice.
    terminated: bool,
}

/// One SSE event reduced to the fields the transforms consume: the last
/// `event:` name and the `data:` lines joined with `\n` (per the SSE spec).
struct ParsedEvent {
    name: Option<String>,
    data: Option<String>,
}

// Parse an event's text per the SSE field rules: `:`-prefixed lines are
// comments, a field's value starts after the colon with at most one leading
// space stripped, and every field other than `event:`/`data:` (`id:`,
// `retry:`, vendor extensions) is ignored per the spec. Lines with no field
// shape at all — bare `[DONE]`, bare JSON (colons inside JSON do not make it a
// field: a `{`/`[`/`"` opener marks a payload line), plain garbage — are
// collected and become the data when no `data:` field is present, so a
// prefix-less payload survives even when a comment or `event:` line shares
// the event block, and garbage still reaches the transforms' fail-fast parse.
fn parse_event(event: &str) -> ParsedEvent {
    let mut name = None;
    let mut data: Option<String> = None;
    let mut bare: Option<String> = None;
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        // Judge the JSON opener on the trimmed line (upstreams may indent a
        // bare payload) but keep the raw line as the payload.
        let json_like = matches!(
            line.trim_start().as_bytes().first(),
            Some(b'{' | b'[' | b'"')
        );
        let colon = if json_like { None } else { line.find(':') };
        match colon {
            Some(idx) => {
                let value = &line[idx + 1..];
                let value = value.strip_prefix(' ').unwrap_or(value);
                match &line[..idx] {
                    // The name is trimmed: exact-match dispatch must tolerate
                    // trailing whitespace an upstream leaves after the value.
                    "event" => name = Some(value.trim().to_string()),
                    "data" => {
                        let buf = data.get_or_insert_with(String::new);
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        buf.push_str(value);
                    }
                    _ => {}
                }
            }
            None => {
                let buf = bare.get_or_insert_with(String::new);
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(line);
            }
        }
    }
    if data.is_none() {
        data = bare.filter(|b| !b.trim().is_empty());
    }
    ParsedEvent { name, data }
}

// ── Anthropic Messages SSE → OpenAI chat.completion.chunk ────────────────────

fn anthropic_chat_stream(
    event: &ParsedEvent,
    fallback_id: &str,
    state: &mut StreamState,
    strict: bool,
) -> Result<Option<String>, ()> {
    match event.name.as_deref() {
        Some("ping") | Some("content_block_stop") => return Ok(None),
        Some("message_stop") => return Ok(Some("data: [DONE]\n\n".to_string())),
        _ => {}
    }
    let payload = event.data.as_deref().unwrap_or("").trim();
    // No payload → no dispatch (name-only keep-alives included); a non-empty
    // malformed payload must still fail the stream rather than be skipped.
    if payload.is_empty() {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(payload).map_err(|_| ())?;
    Ok(anthropic_chat_chunk(&parsed, fallback_id, state, strict))
}

fn anthropic_chat_chunk(
    parsed: &Value,
    fallback_id: &str,
    state: &mut StreamState,
    strict: bool,
) -> Option<String> {
    let model = state.model.clone().unwrap_or_default();

    if parsed.get("type").and_then(Value::as_str) == Some("error") {
        if let Some(error) = parsed.get("error") {
            let body = json!({
                "id": fallback_id,
                "object": "chat.completion.chunk",
                "created": now_secs(),
                "model": "",
                "provider": "anthropic",
                "choices": [{
                    "finish_reason": error.get("type").cloned().unwrap_or(Value::Null),
                    "delta": { "content": "" },
                }],
            });
            return Some(format!("data: {}\n\ndata: [DONE]\n\n", json_str(&body)));
        }
    }

    let message_usage = parsed.get("message").and_then(|m| m.get("usage"));
    if parsed.get("type").and_then(Value::as_str) == Some("message_start") {
        if let Some(usage) = message_usage {
            let input = i64_field(usage, "input_tokens");
            let cache_read = i64_field(usage, "cache_read_input_tokens");
            let cache_creation = i64_field(usage, "cache_creation_input_tokens");
            state.model = Some(
                parsed
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
            let mut usage_state = json!({ "prompt_tokens": input + cache_read + cache_creation });
            if cache_read != 0 || cache_creation != 0 {
                let map = usage_state.as_object_mut().unwrap();
                if let Some(v) = usage.get("cache_read_input_tokens") {
                    map.insert("cache_read_input_tokens".into(), v.clone());
                }
                if let Some(v) = usage.get("cache_creation_input_tokens") {
                    map.insert("cache_creation_input_tokens".into(), v.clone());
                }
            }
            state.usage = Some(usage_state);
            let body = json!({
                "id": fallback_id,
                "object": "chat.completion.chunk",
                "created": now_secs(),
                "model": state.model.clone().unwrap_or_default(),
                "provider": "anthropic",
                "choices": [{
                    "delta": { "content": "" },
                    "index": 0,
                    "logprobs": Value::Null,
                    "finish_reason": Value::Null,
                }],
            });
            return Some(format!("data: {}\n\n", json_str(&body)));
        }
    }

    if parsed.get("type").and_then(Value::as_str) == Some("message_delta") {
        if let Some(usage) = parsed.get("usage") {
            let output = i64_field(usage, "output_tokens");
            let prompt = state
                .usage
                .as_ref()
                .map(|u| i64_field(u, "prompt_tokens"))
                .unwrap_or(0);
            let mut usage_out = json!({ "completion_tokens": output });
            if let Some(state_usage) = state.usage.as_ref().and_then(Value::as_object) {
                for (k, v) in state_usage {
                    usage_out
                        .as_object_mut()
                        .unwrap()
                        .insert(k.clone(), v.clone());
                }
            }
            usage_out
                .as_object_mut()
                .unwrap()
                .insert("total_tokens".into(), json!(prompt + output));
            let stop_reason = parsed
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str);
            let body = json!({
                "id": fallback_id,
                "object": "chat.completion.chunk",
                "created": now_secs(),
                "model": model,
                "provider": "anthropic",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": transform_finish_reason(stop_reason, strict),
                }],
                "usage": usage_out,
            });
            return Some(format!("data: {}\n\n", json_str(&body)));
        }
    }

    // Tool-call and text deltas.
    let mut tool_calls: Vec<Value> = Vec::new();
    let is_tool_block_start = parsed.get("type").and_then(Value::as_str)
        == Some("content_block_start")
        && parsed
            .get("content_block")
            .and_then(|b| b.get("type"))
            .and_then(Value::as_str)
            == Some("tool_use");
    if is_tool_block_start {
        // Index logic: a falsy (None/0) index yields 0, otherwise increments.
        // (A known quirk for >1 tool; kept for wire compatibility.)
        state.tool_index = Some(match state.tool_index {
            Some(n) if n != 0 => n + 1,
            _ => 0,
        });
    }
    let partial_json = parsed
        .get("delta")
        .and_then(|d| d.get("partial_json"))
        .filter(|v| !v.is_null());
    let is_tool_block_delta = parsed.get("type").and_then(Value::as_str)
        == Some("content_block_delta")
        && partial_json.is_some();

    if is_tool_block_start {
        if let Some(block) = parsed.get("content_block") {
            tool_calls.push(json!({
                "index": state.tool_index,
                "id": block.get("id").cloned().unwrap_or(Value::Null),
                "type": "function",
                "function": { "name": block.get("name").cloned().unwrap_or(Value::Null), "arguments": "" },
            }));
        }
    } else if is_tool_block_delta {
        tool_calls.push(json!({
            "index": state.tool_index,
            "function": { "arguments": partial_json.cloned().unwrap_or(Value::Null) },
        }));
    }

    let mut delta = serde_json::Map::new();
    if let Some(text) = parsed.get("delta").and_then(|d| d.get("text")) {
        delta.insert("content".into(), text.clone());
    }
    if !tool_calls.is_empty() {
        delta.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    let body = json!({
        "id": fallback_id,
        "object": "chat.completion.chunk",
        "created": now_secs(),
        "model": model,
        "provider": "anthropic",
        "choices": [{
            "delta": Value::Object(delta),
            "index": 0,
            "logprobs": Value::Null,
            "finish_reason": Value::Null,
        }],
    });
    Some(format!("data: {}\n\n", json_str(&body)))
}

// ── OpenAI chat.completion SSE → Anthropic Messages SSE ──────────────────────

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {}\n\n", json_str(data))
}

/// Emit the events that close an Anthropic message: stops for every open
/// content block, then either an error event or message_delta + message_stop.
///
/// Called from two places, because `[DONE]` is not guaranteed. It is an OpenAI
/// convention, not an SSE requirement, and an upstream may simply stop sending
/// bytes — GMI's OpenAI-compatible endpoint ends after its final usage chunk
/// with no terminator. Emitting this only on `[DONE]` would leave such a stream
/// as an Anthropic message that never stops, which no client can distinguish
/// from a response still in progress.
///
/// Idempotent via `state.terminated`, so a real `[DONE]` and an end-of-stream
/// call cannot both emit.
///
/// Deliberately NOT conditioned on `has_started`: an explicit `[DONE]` has
/// always produced the terminal events even with nothing before it, and that
/// is what the framing tests pin. The end-of-stream caller applies that check
/// itself, where synthesizing a terminal for a stream that produced nothing
/// would be new behaviour rather than preserved behaviour.
fn anthropic_stream_tail(state: &mut StreamState) -> String {
    if state.terminated {
        return String::new();
    }
    state.terminated = true;

    let mut output = String::new();
    if state.content_block_started {
        output.push_str(&sse_event(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": state.current_content_index }),
        ));
        state.content_block_started = false;
    }
    for index in &state.tool_calls_started {
        output.push_str(&sse_event(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": index }),
        ));
    }
    state.tool_calls_started.clear();

    let fr = state.finish_reason.as_deref();
    let is_error_finish = fr == Some("error") || fr.is_some_and(|f| f.ends_with("_error"));
    if is_error_finish {
        let error_type = match fr {
            Some(f) if f.ends_with("_error") => f,
            _ => "api_error",
        };
        output.push_str(&sse_event(
            "error",
            &json!({
                "type": "error",
                "error": { "type": error_type, "message": "The upstream provider returned an error" },
            }),
        ));
        return output;
    }
    output.push_str(&sse_event(
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": { "stop_reason": map_finish_reason(fr), "stop_sequence": Value::Null },
            "usage": { "input_tokens": state.input_tokens, "output_tokens": state.output_tokens },
        }),
    ));
    output.push_str("event: message_stop\ndata: {\"type\": \"message_stop\"}\n\n");
    output
}

fn openai_to_anthropic_messages_stream(
    event: &ParsedEvent,
    fallback_id: &str,
    state: &mut StreamState,
) -> Result<Option<String>, ()> {
    let payload = event.data.as_deref().unwrap_or("").trim();

    if payload == "[DONE]" {
        let tail = anthropic_stream_tail(state);
        return Ok(if tail.is_empty() { None } else { Some(tail) });
    }

    if payload.is_empty() {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(payload).map_err(|_| ())?;

    let mut output = String::new();
    if let Some(id) = parsed.get("id").and_then(Value::as_str) {
        state.id = Some(id.to_string());
    }
    if let Some(model) = parsed.get("model").and_then(Value::as_str) {
        state.model = Some(model.to_string());
    }
    if let Some(usage) = parsed.get("usage") {
        let prompt = i64_field(usage, "prompt_tokens");
        if prompt != 0 {
            state.input_tokens = prompt;
        }
        let completion = i64_field(usage, "completion_tokens");
        if completion != 0 {
            state.output_tokens = completion;
        }
    }

    if !state.has_started {
        let id = parsed
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| state.id.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| {
                if fallback_id.is_empty() {
                    format!("msg_{}", now_millis())
                } else {
                    fallback_id.to_string()
                }
            });
        let model = parsed
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| state.model.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "unknown".to_string());
        let input_tokens = parsed
            .get("usage")
            .map(|u| i64_field(u, "prompt_tokens"))
            .filter(|t| *t != 0)
            .unwrap_or(state.input_tokens);
        output.push_str(&sse_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": id, "type": "message", "role": "assistant", "content": [],
                    "model": model, "stop_reason": Value::Null, "stop_sequence": Value::Null,
                    "usage": { "input_tokens": input_tokens, "output_tokens": 0 },
                },
            }),
        ));
        state.has_started = true;
    }

    let choice = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first());
    let Some(choice) = choice else {
        return Ok(if output.is_empty() {
            None
        } else {
            Some(output)
        });
    };
    let delta = choice.get("delta");

    if let Some(content) = delta
        .and_then(|d| d.get("content"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        if !state.content_block_started {
            output.push_str(&sse_event(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": state.current_content_index,
                    "content_block": { "type": "text", "text": "" },
                }),
            ));
            state.content_block_started = true;
        }
        output.push_str(&sse_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": state.current_content_index,
                "delta": { "type": "text_delta", "text": content },
            }),
        ));
    }

    if let Some(tool_calls) = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for tool_call in tool_calls {
            let tool_index = state.current_content_index
                + 1
                + tool_call.get("index").and_then(Value::as_i64).unwrap_or(0);
            let id = tool_call.get("id").and_then(Value::as_str);
            let name = tool_call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str);
            if let (Some(id), Some(name)) = (id.filter(|s| !s.is_empty()), name) {
                if state.content_block_started && !state.tool_calls_started.contains(&tool_index) {
                    output.push_str(&sse_event(
                        "content_block_stop",
                        &json!({ "type": "content_block_stop", "index": state.current_content_index }),
                    ));
                    state.content_block_started = false;
                }
                output.push_str(&sse_event(
                    "content_block_start",
                    &json!({
                        "type": "content_block_start",
                        "index": tool_index,
                        "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} },
                    }),
                ));
                state.tool_calls_started.insert(tool_index);
            }
            if let Some(arguments) = tool_call
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
            {
                output.push_str(&sse_event(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": tool_index,
                        "delta": { "type": "input_json_delta", "partial_json": arguments },
                    }),
                ));
            }
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.finish_reason = Some(reason.to_string());
    }

    Ok(if output.is_empty() {
        None
    } else {
        Some(output)
    })
}

// ── Anthropic legacy completion SSE → OpenAI completion chunks ───────────────

fn anthropic_complete_stream(event: &ParsedEvent) -> Result<Option<String>, ()> {
    if event.name.as_deref() == Some("ping") {
        return Ok(None);
    }
    let payload = event.data.as_deref().unwrap_or("").trim();
    // No payload → no dispatch, mirroring the chat path.
    if payload.is_empty() {
        return Ok(None);
    }
    if payload == "[DONE]" {
        return Ok(Some("[DONE]".to_string()));
    }
    // Fail the stream on a malformed event rather than skip it.
    let parsed: Value = serde_json::from_str(payload).map_err(|_| ())?;
    let body = json!({
        "id": parsed.get("log_id").cloned().unwrap_or(Value::Null),
        "object": "text_completion",
        "created": now_secs(),
        "model": parsed.get("model").cloned().unwrap_or(Value::Null),
        "provider": "anthropic",
        "choices": [{
            "text": parsed.get("completion").cloned().unwrap_or(Value::Null),
            "index": 0,
            "logprobs": Value::Null,
            "finish_reason": parsed.get("stop_reason").cloned().unwrap_or(Value::Null),
        }],
    });
    Ok(Some(format!("data: {}\n\n", json_str(&body))))
}

fn json_str(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

// ── Stream adapter ───────────────────────────────────────────────────────────

/// Incremental SSE tokenizer: splits raw upstream bytes into events at blank
/// lines. A line terminator is LF, CRLF, or bare CR — a CR-LF pair counts as a
/// single terminator, tracked across chunk boundaries via `pending_cr`, so a
/// chunk ending in `\r` never mis-splits. The pending event is one contiguous
/// byte buffer (lines joined by `\n`), so the cap on its length — the same cap
/// as the meter's line buffer, and covering the open line as a prefix of it —
/// bounds actual memory, not just a logical byte count. Exceeding it is
/// reported as an error (the caller ends the stream as failed).
#[derive(Default)]
struct SseEventReader {
    event_buf: Vec<u8>,
    // Start offset of the open (unterminated) line within `event_buf`.
    line_start: usize,
    pending_cr: bool,
}

impl SseEventReader {
    // Feed one upstream chunk; events completed by it are appended to `events`.
    // `Err(())` means the pending event (hence any single line) ran past the cap.
    fn push_chunk(&mut self, chunk: &[u8], events: &mut Vec<String>) -> Result<(), ()> {
        for &byte in chunk {
            if std::mem::take(&mut self.pending_cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\r' | b'\n' => {
                    self.pending_cr = byte == b'\r';
                    self.end_line(events);
                }
                _ => self.event_buf.push(byte),
            }
            if self.event_buf.len() > MAX_SSE_LINE_BYTES {
                return Err(());
            }
        }
        Ok(())
    }

    fn end_line(&mut self, events: &mut Vec<String>) {
        if self.event_buf.len() == self.line_start {
            // Blank line: dispatch the pending event, if any. Consecutive blank
            // lines are empty events and dispatch nothing.
            if !self.event_buf.is_empty() {
                events.push(event_string(std::mem::take(&mut self.event_buf)));
            }
            self.line_start = 0;
            return;
        }
        self.event_buf.push(b'\n');
        self.line_start = self.event_buf.len();
    }

    // Flush the residual at a clean end of stream: an event without a trailing
    // blank line is still dispatched. The cap already held during accumulation.
    fn finish(&mut self) -> Option<String> {
        self.line_start = 0;
        (!self.event_buf.is_empty()).then(|| event_string(std::mem::take(&mut self.event_buf)))
    }
}

// Reuse the event buffer's allocation when it is valid UTF-8 (the
// overwhelmingly common case); fall back to a lossy copy only on invalid bytes.
fn event_string(buf: Vec<u8>) -> String {
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn framed_event(event: &str) -> String {
    let mut output = event.to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push('\n');
    output
}

fn replace_data_fields(event: &str, replacement: &str) -> String {
    let has_data_field = event.lines().any(|line| {
        line.trim_end_matches('\r')
            .split_once(':')
            .is_some_and(|(field, _)| field == "data")
    });
    let mut output = String::new();
    let mut replaced = false;
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        let is_data = line
            .split_once(':')
            .is_some_and(|(field, _)| field == "data");
        let trimmed = line.trim_start();
        let json_like = matches!(trimmed.as_bytes().first(), Some(b'{' | b'[' | b'"'));
        let is_bare_payload = !has_data_field
            && !line.is_empty()
            && !line.starts_with(':')
            && (json_like || !line.contains(':'));
        if is_data || is_bare_payload {
            if !replaced {
                if has_data_field {
                    output.push_str("data: ");
                }
                output.push_str(replacement);
                output.push('\n');
                replaced = true;
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    output.push('\n');
    output
}

/// Apply an in-place body edit to one SSE event, leaving the framing alone.
///
/// Shared by the two same-format transforms (reasoning exclusion and response
/// sanitization): both parse the payload, edit it, and re-emit only if it changed.
/// Keep-alives, empty payloads and `[DONE]` pass through untouched.
///
/// Never fails the stream. Both callers are cleanup passes layered on top of a
/// response that is already the client's surface — unlike a format conversion,
/// which must parse every frame to produce valid output, an edit that cannot
/// parse a frame has nothing to remove and passes it through verbatim. This is
/// deliberately more tolerant than the conversions: sanitization is applied to
/// every stream including same-format passthrough, which historically forwarded
/// bytes without parsing at all, so a single non-JSON frame must not turn a stream
/// that used to succeed into a failure.
fn edit_event_body(event: &str, edit: impl FnOnce(&mut Value)) -> String {
    let parsed = parse_event(event);
    let Some(payload) = parsed.data.as_deref() else {
        return framed_event(event);
    };
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return framed_event(event);
    }
    let Ok(mut body) = serde_json::from_str::<Value>(payload) else {
        return framed_event(event);
    };
    let original = body.clone();
    edit(&mut body);
    if body == original {
        framed_event(event)
    } else {
        replace_data_fields(event, &json_str(&body))
    }
}

/// Splits the provider byte stream into SSE events and applies a stateful
/// transform to each, emitting client-surface bytes.
pub struct SseTransformStream {
    inner: ServiceResponseStream,
    transform: StreamTransform,
    fallback_id: String,
    state: StreamState,
    reader: SseEventReader,
    queue: VecDeque<Bytes>,
    inner_done: bool,
    // Why the stream ended, when it ended badly. Held until the queue drains so
    // events completed before the failure still reach the client, then yielded
    // as the final item. Ending on a plain `None` would instead read downstream
    // as a clean end-of-stream.
    pending_error: Option<ServiceError>,
}

impl SseTransformStream {
    pub fn new(inner: ServiceResponseStream, transform: StreamTransform) -> Self {
        let fallback_id = format!("{}-{}", transform.provider(), now_millis());
        Self {
            inner,
            transform,
            fallback_id,
            state: StreamState::default(),
            reader: SseEventReader::default(),
            queue: VecDeque::new(),
            inner_done: false,
            pending_error: None,
        }
    }

    // A transform-side failure is still an upstream failure: the provider sent
    // bytes this surface cannot represent.
    fn fail(&mut self, reason: &'static str) {
        self.inner_done = true;
        self.pending_error.get_or_insert_with(|| {
            ServiceError::Upstream(UpstreamError::Transport(reason.to_string()))
        });
    }

    // Returns false if the transform rejected the event (unparseable provider
    // data): the stream must end there, with no terminal marker emitted, so the
    // meter classifies it as failed.
    fn emit(&mut self, event: &str) -> bool {
        match self
            .transform
            .apply(event, &self.fallback_id, &mut self.state)
        {
            Ok(Some(out)) => {
                self.queue.push_back(Bytes::from(out));
                true
            }
            Ok(None) => true,
            Err(()) => false,
        }
    }
}

impl Stream for SseTransformStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(bytes) = this.queue.pop_front() {
                return Poll::Ready(Some(Ok(bytes)));
            }
            if this.inner_done {
                // The queue is drained; surface why the stream ended, once.
                return Poll::Ready(this.pending_error.take().map(Err));
            }
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let mut events = Vec::new();
                    // A cap overflow ends the stream, but events completed
                    // earlier in the same chunk are still emitted first.
                    let overflowed = this.reader.push_chunk(&bytes, &mut events).is_err();
                    for event in &events {
                        if !this.emit(event) {
                            this.fail("provider sent an event this surface cannot represent");
                            break;
                        }
                    }
                    if overflowed {
                        this.fail("provider SSE event exceeded the size cap");
                    }
                    // loop to flush the queue or poll again
                }
                Poll::Ready(None) => {
                    this.inner_done = true;
                    // Flush a residual event once (no trailing blank line).
                    // Deliberate cross-format framing normalization: a final
                    // event whose lines are complete but which the upstream
                    // never closed with a blank line is salvaged and re-framed,
                    // rather than dropped as WHATWG would at EOF. Delivering a
                    // valid last event beats losing it; a truncated (unparseable)
                    // one still fails in `emit`. A byte passthrough cannot do
                    // this, so the two paths differ for this one malformed shape.
                    if let Some(event) = this.reader.finish() {
                        if !this.emit(&event) {
                            this.fail("provider sent an event this surface cannot represent");
                        }
                    }
                    // An Anthropic message has to be closed explicitly, and
                    // `[DONE]` — the only thing that used to trigger it — is an
                    // OpenAI convention some upstreams omit entirely. Synthesize
                    // the tail here so those streams still terminate.
                    //
                    // Only on a clean, protocol-complete end: `pending_error`
                    // means the transport failed, while a missing finish reason
                    // means it ended before OpenAI declared the generation done.
                    // A terminal marker in either case would make a truncated
                    // generation look successful. A no-op if a real `[DONE]`
                    // already closed the message.
                    if matches!(this.transform, StreamTransform::OpenaiToAnthropicMessages)
                        && this.pending_error.is_none()
                        && this.state.has_started
                        && !this.state.terminated
                    {
                        if this.state.finish_reason.is_some() {
                            let tail = anthropic_stream_tail(&mut this.state);
                            if !tail.is_empty() {
                                this.queue.push_back(Bytes::from(tail));
                            }
                        } else {
                            this.fail("provider ended before sending a finish reason");
                        }
                    }
                    // loop to drain any queued output, then end.
                }
                // On an upstream error, end without flushing the (truncated)
                // residual, so a partial event can't synthesize a spurious
                // terminal marker. The error itself is propagated: swallowing
                // it would read downstream as a clean end-of-stream.
                Poll::Ready(Some(Err(err))) => {
                    this.inner_done = true;
                    this.pending_error.get_or_insert(err);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn malformed_anthropic_event_ends_stream_without_terminal() {
        // A bad provider event must end the stream before [DONE], so the meter
        // sees no terminal marker and classifies the stream as failed.
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"c\",\"usage\":{\"input_tokens\":1}}}\n\n",
            )),
            Ok(Bytes::from("event: content_block_delta\ndata: {not json\n\n")),
            Ok(Bytes::from(
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            )),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Result<Bytes, ServiceError>> = stream.collect().await;
        assert!(
            collected.last().expect("stream yielded items").is_err(),
            "a malformed event ends the stream as an error, not a clean EOF"
        );
        let text: String = collected
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(text.contains("chat.completion.chunk"));
        assert!(
            !text.contains("[DONE]"),
            "stream must not emit a terminal after a malformed event: {text}"
        );
    }

    // A CRLF-framed upstream must be split per event, not buffered whole: with a
    // fixed `\n\n` delimiter the events would only surface as one unparseable
    // residual at end of stream (no [DONE], stream classified failed). The
    // doubled blank line after the first event is an empty SSE event; it must be
    // skipped, not surfaced as a lone-`\r` pseudo-event that fails the stream.
    #[tokio::test]
    async fn crlf_framed_events_are_split_and_transformed() {
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(
                "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"c\",\"usage\":{\"input_tokens\":1}}}\r\n\r\n\r\n\r\n",
            )),
            Ok(Bytes::from(
                "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\r\n\r\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
            )),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Bytes> = stream.map(|r| r.unwrap()).collect().await;
        let text: String = collected
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(
            text.contains("\"content\":\"hi\""),
            "delta transformed: {text}"
        );
        assert!(
            text.contains("[DONE]"),
            "message_stop mapped to terminal: {text}"
        );
    }

    // Comment-only events (proxy heartbeats) and space-less `data:` lines are
    // valid SSE; neither may end the stream. Both previously hit the
    // malformed-event path on the Anthropic transforms (no terminal marker, so
    // a healthy stream was metered as failed).
    #[tokio::test]
    async fn comment_and_spaceless_data_events_are_tolerated() {
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(": PROCESSING\n\n")),
            Ok(Bytes::from(
                "event: message_start\ndata:{\"type\":\"message_start\",\"message\":{\"model\":\"c\",\"usage\":{\"input_tokens\":1}}}\n\n",
            )),
            Ok(Bytes::from(
                "event: message_stop\ndata:{\"type\":\"message_stop\"}\n\n",
            )),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Bytes> = stream.map(|r| r.unwrap()).collect().await;
        let text: String = collected
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(text.contains("chat.completion.chunk"), "{text}");
        assert!(
            text.contains("[DONE]"),
            "stream must reach its terminal: {text}"
        );
    }

    // Bare-CR line terminators are valid SSE (a `\r\r` blank line ends an
    // event); a CR-framed stream must split per event, not run into the cap.
    // The first chunk ends in `\r` to exercise the cross-chunk CR/CRLF
    // ambiguity: the reader must not mis-split when the next byte arrives.
    #[tokio::test]
    async fn cr_framed_events_are_split_and_transformed() {
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(
                "event: message_start\rdata: {\"type\":\"message_start\",\"message\":{\"model\":\"c\",\"usage\":{\"input_tokens\":1}}}\r\r",
            )),
            Ok(Bytes::from(
                "event: message_stop\rdata: {\"type\":\"message_stop\"}\r\r",
            )),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Bytes> = stream.map(|r| r.unwrap()).collect().await;
        let text: String = collected
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(text.contains("chat.completion.chunk"), "{text}");
        assert!(
            text.contains("[DONE]"),
            "CR framing reaches terminal: {text}"
        );
    }

    // A comment line attached to the head of an event (a heartbeat injected
    // without its own blank line) and a space-less `event:` field are both
    // valid SSE; neither may derail the transform.
    #[test]
    fn comment_lines_and_spaceless_event_field_are_parsed() {
        let mut state = StreamState::default();
        let out = StreamTransform::AnthropicToOpenaiChat
            .apply(
                ": heartbeat\nevent:message_stop\ndata:{\"type\":\"message_stop\"}",
                "fb",
                &mut state,
            )
            .unwrap()
            .expect("message_stop dispatches its terminal");
        assert!(out.contains("[DONE]"), "{out}");
    }

    // Regressions of the field parser against the old prefix-stripping code:
    // a bare (data:-less) payload must survive an `event:` line or an injected
    // comment in the same event block; `id:`/`retry:`-only and empty-data
    // events must dispatch nothing instead of failing the stream; a trailing
    // space after an event name must not break exact-match dispatch.
    #[test]
    fn bare_payloads_and_ignorable_events_are_handled() {
        let mut state = StreamState::default();
        let out = StreamTransform::AnthropicCompleteToOpenai
            .apply(
                "event: completion\n  {\"completion\":\"hi\"}",
                "fb",
                &mut state,
            )
            .unwrap()
            .expect("bare payload after an event line still transforms");
        assert!(out.contains("\"text\":\"hi\""), "{out}");

        let mut state = StreamState::default();
        let out = StreamTransform::OpenaiToAnthropicMessages
            .apply(": keepalive\n[DONE]", "fb", &mut state)
            .unwrap()
            .expect("a comment must not swallow the bare terminal");
        assert!(out.contains("message_stop"), "{out}");

        let mut state = StreamState::default();
        for ignorable in [
            "retry: 3000",
            "id: 7",
            "x-proxy: heartbeat",
            "data:",
            "data: ",
            "event: heartbeat",
            "event: heartbeat\ndata:",
        ] {
            assert!(
                matches!(
                    StreamTransform::AnthropicToOpenaiChat.apply(ignorable, "fb", &mut state),
                    Ok(None)
                ),
                "{ignorable:?} dispatches nothing"
            );
        }

        let mut state = StreamState::default();
        assert!(
            matches!(
                StreamTransform::AnthropicToOpenaiChat.apply(
                    "event: ping \ndata: {}",
                    "fb",
                    &mut state
                ),
                Ok(None)
            ),
            "trailing space after the event name is tolerated"
        );
    }

    // `data:[DONE]` without the optional space must terminate the
    // OpenAI→Anthropic stream, not be skipped as an unparseable event.
    #[test]
    fn spaceless_done_terminates_openai_to_anthropic() {
        let mut state = StreamState::default();
        let out = StreamTransform::OpenaiToAnthropicMessages
            .apply("data:[DONE]", "fb", &mut state)
            .unwrap()
            .expect("[DONE] emits the terminal events");
        assert!(out.contains("message_stop"), "{out}");
    }

    // A body that never yields an event boundary must not accumulate without
    // bound: past the cap the stream ends with no terminal marker (metered
    // failed), mirroring the meter's own line cap.
    #[tokio::test]
    async fn boundless_body_is_capped() {
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(vec![b'x'; MAX_SSE_LINE_BYTES + 1])),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::OpenaiToAnthropicMessages);
        let collected: Vec<Result<Bytes, ServiceError>> = stream.collect().await;
        assert_eq!(collected.len(), 1, "no payload, only the failure");
        assert!(collected[0].is_err(), "and not a clean EOF");
    }

    /// The full production pipeline for a `/v1/messages` client of an OpenAI
    /// upstream: the format conversion runs first and nests the upstream id and
    /// model inside a `message_start`, then the identity rewrite runs. End to end,
    /// the client must see neither. This is the stacked-transform case a
    /// single-layer unit test cannot cover.
    #[tokio::test]
    async fn anthropic_stream_pipeline_hides_upstream_id_and_model() {
        let upstream = "data: {\"id\":\"chatcmpl-7bdaaade5030\",\"model\":\"vendor-model-int\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"}}]}\n\n";
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(upstream)),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let converted = Box::pin(SseTransformStream::new(
            inner,
            StreamTransform::OpenaiToAnthropicMessages,
        ));
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        });
        // Messages surface: canonicalize no-ops for an unschematized surface, so
        // the anthropic events are left intact and only id/model are rewritten.
        let sanitized = SseTransformStream::new(
            converted,
            StreamTransform::SanitizeResponse(identity, Endpoint::Messages),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = sanitized.collect().await;

        let wire: String = collected
            .into_iter()
            .map(|c| String::from_utf8(c.unwrap().to_vec()).unwrap())
            .collect();
        assert!(wire.contains("message_start"), "sanity: {wire}");
        assert!(!wire.contains("chatcmpl-7bdaaade5030"), "leaked id: {wire}");
        assert!(!wire.contains("vendor-model-int"), "leaked model: {wire}");
        assert!(wire.contains("req_ours"), "{wire}");
        assert!(wire.contains("acme/model-a"), "{wire}");
        assert!(wire.contains("\"text\":\"hi\""), "content survived: {wire}");
    }

    // Replay a fixture's input events through the transform (fixed fallback id),
    // collecting emitted data objects with `created` stripped (and a `__done`
    // sentinel for `[DONE]`), to compare against the Node-generated output.
    fn replay_fixture(transform: StreamTransform, events: &[Value]) -> Vec<Value> {
        let mut state = StreamState::default();
        let mut out = Vec::new();
        for event in events {
            let Ok(Some(result)) = transform.apply(event.as_str().unwrap(), "fb", &mut state)
            else {
                continue;
            };
            for piece in result.split("\n\n") {
                let Some(data) = piece.lines().find_map(|l| l.strip_prefix("data: ")) else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    out.push(json!({ "__done": true }));
                } else if let Ok(mut value) = serde_json::from_str::<Value>(data) {
                    if let Some(map) = value.as_object_mut() {
                        map.remove("created");
                    }
                    out.push(value);
                }
            }
        }
        out
    }

    #[test]
    fn stream_transforms_match_node_fixtures() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/stream_golden.json"))
                .expect("parse stream fixtures");
        assert!(!cases.is_empty());
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            let transform = match (
                case["format"].as_str().unwrap(),
                case["fn"].as_str().unwrap(),
            ) {
                ("anthropic", "chatComplete") => StreamTransform::AnthropicToOpenaiChat,
                ("anthropic", "complete") => StreamTransform::AnthropicCompleteToOpenai,
                ("openai", "messages") => StreamTransform::OpenaiToAnthropicMessages,
                other => panic!("unmapped stream fixture {other:?}"),
            };
            let events = case["events"].as_array().unwrap();
            let output = replay_fixture(transform, events);
            assert_eq!(
                Value::Array(output),
                case["output"],
                "stream case {name} diverges from Node"
            );
        }
    }

    fn run(transform: StreamTransform, events: &[&str]) -> Vec<Value> {
        let mut state = StreamState::default();
        let mut out = Vec::new();
        for event in events {
            if let Ok(Some(result)) = transform.apply(event, "fallback-1", &mut state) {
                // A transform may emit multiple concatenated SSE events.
                for piece in result.split("\n\n") {
                    let data = piece
                        .lines()
                        .find_map(|l| l.strip_prefix("data: "))
                        .unwrap_or("");
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    if let Ok(mut v) = serde_json::from_str::<Value>(data) {
                        if let Some(o) = v.as_object_mut() {
                            o.remove("created");
                        }
                        out.push(v);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn openai_to_anthropic_stream_error_finish_emits_error_event() {
        let events = [
            "data: {\"id\":\"c1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"overloaded_error\"}]}",
            "data: [DONE]",
        ];
        let out = run(StreamTransform::OpenaiToAnthropicMessages, &events);
        let error = out.iter().find(|e| e["type"] == json!("error")).unwrap();
        assert_eq!(error["error"]["type"], json!("overloaded_error"));
        assert!(!out.iter().any(|e| e["type"] == json!("message_stop")));
    }

    #[tokio::test]
    async fn upstream_that_omits_done_still_terminates_the_anthropic_message() {
        // `[DONE]` is an OpenAI convention, not an SSE requirement. GMI's
        // OpenAI-compatible endpoint ends after its final usage chunk without
        // one; a client would otherwise wait on a message that never stops.
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(
                "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\n",
            )),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::OpenaiToAnthropicMessages);
        let text: String = stream
            .collect::<Vec<_>>()
            .await
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();

        assert!(text.contains("message_stop"), "{text}");
        assert_eq!(
            text.matches("message_stop").count(),
            2, // the `event:` line and the payload's "type"
            "the message must be closed exactly once: {text}"
        );
        assert!(text.contains("content_block_stop"), "{text}");
        assert!(
            text.contains("\"output_tokens\":4"),
            "the tail carries the upstream token counts: {text}"
        );
    }

    #[tokio::test]
    async fn a_failed_stream_gets_no_synthesized_terminal() {
        // A terminal on a failed stream would read downstream as a complete
        // response and have the meter score a truncated generation as success.
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(
                "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            )),
            Err(ServiceError::Upstream(UpstreamError::Transport(
                "connection reset".to_string(),
            ))),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::OpenaiToAnthropicMessages);
        let collected: Vec<Result<Bytes, ServiceError>> = stream.collect().await;
        assert!(collected.last().expect("stream yielded items").is_err());
        let text: String = collected
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(
            !text.contains("message_stop"),
            "a failed stream must not be closed as if it succeeded: {text}"
        );
    }

    #[tokio::test]
    async fn a_truncated_final_openai_event_gets_no_synthesized_terminal() {
        let events: Vec<Result<Bytes, ServiceError>> = vec![
            Ok(Bytes::from(
                "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            )),
            Ok(Bytes::from("data: {\"choices\":[")),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::OpenaiToAnthropicMessages);
        let collected: Vec<Result<Bytes, ServiceError>> = stream.collect().await;

        assert!(collected.last().expect("stream yielded items").is_err());
        let text: String = collected
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(
            !text.contains("message_stop"),
            "a truncated provider event must not be closed as a successful response: {text}"
        );
    }

    #[tokio::test]
    async fn openai_eof_without_a_finish_reason_is_not_completed() {
        let events: Vec<Result<Bytes, ServiceError>> = vec![Ok(Bytes::from(
            "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ))];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let stream = SseTransformStream::new(inner, StreamTransform::OpenaiToAnthropicMessages);
        let collected: Vec<Result<Bytes, ServiceError>> = stream.collect().await;

        assert!(collected.last().expect("stream yielded items").is_err());
        let text: String = collected
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(
            !text.contains("message_stop"),
            "EOF before finish_reason must not look successful: {text}"
        );
    }

    #[test]
    fn reasoning_exclusion_preserves_non_data_sse_fields() {
        let output = edit_event_body(
            "event: chunk\nid: 7\nretry: 100\n: keep\n{\"choices\":[{\"delta\":{\"content\":\"answer\",\"reasoning\":\"hidden\"}}],\"usage\":{\"completion_tokens_details\":{\"reasoning_tokens\":3}}}",
            response_transform::exclude_reasoning,
        );
        assert!(output.contains("event: chunk\n"), "{output}");
        assert!(output.contains("id: 7\n"), "{output}");
        assert!(output.contains("retry: 100\n"), "{output}");
        assert!(output.contains(": keep\n"), "{output}");
        assert!(output.contains("\"content\":\"answer\""), "{output}");
        assert!(output.contains("\"reasoning_tokens\":3"), "{output}");
        assert!(!output.contains("hidden"), "{output}");
    }

    /// The same-format streaming path is the one that used to hand provider
    /// bytes to the client untouched, so the rewrite is verified on an event, not
    /// only on a buffered body.
    #[test]
    fn identity_rewrites_a_streamed_chunk() {
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        });
        let mut state = StreamState::default();
        let output = StreamTransform::SanitizeResponse(identity, Endpoint::ChatComplete)
            .apply(
                "data: {\"id\":\"b4fa5a1dc59c4b41\",\"model\":\"vendor-model-int\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"matched_stop\":424242}]}",
                "fb",
                &mut state,
            )
            .unwrap()
            .unwrap();
        assert!(output.contains("req_ours"), "{output}");
        assert!(output.contains("acme/model-a"), "{output}");
        assert!(!output.contains("matched_stop"), "{output}");
        assert!(!output.contains("vendor-model-int"), "{output}");
        assert!(output.contains("\"content\":\"hi\""), "{output}");
    }

    /// A same-format passthrough stream historically forwarded bytes without
    /// parsing, so a lone non-JSON `data:` frame reached the client and the
    /// stream still succeeded. Sanitization runs on every stream now, so it must
    /// be no stricter: a frame it cannot parse is passed through, never rejected
    /// (which `emit` would turn into a failed, truncated stream).
    #[test]
    fn identity_passes_through_an_unparseable_frame() {
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: None,
        });
        let mut state = StreamState::default();
        let result = StreamTransform::SanitizeResponse(identity, Endpoint::ChatComplete).apply(
            "data: : upstream keep-alive text, not json",
            "fb",
            &mut state,
        );
        assert!(
            result.is_ok(),
            "an unparseable frame must not fail the stream"
        );
        assert!(result
            .unwrap()
            .unwrap()
            .contains("upstream keep-alive text"));
    }

    /// `[DONE]` and keep-alives carry no JSON; sanitization must leave them intact
    /// or the client sees a truncated stream.
    #[test]
    fn identity_passes_through_terminators_and_keepalives() {
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: None,
        });
        let transform = StreamTransform::SanitizeResponse(identity, Endpoint::ChatComplete);
        let mut state = StreamState::default();
        for event in ["data: [DONE]", ": PROCESSING", "data: "] {
            let output = transform
                .apply(event, "fb", &mut state)
                .unwrap_or_else(|_| panic!("rejected {event}"))
                .unwrap();
            assert!(
                output.starts_with(event.split('\n').next().unwrap()),
                "{output}"
            );
        }
    }

    #[test]
    fn selection_matrix() {
        assert!(matches!(
            select_stream_transform(ProviderFormat::Anthropic, Endpoint::ChatComplete),
            Some(StreamTransform::AnthropicToOpenaiChat)
        ));
        assert!(matches!(
            select_stream_transform(ProviderFormat::Openai, Endpoint::Messages),
            Some(StreamTransform::OpenaiToAnthropicMessages)
        ));
        assert!(select_stream_transform(ProviderFormat::Openai, Endpoint::ChatComplete).is_none());
    }
}
