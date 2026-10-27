//! Content-derived request features for the pre-request consult.
//!
//! The control plane routes better when it knows what the request demands —
//! how long the prompt is, which modalities it carries, whether it wants
//! tools, structured output, or reasoning — and it can learn that without
//! reading the request. This module is the seam that keeps both: the content
//! stays in the TEE, and only numbers, closed enums and a one-way hash cross.
//!
//! The vocabulary is deliberately the industry's, not ours: modality values
//! are the model catalog's (`text`/`image`/`file`/`audio`/`video`),
//! `reasoning` is Anthropic's thinking.type enum plus `unspecified`, and
//! `response_format` is OpenAI's response_format.type. Both ends of the
//! control plane's capability filter speaking the same words is what keeps
//! its rules subset checks instead of translations.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::request_transform::Endpoint;
use super::types::{ReasoningConfig, ReasoningEffort};

/// HMAC-SHA256 key for the cache-affinity prefix hash. The gateway derives it
/// from dstack KMS, so it never leaves the TEE.
#[derive(Clone)]
pub struct PrefixHashKey([u8; 32]);

impl PrefixHashKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for PrefixHashKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrefixHashKey(..)")
    }
}

/// How much of a conversation's opening feeds the inferred session key, in
/// UTF-16 units, so every component keying sessions this way agrees on which
/// two openings are "the same conversation". Bounds the hashing work a large
/// first message (an inline image) could demand; two openings alike this far
/// would share a prompt cache anyway.
const OPENING_CAP: usize = 16_384;
/// Longest client-named session identifier honored, in UTF-16 units; a
/// longer one is ignored, not truncated, so
/// a client bug cannot pin unrelated conversations together.
const MAX_SESSION_ID: usize = 256;

/// How the session key was obtained: named by the client's
/// `prompt_cache_key` or inferred from the conversation's
/// opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Explicit,
    Inferred,
}

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
    /// Low-biased heuristic estimate of the prompt's token count (no
    /// tokenizer here): ascii_bytes/5 + non_ascii_chars/3 + directly-counted
    /// tokens. The
    /// coefficients sit past the cheap end of what real tokenizers produce —
    /// English runs ~4-4.5 chars/token, CJK on CJK-optimised tokenizers (the
    /// compressed end of the range) runs ~2-2.7 chars/token — so on ordinary text the
    /// true count exceeds this estimate. It is NOT a mathematical bound:
    /// pathologically compressible input (long whitespace/character runs that
    /// BPE folds into run-tokens) can still come in under it. The control
    /// plane's contract absorbs that residue: the estimate only steers between
    /// candidates, and its keep-all rule forbids it from emptying the list.
    pub estimated_prompt_tokens: u64,
    pub has_tools: bool,
    /// Catalog vocabulary, deduped, in stable order.
    pub input_modalities: Vec<&'static str>,
    pub reasoning: ReasoningIntent,
    pub response_format: ResponseFormatKind,
    /// The cache-affinity (session) key: hex HMAC-SHA256, under the
    /// KMS-derived `PrefixHashKey`, of either the client-named session
    /// identifier or the conversation's opening two messages, truncated to
    /// 32 chars (`hash_api_key` style). The wire name predates the session
    /// semantics and is kept for the control plane's sake.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_hash: Option<String>,
    /// Which of the two the key is; absent together with `prefix_hash`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<SessionKind>,
}

/// Walk accumulator: token-estimate tallies and seen modalities.
#[derive(Default)]
struct Acc {
    ascii_bytes: u64,
    non_ascii_chars: u64,
    /// Tokens counted directly rather than estimated from text: media floors
    /// with a documented basis, and literal token-id arrays (exact).
    extra_tokens: u64,
    modalities: BTreeSet<&'static str>,
}

impl Acc {
    fn text(&mut self, text: &str) {
        if !text.is_empty() {
            self.modalities.insert("text");
        }
        self.estimate(text);
    }

