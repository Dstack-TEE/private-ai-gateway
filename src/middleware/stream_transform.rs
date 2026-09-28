//! Stateful SSE response transforms: convert an upstream provider's streaming
//! events into the downstream client surface, event by event, threading mutable
//! state across events (a per-stream transform state).
//!
//! Provider conversions are selected by upstream format and client surface.
//! Same-format streaming reaches this module too: every stream is sanitized
//! here (identity rewrite + canonicalize, per chunk), and reasoning is excluded
//! when the client opts out. Cost injection, TTFT, and outcome are a separate
//! metering pass downstream (`sse`).

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use axum::body::Bytes;
use futures_util::Stream;
use serde_json::{json, Value};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{ServiceError, ServiceResponseStream};

use super::request_transform::{Endpoint, ResponsesToolMap};
use super::response_transform::{
    self, anthropic_usage, chat_cache_tokens, function_call_arguments, i64_field,
    invalid_tool_call_arguments_error, invalid_tool_call_identity_error, item_id, message_item,
    normalize_reasoning_usage_value, now_millis, now_secs, output_text_part, reasoning_item,
    reasoning_text, refusal_part, responses_call_item, responses_call_value, responses_object,
    responses_usage, thinking_block, transform_finish_reason, upstream_failed_error, ChatFinish,
    ResponseIdentity, ResponsesHead,
};
use super::sse::MAX_SSE_LINE_BYTES;
use super::types::ProviderFormat;

const STRICT_OPENAI_COMPLIANCE: bool = true;

/// Where the Responses bridge leaves the upstream's own usage for the meter.
///
/// The meter sits after the bridge and would otherwise read the Responses usage
/// the client receives. That shape has no field for cache-creation tokens, and
/// its `input_tokens` is a total where the control plane reads the same name as
/// cache-excluded. The chat usage the upstream sent is unambiguous, and is what
/// the buffered path reports.
pub type UpstreamUsage = Arc<Mutex<Option<Value>>>;

/// Which streaming transform applies.
///
/// `Clone` rather than `Copy`: `SanitizeResponse` carries per-request values
/// (our request id, the client's model name), which no unit variant can.
#[derive(Debug, Clone)]
pub enum StreamTransform {
    AnthropicToOpenaiChat,
    OpenaiToAnthropicMessages,
    OpenaiChatToResponses(Arc<Value>, Arc<ResponseIdentity>, UpstreamUsage),
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
            StreamTransform::OpenaiChatToResponses(..) => "openai",
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
                    response_transform::canonicalize(
                        body,
                        *endpoint,
                        Some(identity.request_id.as_str()),
                    );
                })))
            }
            _ => {}
        }
        let event = parse_event(event);
        match self {
            StreamTransform::AnthropicToOpenaiChat => {
                anthropic_chat_stream(&event, fallback_id, state, STRICT_OPENAI_COMPLIANCE)
            }
            StreamTransform::OpenaiToAnthropicMessages => state
                .messages
                .get_or_insert_with(|| ChatStream::new(MessagesEvents::default()))
                .event(&event, fallback_id),
            StreamTransform::OpenaiChatToResponses(..) => {
                self.responses_stream(state).event(&event, fallback_id)
            }
            StreamTransform::AnthropicCompleteToOpenai => anthropic_complete_stream(&event),
            StreamTransform::ExcludeReasoning | StreamTransform::SanitizeResponse(_, _) => {
                unreachable!("handled above")
            }
        }
    }

    /// Close what the stream left open when the upstream ends it. `Err(())`
    /// means it ended before the response was complete.
    fn end_of_stream(&self, state: &mut StreamState) -> Result<Option<String>, ()> {
        match self {
            StreamTransform::OpenaiToAnthropicMessages => state
                .messages
                .as_mut()
                .map_or(Ok(None), ChatStream::end_of_stream),
            StreamTransform::OpenaiChatToResponses(..) => state
                .responses
                .as_mut()
                .map_or(Ok(None), ChatStream::end_of_stream),
            _ => Ok(None),
        }
    }

    fn responses_stream<'a>(
        &self,
        state: &'a mut StreamState,
    ) -> &'a mut ChatStream<ResponsesEvents> {
        let StreamTransform::OpenaiChatToResponses(echo, identity, upstream_usage) = self else {
            unreachable!("only the Responses bridge has a Responses stream");
        };
        state.responses.get_or_insert_with(|| {
            ChatStream::new(ResponsesEvents::new(
                echo.clone(),
                identity.clone(),
                upstream_usage.clone(),
            ))
        })
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

/// Mutable per-stream state threaded across events. A given stream only
/// touches the part for its transform.
#[derive(Default)]
struct StreamState {
    // Anthropic -> OpenAI chat.
    tool_index: Option<i64>,
    usage: Option<Value>,
    model: Option<String>,
    // OpenAI chat -> Anthropic Messages / Responses, built on first use.
    messages: Option<ChatStream<MessagesEvents>>,
    responses: Option<ChatStream<ResponsesEvents>>,
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
            // The error rides in the chunk's `error` member, as it does on the
            // same-format path: that is the member a client detects a failed
            // 200 stream by, and the sanitize stage rebuilds its interior into
            // this surface's vocabulary. Putting the upstream's error type in
            // `finish_reason` instead left the client no error to find and
            // relayed a word this surface does not use.
            let body = json!({
                "id": fallback_id,
                "object": "chat.completion.chunk",
                "created": now_secs(),
                "model": "",
                "error": error.clone(),
                // No finish reason: the stream did not finish, it failed, and
                // `stop` would tell a client reading only this field that a
                // truncated response completed normally. The `error` member
                // above is what says what happened.
                "choices": [{
                    "finish_reason": Value::Null,
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

// ── OpenAI chat.completion SSE → Messages / Responses SSE ────────────────────
//
// Both bridges read a Chat stream the same way and differ only in the events
// they write. `ChatStream` owns the reading — strict chunk parsing, tool-call
// assembly, block order, the terminal decision — and drives a `ChatEvents`
// writer for the client's surface.
//
// Output is strictly sequential, as the native protocols stream it: one text,
// reasoning or tool-call block is open at a time, and a block closes when the
// upstream moves on to another. Tool calls open in index order once their id
// and name are known; arguments that arrive earlier are held until then.

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {}\n\n", json_str(data))
}

/// A string field that may be absent or null; any other type is malformed.
fn optional_stream_string(value: Option<&Value>) -> Result<&str, ()> {
    match value {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(()),
    }
}

/// One Chat chunk, strictly typed: a field of the wrong type fails the stream
/// rather than being read as absent.
struct ChatChunk<'a> {
    usage: Option<&'a Value>,
    error: Option<&'a Value>,
    reasoning: Option<&'a str>,
    content: &'a str,
    refusal: &'a str,
    tool_calls: Vec<ToolCallDelta<'a>>,
    finish_reason: Option<&'a str>,
}

struct ToolCallDelta<'a> {
    index: Option<usize>,
    id: &'a str,
    name: &'a str,
    arguments: &'a str,
}

fn parse_chat_chunk(chunk: &Value) -> Result<ChatChunk<'_>, ()> {
    let choice = chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());
    let delta = match choice.and_then(|choice| choice.get("delta")) {
        None | Some(Value::Null) => None,
        Some(Value::Object(delta)) => Some(delta),
        Some(_) => return Err(()),
    };
    let field = |key| optional_stream_string(delta.and_then(|delta| delta.get(key)));
    let tool_calls = match delta.and_then(|delta| delta.get("tool_calls")) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(calls)) => calls
            .iter()
            .map(parse_tool_call_delta)
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(()),
    };
    Ok(ChatChunk {
        usage: chunk.get("usage").filter(|usage| !usage.is_null()),
        error: chunk.get("error").filter(|error| !error.is_null()),
        reasoning: choice
            .and_then(|choice| choice.get("delta"))
            .and_then(reasoning_text),
        content: field("content")?,
        refusal: field("refusal")?,
        tool_calls,
        finish_reason: match choice.and_then(|choice| choice.get("finish_reason")) {
            None | Some(Value::Null) => None,
            Some(Value::String(reason)) => Some(reason),
            Some(_) => return Err(()),
        },
    })
}

