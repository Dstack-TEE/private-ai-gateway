//! Stateful SSE response transforms: convert an upstream provider's streaming
//! events into the downstream client surface, event by event, threading mutable
//! state across events (a per-stream transform state).
//!
//! Provider conversions are selected by upstream format and client surface.
//! Same-format streaming reaches this module too: every stream is sanitized
//! here (identity rewrite + canonicalize, per chunk), and reasoning is excluded
//! when the client opts out. Cost injection, TTFT, and outcome are a separate
//! metering pass downstream (`sse`).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

#[cfg(test)]
use axum::body::Bytes;
use futures_util::Stream;
use serde_json::{json, Map, Value};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::ServiceError;
#[cfg(test)]
use crate::aggregator::service::ServiceResponseStream;

use super::request_transform::{Endpoint, ResponsesToolMap};
use super::response_transform::{
    self, chat_created, chat_text, function_call_arguments, i64_field, item_id,
    malformed_response_error, message_item, messages_usage, normalize_reasoning_usage_value,
    now_millis, now_secs, output_text_part, reasoning_item, reasoning_text, refusal_part,
    responses_annotations, responses_call_item, responses_call_value, responses_object,
    responses_usage, synthesized_call_id, thinking_block, transform_finish_reason,
    unusable_tool_calls_error, ChatFinish, ChatToolCall, ResponseIdentity, ResponsesHead,
};
use super::types::ProviderFormat;
use crate::sse::{Event, EventStream, Payload, Record, MAX_EVENT_BYTES};
use crate::sse_protocol::{event_kind, EventKind, EventMeaning, FailureEvidence, SseProtocol};

const STRICT_OPENAI_COMPLIANCE: bool = true;

/// Where a Chat bridge leaves the usage the meter reports: the shape the
/// buffered path reports for the same surface, known even when the converted
/// stream ends in an error that carries none.
///
/// For Responses it is the upstream's own chat usage: the Responses shape has
/// no field for cache-creation tokens, and its `input_tokens` is a total where
/// the control plane reads the same name as cache-excluded.
pub type UpstreamUsage = Arc<Mutex<Option<Value>>>;

/// Which streaming transform applies.
///
/// `Clone` rather than `Copy`: `SanitizeResponse` carries per-request values
/// (our request id, the client's model name), which no unit variant can.
#[derive(Debug, Clone)]
pub enum StreamTransform {
    AnthropicToOpenaiChat,
    OpenaiToAnthropicMessages(UpstreamUsage),
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
            StreamTransform::OpenaiToAnthropicMessages(_) => "openai",
            StreamTransform::OpenaiChatToResponses(..) => "openai",
            StreamTransform::ExcludeReasoning => "openai",
            StreamTransform::SanitizeResponse(_, _) => "openai",
            _ => "anthropic",
        }
    }

    fn apply_record(
        &self,
        mut record: Record,
        fallback_id: &str,
        state: &mut StreamState,
    ) -> Result<Vec<Record>, ()> {
        match self {
            StreamTransform::ExcludeReasoning => {
                record.edit_json(response_transform::exclude_reasoning);
                return Ok(vec![record]);
            }
            StreamTransform::SanitizeResponse(identity, endpoint) => {
                let meaning = record
                    .event
                    .as_ref()
                    .map(|event| event.meaning)
                    .unwrap_or_default();
                record.edit_json(|body| {
                    response_transform::rewrite_stream_identity(body, identity, meaning.kind);
                    response_transform::canonicalize_stream(
                        body,
                        *endpoint,
                        meaning,
                        Some(identity.request_id.as_str()),
                    );
                });
                return Ok(vec![record]);
            }
            _ => {}
        }
        if record.residual {
            return Ok(vec![record]);
        }
        let Some(event) = record.event.as_ref() else {
            return Ok(Vec::new());
        };
        if event.meaning.failure == Some(FailureEvidence::ConflictingType) {
            return Err(());
        }
        let output = match self {
            StreamTransform::AnthropicToOpenaiChat => {
                anthropic_chat_stream(event, fallback_id, state, STRICT_OPENAI_COMPLIANCE)
            }
            StreamTransform::OpenaiToAnthropicMessages(upstream_usage) => state
                .messages
                .get_or_insert_with(|| ChatStream::new(MessagesEvents::new(upstream_usage.clone())))
                .event(event, fallback_id),
            StreamTransform::OpenaiChatToResponses(echo, identity, upstream_usage) => state
                .responses
                .get_or_insert_with(|| {
                    ChatStream::new(ResponsesEvents::new(
                        echo.clone(),
                        identity.clone(),
                        upstream_usage.clone(),
                    ))
                })
                .event(event, fallback_id),
            StreamTransform::AnthropicCompleteToOpenai => anthropic_complete_stream(event),
            StreamTransform::ExcludeReasoning | StreamTransform::SanitizeResponse(_, _) => {
                unreachable!("handled above")
            }
        }?;
        let mut output = output.unwrap_or_default();
        for record in &mut output {
            if let Some(output) = record.event.as_mut() {
                output.after_terminal = event.after_terminal;
                if event.meaning.failure.is_some() {
                    output.meaning.failure = event.meaning.failure;
                }
            }
        }
        Ok(output)
    }

    /// Close what the stream left open when the upstream ends it. `Err(())`
    /// means it ended before the response was complete.
    fn end_of_stream(&self, state: &mut StreamState) -> Result<Option<Vec<Record>>, ()> {
        match self {
            StreamTransform::OpenaiToAnthropicMessages(_) => state
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
}

/// Select the streaming transform for a committed route format + endpoint, or
/// `None` for native passthrough. A Chat bridge reports its usage through
/// `upstream_usage`.
pub fn select_stream_transform(
    format: ProviderFormat,
    endpoint: Endpoint,
    upstream_usage: &UpstreamUsage,
) -> Option<StreamTransform> {
    use Endpoint::*;
    use ProviderFormat::*;
    match (format, endpoint) {
        (Anthropic, ChatComplete) => Some(StreamTransform::AnthropicToOpenaiChat),
        (Anthropic, Complete) => Some(StreamTransform::AnthropicCompleteToOpenai),
        (Openai, Messages) => Some(StreamTransform::OpenaiToAnthropicMessages(
            upstream_usage.clone(),
        )),
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

// ── Anthropic Messages SSE → OpenAI chat.completion.chunk ────────────────────

fn anthropic_chat_stream(
    event: &Event,
    fallback_id: &str,
    state: &mut StreamState,
    strict: bool,
) -> Result<Option<Vec<Record>>, ()> {
    if event.meaning.failure.is_some() || event.meaning.kind == EventKind::Error {
        let error = event
            .payload
            .json()
            .and_then(|value| value.get("error"))
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(malformed_response_error);
        let usage = event.payload.json().and_then(|value| {
            if matches!(
                event.meaning.kind,
                EventKind::MessageStart | EventKind::MessageDelta
            ) {
                anthropic_chat_chunk(value, event.meaning.kind, fallback_id, state, strict)
                    .and_then(|records| {
                        records.into_iter().find_map(|record| {
                            record.json().and_then(|value| value.get("usage")).cloned()
                        })
                    })
            } else {
                None
            }
        });
        let mut body = json!({
            "id": fallback_id,
            "object": "chat.completion.chunk",
            "created": now_secs(),
            "model": "",
            "error": error,
            "choices": [{"finish_reason": Value::Null, "delta": {"content": ""}}],
        });
        if let Some(usage) = usage {
            body["usage"] = usage;
        }
        return Ok(Some(vec![chat_event(body), done_event()]));
    }
    if event.meaning.terminal.is_some() {
        return Ok(Some(vec![done_event()]));
    }
    match event.meaning.kind {
        EventKind::Ping | EventKind::ContentBlockStop => return Ok(None),
        _ => {}
    }
    if matches!(&event.payload, Payload::Text(text) if text.is_empty()) {
        return Ok(None);
    }
    let parsed = event.payload.json().ok_or(())?;
    Ok(anthropic_chat_chunk(
        parsed,
        event.meaning.kind,
        fallback_id,
        state,
        strict,
    ))
}

fn anthropic_chat_chunk(
    parsed: &Value,
    kind: EventKind,
    fallback_id: &str,
    state: &mut StreamState,
    strict: bool,
) -> Option<Vec<Record>> {
    let model = state.model.clone().unwrap_or_default();

    let message_usage = parsed.get("message").and_then(|m| m.get("usage"));
    if kind == EventKind::MessageStart {
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
            return Some(vec![chat_event(body)]);
        }
    }

    if kind == EventKind::MessageDelta {
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
            return Some(vec![chat_event(body)]);
        }
    }

    // Tool-call and text deltas.
    let mut tool_calls: Vec<Value> = Vec::new();
    let is_tool_block_start = kind == EventKind::ContentBlockStart
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
    let is_tool_block_delta = kind == EventKind::ContentBlockDelta && partial_json.is_some();

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
    Some(vec![chat_event(body)])
}

// ── OpenAI chat.completion SSE → Messages / Responses SSE ────────────────────
//
// Both bridges read a Chat stream the same way and differ only in the events
// they write. `ChatStream` owns the reading — lenient chunk parsing, tool-call
// assembly, the terminal decision — and drives a `ChatEvents` writer for the
// client's surface.

fn done_event() -> Record {
    Record::generated(
        None,
        Payload::Text("[DONE]".into()),
        EventMeaning::generated(SseProtocol::OpenaiChat, EventKind::Done),
    )
}

fn chat_event(body: Value) -> Record {
    Record::generated(None, Payload::Json(body), EventMeaning::default())
}

fn sse_event(event: &str, data: &Value) -> Record {
    named_event(SseProtocol::AnthropicMessages, event, data.clone())
}

fn named_event(protocol: SseProtocol, name: &str, body: Value) -> Record {
    Record::generated(
        Some(name),
        Payload::Json(body),
        EventMeaning::generated(protocol, event_kind(name)),
    )
}

fn stream_str<'a>(object: Option<&'a Map<String, Value>>, key: &str) -> &'a str {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// One Chat chunk, read leniently: a field of an unexpected shape is read as
/// absent, so one odd field never costs the whole response.
struct ChatChunk<'a> {
    has_choice: bool,
    usage: Option<&'a Value>,
    reasoning: &'a str,
    content: Cow<'a, str>,
    refusal: Cow<'a, str>,
    annotations: Vec<Value>,
    tool_calls: Vec<ToolCallDelta<'a>>,
    finish_reason: Option<&'a str>,
}