    /// Text that counts toward the estimate but is not message content —
    /// serialized tools / schemas and tool-call arguments.
    fn estimate(&mut self, text: &str) {
        for c in text.chars() {
            if c.is_ascii() {
                self.ascii_bytes += 1;
            } else {
                self.non_ascii_chars += 1;
            }
        }
    }

    /// A non-text part: record its modality and floor its token cost.
    fn media(&mut self, modality: &'static str, floor_tokens: u64) {
        self.modalities.insert(modality);
        self.extra_tokens += floor_tokens;
    }

    fn estimated_tokens(&self) -> u64 {
        self.ascii_bytes / 5 + self.non_ascii_chars / 3 + self.extra_tokens
    }
}

/// The per-text half of the estimate formula, for callers that need a
/// PER-ITEM count (batch prompts take the max item) rather than Acc's
/// running totals.
fn text_tokens(text: &str) -> u64 {
    let mut ascii_bytes = 0u64;
    let mut non_ascii_chars = 0u64;
    for c in text.chars() {
        if c.is_ascii() {
            ascii_bytes += 1;
        } else {
            non_ascii_chars += 1;
        }
    }
    ascii_bytes / 5 + non_ascii_chars / 3
}

/// RFC 2104 HMAC over SHA-256, hand-rolled from the `sha2` already in the
/// tree rather than a new dependency; pinned against an RFC 4231 vector in
/// the tests below.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(padded.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(padded.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Extract features for the endpoints whose body shape this module knows.
/// `None` for the rest: an absent block routes exactly as before, which is the
/// correct degradation for a shape nobody taught the extractor.
pub fn extract(
    endpoint: Endpoint,
    params: &Value,
    requirements: Option<&ReasoningConfig>,
    prefix_hash_key: &PrefixHashKey,
) -> Option<RequestFeatures> {
    let mut acc = Acc::default();
    match endpoint {
        Endpoint::ChatComplete => walk_openai_messages(params, &mut acc),
        Endpoint::Messages => walk_anthropic(params, &mut acc),
        Endpoint::Complete => match params.get("prompt") {
            Some(Value::String(prompt)) => acc.text(prompt),
            // Legacy completions also allow arrays: a batch of string
            // prompts, ONE tokenized prompt (a flat token-id array — exact
            // count, better than any estimate), or a batch of tokenized
            // prompts. Each batch item runs against the model's context
            // window ON ITS OWN, so the routable number is the LARGEST item,
            // not the sum — summing 100 prompts of 1k tokens each would read
            // as 100k and wrongly rule out every model that could serve them.
            Some(Value::Array(items)) => {
                if !items.is_empty() && items.iter().all(Value::is_number) {
                    acc.extra_tokens += items.len() as u64;
                } else {
                    let mut largest = 0u64;
                    for item in items {
                        let item_tokens = match item {
                            Value::String(text) => {
                                acc.modalities.insert("text");
                                text_tokens(text)
                            }
                            Value::Array(ids) => {
                                ids.iter().filter(|v| v.is_number()).count() as u64
                            }
                            _ => 0,
                        };
                        largest = largest.max(item_tokens);
                    }
                    acc.extra_tokens += largest;
                }
            }
            _ => {}
        },
        Endpoint::Embed
        | Endpoint::CreateModelResponse
        | Endpoint::SystemOne
        | Endpoint::ImagesGenerations => return None,
    }

    // Legacy `functions` is still forwarded by the request transform, so it is
    // tools for routing purposes too. (`function_call` alone, with no
    // functions to call, states nothing.)
    let non_empty =
        |key: &str| matches!(params.get(key), Some(Value::Array(items)) if !items.is_empty());
    let has_tools = non_empty("tools") || non_empty("functions");
    if has_tools {
        // Tool schemas are serialized into the prompt upstream; count them.
        for key in ["tools", "functions"] {
            if non_empty(key) {
                if let Ok(serialized) = serde_json::to_string(&params[key]) {
                    acc.estimate(&serialized);
                }
            }
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
                    acc.estimate(&serialized);
                }
            }
            ResponseFormatKind::JsonSchema
        }
        Some("json_object") => ResponseFormatKind::JsonObject,
        _ => ResponseFormatKind::Text,
    };

    let session = session_key(
        endpoint,
        params,
        explicit_session(params).as_deref(),
        prefix_hash_key,
    );

    Some(RequestFeatures {
        estimated_prompt_tokens: acc.estimated_tokens(),
        has_tools,
        input_modalities: acc.modalities.iter().copied().collect(),
        reasoning: reasoning_intent(endpoint, params, requirements),
        response_format,
        prefix_hash: session.as_ref().map(|(hash, _)| hash.clone()),
        session_kind: session.map(|(_, kind)| kind),
    })
}