fn parse_tool_call_delta(value: &Value) -> Result<ToolCallDelta<'_>, ()> {
    let call = value.as_object().ok_or(())?;
    let index = match call.get("index") {
        None | Some(Value::Null) => None,
        Some(index) => Some(
            index
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or(())?,
        ),
    };
    let function = match call.get("function") {
        None | Some(Value::Null) => None,
        Some(Value::Object(function)) => Some(function),
        Some(_) => return Err(()),
    };
    let function_field =
        |key| optional_stream_string(function.and_then(|function| function.get(key)));
    Ok(ToolCallDelta {
        index,
        id: optional_stream_string(call.get("id"))?,
        name: function_field("name")?,
        arguments: function_field("arguments")?,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Reasoning,
    Text,
    Refusal,
}

/// A Chat tool call assembled from deltas.
#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
    phase: CallPhase,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CallPhase {
    #[default]
    Pending,
    Open,
    Closed,
}

impl ToolCall {
    fn identified(&self) -> bool {
        !self.id.trim().is_empty() && !self.name.trim().is_empty()
    }

    fn started(&self) -> bool {
        !self.id.trim().is_empty()
            || !self.name.trim().is_empty()
            || !self.arguments.trim().is_empty()
    }
}

/// Identity fields may arrive late or repeat, but must not change. There is no
/// unambiguous way to distinguish fragmented names from cumulative updates.
fn set_tool_identity(current: &mut String, value: &str) -> Result<(), ()> {
    if value.is_empty() || current == value {
        return Ok(());
    }
    if !current.is_empty() {
        return Err(());
    }
    current.push_str(value);
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Text(TextKind),
    Call(usize),
}

/// Why a stream that ended in-band did not complete.
enum Failure<'a> {
    /// The upstream reported an error, in an `error` member or its finish.
    Upstream {
        error: Option<&'a Value>,
        reason: Option<&'a str>,
    },
    ToolCallIdentity,
    ToolCallArguments,
}

enum Outcome<'a> {
    Finished { finish: ChatFinish, tool_use: bool },
    Failed(Failure<'a>),
}

/// The events one client surface writes for a Chat stream.
trait ChatEvents {
    /// The first chunk arrived: the message or response begins.
    fn start(&mut self, chunk: &Value, fallback_id: &str, out: &mut String);
    /// The upstream reported usage (reasoning tokens already normalized).
    fn usage(&mut self, _usage: &Value) {}
    fn open_text(&mut self, kind: TextKind, out: &mut String);
    fn text_delta(&mut self, kind: TextKind, delta: &str, out: &mut String);
    fn close_text(&mut self, kind: TextKind, text: &str, out: &mut String);
    /// A tool call starts, with the arguments that arrived before its identity.
    fn open_call(&mut self, call: &ToolCall, out: &mut String);
    fn call_delta(&mut self, delta: &str, out: &mut String);
    /// Close the call; false when the client could not execute it.
    fn close_call(&mut self, call: &ToolCall, out: &mut String) -> bool;
    fn finish(&mut self, outcome: Outcome<'_>, usage: Option<&Value>, out: &mut String);
}

struct ChatStream<E> {
    events: E,
    started: bool,
    terminated: bool,
    open: Option<Block>,
    /// The open text block's text so far.
    text: String,
    calls: BTreeMap<usize, ToolCall>,
    /// The next index allowed to open; calls open in index order.
    next_call: usize,
    /// The call an index-less, id-less delta continues.
    last_call: Option<usize>,
    executable_calls: usize,
    invalid_calls: bool,
    finish_reason: Option<String>,
    usage: Option<Value>,
    error: Option<Value>,
}

impl<E: ChatEvents> ChatStream<E> {
    fn new(events: E) -> Self {
        Self {
            events,
            started: false,
            terminated: false,
            open: None,
            text: String::new(),
            calls: BTreeMap::new(),
            next_call: 0,
            last_call: None,
            executable_calls: 0,
            invalid_calls: false,
            finish_reason: None,
            usage: None,
            error: None,
        }
    }

    fn event(&mut self, event: &ParsedEvent, fallback_id: &str) -> Result<Option<String>, ()> {
        let payload = event.data.as_deref().unwrap_or("").trim();
        let mut out = String::new();
        if payload == "[DONE]" {
            self.finish(&mut out)?;
        } else if !payload.is_empty() && !self.terminated {
            let chunk: Value = serde_json::from_str(payload).map_err(|_| ())?;
            self.chunk(&chunk, fallback_id, &mut out)?;
        }
        Ok((!out.is_empty()).then_some(out))
    }