struct ToolCallDelta<'a> {
    index: Option<usize>,
    id: &'a str,
    name: &'a str,
    arguments: &'a str,
}

fn parse_chat_chunk(chunk: &Value) -> ChatChunk<'_> {
    let choice = chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object);
    let delta = choice
        .and_then(|choice| choice.get("delta"))
        .and_then(Value::as_object);
    let tool_calls = delta
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(|call| {
            let function = call.get("function").and_then(Value::as_object);
            ToolCallDelta {
                index: call
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok()),
                id: stream_str(Some(call), "id"),
                name: stream_str(function, "name"),
                arguments: stream_str(function, "arguments"),
            }
        })
        .collect();
    ChatChunk {
        has_choice: choice.is_some(),
        usage: chunk.get("usage").filter(|usage| usage.is_object()),
        reasoning: choice
            .and_then(|choice| choice.get("delta"))
            .and_then(reasoning_text)
            .unwrap_or(""),
        content: chat_text(delta.and_then(|delta| delta.get("content"))),
        refusal: chat_text(delta.and_then(|delta| delta.get("refusal"))),
        annotations: responses_annotations(delta.and_then(|delta| delta.get("annotations"))),
        tool_calls,
        finish_reason: choice
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(Value::as_str),
    }
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
}

/// Why a stream that ended in-band has no answer to give.
enum Failure<'a> {
    /// The upstream reported an error.
    Upstream(&'a Value),
    /// The stream never carried a choice.
    Empty,
    /// Every tool call lacked a name, so none can be run.
    UnusableToolCalls,
}

enum Outcome<'a> {
    Finished { finish: ChatFinish, tool_use: bool },
    Failed(Failure<'a>),
}

/// The events one client surface writes for a Chat stream.
trait ChatEvents {
    /// The first chunk arrived: the message or response begins.
    fn start(&mut self, chunk: &Value, fallback_id: &str, out: &mut Vec<Record>);
    /// The upstream reported usage (reasoning tokens already normalized).
    fn usage(&mut self, _usage: &Value) {}
    fn open_text(&mut self, kind: TextKind, out: &mut Vec<Record>);
    fn text_delta(&mut self, kind: TextKind, delta: &str, out: &mut Vec<Record>);
    /// A citation in the open text, in Responses form.
    fn annotation(&mut self, _annotation: Value, _out: &mut Vec<Record>) {}
    /// `finished`: false for text the upstream was cut off in.
    fn close_text(&mut self, kind: TextKind, text: &str, finished: bool, out: &mut Vec<Record>);
    /// Write one complete tool call; false when the surface leaves it out.
    /// `truncated`: the upstream was cut off, so the call may be unfinished.
    fn tool_call(
        &mut self,
        call: &ChatToolCall<'_>,
        truncated: bool,
        out: &mut Vec<Record>,
    ) -> bool;
    fn finish(&mut self, outcome: Outcome<'_>, usage: Option<&Value>, out: &mut Vec<Record>);
}

/// Reads a Chat stream for a client surface. Text and reasoning stream as
/// they arrive, one block at a time. Tool calls are assembled by index — or,
/// for an upstream that omits it, by id, and a bare continuation joins the
/// last call — and written complete in index order when the stream finishes,
/// as CLIProxyAPI writes them: deltas of parallel calls may interleave, and a
/// call is only known complete, and valid, at the end.
struct ChatStream<E> {
    events: E,
    /// The upstream's id for the response, for tool call ids it omitted.
    response_id: String,
    answered: bool,
    terminated: bool,
    open: Option<TextKind>,
    /// The open text block's text so far.
    text: String,
    /// Whether a tool call followed the open text, which therefore ended.
    called_after_text: bool,
    /// Keyed by the upstream's index, then by the order of the calls sent
    /// under it.
    calls: BTreeMap<(usize, usize), ToolCall>,
    /// The call each upstream index currently continues.
    bound: HashMap<usize, (usize, usize)>,
    /// The call an index-less, id-less delta continues.
    last_call: Option<(usize, usize)>,
    finish_reason: Option<String>,
    usage: Option<Value>,
    error: Option<Value>,
}

impl<E: ChatEvents> ChatStream<E> {
    fn new(events: E) -> Self {
        Self {
            events,
            response_id: String::new(),
            answered: false,
            terminated: false,
            open: None,
            text: String::new(),
            called_after_text: false,
            calls: BTreeMap::new(),
            bound: HashMap::new(),
            last_call: None,
            finish_reason: None,
            usage: None,
            error: None,
        }
    }

    fn started(&self) -> bool {
        !self.response_id.is_empty()
    }

