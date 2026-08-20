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
    /// The cache-affinity key: hex SHA-256 — or HMAC-SHA256 when
    /// `middleware.prefix_hash_secret` is set — of the canonical first bytes
    /// of the conversation, truncated to 32 chars (`hash_api_key` style).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_hash: Option<String>,
}

/// Walk accumulator: token-estimate tallies, seen modalities, and the capped
/// canonical prefix. The estimate keeps counting after the prefix is full.
#[derive(Default)]
struct Acc {
    ascii_bytes: u64,
    non_ascii_chars: u64,
    /// Tokens counted directly rather than estimated from text: media floors
    /// with a documented basis, and literal token-id arrays (exact).
    extra_tokens: u64,
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
        self.extra_tokens += floor_tokens;
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
        self.ascii_bytes / 5 + self.non_ascii_chars / 3 + self.extra_tokens
    }

    /// The affinity key, or `None` when there is nothing worth keying.
    ///
    /// Emitted only when the canonical prefix filled the whole cap. Below it
    /// the "prefix" is the entire conversation so far, which means two things
    /// at once: the key would change on every appended turn (a single-shot
    /// Redis entry that can never stick), and a conversation under ~1k tokens
    /// is below the smallest prefix providers cache anyway. Dropping those
    /// also removes the one real dictionary target — a complete short prefix
    /// like `user\0hi` is trivially enumerable; a full 4KB one is only
    /// guessable when the guesser already holds every byte of it.
    ///
    /// With a secret the digest is HMAC-SHA256 keyed inside the gateway, so
    /// the control plane cannot even test guesses of fully-known templates;
    /// without one it is plain SHA-256, which keeps prefix equality linkable
    /// and known 4KB templates confirmable (stated in the config docs).
    fn prefix_hash(&self, secret: Option<&[u8]>) -> Option<String> {
        if self.prefix.len() < PREFIX_CAP {
            return None;
        }
        let digest: [u8; 32] = match secret {
            Some(key) => hmac_sha256(key, &self.prefix),
            None => Sha256::digest(&self.prefix).into(),
        };
        Some(hex::encode(digest)[..32].to_string())
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
    prefix_secret: Option<&[u8]>,
) -> Option<RequestFeatures> {
    let mut acc = Acc::default();
    match endpoint {
        Endpoint::ChatComplete => walk_openai_messages(params, &mut acc),
        Endpoint::Messages => walk_anthropic(params, &mut acc),
        Endpoint::Complete => match params.get("prompt") {
            Some(Value::String(prompt)) => {
                acc.begin_message("user");
                acc.text(prompt);
            }
            // Legacy completions also allow arrays: a batch of string
            // prompts, ONE tokenized prompt (a flat token-id array — exact
            // count, better than any estimate), or a batch of tokenized
            // prompts. Each batch item runs against the model's context
            // window ON ITS OWN, so the routable number is the LARGEST item,
            // not the sum — summing 100 prompts of 1k tokens each would read
            // as 100k and wrongly rule out every model that could serve them.
            // No prefix either: a batch is not a conversation, so there is
            // nothing for cache affinity to key.
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
        Endpoint::Embed | Endpoint::CreateModelResponse => return None,
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
                    acc.text_outside_prefix(&serialized);
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
        prefix_hash: acc.prefix_hash(prefix_secret),
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
                        // Audio/video floors are 0: no provider documents a
                        // minimum billing for them, and a floor the input may
                        // undercut (an empty clip) breaks the lower-bound
                        // direction. The modality is the routable fact.
                        Some("input_audio") => acc.media(
                            "audio",
                            0,
                            part.get("input_audio")
                                .and_then(|a| a.get("data"))
                                .and_then(Value::as_str),
                        ),
                        Some("video_url") => acc.media(
                            "video",
                            0,
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
        // Same reasoning as anthropic tool_use: arguments are prompt tokens
        // (estimate), not affinity identity (prefix).
        if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                if let Some(arguments) = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    acc.text_outside_prefix(arguments);
                }
            }
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
        // Tool inputs are real prompt tokens, so they feed the estimate — but
        // deliberately NOT the prefix: the affinity key trades distinctiveness
        // for stability, and two conversations differing only in tool history
        // sharing a routing preference costs nothing.
        Some("tool_use") => {
            if let Some(input) = block.get("input") {
                if let Ok(serialized) = serde_json::to_string(input) {
                    acc.text_outside_prefix(&serialized);
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

    fn features(params: &Value) -> RequestFeatures {
        extract(Endpoint::ChatComplete, params, None, None).unwrap()
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
    fn tools_and_schema_count_toward_the_estimate_but_not_the_prefix() {
        // Long enough to fill the prefix cap, so the hash comparison below is
        // between real keys, not two Nones.
        let plain = chat(json!([{ "role": "user", "content": "h".repeat(5000) }]));
        let mut with_tools = plain.clone();
        with_tools["tools"] = json!([{ "type": "function", "function": {
            "name": "get_weather", "parameters": { "type": "object",
            "properties": { "q": { "type": "string", "description": "x".repeat(400) } } } } }]);
        let (a, b) = (features(&plain), features(&with_tools));
        assert!(b.has_tools);
        assert!(b.estimated_prompt_tokens > a.estimated_prompt_tokens + 80);
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
    fn sub_cap_prefixes_emit_no_hash() {
        // A conversation that has not filled the cap is both worthless to key
        // (the key would change every turn, and it is under the smallest
        // prefix providers cache) and the one dictionary-guessable case —
        // `user\0hi` is enumerable, a full 4KB prefix is not.
        assert_eq!(
            features(&chat(json!([{ "role": "user", "content": "hi" }]))).prefix_hash,
            None
        );
        assert_eq!(features(&chat(json!([]))).prefix_hash, None);
    }

    #[test]
    fn full_prefixes_distinguish_by_role_and_text() {
        let long = "x".repeat(5000);
        let user = features(&chat(json!([{ "role": "user", "content": long }])));
        let assistant = features(&chat(
            json!([{ "role": "assistant", "content": "x".repeat(5000) }]),
        ));
        assert_ne!(user.prefix_hash, assistant.prefix_hash);
        assert_eq!(user.prefix_hash.as_ref().unwrap().len(), 32);
    }

    #[test]
    fn secret_keys_the_hash() {
        let params = chat(json!([{ "role": "user", "content": "s".repeat(5000) }]));
        let hash = |secret: Option<&[u8]>| {
            extract(Endpoint::ChatComplete, &params, None, secret)
                .unwrap()
                .prefix_hash
        };
        let (plain, k1, k2) = (hash(None), hash(Some(b"k1")), hash(Some(b"k2")));
        // Same prefix, three different keys under three different secrets:
        // without the secret the control plane could recompute `plain`; with
        // one it cannot test guesses at all.
        assert!(plain.is_some() && k1.is_some() && k2.is_some());
        assert_ne!(plain, k1);
        assert_ne!(k1, k2);
        // Deterministic under one secret — replicas sharing it share keys.
        assert_eq!(k1, hash(Some(b"k1")));
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
        let f = extract(Endpoint::Messages, &params, None, None).unwrap();
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
                None,
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
        // Not a conversation — nothing for affinity to key.
        assert_eq!(f(json!(["a".repeat(9000)])).prefix_hash, None);
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
            extract(Endpoint::ChatComplete, &plain, Some(&disabled), None)
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
        let f = extract(Endpoint::Messages, &params, None, None).unwrap();
        assert_eq!(f.reasoning, ReasoningIntent::Disabled);
        assert_eq!(f.input_modalities, vec!["image", "text"]);
        assert!(f.estimated_prompt_tokens >= 85);
        // Short conversation: below the cap, so no affinity key (see
        // sub_cap_prefixes_emit_no_hash).
        assert!(f.prefix_hash.is_none());
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
        assert!(extract(Endpoint::Embed, &json!({ "input": "x" }), None, None).is_none());
    }
}