    /// The upstream closed the connection. `[DONE]` is an OpenAI convention
    /// some upstreams omit, so a stream that already declared its finish (or
    /// its error) still terminates; one that did not was cut off.
    fn end_of_stream(&mut self) -> Result<Option<String>, ()> {
        if !self.started || self.terminated {
            return Ok(None);
        }
        if self.finish_reason.is_none() && self.error.is_none() {
            return Err(());
        }
        let mut out = String::new();
        self.finish(&mut out)?;
        Ok((!out.is_empty()).then_some(out))
    }

    fn chunk(&mut self, value: &Value, fallback_id: &str, out: &mut String) -> Result<(), ()> {
        let chunk = parse_chat_chunk(value)?;
        if !self.started {
            self.started = true;
            self.events.start(value, fallback_id, out);
        }
        if let Some(usage) = chunk.usage {
            let mut usage = usage.clone();
            let _ = normalize_reasoning_usage_value(&mut usage);
            self.events.usage(&usage);
            self.usage = Some(usage);
        }
        if let Some(error) = chunk.error {
            self.error = Some(error.clone());
        }
        for (kind, text) in [
            (TextKind::Reasoning, chunk.reasoning.unwrap_or("")),
            (TextKind::Text, chunk.content),
            (TextKind::Refusal, chunk.refusal),
        ] {
            if !text.is_empty() {
                self.text(kind, text, out);
            }
        }
        for delta in &chunk.tool_calls {
            self.tool_delta(delta, out)?;
        }
        if let Some(reason) = chunk.finish_reason {
            self.finish_reason = Some(reason.to_string());
        }
        Ok(())
    }

    fn text(&mut self, kind: TextKind, delta: &str, out: &mut String) {
        if self.open != Some(Block::Text(kind)) {
            self.close(false, out);
            self.open = Some(Block::Text(kind));
            self.events.open_text(kind, out);
        }
        self.text.push_str(delta);
        self.events.text_delta(kind, delta, out);
    }

    fn tool_delta(&mut self, delta: &ToolCallDelta<'_>, out: &mut String) -> Result<(), ()> {
        let slot = self.call_slot(delta)?;
        self.last_call = Some(slot);
        let call = self.calls.entry(slot).or_default();
        set_tool_identity(&mut call.id, delta.id)?;
        set_tool_identity(&mut call.name, delta.name)?;
        match call.phase {
            // The upstream interleaved a call it had already moved on from.
            CallPhase::Closed if !delta.arguments.is_empty() => return Err(()),
            CallPhase::Closed => {}
            CallPhase::Open => {
                call.arguments.push_str(delta.arguments);
                if !delta.arguments.is_empty() {
                    self.events.call_delta(delta.arguments, out);
                }
            }
            CallPhase::Pending => {
                call.arguments.push_str(delta.arguments);
                // A delta for another call means the open one is complete.
                if matches!(self.open, Some(Block::Call(open)) if open != slot) {
                    self.close(false, out);
                }
                self.open_next_call(out);
            }
        }
        Ok(())
    }

    /// The call a delta belongs to: its index, else the call its id names,
    /// else — a continuation with neither — the call the last delta was for.
    fn call_slot(&self, delta: &ToolCallDelta<'_>) -> Result<usize, ()> {
        if let Some(index) = delta.index {
            return Ok(index);
        }
        if delta.id.is_empty() {
            return self.last_call.ok_or(());
        }
        Ok(self
            .calls
            .iter()
            .find(|(_, call)| call.id == delta.id)
            .map(|(slot, _)| *slot)
            .unwrap_or_else(|| self.calls.keys().next_back().map_or(0, |last| last + 1)))
    }

    fn open_next_call(&mut self, out: &mut String) {
        let slot = self.next_call;
        let ready = !matches!(self.open, Some(Block::Call(_)))
            && self
                .calls
                .get(&slot)
                .is_some_and(|call| call.phase == CallPhase::Pending && call.identified());
        if ready {
            self.next_call += 1;
            self.open_call(slot, out);
        }
    }

    fn open_call(&mut self, slot: usize, out: &mut String) {
        self.close(false, out);
        let call = self.calls.get_mut(&slot).expect("tool call exists");
        call.phase = CallPhase::Open;
        self.open = Some(Block::Call(slot));
        self.events.open_call(call, out);
    }

    /// Close the open block. `truncated`: the upstream stopped mid-block, so
    /// an unfinished call is incomplete rather than malformed.
    fn close(&mut self, truncated: bool, out: &mut String) {
        match self.open.take() {
            None => {}
            Some(Block::Text(kind)) => {
                self.events.close_text(kind, &self.text, out);
                self.text.clear();
            }
            Some(Block::Call(slot)) => {
                let call = self.calls.get_mut(&slot).expect("tool call exists");
                call.phase = CallPhase::Closed;
                if self.events.close_call(call, out) {
                    self.executable_calls += 1;
                } else if !truncated {
                    self.invalid_calls = true;
                }
            }
        }
    }

    fn finish(&mut self, out: &mut String) -> Result<(), ()> {
        if self.terminated {
            return Ok(());
        }
        // `[DONE]` with nothing before it: there is no response to close.
        if !self.started {
            return Err(());
        }
        self.terminated = true;
        let reason = self.finish_reason.clone();
        let finish = ChatFinish::parse(reason.as_deref());
        let failed = self.error.is_some() || finish == ChatFinish::Error;
        let truncated = failed || finish.truncated();
        self.close(truncated, out);
        // Calls still waiting on an earlier index or on their identity. One
        // cut off before it was complete is dropped, as Anthropic drops it, and
        // a failed generation offers none.
        let pending: Vec<usize> = self
            .calls
            .iter()
            .filter(|(_, call)| call.phase == CallPhase::Pending)
            .map(|(slot, _)| *slot)
            .collect();
        let mut unidentified = false;
        for slot in pending {
            let call = &self.calls[&slot];
            if !call.identified() {
                unidentified |= call.started() && !truncated;
            } else if !failed && (!truncated || function_call_arguments(&call.arguments).is_some())
            {
                self.open_call(slot, out);
                self.close(truncated, out);
            }
        }
        let outcome = if failed {
            Outcome::Failed(Failure::Upstream {
                error: self.error.as_ref(),
                reason: reason.as_deref(),
            })
        } else if unidentified {
            Outcome::Failed(Failure::ToolCallIdentity)
        } else if self.invalid_calls {
            Outcome::Failed(Failure::ToolCallArguments)
        } else {
            Outcome::Finished {
                finish,
                tool_use: self.executable_calls > 0,
            }
        };
        self.events.finish(outcome, self.usage.as_ref(), out);
        Ok(())
    }
}

// Anthropic Messages events.

#[derive(Default)]
struct MessagesEvents {
    /// The index the next content block takes.
    next_index: usize,
}

impl MessagesEvents {
    fn block_start(&mut self, block: Value, out: &mut String) {
        out.push_str(&sse_event(
            "content_block_start",
            &json!({ "type": "content_block_start", "index": self.next_index, "content_block": block }),
        ));
    }