/// The client-named session: its `prompt_cache_key` (OpenAI's cache-routing
/// hint), honored only as a string of 1..=256 UTF-16 units — an oversized one
/// is ignored, never truncated. Fields named `session_id` / `x-session-id`
/// are deliberately not read: in this gateway a session is an attested
/// upstream session (`provider.aci_session_ids`), and a routing hint must not
/// look like a way to pin one.
pub fn explicit_session(params: &Value) -> Option<String> {
    params
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .filter(|id| (1..=MAX_SESSION_ID).contains(&id.encode_utf16().count()))
        .map(str::to_string)
}

/// The session key and how it was obtained, or `None` when nothing should
/// stick: the caller pinned its own provider order (`provider.order`), or the
/// body has no conversation to key.
///
/// Explicit keys hash the identifier; inferred keys hash the conversation's
/// opening — the first system/developer message and the first other message,
/// which stay the same as turns are appended, so the key is stable from the
/// first turn without any length threshold. Both are HMAC-SHA256 under a key
/// that never leaves the TEE, so the control plane cannot test guesses, even
/// of a fully known opening; it only sees when two requests share one, which
/// is what affinity routes on. The two domains are separated by a tag so a
/// session named after an opening's JSON cannot collide with it.
fn session_key(
    endpoint: Endpoint,
    params: &Value,
    explicit_session: Option<&str>,
    key: &PrefixHashKey,
) -> Option<(String, SessionKind)> {
    let pinned_order = params
        .get("provider")
        .and_then(|p| p.get("order"))
        .and_then(Value::as_array)
        .is_some_and(|order| !order.is_empty());
    if pinned_order {
        return None;
    }
    let (material, kind) = match explicit_session {
        Some(id) => (format!("explicit\0{id}"), SessionKind::Explicit),
        None => {
            let opening = conversation_opening(endpoint, params)?;
            let serialized = serde_json::to_string(&opening).ok()?;
            (
                format!("opening\0{}", utf16_prefix(&serialized, OPENING_CAP)),
                SessionKind::Inferred,
            )
        }
    };
    let digest = hmac_sha256(&key.0, material.as_bytes());
    Some((hex::encode(digest)[..32].to_string(), kind))
}

