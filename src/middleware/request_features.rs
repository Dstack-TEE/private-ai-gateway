//! Content-derived request features for the pre-request consult.
//!
//! The control plane routes better when it knows what the request demands —
//! how long the prompt is, which modalities it carries, whether it wants
//! tools, structured output, or reasoning — but it is content-blind by
//! contract. This module is the seam that keeps both: the content stays in
//! the TEE, and only numbers, closed enums and a one-way hash cross.
//!
//! The vocabulary is deliberately the industry's, not ours: modality values
//! are the model catalog's (`text`/`image`/`file`/`audio`/`video`),
//! `reasoning` is Anthropic's thinking.type enum plus `unspecified`, and
//! `response_format` is OpenAI's response_format.type. Both ends of the
//! control plane's capability filter speaking the same words is what keeps
//! its rules subset checks instead of translations.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::request_transform::Endpoint;
use super::types::{ReasoningConfig, ReasoningEffort};

/// First-bytes cap for the prefix hash. 4096 bytes is ≈1k tokens — aligned
/// with the smallest prefix the major providers cache at all (1024 tokens),
/// so a shorter shared prefix that this cap would have distinguished is one
/// no upstream cache would have rewarded anyway. Within one conversation the
/// first 4KB do not change as turns are appended, which is what makes the
/// hash a stable affinity key.
const PREFIX_CAP: usize = 4096;
/// How much of a media part's url/data feeds the hash: enough to distinguish,
/// cheap enough to never matter.
const MEDIA_SNIPPET: usize = 128;

/// What the client asked reasoning to do — not what any backend will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningIntent {
    Enabled,
    Disabled,
    Unspecified,
}

/// OpenAI `response_format.type`, with absent folded into `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormatKind {
    Text,
    JsonObject,
    JsonSchema,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestFeatures {
    /// Lenient LOWER bound on the prompt's token count (no tokenizer here, and
    /// none needed): ascii_bytes/4 + non_ascii_chars/2, media floors, no
    /// uplift. English sits at ~2.7-3.7 chars/token so /4 under-counts it, and
    /// CJK on CJK-optimised tokenizers (the compressed end of the range)
    /// sits at ~0.37-0.5 tokens/char so /2 under-counts that. The direction is the
    /// contract: the true count is almost certainly >= this, so a filter that
    /// drops on `estimate > window` never drops a request that fit.
    pub estimated_prompt_tokens: u64,
    pub has_tools: bool,
    /// Catalog vocabulary, deduped, in stable order.
    pub input_modalities: Vec<&'static str>,
    pub reasoning: ReasoningIntent,
    pub response_format: ResponseFormatKind,
    /// Hex SHA-256 (truncated to 32 chars, `hash_api_key` style) of the
    /// canonical first bytes of the conversation — the cache-affinity key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_hash: Option<String>,
}

/// Walk accumulator: token-estimate tallies, seen modalities, and the capped
/// canonical prefix. The estimate keeps counting after the prefix is full.
#[derive(Default)]
struct Acc {
    ascii_bytes: u64,
    non_ascii_chars: u64,
    media_tokens: u64,
    modalities: BTreeSet<&'static str>,
    prefix: Vec<u8>,
}

impl Acc {
    fn prefix_push(&mut self, data: &[u8]) {
        if self.prefix.len() < PREFIX_CAP {
            let room = PREFIX_CAP - self.prefix.len();
            self.prefix.extend_from_slice(&data[..data.len().min(room)]);
        }
    }

    /// Every message contributes its role to the prefix: "user: hi" and
    /// "assistant: hi" must not collide.
    fn begin_message(&mut self, role: &str) {
        self.prefix_push(role.as_bytes());
        self.prefix_push(b"\0");
    }

    fn text(&mut self, text: &str) {
        if !text.is_empty() {
            self.modalities.insert("text");
        }
        for c in text.chars() {
            if c.is_ascii() {
                self.ascii_bytes += 1;
            } else {
                self.non_ascii_chars += 1;
            }
        }
        self.prefix_push(text.as_bytes());
    }

    /// Text that counts toward the estimate but not the prefix — serialized
    /// tools / schemas, which sit outside the message sequence the upstream
    /// caches by.
    fn text_outside_prefix(&mut self, text: &str) {
        for c in text.chars() {
            if c.is_ascii() {
                self.ascii_bytes += 1;
            } else {
                self.non_ascii_chars += 1;
            }
        }
    }

    /// A non-text part: record its modality, floor its token cost, and feed a
    /// distinguishing snippet of its url/data to the prefix.
    fn media(&mut self, modality: &'static str, floor_tokens: u64, reference: Option<&str>) {
        self.modalities.insert(modality);
        self.media_tokens += floor_tokens;
        self.prefix_push(modality.as_bytes());
        self.prefix_push(b"\0");
        if let Some(reference) = reference {
            let mut end = reference.len().min(MEDIA_SNIPPET);
            while !reference.is_char_boundary(end) {
                end -= 1;
            }
            self.prefix_push(&reference.as_bytes()[..end]);
        }
    }