    fn block_delta(&self, delta: Value, out: &mut String) {
        out.push_str(&sse_event(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": self.next_index, "delta": delta }),
        ));
    }

    fn block_stop(&mut self, out: &mut String) {
        out.push_str(&sse_event(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": self.next_index }),
        ));
        self.next_index += 1;
    }
}

fn messages_usage(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let (cache_read, cache_creation) = chat_cache_tokens(usage);
    anthropic_usage(
        i64_field(usage, "prompt_tokens"),
        i64_field(usage, "completion_tokens"),
        cache_read,
        cache_creation,
    )
}

impl ChatEvents for MessagesEvents {
    fn start(&mut self, chunk: &Value, fallback_id: &str, out: &mut String) {
        let text = |key| {
            chunk
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        };
        let id = text("id").map(str::to_string).unwrap_or_else(|| {
            if fallback_id.is_empty() {
                format!("msg_{}", now_millis())
            } else {
                fallback_id.to_string()
            }
        });
        let mut usage = messages_usage(chunk.get("usage"));
        usage["output_tokens"] = json!(0);
        out.push_str(&sse_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": id, "type": "message", "role": "assistant", "content": [],
                    "model": text("model").unwrap_or("unknown"),
                    "stop_reason": Value::Null, "stop_sequence": Value::Null,
                    "usage": usage,
                },
            }),
        ));
    }

    fn open_text(&mut self, kind: TextKind, out: &mut String) {
        let block = match kind {
            TextKind::Reasoning => thinking_block(""),
            TextKind::Text | TextKind::Refusal => json!({ "type": "text", "text": "" }),
        };
        self.block_start(block, out);
    }

    fn text_delta(&mut self, kind: TextKind, delta: &str, out: &mut String) {
        self.block_delta(
            match kind {
                TextKind::Reasoning => json!({ "type": "thinking_delta", "thinking": delta }),
                TextKind::Text | TextKind::Refusal => {
                    json!({ "type": "text_delta", "text": delta })
                }
            },
            out,
        );
    }

    fn close_text(&mut self, _kind: TextKind, _text: &str, out: &mut String) {
        self.block_stop(out);
    }

    fn open_call(&mut self, call: &ToolCall, out: &mut String) {
        self.block_start(
            json!({ "type": "tool_use", "id": call.id, "name": call.name, "input": {} }),
            out,
        );
        if !call.arguments.is_empty() {
            self.call_delta(&call.arguments, out);
        }
    }

    fn call_delta(&mut self, delta: &str, out: &mut String) {
        self.block_delta(
            json!({ "type": "input_json_delta", "partial_json": delta }),
            out,
        );
    }

    fn close_call(&mut self, call: &ToolCall, out: &mut String) -> bool {
        self.block_stop(out);
        function_call_arguments(&call.arguments).is_some()
    }

    fn finish(&mut self, outcome: Outcome<'_>, usage: Option<&Value>, out: &mut String) {
        let (finish, tool_use) = match outcome {
            Outcome::Finished { finish, tool_use } => (finish, tool_use),
            Outcome::Failed(failure) => {
                // Rebuilt by the sanitize stage; only a relayable kind survives.
                let error = match failure {
                    Failure::Upstream {
                        error: Some(error), ..
                    } => error.clone(),
                    Failure::Upstream { reason, .. } => json!({
                        "type": reason.filter(|reason| reason.ends_with("_error")).unwrap_or("api_error"),
                        "message": "The upstream provider returned an error",
                    }),
                    Failure::ToolCallIdentity | Failure::ToolCallArguments => json!({
                        "type": "api_error",
                        "message": "The upstream provider returned an invalid tool call",
                    }),
                };
                out.push_str(&sse_event(
                    "error",
                    &json!({ "type": "error", "error": error }),
                ));
                return;
            }
        };
        out.push_str(&sse_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": finish.stop_reason(tool_use), "stop_sequence": Value::Null },
                "usage": messages_usage(usage),
            }),
        ));
        out.push_str(&sse_event(
            "message_stop",
            &json!({ "type": "message_stop" }),
        ));
    }
}

// OpenAI Responses events.

struct ResponsesEvents {
    echo: Arc<Value>,
    identity: Arc<ResponseIdentity>,
    upstream_usage: UpstreamUsage,
    tools: ResponsesToolMap,
    sequence: u64,
    model: Value,
    created_at: u64,
    output: Vec<Value>,
}

impl ResponsesEvents {
    fn new(
        echo: Arc<Value>,
        identity: Arc<ResponseIdentity>,
        upstream_usage: UpstreamUsage,
    ) -> Self {
        let tools = ResponsesToolMap::from_echo(&echo);
        Self {
            echo,
            identity,
            upstream_usage,
            tools,
            sequence: 0,
            model: Value::Null,
            created_at: 0,
            output: Vec::new(),
        }
    }

