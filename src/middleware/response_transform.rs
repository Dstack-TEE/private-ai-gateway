//! Buffered response transforms: convert an upstream provider's 2xx response
//! body into the downstream client surface. Non-2xx responses are normalized by
//! `errors` before reaching here, so these assume a success body. Streaming (SSE)
//! transforms live in `stream_transform`.
//!
//! Cost injection is a separate metering pass; these transforms only normalize
//! the body shape (including the canonical `usage`).

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::request_transform::Endpoint;
use super::types::ProviderFormat;

const STRICT_OPENAI_COMPLIANCE: bool = true;

/// Transform a 2xx upstream body for `format`/`endpoint` into the client surface.
/// Returns the body unchanged when no transform applies (native passthrough).
pub fn transform_response(format: ProviderFormat, endpoint: Endpoint, body: Value) -> Value {
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

// Read a token count, accepting integer- or float-encoded numbers.
fn i64_field(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

// Anthropic stop_reason -> OpenAI finish_reason (strict compliance maps it).
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

// OpenAI finish_reason -> Anthropic stop_reason (downstream surface).
fn map_finish_reason(finish_reason: Option<&str>) -> &'static str {
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
}
