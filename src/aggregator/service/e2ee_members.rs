//! The E2EE v2.1 request allowlist (spec/e2ee-v2.md §10.5). A v2.1 request may
//! carry only the members v2.1 encrypts and readable parameters the body
//! digest binds; anything else is refused before forwarding, so nothing v2.1
//! does not protect reaches an upstream. The lists are those of
//! `spec/e2ee-v2.1-request-members.json`.

use serde_json::{Map, Value};

use super::{E2eeError, CHAT_COMPLETIONS_PATH, COMPLETIONS_PATH, EMBEDDINGS_PATH};

const CHAT: &[&str] = &[
    "messages",
    "model",
    "stream",
    "stream_options",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "frequency_penalty",
    "presence_penalty",
    "repetition_penalty",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "seed",
    "stop",
    "n",
    "response_format",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning_effort",
    "reasoning",
    "user",
    "provider",
];
const COMPLETIONS: &[&str] = &[
    "prompt",
    "model",
    "stream",
    "stream_options",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "frequency_penalty",
    "presence_penalty",
    "repetition_penalty",
    "logit_bias",
    "logprobs",
    "seed",
    "stop",
    "n",
    "user",
    "provider",
];
const EMBEDDINGS: &[&str] = &[
    "input",
    "model",
    "encoding_format",
    "dimensions",
    "user",
    "provider",
];
const MESSAGE: &[&str] = &[
    "role",
    "content",
    "name",
    "tool_calls",
    "tool_call_id",
    "reasoning",
    "reasoning_content",
];
const TOOL_CALL: &[&str] = &["id", "type", "function", "index"];
const TOOL_CALL_FUNCTION: &[&str] = &["name", "arguments"];

/// Refuse a v2.1 request body that carries anything outside the allowlist, or
/// a content location in a form v2.1 does not encrypt: a structured
/// `content` array, a non-string reasoning value, or token-id prompt items.
pub(super) fn check_v2_1_request_members(
    endpoint_path: &str,
    payload: &Value,
) -> Result<(), E2eeError> {
    let body = payload.as_object().ok_or(E2eeError::DecryptionFailed)?;
    match endpoint_path {
        CHAT_COMPLETIONS_PATH => {
            only(body, CHAT, "")?;
            let messages = body
                .get("messages")
                .and_then(Value::as_array)
                .ok_or_else(|| unsupported("messages"))?;
            for (m, message) in messages.iter().enumerate() {
                check_message(message, &format!("messages.{m}"))?;
            }
            Ok(())
        }
        COMPLETIONS_PATH => {
            only(body, COMPLETIONS, "")?;
            check_text_items(body, "prompt")
        }
        EMBEDDINGS_PATH => {
            only(body, EMBEDDINGS, "")?;
            check_text_items(body, "input")
        }
        // No other endpoint terminates E2EE.
        _ => Err(E2eeError::DecryptionFailed),
    }
}

fn check_message(message: &Value, path: &str) -> Result<(), E2eeError> {
    let message = message.as_object().ok_or_else(|| unsupported(path))?;
    only(message, MESSAGE, path)?;
    // v2.1 seals content whole; reasoning is a string (§10.5).
    for key in ["content", "reasoning", "reasoning_content"] {
        string_or_null(message, key, path)?;
    }
    let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) else {
        return Ok(());
    };
    let calls = calls
        .as_array()
        .ok_or_else(|| unsupported(&format!("{path}.tool_calls")))?;
    for (t, call) in calls.iter().enumerate() {
        let path = format!("{path}.tool_calls.{t}");
        let call = call.as_object().ok_or_else(|| unsupported(&path))?;
        only(call, TOOL_CALL, &path)?;
        if let Some(function) = call.get("function").filter(|function| !function.is_null()) {
            let path = format!("{path}.function");
            let function = function.as_object().ok_or_else(|| unsupported(&path))?;
            only(function, TOOL_CALL_FUNCTION, &path)?;
            string_or_null(function, "arguments", &path)?;
        }
    }
    Ok(())
}

/// `prompt` / `input`: a string or an array of strings. Token-id items are
/// content v2.1 cannot encrypt.
fn check_text_items(body: &Map<String, Value>, key: &str) -> Result<(), E2eeError> {
    match body.get(key) {
        Some(Value::String(_)) => Ok(()),
        Some(Value::Array(items)) => match items.iter().position(|item| !item.is_string()) {
            Some(i) => Err(unsupported(&format!("{key}.{i}"))),
            None => Ok(()),
        },
        _ => Err(unsupported(key)),
    }
}