/// The two messages that identify a conversation:
/// `[[role, content] | null, [role, content] | null]` for chat and Messages
/// (Anthropic's top-level `system` stands in as the system entry), `[prompt]`
/// for completions. `None` when the body carries neither.
fn conversation_opening(endpoint: Endpoint, params: &Value) -> Option<Value> {
    let pick = |msg: &Value| {
        Value::Array(vec![
            msg.get("role").cloned().unwrap_or(Value::Null),
            msg.get("content").cloned().unwrap_or(Value::Null),
        ])
    };
    let is_system = |msg: &Value| {
        matches!(
            msg.get("role").and_then(Value::as_str),
            Some("system") | Some("developer")
        )
    };
    match endpoint {
        Endpoint::ChatComplete => {
            let messages = params.get("messages").and_then(Value::as_array)?;
            let system = messages.iter().find(|m| is_system(m)).map(pick);
            let first = messages.iter().find(|m| !is_system(m)).map(pick);
            if system.is_none() && first.is_none() {
                return None;
            }
            Some(Value::Array(vec![
                system.unwrap_or(Value::Null),
                first.unwrap_or(Value::Null),
            ]))
        }
        Endpoint::Messages => {
            let system = params
                .get("system")
                .map(|system| Value::Array(vec![Value::from("system"), system.clone()]));
            let first = params
                .get("messages")
                .and_then(Value::as_array)
                .and_then(|messages| messages.first())
                .map(pick);
            if system.is_none() && first.is_none() {
                return None;
            }
            Some(Value::Array(vec![
                system.unwrap_or(Value::Null),
                first.unwrap_or(Value::Null),
            ]))
        }
        Endpoint::Complete => params
            .get("prompt")
            .map(|prompt| Value::Array(vec![prompt.clone()])),
        Endpoint::Embed
        | Endpoint::CreateModelResponse
        | Endpoint::SystemOne
        | Endpoint::ImagesGenerations => None,
    }
}

/// The longest prefix of `text` holding at most `cap` UTF-16 units, cut on a
/// character boundary (a surrogate pair is kept or dropped whole).
fn utf16_prefix(text: &str, cap: usize) -> &str {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        units += c.len_utf16();
        if units > cap {
            return &text[..index];
        }
    }
    text
}

/// OpenAI `messages[]`: content is a string or an array of typed parts. Shape
/// knowledge mirrors the request transforms; a part type neither knows is
/// skipped here exactly as it is dropped there.
fn walk_openai_messages(params: &Value, acc: &mut Acc) {
    let Some(messages) = params.get("messages").and_then(Value::as_array) else {
        return;
    };
    for msg in messages {
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
                        Some("image_url") => acc.media("image", 85),
                        // Audio/video floors are 0: no provider documents a
                        // minimum billing for them, and a floor the input may
                        // undercut (an empty clip) breaks the lower-bound
                        // direction. The modality is the routable fact.
                        Some("input_audio") => acc.media("audio", 0),
                        Some("video_url") => acc.media("video", 0),
                        // No token floor: a file's cost is unknowable here and
                        // the estimate is a lower bound.
                        Some("file") => acc.media("file", 0),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                if let Some(arguments) = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    acc.estimate(arguments);
                }
            }
        }
    }
}

/// Anthropic-native shape: `system` (string or blocks) stands before
/// `messages[]`, whose blocks type differently from OpenAI parts.
fn walk_anthropic(params: &Value, acc: &mut Acc) {
    match params.get("system") {
        Some(Value::String(text)) => acc.text(text),
        Some(Value::Array(blocks)) => {
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
        match msg.get("content") {
            Some(Value::String(text)) => acc.text(text),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    anthropic_content_block(block, acc);
                }
            }
            _ => {}
        }
    }
}