    /// Apply one SSE event. `Err(())` only for a payload that is not JSON:
    /// the stream is corrupt, and skipping the event could silently drop part
    /// of the answer.
    fn event(&mut self, event: &Event, fallback_id: &str) -> Result<Option<Vec<Record>>, ()> {
        let empty = matches!(&event.payload, Payload::Text(text) if text.is_empty());
        let mut out = Vec::new();
        if self.terminated {
            return Ok(None);
        }
        if event.meaning.failure.is_some() && self.error.is_none() {
            self.start(
                event.payload.json().unwrap_or(&Value::Null),
                fallback_id,
                &mut out,
            );
            self.error = Some(
                event
                    .payload
                    .json()
                    .and_then(|chunk| chunk.get("error"))
                    .filter(|error| !error.is_null())
                    .cloned()
                    .unwrap_or_else(malformed_response_error),
            );
        }
        if event.meaning.terminal.is_some() {
            self.finish(&mut out)?;
        } else if !empty {
            let chunk = event.payload.json().ok_or(())?;
            self.chunk(chunk, fallback_id, &mut out);
        }
        Ok((!out.is_empty()).then_some(out))
    }

    /// The upstream closed the connection. `[DONE]` is an OpenAI convention
    /// some upstreams omit, so a stream that already declared its finish (or
    /// its error) still terminates; one that did not, or said nothing, was
    /// cut off.
    fn end_of_stream(&mut self) -> Result<Option<Vec<Record>>, ()> {
        if self.terminated {
            return Ok(None);
        }
        if self.finish_reason.is_none() && self.error.is_none() {
            return Err(());
        }
        let mut out = Vec::new();
        self.finish(&mut out)?;
        Ok((!out.is_empty()).then_some(out))
    }

    fn start(&mut self, value: &Value, fallback_id: &str, out: &mut Vec<Record>) {
        if !self.started() {
            self.response_id = value
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .unwrap_or(fallback_id)
                .to_string();
            self.events.start(value, fallback_id, out);
        }
    }

    fn chunk(&mut self, value: &Value, fallback_id: &str, out: &mut Vec<Record>) {
        let chunk = parse_chat_chunk(value);
        self.start(value, fallback_id, out);
        if let Some(usage) = chunk.usage {
            let mut usage = usage.clone();
            let _ = normalize_reasoning_usage_value(&mut usage);
            self.events.usage(&usage);
            self.usage = Some(usage);
        }
        self.answered |= chunk.has_choice;
        let mut annotations = chunk.annotations.into_iter();
        for (kind, text) in [
            (TextKind::Reasoning, chunk.reasoning),
            (TextKind::Text, &*chunk.content),
            (TextKind::Refusal, &*chunk.refusal),
        ] {
            if !text.is_empty() {
                self.text(kind, text, out);
            }
            // Citations belong to the text, before a refusal closes it.
            if self.open == Some(TextKind::Text) {
                for annotation in annotations.by_ref() {
                    self.events.annotation(annotation, out);
                }
            }
        }
        for delta in &chunk.tool_calls {
            self.tool_delta(delta);
        }
        if let Some(reason) = chunk.finish_reason {
            self.finish_reason = Some(reason.to_string());
        }
    }

    fn text(&mut self, kind: TextKind, delta: &str, out: &mut Vec<Record>) {
        if self.open != Some(kind) {
            self.close_text(true, out);
            self.open = Some(kind);
            self.events.open_text(kind, out);
        }
        self.called_after_text = false;
        self.text.push_str(delta);
        self.events.text_delta(kind, delta, out);
    }

    fn close_text(&mut self, finished: bool, out: &mut Vec<Record>) {
        if let Some(kind) = self.open.take() {
            self.events.close_text(kind, &self.text, finished, out);
            self.text.clear();
        }
    }

    /// Identity fields may arrive late or repeat; the first non-empty value
    /// stands, as there is no telling fragments from restatements. Another id
    /// at an index already in use starts another call, as relaykit reads it.
    fn tool_delta(&mut self, delta: &ToolCallDelta<'_>) {
        let known = self
            .calls
            .iter()
            .find(|(_, call)| !delta.id.is_empty() && call.id == delta.id)
            .map(|(slot, _)| *slot);
        let slot = match (known, delta.index) {
            (Some(slot), _) => slot,
            (None, Some(index)) => {
                let current = self.bound.get(&index).copied().unwrap_or((index, 0));
                match self.calls.get(&current) {
                    Some(call) if !delta.id.is_empty() && !call.id.is_empty() => {
                        let last = self
                            .calls
                            .range((index, 0)..=(index, usize::MAX))
                            .next_back();
                        (index, last.map_or(0, |(&(_, n), _)| n + 1))
                    }
                    _ => current,
                }
            }
            // A continuation with no call to continue has nowhere to go.
            (None, None) if delta.id.is_empty() => match self.last_call {
                Some(slot) => slot,
                None => return,
            },
            (None, None) => {
                let next = self
                    .calls
                    .keys()
                    .next_back()
                    .map_or(0, |(last, _)| last + 1);
                (next, 0)
            }
        };
        if let Some(index) = delta.index {
            self.bound.insert(index, slot);
        }
        self.last_call = Some(slot);
        self.called_after_text = true;
        let call = self.calls.entry(slot).or_default();
        for (field, value) in [(&mut call.id, delta.id), (&mut call.name, delta.name)] {
            if field.is_empty() {
                field.push_str(value);
            }
        }
        call.arguments.push_str(delta.arguments);
    }

    fn finish(&mut self, out: &mut Vec<Record>) -> Result<(), ()> {
        if self.terminated {
            return Ok(());
        }
        // `[DONE]` with nothing before it: there is no response to close.
        if !self.started() {
            return Err(());
        }
        self.terminated = true;
        let finish = ChatFinish::parse(self.finish_reason.as_deref());
        // Text the turn was cut off in is unfinished, unless calls followed it.
        self.close_text(!finish.truncated() || self.called_after_text, out);
        let outcome = if let Some(error) = &self.error {
            Outcome::Failed(Failure::Upstream(error))
        } else if !self.answered {
            Outcome::Failed(Failure::Empty)
        } else {
            let mut named = 0;
            let mut tool_use = false;
            for (position, call) in self.calls.values().enumerate() {
                let name = call.name.trim();
                if name.is_empty() {
                    continue;
                }
                named += 1;
                let id = match call.id.trim() {
                    "" => synthesized_call_id(&self.response_id, position),
                    id => id.to_string(),
                };
                let call = ChatToolCall {
                    id,
                    name,
                    arguments: &call.arguments,
                };
                tool_use |= self.events.tool_call(&call, finish.truncated(), out);
            }
            if named == 0 && !self.calls.is_empty() && !finish.truncated() {
                Outcome::Failed(Failure::UnusableToolCalls)
            } else {
                Outcome::Finished { finish, tool_use }
            }
        };
        self.events.finish(outcome, self.usage.as_ref(), out);
        Ok(())
    }
}

// Anthropic Messages events.

struct MessagesEvents {
    /// The index the next content block takes.
    next_index: usize,
    upstream_usage: UpstreamUsage,
}

impl MessagesEvents {
    fn new(upstream_usage: UpstreamUsage) -> Self {
        Self {
            next_index: 0,
            upstream_usage,
        }
    }

    fn block_start(&mut self, block: Value, out: &mut Vec<Record>) {
        out.push(sse_event(
            "content_block_start",
            &json!({ "type": "content_block_start", "index": self.next_index, "content_block": block }),
        ));
    }

    fn block_delta(&self, delta: Value, out: &mut Vec<Record>) {
        out.push(sse_event(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": self.next_index, "delta": delta }),
        ));
    }

