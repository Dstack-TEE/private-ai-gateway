//! Stateful SSE response transforms: convert an upstream provider's streaming
//! events into the downstream client surface, event by event, threading mutable
//! state across events (a per-stream transform state).
//!
//! Three conversions are supported; same-format streaming is native passthrough
//! and never reaches this module. Cost injection, TTFT, and outcome are a
//! separate metering pass downstream (`sse`).

use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use futures_util::Stream;
use serde_json::{json, Value};

use crate::aggregator::service::{ServiceError, ServiceResponseStream};

use super::request_transform::Endpoint;
use super::types::ProviderFormat;

const STRICT_OPENAI_COMPLIANCE: bool = true;

/// Which streaming transform applies, plus the SSE split delimiter it expects.
#[derive(Debug, Clone, Copy)]
pub enum StreamTransform {
    AnthropicToOpenaiChat,
    OpenaiToAnthropicMessages,
    AnthropicCompleteToOpenai,
}

impl StreamTransform {
    fn split_pattern(self) -> &'static [u8] {
        match self {
            // Native Anthropic legacy completion streams use CRLF-CRLF.
            StreamTransform::AnthropicCompleteToOpenai => b"\r\n\r\n",
            _ => b"\n\n",
        }
    }

    fn provider(self) -> &'static str {
        match self {
            StreamTransform::OpenaiToAnthropicMessages => "openai",
            _ => "anthropic",
        }
    }

    // `Err(())` means the provider sent an unparseable event; on the Anthropic
    // paths this ends the stream and classifies it failed rather than skipping.
    fn apply(
        self,
        event: &str,
        fallback_id: &str,
        state: &mut StreamState,
    ) -> Result<Option<String>, ()> {
        match self {
            StreamTransform::AnthropicToOpenaiChat => {
                anthropic_chat_stream(event, fallback_id, state, STRICT_OPENAI_COMPLIANCE)
            }
            StreamTransform::OpenaiToAnthropicMessages => {
                // Parse errors here are caught and the event is skipped.
                Ok(openai_to_anthropic_messages_stream(
                    event,
                    fallback_id,
                    state,
                ))
            }
            StreamTransform::AnthropicCompleteToOpenai => anthropic_complete_stream(event),
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
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// Strip a leading `event: ...` line and a leading `data: ` prefix, then trim.
fn strip_event_and_data(event: &str) -> &str {
    let mut text = event.trim();
    if text.starts_with("event: ") {
        text = match text.find('\n') {
            Some(nl) => text[nl + 1..].trim_start_matches(['\r', '\n']),
            None => "",
        };
    }
    text.strip_prefix("data: ").unwrap_or(text).trim()
}

fn i64_field(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn transform_finish_reason(stop_reason: Option<&str>, strict: bool) -> String {
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

fn map_finish_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ => "end_turn",
    }
}

// ── Anthropic Messages SSE → OpenAI chat.completion.chunk ────────────────────

fn anthropic_chat_stream(
    event: &str,
    fallback_id: &str,
    state: &mut StreamState,
    strict: bool,
) -> Result<Option<String>, ()> {
    let chunk = event.trim();
    if chunk.starts_with("event: ping") || chunk.starts_with("event: content_block_stop") {
        return Ok(None);
    }
    if chunk.starts_with("event: message_stop") {
        return Ok(Some("data: [DONE]\n\n".to_string()));
    }

    let payload = strip_event_and_data(chunk);
    // A malformed provider event must fail the stream here rather than be
    // silently skipped.
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

fn openai_to_anthropic_messages_stream(
    event: &str,
    fallback_id: &str,
    state: &mut StreamState,
) -> Option<String> {
    let chunk = event.trim();

    if chunk == "data: [DONE]" || chunk == "[DONE]" {
        let mut output = String::new();
        if state.content_block_started {
            output.push_str(&sse_event(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": state.current_content_index }),
            ));
        }
        for index in &state.tool_calls_started {
            output.push_str(&sse_event(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": index }),
            ));
        }
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
            return Some(output);
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
        return Some(output);
    }

    let payload = chunk.strip_prefix("data: ").unwrap_or(chunk);
    if payload.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(payload).ok()?;

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
        return if output.is_empty() {
            None
        } else {
            Some(output)
        };
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

    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

// ── Anthropic legacy completion SSE → OpenAI completion chunks ───────────────

fn anthropic_complete_stream(event: &str) -> Result<Option<String>, ()> {
    let chunk = event.trim();
    if chunk.starts_with("event: ping") {
        return Ok(None);
    }
    let mut payload = chunk;
    if payload.starts_with("event: completion") {
        payload = match payload.find('\n') {
            Some(nl) => payload[nl + 1..].trim_start_matches(['\r', '\n']),
            None => "",
        };
    }
    let payload = payload.strip_prefix("data: ").unwrap_or(payload).trim();
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

/// Splits the provider byte stream into SSE events and applies a stateful
/// transform to each, emitting client-surface bytes.
pub struct SseTransformStream {
    inner: ServiceResponseStream,
    transform: StreamTransform,
    fallback_id: String,
    state: StreamState,
    split: &'static [u8],
    buffer: Vec<u8>,
    queue: VecDeque<Bytes>,
    inner_done: bool,
}

impl SseTransformStream {
    pub fn new(inner: ServiceResponseStream, transform: StreamTransform) -> Self {
        let fallback_id = format!("{}-{}", transform.provider(), now_millis());
        Self {
            inner,
            transform,
            fallback_id,
            state: StreamState::default(),
            split: transform.split_pattern(),
            buffer: Vec::new(),
            queue: VecDeque::new(),
            inner_done: false,
        }
    }

    // Returns false if the transform rejected the event (unparseable provider
    // data): the stream must end there, with no terminal marker emitted, so the
    // meter classifies it as failed.
    fn emit(&mut self, event_bytes: &[u8]) -> bool {
        if event_bytes.is_empty() {
            return true;
        }
        let event = String::from_utf8_lossy(event_bytes);
        match self
            .transform
            .apply(&event, &self.fallback_id, &mut self.state)
        {
            Ok(Some(out)) => {
                self.queue.push_back(Bytes::from(out));
                true
            }
            Ok(None) => true,
            Err(()) => false,
        }
    }

    // Drain all complete events currently in the buffer into the output queue.
    fn drain_buffer(&mut self) {
        while let Some(pos) = find_subslice(&self.buffer, self.split) {
            let event: Vec<u8> = self.buffer.drain(..pos + self.split.len()).collect();
            if !self.emit(&event[..pos]) {
                self.inner_done = true;
                self.buffer.clear();
                return;
            }
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
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
                return Poll::Ready(None);
            }
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    this.buffer.extend_from_slice(&bytes);
                    this.drain_buffer();
                    // loop to flush the queue or poll again
                }
                Poll::Ready(None) => {
                    this.inner_done = true;
                    // Flush a non-empty residual event once (no trailing delimiter).
                    if !this.buffer.is_empty() {
                        let residual = std::mem::take(&mut this.buffer);
                        this.emit(&residual);
                    }
                    // loop to drain any queued output, then return None.
                }
                // On an upstream error, end without flushing the (truncated)
                // residual, so a partial event can't synthesize a spurious
                // terminal marker.
                Poll::Ready(Some(Err(_))) => {
                    this.inner_done = true;
                    this.buffer.clear();
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
        let collected: Vec<Bytes> = stream.map(|r| r.unwrap()).collect().await;
        let text: String = collected
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(text.contains("chat.completion.chunk"));
        assert!(
            !text.contains("[DONE]"),
            "stream must not emit a terminal after a malformed event: {text}"
        );
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