/// A member whose value is null counts as absent (§10.5), so a reply sent
/// back as history (`refusal: null`, `tool_calls: null`, ...) passes.
fn only(object: &Map<String, Value>, allowed: &[&str], path: &str) -> Result<(), E2eeError> {
    match object
        .iter()
        .find(|(key, value)| !value.is_null() && !allowed.contains(&key.as_str()))
        .map(|(key, _)| key)
    {
        Some(key) if path.is_empty() => Err(unsupported(key)),
        Some(key) => Err(unsupported(&format!("{path}.{key}"))),
        None => Ok(()),
    }
}

fn string_or_null(object: &Map<String, Value>, key: &str, path: &str) -> Result<(), E2eeError> {
    match object.get(key) {
        None | Some(Value::Null | Value::String(_)) => Ok(()),
        Some(_) => Err(unsupported(&format!("{path}.{key}"))),
    }
}

fn unsupported(path: &str) -> E2eeError {
    E2eeError::UnsupportedMember(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lists_match_the_spec_allowlist() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/spec/e2ee-v2.1-request-members.json"
        );
        let spec: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for (name, list) in [
            ("chat", CHAT),
            ("completions", COMPLETIONS),
            ("embeddings", EMBEDDINGS),
            ("message", MESSAGE),
            ("tool_call", TOOL_CALL),
            ("tool_call_function", TOOL_CALL_FUNCTION),
        ] {
            assert_eq!(spec[name], json!(list), "{name}");
        }
    }

    #[test]
    fn accepts_a_reply_sent_back_as_history() {
        let body = json!({"model": "m", "messages": [
            {"role": "user", "content": "q"},
            {"role": "assistant", "content": "a", "refusal": null, "tool_calls": null,
             "annotations": null, "audio": null, "function_call": null},
            {"role": "user", "content": "q2"},
        ]});
        check_v2_1_request_members(CHAT_COMPLETIONS_PATH, &body).unwrap();
    }

    #[test]
    fn a_null_tool_call_function_counts_as_absent() {
        for call in [json!({"id": "c"}), json!({"id": "c", "function": null})] {
            let body =
                json!({"model": "m", "messages": [{"role": "assistant", "tool_calls": [call]}]});
            check_v2_1_request_members(CHAT_COMPLETIONS_PATH, &body).unwrap();
        }
    }

    #[test]
    fn refuses_what_v2_1_does_not_protect() {
        let rejected = |path: &str, body: Value| match check_v2_1_request_members(path, &body) {
            Err(E2eeError::UnsupportedMember(member)) => member,
            other => panic!("{body} was not refused: {other:?}"),
        };
        let chat = |extra: Value| {
            let mut body = json!({"model": "m", "messages": [{"role": "user", "content": "c"}]});
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            body
        };
        assert!(
            check_v2_1_request_members(CHAT_COMPLETIONS_PATH, &chat(json!({"tools": []}))).is_ok()
        );
        for member in [
            "functions",
            "function_call",
            "prediction",
            "modalities",
            "audio",
        ] {
            assert_eq!(
                rejected(CHAT_COMPLETIONS_PATH, chat(json!({member: {}}))),
                member
            );
        }
        let message = |m: Value| json!({"model": "m", "messages": [m]});
        assert_eq!(
            rejected(
                CHAT_COMPLETIONS_PATH,
                message(json!({"role": "user", "content": [{"type": "text", "text": "x"}]}))
            ),
            "messages.0.content"
        );
        assert_eq!(
            rejected(
                CHAT_COMPLETIONS_PATH,
                message(json!({"role": "assistant", "function_call": {}}))
            ),
            "messages.0.function_call"
        );
        assert_eq!(
            rejected(
                CHAT_COMPLETIONS_PATH,
                message(
                    json!({"role": "assistant", "tool_calls": [{"function": {"name": "f", "arguments": "a", "x": 1}}]})
                )
            ),
            "messages.0.tool_calls.0.function.x"
        );
        assert_eq!(
            rejected(
                COMPLETIONS_PATH,
                json!({"model": "m", "prompt": "p", "suffix": "s"})
            ),
            "suffix"
        );
        assert_eq!(
            rejected(COMPLETIONS_PATH, json!({"model": "m", "prompt": [1, 2]})),
            "prompt.0"
        );
        assert_eq!(
            rejected(EMBEDDINGS_PATH, json!({"model": "m", "input": ["a", [3]]})),
            "input.1"
        );
    }
}