    fn emit(&mut self, kind: &str, mut body: Value, out: &mut String) {
        body["type"] = json!(kind);
        body["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        out.push_str(&sse_event(kind, &body));
    }

    fn response(
        &self,
        status: &str,
        incomplete_details: Value,
        error: Value,
        usage: Option<Value>,
    ) -> Value {
        responses_object(
            &self.echo,
            ResponsesHead {
                id: &self.identity.request_id,
                created_at: self.created_at,
                model: self.model.clone(),
                status,
                incomplete_details,
                error,
            },
            self.output.clone(),
            usage,
        )
    }

    /// The open item: the last one added, which stays open until it is done.
    fn open_item(&self) -> (usize, String) {
        let index = self.output.len() - 1;
        let id = self.output[index]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (index, id)
    }

    fn add_item(&mut self, item: Value, out: &mut String) {
        self.output.push(item.clone());
        let output_index = self.output.len() - 1;
        self.emit(
            "response.output_item.added",
            json!({ "output_index": output_index, "item": item }),
            out,
        );
    }

    fn done_item(&mut self, item: Value, out: &mut String) {
        let (output_index, _) = self.open_item();
        self.output[output_index] = item.clone();
        self.emit(
            "response.output_item.done",
            json!({ "output_index": output_index, "item": item }),
            out,
        );
    }

    fn text_item(&self, kind: TextKind, output_index: usize, text: &str, status: &str) -> Value {
        let request_id = &self.identity.request_id;
        match kind {
            TextKind::Reasoning => {
                reasoning_item(&item_id("rs", request_id, output_index), text, status)
            }
            TextKind::Text | TextKind::Refusal => message_item(
                &item_id("msg", request_id, output_index),
                message_part(kind, text),
                status,
            ),
        }
    }
}

fn message_part(kind: TextKind, text: &str) -> Value {
    match kind {
        TextKind::Refusal => refusal_part(text),
        TextKind::Text | TextKind::Reasoning => output_text_part(text),
    }
}

fn text_event(kind: TextKind, suffix: &str) -> String {
    let stem = match kind {
        TextKind::Reasoning => "reasoning_text",
        TextKind::Text => "output_text",
        TextKind::Refusal => "refusal",
    };
    format!("response.{stem}.{suffix}")
}

impl ChatEvents for ResponsesEvents {
    fn start(&mut self, chunk: &Value, _fallback_id: &str, out: &mut String) {
        self.model = chunk.get("model").cloned().unwrap_or(Value::Null);
        self.created_at = chunk
            .get("created")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_secs);
        for kind in ["response.created", "response.in_progress"] {
            let response = self.response("in_progress", Value::Null, Value::Null, None);
            self.emit(kind, json!({ "response": response }), out);
        }
    }

    fn usage(&mut self, usage: &Value) {
        *self
            .upstream_usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(usage.clone());
    }

    fn open_text(&mut self, kind: TextKind, out: &mut String) {
        let item = self.text_item(kind, self.output.len(), "", "in_progress");
        self.add_item(item, out);
        if kind != TextKind::Reasoning {
            let (output_index, item_id) = self.open_item();
            let part = message_part(kind, "");
            self.emit(
                "response.content_part.added",
                json!({ "item_id": item_id, "output_index": output_index, "content_index": 0, "part": part }),
                out,
            );
        }
    }

    fn text_delta(&mut self, kind: TextKind, delta: &str, out: &mut String) {
        let (output_index, item_id) = self.open_item();
        let mut body = json!({ "item_id": item_id, "output_index": output_index, "content_index": 0, "delta": delta });
        if kind == TextKind::Text {
            body["logprobs"] = json!([]);
        }
        self.emit(&text_event(kind, "delta"), body, out);
    }

    fn close_text(&mut self, kind: TextKind, text: &str, out: &mut String) {
        let (output_index, item_id) = self.open_item();
        let mut body =
            json!({ "item_id": item_id, "output_index": output_index, "content_index": 0 });
        body[if kind == TextKind::Refusal {
            "refusal"
        } else {
            "text"
        }] = json!(text);
        if kind == TextKind::Text {
            body["logprobs"] = json!([]);
        }
        self.emit(&text_event(kind, "done"), body, out);
        if kind != TextKind::Reasoning {
            self.emit(
                "response.content_part.done",
                json!({
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": message_part(kind, text),
                }),
                out,
            );
        }
        let item = self.text_item(kind, output_index, text, "completed");
        self.done_item(item, out);
    }

    fn open_call(&mut self, call: &ToolCall, out: &mut String) {
        let item = responses_call_item(&self.tools, &call.id, &call.name, "", "in_progress");
        self.add_item(item, out);
        if !call.arguments.is_empty() {
            self.call_delta(&call.arguments, out);
        }
    }

    /// Custom tool input is wrapped in JSON upstream and only known once the
    /// call is complete, so only function arguments stream.
    fn call_delta(&mut self, delta: &str, out: &mut String) {
        let (output_index, item_id) = self.open_item();
        if self.output[output_index]["type"] == "function_call" {
            self.emit(
                "response.function_call_arguments.delta",
                json!({ "item_id": item_id, "output_index": output_index, "delta": delta }),
                out,
            );
        }
    }

    fn close_call(&mut self, call: &ToolCall, out: &mut String) -> bool {
        let (output_index, item_id) = self.open_item();
        let Some(value) = responses_call_value(&self.tools, &call.name, &call.arguments) else {
            let item = responses_call_item(&self.tools, &call.id, &call.name, "", "incomplete");
            self.done_item(item, out);
            return false;
        };
        let item = responses_call_item(&self.tools, &call.id, &call.name, &value, "completed");
        let ids = json!({ "item_id": item_id, "output_index": output_index });
        if item["type"] == "custom_tool_call" {
            if !value.is_empty() {
                let mut body = ids.clone();
                body["delta"] = json!(value);
                self.emit("response.custom_tool_call_input.delta", body, out);
            }
            let mut body = ids;
            body["input"] = json!(value);
            self.emit("response.custom_tool_call_input.done", body, out);
        } else {
            let mut body = ids;
            body["name"] = item["name"].clone();
            body["arguments"] = json!(value);
            self.emit("response.function_call_arguments.done", body, out);
        }
        self.done_item(item, out);
        true
    }