    fn estimated_tokens(&self) -> u64 {
        self.ascii_bytes / 4 + self.non_ascii_chars / 2 + self.media_tokens
    }

    fn prefix_hash(&self) -> Option<String> {
        if self.prefix.is_empty() {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(&self.prefix);
        Some(hex::encode(hasher.finalize())[..32].to_string())
    }
}

/// Extract features for the endpoints whose body shape this module knows.
/// `None` for the rest: an absent block routes exactly as before, which is the
/// correct degradation for a shape nobody taught the extractor.
pub fn extract(
    endpoint: Endpoint,
    params: &Value,
    requirements: Option<&ReasoningConfig>,
) -> Option<RequestFeatures> {
    let mut acc = Acc::default();
    match endpoint {
        Endpoint::ChatComplete => walk_openai_messages(params, &mut acc),
        Endpoint::Messages => walk_anthropic(params, &mut acc),
        Endpoint::Complete => {
            if let Some(prompt) = params.get("prompt").and_then(Value::as_str) {
                acc.begin_message("user");
                acc.text(prompt);
            }
        }
        Endpoint::Embed | Endpoint::CreateModelResponse => return None,
    }

    let has_tools = matches!(params.get("tools"), Some(Value::Array(tools)) if !tools.is_empty());
    if has_tools {
        // Tool schemas are serialized into the prompt upstream; count them,
        // JSON being ASCII-heavy the /4 lower bound holds.
        if let Ok(serialized) = serde_json::to_string(&params["tools"]) {
            acc.text_outside_prefix(&serialized);
        }
    }

    let response_format = match params
        .get("response_format")
        .and_then(|f| f.get("type"))
        .and_then(Value::as_str)
    {
        Some("json_schema") => {
            if let Some(schema) = params
                .get("response_format")
                .and_then(|f| f.get("json_schema"))
            {
                if let Ok(serialized) = serde_json::to_string(schema) {
                    acc.text_outside_prefix(&serialized);
                }
            }
            ResponseFormatKind::JsonSchema
        }
        Some("json_object") => ResponseFormatKind::JsonObject,
        _ => ResponseFormatKind::Text,
    };

    Some(RequestFeatures {
        estimated_prompt_tokens: acc.estimated_tokens(),
        has_tools,
        input_modalities: acc.modalities.iter().copied().collect(),
        reasoning: reasoning_intent(endpoint, params, requirements),
        response_format,
        prefix_hash: acc.prefix_hash(),
    })
}

/// OpenAI `messages[]`: content is a string or an array of typed parts. Shape
/// knowledge mirrors the request transforms; a part type neither knows is
/// skipped here exactly as it is dropped there.
fn walk_openai_messages(params: &Value, acc: &mut Acc) {
    let Some(messages) = params.get("messages").and_then(Value::as_array) else {
        return;
    };
    for msg in messages {
        acc.begin_message(msg.get("role").and_then(Value::as_str).unwrap_or(""));
        match msg.get("content") {
            Some(Value::String(text)) => acc.text(text),
            Some(Value::Array(parts)) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            acc.text(part.get("text").and_then(Value::as_str).unwrap_or(""))
                        }
                        // 85 tokens is OpenAI's low-detail image floor — the
                        // smallest any vision model bills an image at.
                        Some("image_url") => acc.media(
                            "image",
                            85,
                            part.get("image_url")
                                .and_then(|i| i.get("url"))
                                .and_then(Value::as_str),
                        ),
                        Some("input_audio") => acc.media(
                            "audio",
                            500,
                            part.get("input_audio")
                                .and_then(|a| a.get("data"))
                                .and_then(Value::as_str),
                        ),
                        Some("video_url") => acc.media(
                            "video",
                            500,
                            part.get("video_url")
                                .and_then(|v| v.get("url"))
                                .and_then(Value::as_str),
                        ),
                        // No token floor: a file's cost is unknowable here and
                        // the estimate is a lower bound.
                        Some("file") => acc.media(
                            "file",
                            0,
                            part.get("file")
                                .and_then(|f| f.get("file_url").or_else(|| f.get("file_data")))
                                .and_then(Value::as_str),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Anthropic-native shape: `system` (string or blocks) stands before
/// `messages[]`, whose blocks type differently from OpenAI parts.
fn walk_anthropic(params: &Value, acc: &mut Acc) {
    match params.get("system") {
        Some(Value::String(text)) => {
            acc.begin_message("system");
            acc.text(text);
        }
        Some(Value::Array(blocks)) => {
            acc.begin_message("system");
            for block in blocks {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    acc.text(text);
                }
            }
        }
        _ => {}
    }
    let Some(messages) = params.get("messages").and_then(Value::as_array) else {
        return;
    };
    for msg in messages {
        acc.begin_message(msg.get("role").and_then(Value::as_str).unwrap_or(""));
        match msg.get("content") {
            Some(Value::String(text)) => acc.text(text),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            acc.text(block.get("text").and_then(Value::as_str).unwrap_or(""))
                        }
                        Some("image") => {
                            let source = block.get("source");
                            acc.media(
                                "image",
                                85,
                                source
                                    .and_then(|s| s.get("url").or_else(|| s.get("data")))
                                    .and_then(Value::as_str),
                            );
                        }
                        Some("document") => {
                            let source = block.get("source");
                            acc.media(
                                "file",
                                0,
                                source
                                    .and_then(|s| s.get("url").or_else(|| s.get("data")))
                                    .and_then(Value::as_str),
                            );
                        }
                        Some("tool_result") => {
                            if let Some(text) = block.get("content").and_then(Value::as_str) {
                                acc.text(text);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// What the client asked reasoning to do. `exclude_reasoning` deliberately
/// plays no part: excluding reasoning from the RESPONSE (the public
/// `reasoning.exclude` / `include_reasoning` semantics) still reasons — it is
/// not `enabled: false`.
fn reasoning_intent(
    endpoint: Endpoint,
    params: &Value,
    requirements: Option<&ReasoningConfig>,
) -> ReasoningIntent {
    if let Some(config) = requirements {
        return match config.enabled {
            Some(false) => ReasoningIntent::Disabled,
            Some(true) => ReasoningIntent::Enabled,
            // normalize_chat_request always derives `enabled` when effort or a
            // budget is present, so this arm is only reachable for a config
            // built elsewhere; judge it the same way.
            None => match (config.effort, config.max_tokens) {
                (Some(ReasoningEffort::None), _) => ReasoningIntent::Disabled,
                (Some(_), _) | (None, Some(_)) => ReasoningIntent::Enabled,
                (None, None) => ReasoningIntent::Unspecified,
            },
        };
    }
    // OpenAI-format bodies: the chat-template switches, judged with the same
    // precedence as chat_template_reasoning_intent (any explicit "on" wins;
    // conflicting aliases state no intent to disable).
    if endpoint == Endpoint::ChatComplete {
        if let Some(kwargs) = params
            .get("chat_template_kwargs")
            .and_then(Value::as_object)
        {
            let mut disabled = false;
            for key in ["thinking", "enable_thinking"] {
                match kwargs.get(key).and_then(Value::as_bool) {
                    Some(true) => return ReasoningIntent::Enabled,
                    Some(false) => disabled = true,
                    None => {}
                }
            }
            if disabled {
                return ReasoningIntent::Disabled;
            }
        }
    }
    // Anthropic-native bodies say it directly: thinking.type.
    if endpoint == Endpoint::Messages {
        match params
            .get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str)
        {
            Some("enabled") => return ReasoningIntent::Enabled,
            Some("disabled") => return ReasoningIntent::Disabled,
            _ => {}
        }
    }
    ReasoningIntent::Unspecified
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chat(messages: Value) -> Value {
        json!({ "model": "m", "messages": messages })
    }

    fn features(params: &Value) -> RequestFeatures {
        extract(Endpoint::ChatComplete, params, None).unwrap()
    }

    #[test]
    fn estimate_is_a_lower_bound_for_ascii_and_cjk() {
        // 400 ASCII chars ≈ 100+ real tokens; /4 stays at or under.
        let ascii = features(&chat(
            json!([{ "role": "user", "content": "a".repeat(400) }]),
        ));
        assert_eq!(ascii.estimated_prompt_tokens, 100);
        // 400 CJK chars: the design-doc coefficient (1 token/char) would say
        // 400+, which OVERSHOOTS the ~150-200 a CJK-optimised tokenizer
        // produces; /2 stays under it.
        let cjk = features(&chat(
            json!([{ "role": "user", "content": "谢".repeat(400) }]),
        ));
        assert_eq!(cjk.estimated_prompt_tokens, 200);
    }

    #[test]
    fn media_parts_floor_the_estimate_and_set_modalities() {
        let f = features(&chat(json!([{ "role": "user", "content": [
            { "type": "text", "text": "look" },
            { "type": "image_url", "image_url": { "url": "https://x/img.png" } },
        ]}])));
        assert!(f.estimated_prompt_tokens >= 85);
        assert_eq!(f.input_modalities, vec!["image", "text"]);
    }

    #[test]
    fn tools_and_schema_count_toward_the_estimate_but_not_the_prefix() {
        let plain = chat(json!([{ "role": "user", "content": "hi" }]));
        let mut with_tools = plain.clone();
        with_tools["tools"] = json!([{ "type": "function", "function": {
            "name": "get_weather", "parameters": { "type": "object",
            "properties": { "q": { "type": "string", "description": "x".repeat(400) } } } } }]);
        let (a, b) = (features(&plain), features(&with_tools));
        assert!(b.has_tools);
        assert!(b.estimated_prompt_tokens > a.estimated_prompt_tokens + 100);
        // The affinity key must not fracture on tool-schema edits: the
        // upstream caches by the message prefix, not the tool block.
        assert_eq!(a.prefix_hash, b.prefix_hash);
    }

    #[test]
    fn prefix_hash_is_stable_across_appended_turns() {
        let system = json!({ "role": "system", "content": "s".repeat(5000) });
        let turn1 = chat(json!([system, { "role": "user", "content": "first" }]));
        let turn2 = chat(json!([system, { "role": "user", "content": "first" },
            { "role": "assistant", "content": "answer" },
            { "role": "user", "content": "second" }]));
        // The 4KB cap is inside the long system prompt, so appended turns
        // cannot move the hash…
        assert_eq!(features(&turn1).prefix_hash, features(&turn2).prefix_hash);
        // …while a different system prompt lands a different key.
        let other = chat(json!([{ "role": "system", "content": "t".repeat(5000) }]));
        assert_ne!(features(&turn1).prefix_hash, features(&other).prefix_hash);
    }

    #[test]
    fn short_prefixes_distinguish_by_role_and_text() {
        let user = features(&chat(json!([{ "role": "user", "content": "hi" }])));
        let assistant = features(&chat(json!([{ "role": "assistant", "content": "hi" }])));
        assert_ne!(user.prefix_hash, assistant.prefix_hash);
        assert_eq!(user.prefix_hash.as_ref().unwrap().len(), 32);
        // No messages, nothing to key on.
        assert_eq!(features(&chat(json!([]))).prefix_hash, None);
    }

    #[test]
    fn reasoning_intent_reads_the_switches_not_response_visibility() {
        // chat_template switch off → disabled; conflicting aliases → the "on"
        // wins (same precedence as chat_template_reasoning_intent).
        let mut off = chat(json!([{ "role": "user", "content": "hi" }]));
        off["chat_template_kwargs"] = json!({ "thinking": false });
        assert_eq!(features(&off).reasoning, ReasoningIntent::Disabled);
        off["chat_template_kwargs"] = json!({ "thinking": false, "enable_thinking": true });
        assert_eq!(features(&off).reasoning, ReasoningIntent::Enabled);

        // Normalized requirements: enabled beats everything.
        let plain = chat(json!([{ "role": "user", "content": "hi" }]));
        let disabled = ReasoningConfig {
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(
            extract(Endpoint::ChatComplete, &plain, Some(&disabled))
                .unwrap()
                .reasoning,
            ReasoningIntent::Disabled
        );
        // reasoning.exclude is response visibility, not intent — it arrives
        // here as NO requirements, and must not read as disabled.
        assert_eq!(features(&plain).reasoning, ReasoningIntent::Unspecified);
    }

    #[test]
    fn anthropic_shape_walks_system_first_and_thinking_type() {
        let params = json!({
            "model": "m",
            "system": "you are terse",
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "hello" },
                { "type": "image", "source": { "type": "url", "url": "https://x/i.png" } },
            ]}],
            "thinking": { "type": "disabled" },
        });
        let f = extract(Endpoint::Messages, &params, None).unwrap();
        assert_eq!(f.reasoning, ReasoningIntent::Disabled);
        assert_eq!(f.input_modalities, vec!["image", "text"]);
        assert!(f.estimated_prompt_tokens >= 85);
        // The system prompt leads the canonical prefix: same first bytes as an
        // OpenAI-shape body would produce for the same conversation start.
        assert!(f.prefix_hash.is_some());
    }

    #[test]
    fn response_format_uses_the_openai_type_vocabulary() {
        let mut params = chat(json!([{ "role": "user", "content": "hi" }]));
        assert_eq!(features(&params).response_format, ResponseFormatKind::Text);
        params["response_format"] = json!({ "type": "json_object" });
        assert_eq!(
            features(&params).response_format,
            ResponseFormatKind::JsonObject
        );
        params["response_format"] =
            json!({ "type": "json_schema", "json_schema": { "schema": {} } });
        assert_eq!(
            features(&params).response_format,
            ResponseFormatKind::JsonSchema
        );
    }

    #[test]
    fn unknown_shapes_send_nothing() {
        assert!(extract(Endpoint::Embed, &json!({ "input": "x" }), None).is_none());
    }
}