    fn block_stop(&mut self, out: &mut Vec<Record>) {
        out.push(sse_event(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": self.next_index }),
        ));
        self.next_index += 1;
    }
}

fn report_usage(slot: &UpstreamUsage, usage: Value) {
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(usage);
}

impl ChatEvents for MessagesEvents {
    fn start(&mut self, chunk: &Value, fallback_id: &str, out: &mut Vec<Record>) {
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
        // Usage on an early Chat chunk can be a running total that lacks the
        // final cache split, which would overstate `input_tokens`. Like the
        // Responses bridge, state usage only once it is final, in
        // `message_delta`.
        let usage = messages_usage(None);
        out.push(sse_event(
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

    fn usage(&mut self, usage: &Value) {
        report_usage(&self.upstream_usage, messages_usage(Some(usage)));
    }

    fn open_text(&mut self, kind: TextKind, out: &mut Vec<Record>) {
        let block = match kind {
            TextKind::Reasoning => thinking_block(""),
            TextKind::Text | TextKind::Refusal => json!({ "type": "text", "text": "" }),
        };
        self.block_start(block, out);
    }

    fn text_delta(&mut self, kind: TextKind, delta: &str, out: &mut Vec<Record>) {
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

    fn close_text(&mut self, _kind: TextKind, _text: &str, _finished: bool, out: &mut Vec<Record>) {
        self.block_stop(out);
    }

    /// Malformed arguments become an empty input, as in the buffered
    /// message; a call cut off by a limit is dropped, as Anthropic drops it.
    fn tool_call(
        &mut self,
        call: &ChatToolCall<'_>,
        truncated: bool,
        out: &mut Vec<Record>,
    ) -> bool {
        let input = match function_call_arguments(call.arguments) {
            Some(input) => input,
            None if truncated => return false,
            None => "{}",
        };
        self.block_start(
            json!({ "type": "tool_use", "id": call.id, "name": call.name, "input": {} }),
            out,
        );
        self.block_delta(
            json!({ "type": "input_json_delta", "partial_json": input }),
            out,
        );
        self.block_stop(out);
        true
    }

    fn finish(&mut self, outcome: Outcome<'_>, usage: Option<&Value>, out: &mut Vec<Record>) {
        let (finish, tool_use) = match outcome {
            Outcome::Finished { finish, tool_use } => (finish, tool_use),
            Outcome::Failed(failure) => {
                // Rebuilt by the sanitize stage; only a relayable kind survives.
                let error = match failure {
                    Failure::Upstream(error) => error.clone(),
                    Failure::Empty => json!({
                        "type": "api_error",
                        "message": "The upstream provider returned no response",
                    }),
                    Failure::UnusableToolCalls => json!({
                        "type": "api_error",
                        "message": "The upstream provider returned tool calls without a name",
                    }),
                };
                out.push(sse_event(
                    "error",
                    &json!({ "type": "error", "error": error }),
                ));
                return;
            }
        };
        out.push(sse_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": finish.stop_reason(tool_use), "stop_sequence": Value::Null },
                "usage": messages_usage(usage),
            }),
        ));
        out.push(sse_event(
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
    /// The item being streamed; items are added and done one at a time.
    active: Option<ActiveItem>,
    /// The open text's citations.
    annotations: Vec<Value>,
}

struct ActiveItem {
    output_index: usize,
    id: String,
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
            active: None,
            annotations: Vec::new(),
        }
    }

    fn emit(&mut self, kind: &str, mut body: Value, out: &mut Vec<Record>) {
        body["type"] = json!(kind);
        body["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        out.push(named_event(SseProtocol::OpenaiResponses, kind, body));
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

    fn active(&self) -> (usize, String) {
        let item = self.active.as_ref().expect("an item is open");
        (item.output_index, item.id.clone())
    }

    /// Add an item. Its content starts empty and grows through part events.
    fn add_item(&mut self, mut item: Value, out: &mut Vec<Record>) {
        if item.get("content").is_some() {
            item["content"] = json!([]);
        }
        let output_index = self.output.len();
        self.active = Some(ActiveItem {
            output_index,
            id: item["id"].as_str().unwrap_or_default().to_string(),
        });
        self.output.push(item.clone());
        self.emit(
            "response.output_item.added",
            json!({ "output_index": output_index, "item": item }),
            out,
        );
    }

    fn done_item(&mut self, item: Value, out: &mut Vec<Record>) {
        let output_index = self.active.take().expect("an item is open").output_index;
        self.output[output_index] = item.clone();
        self.emit(
            "response.output_item.done",
            json!({ "output_index": output_index, "item": item }),
            out,
        );
    }

    fn text_item(&self, kind: TextKind, output_index: usize, part: Value, status: &str) -> Value {
        let request_id = &self.identity.request_id;
        match kind {
            TextKind::Reasoning => reasoning_item(
                &item_id("rs", request_id, output_index),
                part["text"].as_str().unwrap_or_default(),
                status,
            ),
            TextKind::Text | TextKind::Refusal => message_item(
                &item_id("msg", request_id, output_index),
                vec![part],
                status,
            ),
        }
    }

    /// The content part a text item streams into, alike for every kind.
    fn text_part(&self, kind: TextKind, text: &str) -> Value {
        match kind {
            TextKind::Reasoning => json!({ "type": "reasoning_text", "text": text }),
            TextKind::Text => output_text_part(text, self.annotations.clone()),
            TextKind::Refusal => refusal_part(text),
        }
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
    fn start(&mut self, chunk: &Value, _fallback_id: &str, out: &mut Vec<Record>) {
        self.model = chunk.get("model").cloned().unwrap_or(Value::Null);
        self.created_at = chat_created(chunk);
        for kind in ["response.created", "response.in_progress"] {
            let response = self.response("in_progress", Value::Null, Value::Null, None);
            self.emit(kind, json!({ "response": response }), out);
        }
    }

    fn usage(&mut self, usage: &Value) {
        report_usage(&self.upstream_usage, usage.clone());
    }

    fn open_text(&mut self, kind: TextKind, out: &mut Vec<Record>) {
        self.annotations.clear();
        let part = self.text_part(kind, "");
        let item = self.text_item(kind, self.output.len(), part, "in_progress");
        self.add_item(item, out);
        let (output_index, item_id) = self.active();
        self.emit(
            "response.content_part.added",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": self.text_part(kind, ""),
            }),
            out,
        );
    }

    fn text_delta(&mut self, kind: TextKind, delta: &str, out: &mut Vec<Record>) {
        let (output_index, item_id) = self.active();
        let mut body = json!({ "item_id": item_id, "output_index": output_index, "content_index": 0, "delta": delta });
        if kind == TextKind::Text {
            body["logprobs"] = json!([]);
        }
        self.emit(&text_event(kind, "delta"), body, out);
    }

    fn annotation(&mut self, annotation: Value, out: &mut Vec<Record>) {
        let (output_index, item_id) = self.active();
        self.emit(
            "response.output_text.annotation.added",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "annotation_index": self.annotations.len(),
                "annotation": annotation,
            }),
            out,
        );
        self.annotations.push(annotation);
    }

    fn close_text(&mut self, kind: TextKind, text: &str, finished: bool, out: &mut Vec<Record>) {
        let (output_index, item_id) = self.active();
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
        self.emit(
            "response.content_part.done",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": self.text_part(kind, text),
            }),
            out,
        );
        let status = if finished { "completed" } else { "incomplete" };
        let item = self.text_item(kind, output_index, self.text_part(kind, text), status);
        self.done_item(item, out);
    }