/// One Anthropic content block. Split out because `tool_result.content` may
/// itself be a block array (text/image/document all legal there) — a
/// tool-returned image must set the modality like a user-sent one, or routing
/// hands the request to a text-only backend that the upstream then rejects.
fn anthropic_content_block(block: &Value, acc: &mut Acc) {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => acc.text(block.get("text").and_then(Value::as_str).unwrap_or("")),
        Some("image") => acc.media("image", 85),
        Some("document") => acc.media("file", 0),
        Some("tool_use") => {
            if let Some(input) = block.get("input") {
                if let Ok(serialized) = serde_json::to_string(input) {
                    acc.estimate(&serialized);
                }
            }
        }
        Some("tool_result") => match block.get("content") {
            Some(Value::String(text)) => acc.text(text),
            Some(Value::Array(blocks)) => {
                for inner in blocks {
                    anthropic_content_block(inner, acc);
                }
            }
            _ => {}
        },
        _ => {}
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

    fn test_key() -> PrefixHashKey {
        PrefixHashKey::new([7; 32])
    }

    fn features(params: &Value) -> RequestFeatures {
        extract(Endpoint::ChatComplete, params, None, &test_key()).unwrap()
    }

    #[test]
    fn estimate_pins_the_formula_coefficients() {
        // A FORMULA pin, not a tokenizer comparison: the estimate depends only
        // on character counts, so any 400-ASCII-char input yields the same 80
        // (repeated chars included — which real BPE would compress far below
        // that, one reason the estimate is a heuristic, not a bound). The
        // coefficient rationale — /5 under English's ~4-4.5 chars/token, /3
        // under CJK-optimised tokenizers' ~2-2.7 — lives on
        // `estimated_prompt_tokens`; validating it against real tokenizers is
        // production-sample work, not a unit test.
        let ascii = features(&chat(
            json!([{ "role": "user", "content": "a".repeat(400) }]),
        ));
        assert_eq!(ascii.estimated_prompt_tokens, 80);
        let cjk = features(&chat(
            json!([{ "role": "user", "content": "谢".repeat(400) }]),
        ));
        assert_eq!(cjk.estimated_prompt_tokens, 133);
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
    fn audio_sets_the_modality_but_never_invents_tokens() {
        // An empty clip must not carry a made-up floor — the estimate's only
        // contract is the lower-bound direction.
        let f = features(&chat(json!([{ "role": "user", "content": [
            { "type": "input_audio", "input_audio": { "data": "" } },
        ]}])));
        assert_eq!(f.input_modalities, vec!["audio"]);
        assert_eq!(f.estimated_prompt_tokens, 0);
    }

    #[test]
    fn legacy_functions_are_tools() {
        let mut params = chat(json!([{ "role": "user", "content": "hi" }]));
        params["functions"] = json!([{ "name": "lookup", "parameters": {
            "type": "object", "properties": { "q": { "type": "string",
            "description": "d".repeat(600) } } } }]);
        let f = features(&params);
        assert!(f.has_tools);
        assert!(f.estimated_prompt_tokens > 100);
    }

    #[test]
    fn tools_and_schema_count_toward_the_estimate_but_not_the_session_key() {
        let plain = chat(json!([{ "role": "user", "content": "h".repeat(500) }]));
        let mut with_tools = plain.clone();
        with_tools["tools"] = json!([{ "type": "function", "function": {
            "name": "get_weather", "parameters": { "type": "object",
            "properties": { "q": { "type": "string", "description": "x".repeat(400) } } } } }]);
        let (a, b) = (features(&plain), features(&with_tools));
        assert!(b.has_tools);
        assert!(b.estimated_prompt_tokens > a.estimated_prompt_tokens + 80);
        // The affinity key must not fracture on tool-schema edits: the
        // opening is the conversation's messages, not the tool block.
        assert_eq!(a.prefix_hash, b.prefix_hash);
    }

    #[test]
    fn inferred_key_is_the_opening_and_survives_appended_turns() {
        let system = json!({ "role": "system", "content": "be terse" });
        let turn1 = chat(json!([system, { "role": "user", "content": "first" }]));
        let turn2 = chat(json!([system, { "role": "user", "content": "first" },
            { "role": "assistant", "content": "answer" },
            { "role": "user", "content": "second" }]));
        let (a, b) = (features(&turn1), features(&turn2));
        // Short conversations key from the first turn — no length threshold —
        // and appended turns cannot move the key…
        assert_eq!(a.session_kind, Some(SessionKind::Inferred));
        assert_eq!(a.prefix_hash, b.prefix_hash);
        assert_eq!(a.prefix_hash.as_ref().unwrap().len(), 32);
        // …while a different system prompt alone lands a different key.
        let other = chat(json!([{ "role": "system", "content": "be verbose" },
            { "role": "user", "content": "first" }]));
        assert_ne!(a.prefix_hash, features(&other).prefix_hash);
        // The role is part of the opening: the same text as an assistant turn
        // is a different conversation.
        let user = features(&chat(json!([{ "role": "user", "content": "x" }])));
        let assistant = features(&chat(json!([{ "role": "assistant", "content": "x" }])));
        assert_ne!(user.prefix_hash, assistant.prefix_hash);
    }

    #[test]
    fn empty_conversation_emits_no_key() {
        let f = features(&chat(json!([])));
        assert_eq!(f.prefix_hash, None);
        assert_eq!(f.session_kind, None);
    }

    #[test]
    fn prompt_cache_key_names_the_session_and_outranks_the_opening() {
        let mut params = chat(json!([{ "role": "user", "content": "hi" }]));
        let inferred = features(&params);
        params["prompt_cache_key"] = json!("cache-1");
        assert_eq!(explicit_session(&params).as_deref(), Some("cache-1"));
        // An explicit key is keyed from the identifier, not the opening, so a
        // different conversation naming the same key shares it…
        let named = features(&params);
        let mut other = chat(json!([{ "role": "user", "content": "something else" }]));
        other["prompt_cache_key"] = json!("cache-1");
        assert_eq!(named.session_kind, Some(SessionKind::Explicit));
        assert_eq!(named.prefix_hash, features(&other).prefix_hash);
        // …and differs from the inferred key of the same opening.
        assert_ne!(named.prefix_hash, inferred.prefix_hash);
    }

    #[test]
    fn session_named_fields_are_not_read() {
        // `session_id` would read as pinning an attested session; it is not
        // a routing hint here, so the key stays inferred.
        let mut params = chat(json!([{ "role": "user", "content": "hi" }]));
        params["session_id"] = json!("sess-1");
        assert_eq!(explicit_session(&params), None);
        assert_eq!(features(&params).session_kind, Some(SessionKind::Inferred));
    }

    #[test]
    fn oversized_or_non_string_cache_keys_are_ignored() {
        let mut params = chat(json!([{ "role": "user", "content": "hi" }]));
        params["prompt_cache_key"] = json!("s".repeat(257));
        assert_eq!(explicit_session(&params), None);
        // Exactly 256 is honored; an astral character counts two units.
        params["prompt_cache_key"] = json!("s".repeat(256));
        assert!(explicit_session(&params).is_some());
        params["prompt_cache_key"] = json!("😀".repeat(128) + "s");
        assert_eq!(explicit_session(&params), None);
        params["prompt_cache_key"] = json!(42);
        assert_eq!(explicit_session(&params), None);
        params["prompt_cache_key"] = json!("");
        assert_eq!(explicit_session(&params), None);
    }

    #[test]
    fn pinned_provider_order_emits_no_key() {
        // The caller's own order wins over stickiness.
        let mut params = chat(json!([{ "role": "user", "content": "hi" }]));
        params["provider"] = json!({ "order": ["a", "b"] });
        // Even a named session does not stick against a pinned order.
        params["prompt_cache_key"] = json!("sess-1");
        let f = features(&params);
        assert_eq!(f.prefix_hash, None);
        assert_eq!(f.session_kind, None);
        // An empty order pins nothing.
        params["provider"] = json!({ "order": [] });
        assert!(features(&params).prefix_hash.is_some());
    }

    #[test]
    fn opening_is_cut_on_utf16_units() {
        // One astral character past the cap: the cut keeps the pair whole.
        let text = "a".repeat(OPENING_CAP - 1) + "😀";
        assert_eq!(
            utf16_prefix(&text, OPENING_CAP).encode_utf16().count(),
            OPENING_CAP - 1
        );
        assert_eq!(utf16_prefix("short", OPENING_CAP), "short");
        // Two openings that differ only past the cap share a key.
        let a = features(&chat(
            json!([{ "role": "user", "content": "x".repeat(20_000) + "a" }]),
        ));
        let b = features(&chat(
            json!([{ "role": "user", "content": "x".repeat(20_000) + "b" }]),
        ));
        assert_eq!(a.prefix_hash, b.prefix_hash);
    }

    #[test]
    fn secret_keys_the_hash() {
        let params = chat(json!([{ "role": "user", "content": "s".repeat(50) }]));
        let hash = |key: [u8; 32]| {
            extract(
                Endpoint::ChatComplete,
                &params,
                None,
                &PrefixHashKey::new(key),
            )
            .unwrap()
            .prefix_hash
        };
        let (k1, k2) = (hash([1; 32]), hash([2; 32]));
        // Same prefix, different keys under different secrets: without the
        // key the control plane cannot recompute or test the hash.
        assert!(k1.is_some() && k2.is_some());
        assert_ne!(k1, k2);
        // Deterministic under one key — replicas deriving it share hashes.
        assert_eq!(k1, hash([1; 32]));
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 2 pins the hand-rolled construction.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn anthropic_tool_result_blocks_carry_modalities() {
        // A tool that returns an image makes this a vision request as surely
        // as a user attaching one — text-only candidates must drop.
        let params = json!({ "model": "m", "messages": [{ "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1", "content": [
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "aGk=" } },
                { "type": "text", "text": "the chart" },
            ]},
        ]}]});
        let f = extract(Endpoint::Messages, &params, None, &test_key()).unwrap();
        assert_eq!(f.input_modalities, vec!["image", "text"]);
        assert!(f.estimated_prompt_tokens >= 85);
    }

    #[test]
    fn batch_prompts_report_the_largest_item_not_the_sum() {
        let f = |prompt: Value| {
            extract(
                Endpoint::Complete,
                &json!({ "model": "m", "prompt": prompt }),
                None,
                &test_key(),
            )
            .unwrap()
        };
        // Each batch item meets the context window alone, so the routable
        // number is the largest item: summing would wrongly rule out every
        // model that can serve the batch one prompt at a time.
        let strings = f(json!(["a".repeat(500), "b".repeat(1000)]));
        assert_eq!(strings.estimated_prompt_tokens, 200);
        assert_eq!(strings.input_modalities, vec!["text"]);
        // A flat token-id array is ONE tokenized prompt: exact count.
        assert_eq!(f(json!([1, 2, 3, 4])).estimated_prompt_tokens, 4);
        // A batch of tokenized prompts: largest again.
        assert_eq!(f(json!([[1, 2, 3], [4, 5]])).estimated_prompt_tokens, 3);
        // A batch keys too: the prompt array is the opening.
        let batch = f(json!(["a".repeat(90), "b"]));
        assert_eq!(batch.session_kind, Some(SessionKind::Inferred));
        assert_eq!(
            batch.prefix_hash,
            f(json!(["a".repeat(90), "b"])).prefix_hash
        );
        assert_ne!(batch.prefix_hash, f(json!(["a".repeat(90)])).prefix_hash);
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
            extract(Endpoint::ChatComplete, &plain, Some(&disabled), &test_key())
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
        let f = extract(Endpoint::Messages, &params, None, &test_key()).unwrap();
        assert_eq!(f.reasoning, ReasoningIntent::Disabled);
        assert_eq!(f.input_modalities, vec!["image", "text"]);
        assert!(f.estimated_prompt_tokens >= 85);
        // The opening keys from the first turn: Anthropic's top-level
        // `system` is the system entry, so changing it changes the key.
        assert_eq!(f.session_kind, Some(SessionKind::Inferred));
        let mut other = params.clone();
        other["system"] = json!("you are verbose");
        let g = extract(Endpoint::Messages, &other, None, &test_key()).unwrap();
        assert_ne!(f.prefix_hash, g.prefix_hash);
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
        assert!(extract(Endpoint::Embed, &json!({ "input": "x" }), None, &test_key()).is_none());
    }
}