    fn finish(&mut self, outcome: Outcome<'_>, usage: Option<&Value>, out: &mut String) {
        let (event, status, incomplete_details, error) = match outcome {
            Outcome::Finished { finish, .. } => {
                let (status, details) = finish.responses_status();
                let event = if status == "incomplete" {
                    "response.incomplete"
                } else {
                    "response.completed"
                };
                (event, status, details, Value::Null)
            }
            Outcome::Failed(failure) => {
                let error = match failure {
                    Failure::Upstream {
                        error: Some(error), ..
                    } => error.clone(),
                    Failure::Upstream { .. } => upstream_failed_error(),
                    Failure::ToolCallIdentity => invalid_tool_call_identity_error(),
                    Failure::ToolCallArguments => invalid_tool_call_arguments_error(),
                };
                ("response.failed", "failed", Value::Null, error)
            }
        };
        let response = self.response(
            status,
            incomplete_details,
            error,
            Some(responses_usage(usage)),
        );
        self.emit(event, json!({ "response": response }), out);
    }
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

fn replace_data_fields(event: &str, replacement: &str, event_name: Option<&str>) -> String {
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
        } else if event_name.is_some()
            && line
                .split_once(':')
                .is_some_and(|(field, _)| field == "event")
        {
            output.push_str("event: ");
            output.push_str(event_name.unwrap_or_default());
            output.push('\n');
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
        return framed_event(event);
    }
    // An edit that renames the payload's `type` (the `response.error` →
    // canonical `error` rebuild) must rename the SSE event field with it — a
    // client subscribing by event name would otherwise never see it.
    let renamed = match (
        original.get("type").and_then(Value::as_str),
        body.get("type").and_then(Value::as_str),
    ) {
        (Some(old), Some(new)) if old != new => Some(new.to_string()),
        _ => None,
    };
    replace_data_fields(event, &json_str(&body), renamed.as_deref())
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
                    // `[DONE]` is an OpenAI convention some upstreams omit, so
                    // a bridged stream is closed here too. Only on a clean end:
                    // after a transport failure a synthesized terminal would make
                    // a truncated generation look complete.
                    if this.pending_error.is_none() {
                        match this.transform.end_of_stream(&mut this.state) {
                            Ok(Some(tail)) => this.queue.push_back(Bytes::from(tail)),
                            Ok(None) => {}
                            Err(()) => this.fail("provider ended before finishing its response"),
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
    use std::collections::BTreeSet;

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

        let mut state = started_messages_state();
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

    fn started_messages_state() -> StreamState {
        let mut state = StreamState::default();
        StreamTransform::OpenaiToAnthropicMessages
            .apply(
                r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#,
                "fb",
                &mut state,
            )
            .unwrap();
        state
    }

    // `data:[DONE]` without the optional space must terminate the
    // OpenAI→Anthropic stream, not be skipped as an unparseable event.
    #[test]
    fn spaceless_done_terminates_openai_to_anthropic() {
        let mut state = started_messages_state();
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
            Ok(Bytes::from(
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            )),
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
        // Messages surface: canonicalize touches only an in-band `error` event
        // on this unschematized surface, so the anthropic events here are left
        // intact and only id/model are rewritten.
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

    /// A `response.error`-framed upstream error through the sanitize stage: the
    /// payload is rebuilt as the canonical `error` event, and the SSE `event:`
    /// field must be renamed with it — a client subscribing by event name would
    /// otherwise never see the error the payload now claims to be.
    #[tokio::test]
    async fn sanitize_renames_the_sse_event_with_the_rebuilt_payload() {
        let upstream = concat!(
            "event: response.error\n",
            "data: {\"type\":\"response.error\",\"sequence_number\":2,\"error\":{\"type\":\"insufficient_quota\",\"code\":\"credit_balance_exhausted\",\"message\":\"no credits, top up at https://console.acme.ai/billing\"}}\n\n",
        );
        let events: Vec<Result<Bytes, ServiceError>> = vec![Ok(Bytes::from(upstream))];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        });
        let sanitized = SseTransformStream::new(
            inner,
            StreamTransform::SanitizeResponse(identity, Endpoint::CreateModelResponse),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = sanitized.collect().await;
        let wire: String = collected
            .into_iter()
            .map(|c| String::from_utf8(c.unwrap().to_vec()).unwrap())
            .collect();
        assert!(wire.contains("event: error\n"), "{wire}");
        assert!(!wire.contains("response.error"), "{wire}");
        assert!(wire.contains("\"type\":\"error\""), "{wire}");
        assert!(wire.contains("\"sequence_number\":2"), "{wire}");
        assert!(!wire.contains("credit_balance_exhausted"), "{wire}");
        assert!(!wire.contains("console.acme.ai"), "{wire}");
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
            if case["fn"] == json!("createModelResponse") {
                continue;
            }
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

    fn assert_responses_event_shape(event: &Value) {
        let kind = event["type"].as_str().expect("event type");
        let expected: &[&str] = match kind {
            "response.created"
            | "response.in_progress"
            | "response.completed"
            | "response.incomplete"
            | "response.failed" => &["type", "sequence_number", "response"],
            "response.output_item.added" | "response.output_item.done" => {
                &["type", "sequence_number", "output_index", "item"]
            }
            "response.content_part.added" | "response.content_part.done" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "content_index",
                "part",
            ],
            "response.reasoning_text.delta" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "content_index",
                "delta",
            ],
            "response.reasoning_text.done" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "content_index",
                "text",
            ],
            "response.output_text.delta" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "content_index",
                "delta",
                "logprobs",
            ],
            "response.output_text.done" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "content_index",
                "text",
                "logprobs",
            ],
            "response.refusal.delta" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "content_index",
                "delta",
            ],
            "response.refusal.done" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "content_index",
                "refusal",
            ],
            "response.function_call_arguments.delta" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "delta",
            ],
            "response.function_call_arguments.done" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "name",
                "arguments",
            ],
            "response.custom_tool_call_input.delta" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "delta",
            ],
            "response.custom_tool_call_input.done" => &[
                "type",
                "sequence_number",
                "item_id",
                "output_index",
                "input",
            ],
            other => panic!("unexpected Responses event type {other}"),
        };
        let actual: BTreeSet<&str> = event
            .as_object()
            .expect("event object")
            .keys()
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<&str> = expected.iter().copied().collect();
        assert_eq!(actual, expected, "unexpected fields on {kind}: {event}");
    }

    async fn replay_responses_fixture(case: &Value) -> (Vec<Value>, Option<ServiceError>, String) {
        let chunks = case["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| {
                Ok::<Bytes, ServiceError>(Bytes::from(format!(
                    "{}\n\n",
                    event.as_str().expect("event string")
                )))
            })
            .collect::<Vec<_>>();
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(chunks));
        let identity = Arc::new(ResponseIdentity {
            request_id: "resp_fixture".to_string(),
            user_model: Some("gpt-test".to_string()),
        });
        let stream = SseTransformStream::new(
            inner,
            StreamTransform::OpenaiChatToResponses(
                Arc::new(json!({})),
                identity,
                UpstreamUsage::default(),
            ),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = stream.collect().await;
        let mut events = Vec::new();
        let mut wire = String::new();
        let mut error = None;
        for chunk in collected {
            match chunk {
                Ok(bytes) => wire.push_str(&String::from_utf8_lossy(&bytes)),
                Err(err) => error = Some(err),
            }
        }
        for block in wire.split("\n\n").filter(|block| !block.trim().is_empty()) {
            let parsed = parse_event(block);
            let data = parsed.data.as_deref().expect("event data");
            assert_ne!(data, "[DONE]", "Responses surface emitted [DONE]");
            let value: Value = serde_json::from_str(data).expect("Responses event JSON");
            assert_eq!(
                parsed.name.as_deref(),
                value["type"].as_str(),
                "SSE event name must match payload type"
            );
            events.push(value);
        }
        (events, error, wire)
    }

    #[tokio::test]
    async fn responses_stream_identity_is_immutable() {
        for (first_name, id, name, accepted) in [
            ("lookup", "call_1", "lookup", true),
            ("lookup", "", "", true),
            ("lookup", "call_12", "lookup", false),
            ("lookup", "call_1", "lookup_more", false),
            ("", "call_1", "lookup", true),
            ("", "call_12", "lookup", false),
        ] {
            let first = json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "call_1",
                "function": { "name": first_name, "arguments": "" }
            }] } }] });
            let second = json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": id,
                "function": { "name": name, "arguments": "{}" }
            }] }, "finish_reason": "tool_calls" }] });
            let (events, error, wire) = replay_responses_fixture(&json!({
                "events": [format!("data: {first}"), format!("data: {second}"), "data: [DONE]"]
            }))
            .await;
            assert_eq!(error.is_none(), accepted, "{wire}");
            for event in &events {
                if let Some(item_id) = event.get("item_id") {
                    assert_eq!(item_id, "fc_call_1");
                }
                if let Some(item) = event.get("item") {
                    assert_eq!(item["id"], "fc_call_1");
                    assert_eq!(item["name"], "lookup");
                }
            }
            assert_eq!(
                events
                    .iter()
                    .any(|event| event["type"] == "response.completed"),
                accepted
            );
        }
    }

    #[tokio::test]
    async fn responses_stream_assembles_index_less_calls() {
        let events = [
            tool_event(
                json!({ "id": "call_1", "function": { "name": "lookup", "arguments": "" } }),
            ),
            chat_event(json!({}), Some("tool_calls")),
            "data: [DONE]".to_string(),
        ];
        let (events, error, wire) = replay_responses_fixture(&json!({ "events": events })).await;
        assert!(error.is_none(), "{wire}");
        let terminal = events.last().unwrap();
        assert_eq!(terminal["type"], "response.completed");
        assert_eq!(terminal["response"]["output"][0]["call_id"], "call_1");
        assert_eq!(terminal["response"]["output"][0]["arguments"], "{}");
    }

    #[tokio::test]
    async fn openai_chat_to_responses_stream_matches_fixtures() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/stream_golden.json"))
                .expect("parse stream fixtures");
        for case in cases
            .iter()
            .filter(|case| case["fn"] == json!("createModelResponse"))
        {
            let name = case["name"].as_str().unwrap();
            let (events, error, wire) = replay_responses_fixture(case).await;
            let types: Vec<&str> = events
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect();
            let expected_types: Vec<&str> = case["types"]
                .as_array()
                .unwrap()
                .iter()
                .map(|kind| kind.as_str().unwrap())
                .collect();
            assert_eq!(types, expected_types, "case {name}: event order\n{wire}");
            for (sequence, event) in events.iter().enumerate() {
                assert_eq!(event["sequence_number"], json!(sequence));
                assert_responses_event_shape(event);
            }
            if case.get("error").and_then(Value::as_bool) == Some(true) {
                assert!(
                    error
                        .as_ref()
                        .is_some_and(|err| err.to_string().contains("before finishing")),
                    "case {name}: expected a truncation failure, got {error:?}"
                );
                continue;
            }
            assert!(error.is_none(), "case {name}: unexpected error {error:?}");
            let terminal = events.last().expect("terminal event");
            let expected = &case["terminal"];
            assert_eq!(terminal["response"]["status"], expected["status"]);
            assert_eq!(terminal["response"]["output"], expected["output"]);
            assert_eq!(terminal["response"]["usage"], expected["usage"]);
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

    fn chat_event(delta: Value, finish_reason: Option<&str>) -> String {
        format!(
            "data: {}",
            json!({ "choices": [{ "delta": delta, "finish_reason": finish_reason }] })
        )
    }

    fn tool_event(call: Value) -> String {
        chat_event(json!({ "tool_calls": [call] }), None)
    }

    #[test]
    fn openai_to_anthropic_blocks_are_sequential() {
        let events = [
            chat_event(json!({ "reasoning_content": "plan" }), None),
            chat_event(json!({ "content": "calling" }), None),
            // Arguments may precede the identity; the block waits for it.
            tool_event(json!({ "index": 0, "function": { "arguments": "{\"q\":" } })),
            tool_event(json!({ "index": 0, "id": "call_1", "function": { "name": "first" } })),
            tool_event(json!({ "index": 0, "function": { "arguments": "\"x\"}" } })),
            tool_event(json!({ "index": 1, "id": "call_2", "function": { "name": "second" } })),
            chat_event(json!({ "content": "after" }), Some("stop")),
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = events.iter().map(String::as_str).collect();
        let out = run(StreamTransform::OpenaiToAnthropicMessages, &refs);

        let mut open = None;
        let mut blocks = Vec::new();
        let mut inputs: BTreeMap<i64, String> = BTreeMap::new();
        for event in &out {
            let index = event["index"].as_i64();
            match event["type"].as_str().unwrap() {
                "content_block_start" => {
                    assert!(open.is_none(), "two blocks open at once: {out:?}");
                    open = index;
                    blocks.push(event["content_block"].clone());
                }
                "content_block_delta" => {
                    assert_eq!(index, open);
                    if let Some(json) = event["delta"]["partial_json"].as_str() {
                        inputs.entry(index.unwrap()).or_default().push_str(json);
                    }
                }
                "content_block_stop" => assert_eq!(open.take(), index),
                _ => {}
            }
        }
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|block| block["type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["thinking", "text", "tool_use", "tool_use", "text"]);
        assert_eq!(blocks[2]["name"], "first");
        assert_eq!(blocks[3]["name"], "second");
        assert_eq!(inputs[&2], r#"{"q":"x"}"#);
        assert!(
            !inputs.contains_key(&3),
            "a no-argument call streams no input"
        );
        let delta = out.iter().find(|e| e["type"] == "message_delta").unwrap();
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
        assert_eq!(out.last().unwrap()["type"], "message_stop");
    }

    #[test]
    fn index_less_tool_calls_are_assembled_by_id() {
        let events = [
            tool_event(
                json!({ "id": "call_1", "function": { "name": "f", "arguments": "{\"a\":" } }),
            ),
            tool_event(json!({ "function": { "arguments": "1}" } })),
            tool_event(json!({ "id": "call_2", "function": { "name": "g", "arguments": "{}" } })),
            chat_event(json!({}), Some("tool_calls")),
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = events.iter().map(String::as_str).collect();
        let out = run(StreamTransform::OpenaiToAnthropicMessages, &refs);
        let ids: Vec<&Value> = out
            .iter()
            .filter(|event| event["type"] == "content_block_start")
            .map(|event| &event["content_block"]["id"])
            .collect();
        assert_eq!(ids, [&json!("call_1"), &json!("call_2")]);
        assert!(out
            .iter()
            .any(|event| event["delta"]["partial_json"] == "1}"));
        assert_eq!(out.last().unwrap()["type"], "message_stop");
    }

    #[test]
    fn openai_to_anthropic_stream_endings() {
        let transform = StreamTransform::OpenaiToAnthropicMessages;
        let partial_call = tool_event(
            json!({ "index": 0, "id": "c", "function": { "name": "f", "arguments": "{\"a\":" } }),
        );
        let stop_reason = |events: &[&str]| {
            run(transform.clone(), events)
                .iter()
                .find(|event| event["type"] == "message_delta")
                .map(|event| event["delta"]["stop_reason"].clone())
        };
        // `[DONE]` is the terminal even without a finish reason.
        let text = chat_event(json!({ "content": "hi" }), None);
        assert_eq!(
            stop_reason(&[&text, "data: [DONE]"]),
            Some(json!("end_turn"))
        );
        // A call cut off by the token limit is the limit's doing, not malformed.
        let length = chat_event(json!({}), Some("length"));
        assert_eq!(
            stop_reason(&[&partial_call, &length, "data: [DONE]"]),
            Some(json!("max_tokens"))
        );
        // Unknown vendor reasons end the turn.
        let eos = chat_event(json!({ "content": "hi" }), Some("eos"));
        assert_eq!(
            stop_reason(&[&eos, "data: [DONE]"]),
            Some(json!("end_turn"))
        );

        // Calls the client cannot execute end the stream with an in-band error.
        let tool_calls = chat_event(json!({}), Some("tool_calls"));
        let unidentified = tool_event(json!({ "index": 0, "function": { "arguments": "{}" } }));
        for events in [
            [partial_call.as_str(), tool_calls.as_str(), "data: [DONE]"],
            [unidentified.as_str(), tool_calls.as_str(), "data: [DONE]"],
        ] {
            let out = run(transform.clone(), &events);
            assert_eq!(out.last().unwrap()["type"], "error", "{out:?}");
            assert!(!out.iter().any(|event| event["type"] == "message_stop"));
        }

        // An upstream error member fails the stream in-band.
        let error = format!("data: {}", json!({ "error": { "message": "boom" } }));
        let out = run(transform.clone(), &[&text, &error, "data: [DONE]"]);
        assert_eq!(out.last().unwrap()["type"], "error");
    }

    #[test]
    fn malformed_tool_streams_fail() {
        let first = tool_event(
            json!({ "index": 0, "id": "call_1", "function": { "name": "f", "arguments": "{}" } }),
        );
        let second = tool_event(
            json!({ "index": 1, "id": "call_2", "function": { "name": "g", "arguments": "{}" } }),
        );
        for events in [
            // Identity may not change.
            vec![
                first.clone(),
                tool_event(json!({ "index": 0, "id": "call_9" })),
            ],
            // A call the upstream moved on from cannot grow.
            vec![
                first.clone(),
                second,
                tool_event(json!({ "index": 0, "function": { "arguments": " " } })),
            ],
            // A continuation with no call to continue.
            vec![tool_event(json!({ "function": { "arguments": "{}" } }))],
            vec![chat_event(json!({ "content": 7 }), None)],
            vec![chat_event(json!({ "tool_calls": {} }), None)],
            vec![tool_event(json!({ "index": -1, "id": "c" }))],
        ] {
            for transform in [
                StreamTransform::OpenaiToAnthropicMessages,
                StreamTransform::OpenaiChatToResponses(
                    Arc::new(json!({})),
                    Arc::new(ResponseIdentity {
                        request_id: "resp".into(),
                        user_model: None,
                    }),
                    UpstreamUsage::default(),
                ),
            ] {
                let mut state = StreamState::default();
                let failed = events
                    .iter()
                    .any(|event| transform.apply(event, "fallback", &mut state).is_err());
                assert!(failed, "malformed tool stream was accepted: {events:?}");
            }
        }
    }

    /// An Anthropic upstream's error on the chat surface must reach the client
    /// as this surface's own in-band error — the `error` member a client
    /// detects a failed 200 stream by — not as the upstream's error type in
    /// `finish_reason`, which left nothing to detect and named a vocabulary
    /// this surface does not use. Run through the production pair
    /// (convert, then sanitize), since the rebuild happens in the second stage.
    #[tokio::test]
    async fn anthropic_stream_error_reaches_the_chat_client_as_an_error_member() {
        let upstream = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"billing_error\",\"message\":\"credit balance too low\"}}\n\n";
        let events: Vec<Result<Bytes, ServiceError>> = vec![Ok(Bytes::from(upstream))];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let converted = Box::pin(SseTransformStream::new(
            inner,
            StreamTransform::AnthropicToOpenaiChat,
        ));
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        });
        let sanitized = SseTransformStream::new(
            converted,
            StreamTransform::SanitizeResponse(identity, Endpoint::ChatComplete),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = sanitized.collect().await;
        let wire: String = collected
            .into_iter()
            .map(|c| String::from_utf8(c.unwrap().to_vec()).unwrap())
            .collect();
        assert!(wire.contains("\"error\""), "no error member: {wire}");
        assert!(!wire.contains("billing_error"), "leaked kind: {wire}");
        assert!(!wire.contains("credit balance"), "leaked message: {wire}");
        assert!(wire.contains("data: [DONE]"), "{wire}");
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