    /// A call reaches the client complete: added, its input in one delta and
    /// its done event (a tool search carries its arguments in the item), then
    /// done. One cut off by a limit is reported incomplete.
    fn tool_call(
        &mut self,
        call: &ChatToolCall<'_>,
        truncated: bool,
        out: &mut Vec<Record>,
    ) -> bool {
        let item = responses_call_item(&self.tools, &call.id, call.name, "", "in_progress");
        let kind = item["type"].clone();
        self.add_item(item, out);
        let Some(value) = responses_call_value(&self.tools, call.name, call.arguments, truncated)
        else {
            let item = responses_call_item(&self.tools, &call.id, call.name, "", "incomplete");
            self.done_item(item, out);
            return true;
        };
        let (output_index, item_id) = self.active();
        let ids = json!({ "item_id": item_id, "output_index": output_index });
        let item = responses_call_item(&self.tools, &call.id, call.name, &value, "completed");
        let (stem, key) = match kind.as_str() {
            // A tool search carries its arguments in the item alone.
            Some("tool_search_call") => {
                self.done_item(item, out);
                return true;
            }
            Some("custom_tool_call") => ("custom_tool_call_input", "input"),
            _ => ("function_call_arguments", "arguments"),
        };
        if !value.is_empty() {
            let mut body = ids.clone();
            body["delta"] = json!(value);
            self.emit(&format!("response.{stem}.delta"), body, out);
        }
        let mut body = ids;
        body[key] = json!(value);
        if stem == "function_call_arguments" {
            body["name"] = item["name"].clone();
        }
        self.emit(&format!("response.{stem}.done"), body, out);
        self.done_item(item, out);
        true
    }

    fn finish(&mut self, outcome: Outcome<'_>, usage: Option<&Value>, out: &mut Vec<Record>) {
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
                    Failure::Upstream(error) => error.clone(),
                    Failure::Empty => malformed_response_error(),
                    Failure::UnusableToolCalls => unusable_tool_calls_error(),
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

fn anthropic_complete_stream(event: &Event) -> Result<Option<Vec<Record>>, ()> {
    if event.meaning.failure.is_some() {
        let error = event
            .payload
            .json()
            .and_then(|value| value.get("error"))
            .filter(|error| !error.is_null())
            .cloned()
            .unwrap_or_else(malformed_response_error);
        return Ok(Some(vec![chat_event(json!({"error":error})), done_event()]));
    }
    if event.meaning.kind == EventKind::Ping {
        return Ok(None);
    }
    let empty = matches!(&event.payload, Payload::Text(text) if text.is_empty());
    // No payload → no dispatch, mirroring the chat path.
    if empty {
        return Ok(None);
    }
    if event.meaning.kind == EventKind::Done {
        return Ok(Some(vec![done_event()]));
    }
    // Fail the stream on a malformed event rather than skip it.
    let parsed = event.payload.json().ok_or(())?;
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
    Ok(Some(vec![chat_event(body)]))
}

/// Stateful event transforms. The source has already decoded and classified
/// each payload, so neither conversion nor sanitation reparses wire bytes.
pub(crate) struct EventTransformStream {
    inner: EventStream,
    transform: StreamTransform,
    fallback_id: String,
    state: StreamState,
    queue: VecDeque<Record>,
    inner_done: bool,
    pending_error: Option<ServiceError>,
    residual: bool,
}

impl EventTransformStream {
    pub(crate) fn new(inner: EventStream, transform: StreamTransform) -> Self {
        let fallback_id = format!("{}-{}", transform.provider(), now_millis());
        Self {
            inner,
            transform,
            fallback_id,
            state: StreamState::default(),
            queue: VecDeque::new(),
            inner_done: false,
            pending_error: None,
            residual: false,
        }
    }
    fn fail(&mut self, reason: &'static str) {
        self.inner_done = true;
        self.pending_error.get_or_insert_with(|| {
            ServiceError::Upstream(UpstreamError::Transport(reason.to_string()))
        });
    }
    fn enqueue(&mut self, records: Vec<Record>) {
        let bytes = records
            .iter()
            .map(Record::buffered_bytes)
            .fold(0, usize::saturating_add);
        if bytes > MAX_EVENT_BYTES {
            self.fail("converted SSE output exceeded the size cap");
        } else {
            self.queue.extend(records);
        }
    }
}

impl Stream for EventTransformStream {
    type Item = Result<Record, ServiceError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(record) = this.queue.pop_front() {
                return Poll::Ready(Some(Ok(record)));
            }
            if this.inner_done {
                return Poll::Ready(this.pending_error.take().map(Err));
            }
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(record))) => {
                    this.residual |= record.residual;
                    match this
                        .transform
                        .apply_record(record, &this.fallback_id, &mut this.state)
                    {
                        Ok(records) => this.enqueue(records),
                        Err(()) => {
                            this.fail("provider sent an event this surface cannot represent")
                        }
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    this.inner_done = true;
                    this.pending_error = Some(error);
                }
                Poll::Ready(None) => {
                    this.inner_done = true;
                    if this.residual {
                        this.fail("provider ended mid-event");
                        continue;
                    }
                    match this.transform.end_of_stream(&mut this.state) {
                        Ok(Some(records)) => this.enqueue(records),
                        Ok(None) => {}
                        Err(()) => this.fail("provider ended before finishing its response"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(
        stream: impl Stream<Item = Result<Record, ServiceError>> + Send + 'static,
    ) -> ServiceResponseStream {
        Box::pin(stream.filter_map(|item| {
            std::future::ready(match item {
                Ok(record) => match crate::sse::serialize(&record) {
                    Ok(bytes) if bytes.is_empty() => None,
                    result => Some(result),
                },
                Err(error) => Some(Err(error)),
            })
        }))
    }

    fn decode_record(text: &str) -> Record {
        crate::sse::decode_buffered(
            Bytes::from(format!("{text}\n\n")),
            Some(SseProtocol::OpenaiChat),
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| Record::from_sse(sse_stream::Sse::default(), None))
    }
    fn decode_event(text: &str) -> Event {
        decode_record(text).event.unwrap()
    }
    fn edit_event_body(text: &str, edit: impl FnOnce(&mut Value)) -> String {
        let mut record = decode_record(text);
        record.edit_json(edit);
        String::from_utf8(crate::sse::serialize(&record).unwrap().to_vec()).unwrap()
    }
    impl StreamTransform {
        fn apply(
            &self,
            text: &str,
            fallback: &str,
            state: &mut StreamState,
        ) -> Result<Option<String>, ()> {
            let protocol = match self {
                StreamTransform::AnthropicToOpenaiChat => SseProtocol::AnthropicMessages,
                _ => SseProtocol::OpenaiChat,
            };
            let record =
                crate::sse::decode_buffered(Bytes::from(format!("{text}\n\n")), Some(protocol))
                    .unwrap()
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| {
                        Record::from_sse(sse_stream::Sse::default(), Some(protocol))
                    });
            let records = self.apply_record(record, fallback, state)?;
            let mut bytes = Vec::new();
            for record in records {
                bytes.extend_from_slice(&crate::sse::serialize(&record).unwrap());
            }
            Ok((!bytes.is_empty()).then(|| String::from_utf8(bytes).unwrap()))
        }
    }

    fn messages_transform() -> StreamTransform {
        StreamTransform::OpenaiToAnthropicMessages(UpstreamUsage::default())
    }
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::AnthropicMessages),
        ));
        let stream = EventTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Result<Bytes, ServiceError>> = wire(stream).collect().await;
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::AnthropicMessages),
        ));
        let stream = EventTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Bytes> = wire(stream).map(|r| r.unwrap()).collect().await;
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::AnthropicMessages),
        ));
        let stream = EventTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Bytes> = wire(stream).map(|r| r.unwrap()).collect().await;
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::AnthropicMessages),
        ));
        let stream = EventTransformStream::new(inner, StreamTransform::AnthropicToOpenaiChat);
        let collected: Vec<Bytes> = wire(stream).map(|r| r.unwrap()).collect().await;
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

    // Intentional WHATWG correction: bare payloads are unknown fields.
    #[test]
    fn bare_payloads_never_dispatch_and_event_names_are_exact() {
        let mut state = StreamState::default();
        let output = StreamTransform::AnthropicToOpenaiChat
            .apply("event: ping \ndata: {}", "fb", &mut state)
            .unwrap();
        assert!(
            output.is_some(),
            "a trailing space is part of the event name"
        );
        let record = decode_record("event: message_stop\nevent\ndata: {}");
        assert_eq!(record.event.as_ref().unwrap().name, "message");
        let output = StreamTransform::AnthropicToOpenaiChat
            .apply_record(record, "fb", &mut state)
            .unwrap();
        assert!(output.iter().all(|record| record
            .event
            .as_ref()
            .unwrap()
            .meaning
            .terminal
            .is_none()));
    }

    fn started_messages_state() -> StreamState {
        let mut state = StreamState::default();
        messages_transform()
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
        let out = messages_transform()
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
            Ok(Bytes::from(vec![b'x'; MAX_EVENT_BYTES + 1])),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(events));
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let stream = EventTransformStream::new(inner, messages_transform());
        let collected: Vec<Result<Bytes, ServiceError>> = wire(stream).collect().await;
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let converted = Box::pin(EventTransformStream::new(inner, messages_transform()));
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        });
        // Messages surface: canonicalize touches only an in-band `error` event
        // on this unschematized surface, so the anthropic events here are left
        // intact and only id/model are rewritten.
        let sanitized = EventTransformStream::new(
            converted,
            StreamTransform::SanitizeResponse(identity, Endpoint::Messages),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = wire(sanitized).collect().await;

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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        });
        let sanitized = EventTransformStream::new(
            inner,
            StreamTransform::SanitizeResponse(identity, Endpoint::CreateModelResponse),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = wire(sanitized).collect().await;
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
                ("openai", "messages") => messages_transform(),
                other => panic!("unmapped stream fixture {other:?}"),
            };
            let events = case["events"].as_array().unwrap();
            let output = replay_fixture(transform, events);
            let mut expected = case["output"].clone();
            // The legacy bridge now emits one properly framed typed terminal.
            if case["fn"] == json!("complete") {
                expected
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"__done":true}));
            }
            assert_eq!(
                Value::Array(output),
                expected,
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let identity = Arc::new(ResponseIdentity {
            request_id: "resp_fixture".to_string(),
            user_model: Some("gpt-test".to_string()),
        });
        let stream = EventTransformStream::new(
            inner,
            StreamTransform::OpenaiChatToResponses(
                Arc::new(json!({})),
                identity,
                UpstreamUsage::default(),
            ),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = wire(stream).collect().await;
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
            let parsed = decode_event(block);
            let data = parsed.data.as_str();
            assert_ne!(data, "[DONE]", "Responses surface emitted [DONE]");
            let value: Value = serde_json::from_str(data).expect("Responses event JSON");
            assert_eq!(
                Some(parsed.name.as_str()),
                value["type"].as_str(),
                "SSE event name must match payload type"
            );
            events.push(value);
        }
        (events, error, wire)
    }

    /// The Responses ends of the cases `openai_to_anthropic_stream_endings`
    /// covers for Messages.
    #[tokio::test]
    async fn responses_stream_endings() {
        let text = chat_event(json!({ "content": "partial" }), None);
        let nameless =
            tool_event(json!({ "index": 0, "id": "c", "function": { "arguments": "{" } }));
        let length = chat_event(json!({}), Some("length"));
        let error = format!("data: {}", json!({ "error": { "message": "failed" } }));
        let no_choice = "data: {}".to_string();
        for (events, terminal) in [
            // Cut off mid-call: incomplete, even with no named call.
            (vec![&text, &nameless, &length], "response.incomplete"),
            (vec![&text, &error], "response.failed"),
            (vec![&no_choice], "response.failed"),
        ] {
            let mut events: Vec<&str> = events.into_iter().map(String::as_str).collect();
            events.push("data: [DONE]");
            let (out, error, wire) = replay_responses_fixture(&json!({ "events": events })).await;
            assert!(error.is_none(), "{wire}");
            assert_eq!(out.last().unwrap()["type"], terminal, "{wire}");
        }
        // Text the limit cut off is unfinished, though a call came before it;
        // citations stay with their text when a refusal follows in one delta.
        let call = tool_event(
            json!({ "index": 0, "id": "c", "function": { "name": "f", "arguments": "{}" } }),
        );
        let citation = json!({
            "type": "url_citation",
            "url_citation": { "start_index": 0, "end_index": 7, "url": "https://x", "title": "x" }
        });
        let cited = chat_event(
            json!({ "content": "partial", "refusal": "no", "annotations": [citation] }),
            None,
        );
        let events = json!({ "events": [&call, cited, &length, "data: [DONE]"] });
        let (out, _, wire) = replay_responses_fixture(&events).await;
        let output = &out.last().unwrap()["response"]["output"];
        assert_eq!(output[0]["status"], "completed", "{wire}");
        assert_eq!(
            output[0]["content"][0]["annotations"][0]["url"],
            "https://x"
        );
        assert_eq!(output[1]["content"][0]["type"], "refusal");
        assert_eq!(output[1]["status"], "incomplete", "{wire}");
        assert_eq!(output[2]["type"], "function_call");
        // Text that resumed after a call is the text the limit cut off.
        let events = json!({ "events": [&text, &call, &text, &length, "data: [DONE]"] });
        let (out, _, wire) = replay_responses_fixture(&events).await;
        let output = &out.last().unwrap()["response"]["output"];
        assert_eq!(output[0]["status"], "incomplete", "{wire}");
    }

    #[test]
    fn responses_stream_restores_tool_search_calls() {
        let transform = StreamTransform::OpenaiChatToResponses(
            Arc::new(json!({ "tools": [{ "type": "tool_search" }] })),
            Arc::new(ResponseIdentity {
                request_id: "resp".into(),
                user_model: None,
            }),
            UpstreamUsage::default(),
        );
        let call = tool_event(json!({
            "index": 0, "id": "ts_1",
            "function": { "name": "tool_search", "arguments": "{\"query\":\"mail\"}" }
        }));
        let finish = chat_event(json!({}), Some("tool_calls"));
        let out = run(transform, &[&call, &finish, "data: [DONE]"]);
        let kinds: Vec<&str> = out
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.output_item.done",
                "response.completed"
            ]
        );
        assert_eq!(
            out[3]["item"],
            json!({
                "id": "tsc_ts_1", "type": "tool_search_call", "call_id": "ts_1",
                "status": "completed", "execution": "client", "arguments": { "query": "mail" }
            })
        );
    }

    /// Rebuild the output the way a client applies events one by one: every
    /// part a delta names must have been added, and every done item must be
    /// what the deltas built.
    fn accumulate_responses(events: &[Value]) -> Value {
        let mut output: Vec<Value> = Vec::new();
        for event in events {
            let kind = event["type"].as_str().unwrap();
            let index = event["output_index"].as_u64().map(|index| index as usize);
            match kind {
                "response.output_item.added" => {
                    assert_eq!(index, Some(output.len()), "{event}");
                    output.push(event["item"].clone());
                }
                "response.content_part.added" => {
                    let content = output[index.unwrap()]["content"].as_array_mut().unwrap();
                    assert_eq!(event["content_index"], json!(content.len()), "{event}");
                    content.push(event["part"].clone());
                }
                "response.output_text.delta"
                | "response.reasoning_text.delta"
                | "response.refusal.delta" => {
                    let content_index = event["content_index"].as_u64().unwrap() as usize;
                    let part = output[index.unwrap()]["content"]
                        .get_mut(content_index)
                        .unwrap_or_else(|| panic!("delta for a part never added: {event}"));
                    let key = if kind == "response.refusal.delta" {
                        "refusal"
                    } else {
                        "text"
                    };
                    let text = format!(
                        "{}{}",
                        part[key].as_str().unwrap(),
                        event["delta"].as_str().unwrap()
                    );
                    part[key] = json!(text);
                }
                "response.function_call_arguments.delta" => {
                    let item = &mut output[index.unwrap()];
                    let arguments = format!(
                        "{}{}",
                        item["arguments"].as_str().unwrap(),
                        event["delta"].as_str().unwrap()
                    );
                    item["arguments"] = json!(arguments);
                }
                "response.output_item.done" => {
                    let item = &mut output[index.unwrap()];
                    if item.get("content").is_some() {
                        assert_eq!(item["content"], event["item"]["content"], "{event}");
                    }
                    if item["type"] == "function_call" && event["item"]["status"] == "completed" {
                        assert_eq!(item["arguments"], event["item"]["arguments"], "{event}");
                    }
                    *item = event["item"].clone();
                }
                _ => {}
            }
        }
        Value::Array(output)
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
            assert_eq!(
                accumulate_responses(&events),
                events.last().unwrap()["response"]["output"],
                "case {name}: events do not build the final output"
            );
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
            let result = transform
                .apply(event, "fallback-1", &mut state)
                .unwrap_or_else(|()| panic!("transform rejected {event}"));
            if let Some(result) = result {
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

    fn chat_event(delta: Value, finish_reason: Option<&str>) -> String {
        format!(
            "data: {}",
            json!({ "choices": [{ "delta": delta, "finish_reason": finish_reason }] })
        )
    }

    fn tool_event(call: Value) -> String {
        chat_event(json!({ "tool_calls": [call] }), None)
    }

    #[tokio::test]
    async fn openai_to_anthropic_states_usage_only_once_final() {
        // vLLM-style continuous usage puts a running total on every chunk, and
        // only the last carries the cache split. A per-field max over
        // message_start and message_delta must not bill cached input in full.
        let events = [
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":1}}".to_string(),
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":80}}}".to_string(),
            "data: [DONE]".to_string(),
        ];
        let wire = format!("{}\n\n", events.join("\n\n"));
        let chunks: Vec<_> = wire
            .as_bytes()
            .chunks(7)
            .map(|bytes| Ok(Bytes::copy_from_slice(bytes)))
            .collect();
        let source = Box::pin(crate::sse::DecodedStream::new(
            Box::pin(futures_util::stream::iter(chunks)),
            Some(SseProtocol::OpenaiChat),
        ));
        let out: Vec<Value> = EventTransformStream::new(source, messages_transform())
            .filter_map(|record| std::future::ready(record.unwrap().json().cloned()))
            .collect()
            .await;
        let start = out.iter().find(|e| e["type"] == "message_start").unwrap();
        assert_eq!(
            start["message"]["usage"],
            json!({ "input_tokens": 0, "output_tokens": 0 })
        );
        let delta = out.iter().find(|e| e["type"] == "message_delta").unwrap();
        assert_eq!(
            out.iter().filter(|e| e["type"] == "message_delta").count(),
            1
        );
        assert_eq!(
            delta["usage"],
            json!({ "input_tokens": 20, "output_tokens": 2, "cache_read_input_tokens": 80 })
        );
    }

    #[test]
    fn openai_to_anthropic_blocks_are_sequential() {
        let events = [
            chat_event(json!({ "reasoning_content": "plan" }), None),
            chat_event(json!({ "content": "calling" }), None),
            // Arguments may precede the identity, and parallel calls interleave.
            tool_event(json!({ "index": 0, "function": { "arguments": "{\"q\":" } })),
            tool_event(json!({ "index": 0, "id": "call_1", "function": { "name": "first" } })),
            tool_event(
                json!({ "index": 1, "id": "call_2", "function": { "name": "second", "arguments": "{\"b\":" } }),
            ),
            tool_event(json!({ "index": 0, "function": { "arguments": "\"x\"}" } })),
            tool_event(json!({ "index": 1, "function": { "arguments": "2}" } })),
            chat_event(json!({ "content": " after" }), Some("stop")),
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = events.iter().map(String::as_str).collect();
        let out = run(messages_transform(), &refs);

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
        // Calls are written complete once the stream finishes, after the text.
        assert_eq!(kinds, ["thinking", "text", "tool_use", "tool_use"]);
        assert_eq!(blocks[2]["name"], "first");
        assert_eq!(blocks[3]["name"], "second");
        assert_eq!(inputs[&2], r#"{"q":"x"}"#);
        assert_eq!(inputs[&3], r#"{"b":2}"#);
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
            tool_event(json!({ "id": "call_2", "function": { "name": "g", "arguments": "{" } })),
            // An indexed delta for the call the id opened, then a bare one.
            tool_event(json!({ "index": 1, "function": { "arguments": "\"b\":" } })),
            tool_event(json!({ "function": { "arguments": "2}" } })),
            chat_event(json!({}), Some("tool_calls")),
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = events.iter().map(String::as_str).collect();
        let out = run(messages_transform(), &refs);
        let ids: Vec<&Value> = out
            .iter()
            .filter(|event| event["type"] == "content_block_start")
            .map(|event| &event["content_block"]["id"])
            .collect();
        assert_eq!(ids, [&json!("call_1"), &json!("call_2")]);
        let input = |index: i64| -> String {
            out.iter()
                .filter(|event| event["index"] == index)
                .filter_map(|event| event["delta"]["partial_json"].as_str())
                .collect()
        };
        assert_eq!(input(0), r#"{"a":1}"#);
        assert_eq!(input(1), r#"{"b":2}"#);
        assert_eq!(out.last().unwrap()["type"], "message_stop");
    }

    #[test]
    fn openai_to_anthropic_stream_endings() {
        let transform = messages_transform();
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
        // A call cut off by the token limit is the limit's doing, not
        // malformed, whether or not it got its name.
        let length = chat_event(json!({}), Some("length"));
        let nameless =
            tool_event(json!({ "index": 0, "id": "c", "function": { "arguments": "{}" } }));
        for call in [&partial_call, &nameless] {
            assert_eq!(
                stop_reason(&[call, &length, "data: [DONE]"]),
                Some(json!("max_tokens"))
            );
        }
        // Reasons outside the OpenAI vocabulary end the turn.
        for reason in ["eos", "abort"] {
            let event = chat_event(json!({ "content": "hi" }), Some(reason));
            assert_eq!(
                stop_reason(&[&event, "data: [DONE]"]),
                Some(json!("end_turn"))
            );
        }

        // A garbled call gets an empty input.
        let tool_calls = chat_event(json!({}), Some("tool_calls"));
        let out = run(
            transform.clone(),
            &[&partial_call, &tool_calls, "data: [DONE]"],
        );
        assert!(out
            .iter()
            .any(|event| event["delta"]["partial_json"] == "{}"));
        assert_eq!(out.last().unwrap()["type"], "message_stop");

        // Only a stream with no answer fails: an upstream error, no choice, or
        // a finished turn whose tool calls none has a name.
        for events in [
            vec!["data: {}", "data: [DONE]"],
            vec![r#"data: {"choices":[null]}"#, "data: [DONE]"],
            vec![nameless.as_str(), tool_calls.as_str(), "data: [DONE]"],
        ] {
            let out = run(transform.clone(), &events);
            assert_eq!(out.last().unwrap()["type"], "error", "{out:?}");
        }
        let error = format!(
            "data: {}",
            json!({ "error": { "type": "overloaded_error", "message": "busy" } })
        );
        let out = run(transform.clone(), &[&text, &error, "data: [DONE]"]);
        assert_eq!(out.last().unwrap()["error"]["type"], "overloaded_error");
    }

    #[test]
    fn odd_upstream_chunks_do_not_cost_the_answer() {
        let first = tool_event(
            json!({ "index": 0, "id": "call_1", "function": { "name": "f", "arguments": "{}" } }),
        );
        let second = tool_event(
            json!({ "index": 1, "id": "call_2", "function": { "name": "g", "arguments": "{}" } }),
        );
        for odd in [
            // A restated identity: the first one stands.
            tool_event(json!({ "index": 0, "id": "call_1", "function": { "name": "h" } })),
            // An interleaved fragment of an earlier call joins it.
            tool_event(json!({ "index": 0, "function": { "arguments": " " } })),
            // Fields of an unexpected shape read as absent.
            chat_event(json!({ "content": 7, "tool_calls": {} }), None),
            tool_event(json!({ "index": -1, "function": 3 })),
            "data: null".to_string(),
            r#"data: {"choices":[null]}"#.to_string(),
        ] {
            let events = [
                first.clone(),
                second.clone(),
                odd.clone(),
                chat_event(json!({}), Some("tool_calls")),
                "data: [DONE]".to_string(),
            ];
            let refs: Vec<&str> = events.iter().map(String::as_str).collect();
            let out = run(messages_transform(), &refs);
            let names: Vec<&Value> = out
                .iter()
                .filter(|event| event["type"] == "content_block_start")
                .map(|event| &event["content_block"]["name"])
                .collect();
            assert_eq!(names, [&json!("f"), &json!("g")], "{odd}");
            assert_eq!(out.last().unwrap()["type"], "message_stop", "{odd}");
        }
        // Another id at an index in use is another call; a known id continues
        // its own call, and the id-less fragments after it at that index.
        let fragment = |index: u64, id: &str, name: &str, arguments: &str| {
            let mut call =
                json!({ "index": index, "function": { "name": name, "arguments": arguments } });
            if !id.is_empty() {
                call["id"] = json!(id);
            }
            tool_event(call)
        };
        for b_index in [0, 1] {
            let events = [
                fragment(0, "call_1", "f", "{\"x\":"),
                fragment(b_index, "call_9", "h", "{}"),
                fragment(b_index, "call_1", "", "1"),
                fragment(b_index, "", "", "}"),
                chat_event(json!({}), Some("tool_calls")),
                "data: [DONE]".to_string(),
            ];
            let refs: Vec<&str> = events.iter().map(String::as_str).collect();
            let out = run(messages_transform(), &refs);
            let calls: Vec<(&Value, String)> = out
                .iter()
                .filter(|event| event["type"] == "content_block_start")
                .map(|start| {
                    let input = out
                        .iter()
                        .filter(|event| event["index"] == start["index"])
                        .filter_map(|event| event["delta"]["partial_json"].as_str())
                        .collect();
                    (&start["content_block"]["id"], input)
                })
                .collect();
            assert_eq!(
                calls,
                [
                    (&json!("call_1"), r#"{"x":1}"#.to_string()),
                    (&json!("call_9"), "{}".to_string())
                ],
                "B at index {b_index}"
            );
        }
        // Only a payload that is not JSON fails: the stream itself is corrupt.
        let mut state = StreamState::default();
        assert!(messages_transform()
            .apply("data: {\"choices\":", "fallback", &mut state)
            .is_err());
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::AnthropicMessages),
        ));
        let converted = Box::pin(EventTransformStream::new(
            inner,
            StreamTransform::AnthropicToOpenaiChat,
        ));
        let identity = Arc::new(ResponseIdentity {
            request_id: "req_ours".to_string(),
            user_model: Some("acme/model-a".to_string()),
        });
        let sanitized = EventTransformStream::new(
            converted,
            StreamTransform::SanitizeResponse(identity, Endpoint::ChatComplete),
        );
        let collected: Vec<Result<Bytes, ServiceError>> = wire(sanitized).collect().await;
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let stream = EventTransformStream::new(inner, messages_transform());
        let text: String = wire(stream)
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let stream = EventTransformStream::new(inner, messages_transform());
        let collected: Vec<Result<Bytes, ServiceError>> = wire(stream).collect().await;
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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let stream = EventTransformStream::new(inner, messages_transform());
        let collected: Vec<Result<Bytes, ServiceError>> = wire(stream).collect().await;

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
        let inner: EventStream = Box::pin(crate::sse::DecodedStream::new(
            inner,
            Some(SseProtocol::OpenaiChat),
        ));
        let stream = EventTransformStream::new(inner, messages_transform());
        let collected: Vec<Result<Bytes, ServiceError>> = wire(stream).collect().await;

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
            "event: chunk\nid: 7\nretry: 100\n: keep\ndata: {\"choices\":[{\"delta\":{\"content\":\"answer\",\"reasoning\":\"hidden\"}}],\"usage\":{\"completion_tokens_details\":{\"reasoning_tokens\":3}}}",
            response_transform::exclude_reasoning,
        );
        assert!(output.contains("event: chunk\n"), "{output}");
        assert!(output.contains("id: 7\n"), "{output}");
        assert!(output.contains("retry: 100\n"), "{output}");
        assert!(
            !output.contains(": keep\n"),
            "upstream comments are dropped: {output}"
        );
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
        for event in ["data: [DONE]", "data: "] {
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
    fn named_anthropic_error_crosses_bridge_with_failure_and_terminal() {
        let record = crate::sse::decode_buffered(Bytes::from_static(b"event: error\ndata: {\"error\":{\"type\":\"api_error\",\"message\":\"failed\"}}\n\n"), Some(SseProtocol::AnthropicMessages)).unwrap().remove(0);
        let output = StreamTransform::AnthropicToOpenaiChat
            .apply_record(record, "fixed", &mut StreamState::default())
            .unwrap();
        assert_eq!(output.len(), 2);
        assert!(output[0].json().unwrap().get("error").is_some());
        assert_eq!(
            output[1].event.as_ref().unwrap().meaning.kind,
            EventKind::Done
        );
        assert!(output.iter().all(|record| record
            .event
            .as_ref()
            .unwrap()
            .meaning
            .failure
            .is_some()));
    }

    #[test]
    fn sanitize_renaming_preserves_terminal_failure_metadata() {
        let record = crate::sse::decode_buffered(Bytes::from_static(b"event: response.error\ndata: {\"type\":\"response.error\",\"error\":{\"type\":\"api_error\",\"message\":\"failed\"}}\n\n"), Some(SseProtocol::OpenaiResponses)).unwrap().remove(0);
        let meaning = record.event.as_ref().unwrap().meaning;
        let transform = StreamTransform::SanitizeResponse(
            Arc::new(ResponseIdentity {
                request_id: "fixed".into(),
                user_model: None,
            }),
            Endpoint::CreateModelResponse,
        );
        let output = transform
            .apply_record(record, "fixed", &mut StreamState::default())
            .unwrap();
        assert_eq!(output[0].event.as_ref().unwrap().meaning, meaning);
        assert!(meaning.terminal.is_some());
        assert!(meaning.failure.is_some());
        assert_eq!(output[0].json().unwrap()["type"], "error");
    }
}
