use super::wire::{E2eeAadMode, E2eeDecryptor};
use super::ServiceError;

use serde_json::{json, Value};

use super::{E2eeError, E2eeRequestContext, COMPLETIONS_PATH, EMBEDDINGS_PATH};
use crate::aci::digest;

/// Canonical JCS bytes for the AAD documents.
fn canonicalize(value: &Value) -> Result<Vec<u8>, digest::JcsError> {
    digest::jcs_bytes(value)
}
use crate::aci::e2ee::{
    encrypt_aci_e2ee_for_public_key, encrypt_legacy_for_public_key,
    normalize_secp256k1_public_key_hex, E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519,
};
use crate::aci::keys::KeyProvider;

/// E2EE v2 nonce (§7): a per-request replay token — exactly 32 bytes of CSPRNG
/// output, hex-encoded (64 hex characters, either case). The fixed width is the
/// only rule; the service cannot verify entropy.
pub(super) fn validate_aci_e2ee_nonce(nonce: &str) -> Result<(), E2eeError> {
    let is_64_hex = nonce.len() == 64 && nonce.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_64_hex {
        return Err(E2eeError::InvalidNonce);
    }
    Ok(())
}

pub(super) fn legacy_public_keys_match(
    signing_algo: &str,
    expected_hex: &str,
    supplied_hex: &str,
) -> bool {
    match signing_algo {
        E2EE_ALGO_LEGACY_ECDSA => {
            normalize_secp256k1_public_key_hex(expected_hex).is_ok_and(|expected| {
                normalize_secp256k1_public_key_hex(supplied_hex)
                    .is_ok_and(|supplied| supplied == expected)
            })
        }
        E2EE_ALGO_LEGACY_ED25519 => {
            normalize_ed25519_public_key_hex(expected_hex).is_ok_and(|expected| {
                normalize_ed25519_public_key_hex(supplied_hex)
                    .is_ok_and(|supplied| supplied == expected)
            })
        }
        _ => false,
    }
}

pub(super) fn normalize_legacy_public_key_for_replay(
    signing_algo: &str,
    value: &str,
) -> Result<String, E2eeError> {
    match signing_algo {
        E2EE_ALGO_LEGACY_ECDSA => {
            normalize_secp256k1_public_key_hex(value).map_err(|_| E2eeError::InvalidPublicKey)
        }
        E2EE_ALGO_LEGACY_ED25519 => {
            normalize_ed25519_public_key_hex(value).map_err(|_| E2eeError::InvalidPublicKey)
        }
        _ => Err(E2eeError::InvalidSigningAlgo),
    }
}

pub(super) fn normalize_ed25519_public_key_hex(value: &str) -> Result<String, E2eeError> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .map_err(|_| E2eeError::InvalidPublicKey)?;
    if bytes.len() != 32 {
        return Err(E2eeError::InvalidPublicKey);
    }
    Ok(hex::encode(bytes))
}

/// E2EE v2 model (§6): present and a string. No ambiguity check — the JCS
/// AAD needs no escaping.
pub(super) fn validate_aci_payload_model(payload: &Value) -> Result<String, E2eeError> {
    payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(E2eeError::InvalidPayloadModel)
}

const REQUEST_PURPOSE_V2: &str = "aci.e2ee.request.v2";
const RESPONSE_PURPOSE_V2: &str = "aci.e2ee.response.v2";
const REQUEST_PURPOSE_V2_1: &str = "aci.e2ee.request.v2.1";
const RESPONSE_PURPOSE_V2_1: &str = "aci.e2ee.response.v2.1";
const REQUEST_KEY_PURPOSE_V2_1: &str = "aci.e2ee.request-key.v2.1";

/// The request and response AAD purposes for an ACI mode.
fn aci_purposes(mode: E2eeAadMode) -> Result<(&'static str, &'static str), E2eeError> {
    match mode {
        E2eeAadMode::AciV2 => Ok((REQUEST_PURPOSE_V2, RESPONSE_PURPOSE_V2)),
        E2eeAadMode::AciV2_1 => Ok((REQUEST_PURPOSE_V2_1, RESPONSE_PURPOSE_V2_1)),
        E2eeAadMode::LegacyV1 => Err(E2eeError::DecryptionFailed),
    }
}

/// E2EE v2 request AAD (§6): the JCS canonicalization of the purpose-tagged
/// object. `field` is the encrypted location's field path (§5).
#[cfg(test)]
fn aci_request_aad(
    algo: &str,
    model: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
) -> Result<Vec<u8>, E2eeError> {
    request_aad(REQUEST_PURPOSE_V2, algo, model, field, nonce, timestamp)
}

fn request_aad(
    purpose: &str,
    algo: &str,
    model: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
) -> Result<Vec<u8>, E2eeError> {
    canonicalize(&json!({
        "purpose": purpose,
        "algo": algo,
        "model": model,
        "field": field,
        "nonce": nonce,
        "ts": timestamp,
    }))
    .map_err(|_| E2eeError::DecryptionFailed)
}

/// E2EE v2 response AAD (§6): like the request AAD but tagged
/// `aci.e2ee.response.v2` and additionally binding the response `id`.
#[cfg(test)]
fn aci_response_aad(
    algo: &str,
    model: &str,
    response_id: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
) -> Result<Vec<u8>, E2eeError> {
    response_aad(
        RESPONSE_PURPOSE_V2,
        algo,
        model,
        response_id,
        field,
        nonce,
        timestamp,
    )
}

fn response_aad(
    purpose: &str,
    algo: &str,
    model: &str,
    response_id: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
) -> Result<Vec<u8>, E2eeError> {
    canonicalize(&json!({
        "purpose": purpose,
        "algo": algo,
        "model": model,
        "id": response_id,
        "field": field,
        "nonce": nonce,
        "ts": timestamp,
    }))
    .map_err(|_| E2eeError::EncryptionFailed)
}

/// E2EE v2.1 `X-E2EE-Request-Key` AAD: binds the request secret to the
/// suite, model, client reply key, nonce, and timestamp.
pub(super) fn request_key_aad(
    algo: &str,
    model: &str,
    client_public_key_hex: &str,
    nonce: &str,
    timestamp: u64,
) -> Result<Vec<u8>, E2eeError> {
    canonicalize(&json!({
        "purpose": REQUEST_KEY_PURPOSE_V2_1,
        "algo": algo,
        "model": model,
        "client_pub_key": client_public_key_hex,
        "nonce": nonce,
        "ts": timestamp,
    }))
    .map_err(|_| E2eeError::DecryptionFailed)
}

/// E2EE v2.1 body digest (§10.3): SHA-256 of the RFC 8785 JCS form of the
/// request body as received, ciphertext fields in place.
pub(super) fn request_body_digest(payload: &Value) -> Result<[u8; 32], E2eeError> {
    use sha2::{Digest, Sha256};
    let bytes = serde_json_canonicalizer::to_vec(payload).map_err(|_| E2eeError::BodyMismatch)?;
    Ok(Sha256::digest(bytes).into())
}

pub(super) struct E2eeFieldCrypto<'a> {
    pub(super) keys: &'a dyn KeyProvider,
    pub(super) decryptor: E2eeDecryptor<'a>,
    pub(super) algo: &'a str,
    pub(super) aad_mode: E2eeAadMode,
    pub(super) model: &'a str,
    pub(super) nonce: Option<&'a str>,
    pub(super) timestamp: Option<u64>,
}

impl E2eeFieldCrypto<'_> {
    /// The request nonce and timestamp, required whenever the mode builds AAD.
    fn nonce_ts(&self) -> Result<(&str, u64), E2eeError> {
        let nonce = self.nonce.ok_or(E2eeError::DecryptionFailed)?;
        let timestamp = self.timestamp.ok_or(E2eeError::DecryptionFailed)?;
        Ok((nonce, timestamp))
    }
}

pub(super) fn decrypt_request_payload(
    crypto: &E2eeFieldCrypto<'_>,
    endpoint_path: &str,
    payload: &mut Value,
) -> Result<(), E2eeError> {
    if crypto.aad_mode == E2eeAadMode::AciV2_1 {
        // The request key binds the whole body, so a v2.1 request with nothing
        // to encrypt, such as a token-id prompt, is still v2.1.
        let body = payload.as_object_mut().ok_or(E2eeError::DecryptionFailed)?;
        return visit_v2_1_request(endpoint_path, body, &mut |value, field| {
            open_v2_1_value(crypto, value, field)
        });
    }
    let decrypted_count = match endpoint_path {
        COMPLETIONS_PATH => decrypt_text_items(crypto, payload, "prompt")?,
        EMBEDDINGS_PATH => decrypt_text_items(crypto, payload, "input")?,
        _ => decrypt_chat_payload(crypto, payload)?,
    };
    // v2 and legacy need one ciphertext to tell an E2EE request from a
    // plaintext one.
    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
}

fn decrypt_chat_payload(
    crypto: &E2eeFieldCrypto<'_>,
    payload: &mut Value,
) -> Result<usize, E2eeError> {
    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return Ok(0);
    };
    let mut decrypted_count = 0usize;
    for (message_index, message) in messages.iter_mut().enumerate() {
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        decrypted_count += decrypt_content_value(crypto, message_index, content)?;
    }
    Ok(decrypted_count)
}

/// `prompt` / `input`: decrypt the string, or each string element of an array.
/// Other elements, such as token ids, pass through unencrypted (§5).
fn decrypt_text_items(
    crypto: &E2eeFieldCrypto<'_>,
    payload: &mut Value,
    key: &str,
) -> Result<usize, E2eeError> {
    let decrypt = |ciphertext_hex: &mut String, field: &str| {
        let aad = request_field_aad(crypto, field)?;
        let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
        *ciphertext_hex = String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
        Ok::<_, E2eeError>(1)
    };
    match payload.get_mut(key) {
        Some(Value::String(ciphertext_hex)) => decrypt(ciphertext_hex, key),
        Some(Value::Array(items)) => {
            let mut decrypted_count = 0usize;
            for (index, item) in items.iter_mut().enumerate() {
                if let Value::String(ciphertext_hex) = item {
                    decrypted_count += decrypt(ciphertext_hex, &format!("{key}.{index}"))?;
                }
            }
            Ok(decrypted_count)
        }
        _ => Ok(0),
    }
}

pub(super) fn decrypt_content_value(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content: &mut Value,
) -> Result<usize, E2eeError> {
    match content {
        // Whole-content encryption, any modality: `messages.{m}.content`.
        Value::String(ciphertext_hex) => {
            let field = format!("messages.{message_index}.content");
            let aad = request_field_aad(crypto, &field)?;
            let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
            let plaintext =
                String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
            *content = decrypted_chat_content_value(plaintext);
            Ok(1)
        }
        Value::Array(items) => {
            let mut decrypted_count = 0usize;
            for (content_index, item) in items.iter_mut().enumerate() {
                let Some(part) = item.as_object_mut() else {
                    continue;
                };
                decrypted_count +=
                    decrypt_content_part(crypto, message_index, content_index, part)?;
            }
            Ok(decrypted_count)
        }
        _ => Ok(0),
    }
}

/// Visits one v2.1 location: a present member value and its field path.
type V2_1Visit<'a> = dyn FnMut(&mut Value, &str) -> Result<(), E2eeError> + 'a;

/// The v2.1 members of a chat message or delta whose whole value is content
/// (§10.5); they also come back as history in later requests.
const V2_1_MESSAGE_MEMBERS: [&str; 9] = [
    "content",
    "reasoning",
    "reasoning_content",
    "refusal",
    "annotations",
    "reasoning_details",
    "thinking_blocks",
    "reasoning_items",
    "images",
];

/// vLLM members that repeat prompt or generated tokens, on a choice; the
/// `prompt_*` ones also at the top of a response.
const V2_1_CHOICE_MEMBERS: [&str; 4] = [
    "logprobs",
    "token_ids",
    "prompt_token_ids",
    "prompt_logprobs",
];

fn visit_member(
    container: &mut serde_json::Map<String, Value>,
    key: &str,
    field: &str,
    visit: &mut V2_1Visit<'_>,
) -> Result<(), E2eeError> {
    match container.get_mut(key) {
        Some(value) => visit(value, field),
        None => Ok(()),
    }
}

/// Every v2.1 location of a chat message or delta under `prefix`, the same
/// set in requests (history) and responses (§10.5).
fn visit_v2_1_message(
    message: &mut serde_json::Map<String, Value>,
    prefix: &str,
    visit: &mut V2_1Visit<'_>,
) -> Result<(), E2eeError> {
    for key in V2_1_MESSAGE_MEMBERS {
        visit_member(message, key, &format!("{prefix}.{key}"), visit)?;
    }
    if let Some(Value::Object(audio)) = message.get_mut("audio") {
        for key in ["data", "transcript"] {
            visit_member(audio, key, &format!("{prefix}.audio.{key}"), visit)?;
        }
    }
    if let Some(Value::Object(function_call)) = message.get_mut("function_call") {
        let field = format!("{prefix}.function_call.arguments");
        visit_member(function_call, "arguments", &field, visit)?;
    }
    if let Some(Value::Array(calls)) = message.get_mut("tool_calls") {
        for (position, call) in calls.iter_mut().enumerate() {
            let call_index = entry_index(call, position);
            for (kind, key) in [("function", "arguments"), ("custom", "input")] {
                if let Some(Value::Object(inner)) = call.get_mut(kind) {
                    let field = format!("{prefix}.tool_calls.{call_index}.{kind}.{key}");
                    visit_member(inner, key, &field, visit)?;
                }
            }
        }
    }
    Ok(())
}

fn visit_v2_1_messages(
    body: &mut serde_json::Map<String, Value>,
    visit: &mut V2_1Visit<'_>,
) -> Result<(), E2eeError> {
    let Some(Value::Array(messages)) = body.get_mut("messages") else {
        return Ok(());
    };
    for (message_index, message) in messages.iter_mut().enumerate() {
        if let Some(message) = message.as_object_mut() {
            visit_v2_1_message(message, &format!("messages.{message_index}"), visit)?;
        }
    }
    Ok(())
}

/// Token ids, an integer or an array of integers, stay readable in `prompt`
/// and `input` (§5, §10.5).
fn is_token_ids(value: &Value) -> bool {
    match value {
        Value::Number(_) => true,
        Value::Array(items) => items.iter().all(Value::is_number),
        _ => false,
    }
}

/// `prompt` / `input`: the whole value, or each array item, unless it is
/// token ids.
fn visit_v2_1_text_items(
    body: &mut serde_json::Map<String, Value>,
    key: &str,
    visit: &mut V2_1Visit<'_>,
) -> Result<(), E2eeError> {
    match body.get_mut(key) {
        Some(Value::Array(items)) => {
            for (index, item) in items.iter_mut().enumerate() {
                if !is_token_ids(item) {
                    visit(item, &format!("{key}.{index}"))?;
                }
            }
            Ok(())
        }
        Some(value) if !is_token_ids(value) => visit(value, key),
        _ => Ok(()),
    }
}

/// Every v2.1 request location (§10.5).
fn visit_v2_1_request(
    endpoint_path: &str,
    body: &mut serde_json::Map<String, Value>,
    visit: &mut V2_1Visit<'_>,
) -> Result<(), E2eeError> {
    match endpoint_path {
        COMPLETIONS_PATH => {
            visit_v2_1_text_items(body, "prompt", visit)?;
            visit_member(body, "suffix", "suffix", visit)
        }
        EMBEDDINGS_PATH => {
            visit_v2_1_text_items(body, "input", visit)?;
            visit_v2_1_messages(body, visit)
        }
        _ => {
            if let Some(Value::Object(prediction)) = body.get_mut("prediction") {
                visit_member(prediction, "content", "prediction.content", visit)?;
            }
            visit_v2_1_messages(body, visit)
        }
    }
}

/// Every v2.1 response location of one response object (§5, §10.5). A chat
/// choice's `message` and `delta` are both visited whenever present, buffered
/// or streamed.
fn visit_v2_1_response(
    endpoint_path: &str,
    payload: &mut serde_json::Map<String, Value>,
    visit: &mut V2_1Visit<'_>,
) -> Result<(), E2eeError> {
    for key in ["prompt_token_ids", "prompt_logprobs"] {
        visit_member(payload, key, key, visit)?;
    }
    let list = if endpoint_path == EMBEDDINGS_PATH {
        "data"
    } else {
        "choices"
    };
    let Some(Value::Array(entries)) = payload.get_mut(list) else {
        return Ok(());
    };
    for (position, entry) in entries.iter_mut().enumerate() {
        let index = entry_index(entry, position);
        let Some(entry) = entry.as_object_mut() else {
            continue;
        };
        let prefix = format!("{list}.{index}");
        if endpoint_path == EMBEDDINGS_PATH {
            visit_member(entry, "embedding", &format!("{prefix}.embedding"), visit)?;
            continue;
        }
        for key in V2_1_CHOICE_MEMBERS {
            visit_member(entry, key, &format!("{prefix}.{key}"), visit)?;
        }
        if endpoint_path == COMPLETIONS_PATH {
            visit_member(entry, "text", &format!("{prefix}.text"), visit)?;
            continue;
        }
        for part in ["message", "delta"] {
            if let Some(Value::Object(message)) = entry.get_mut(part) {
                visit_v2_1_message(message, &format!("{prefix}.{part}"), visit)?;
            }
        }
    }
    Ok(())
}

/// Open one v2.1 request location: the plaintext is the JSON encoding of the
/// value (§10.5). Any present value but a ciphertext string or `null` is
/// content in clear and fails.
fn open_v2_1_value(
    crypto: &E2eeFieldCrypto<'_>,
    value: &mut Value,
    field: &str,
) -> Result<(), E2eeError> {
    let ciphertext_hex = match value {
        Value::Null => return Ok(()),
        Value::String(ciphertext_hex) => ciphertext_hex,
        _ => return Err(E2eeError::DecryptionFailed),
    };
    let aad = request_field_aad(crypto, field)?;
    let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
    *value = serde_json::from_slice(&plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
    Ok(())
}

/// Seal one v2.1 response location as the compact JSON of its value, whatever
/// its type (§10.5). `null` stays as it is.
fn seal_v2_1_value(
    ctx: &E2eeRequestContext,
    response_id: &str,
    value: &mut Value,
    field: &str,
) -> Result<(), E2eeError> {
    if value.is_null() {
        return Ok(());
    }
    let plaintext = serde_json::to_vec(value).map_err(|_| E2eeError::EncryptionFailed)?;
    let aad = response_aad_for_context(ctx, response_id, field)?;
    *value = Value::String(encrypt_response_plaintext(ctx, &plaintext, aad.as_deref())?);
    Ok(())
}

/// Seal every v2.1 location of one response object and add its MAC.
fn seal_v2_1_response(
    payload: &mut Value,
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
    seq: Option<u64>,
) -> Result<(), E2eeError> {
    let object = payload.as_object_mut().ok_or(E2eeError::EncryptionFailed)?;
    let response_id = object
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    visit_v2_1_response(endpoint_path, object, &mut |value, field| {
        seal_v2_1_value(ctx, &response_id, value, field)
    })?;
    seal_response_object(payload, ctx, seq)
}

/// An array entry's path index: its `index` member when it is a non-negative
/// integer up to 2^53 - 1, its array position otherwise (§10.5). An integral
/// float such as `1.0` counts as that integer, as it does for a JavaScript
/// client, whose `JSON.parse` cannot tell `1` from `1.0` and cannot hold a
/// larger integer exactly.
fn entry_index(entry: &Value, position: usize) -> u64 {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    entry
        .get("index")
        .and_then(Value::as_f64)
        .filter(|value| value.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(value))
        .map_or(position as u64, |value| value as u64)
}

/// Decrypt one structured content part in place. `text` parts are decrypted in
/// every mode; the per-part `image_url.url` and `input_audio.data` locations
/// (E2EE v2 §5) are extension-only — the legacy compatibility modes never defined field
/// paths for them, so there they pass through unchanged.
fn decrypt_content_part(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content_index: usize,
    part: &mut serde_json::Map<String, Value>,
) -> Result<usize, E2eeError> {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => {
            let field = format!("messages.{message_index}.content.{content_index}.text");
            decrypt_string_member(crypto, part, "text", &field)
        }
        Some("image_url") if crypto.aad_mode.is_aci() => {
            let field = format!("messages.{message_index}.content.{content_index}.image_url.url");
            decrypt_content_part_nested(crypto, part, "image_url", "url", &field)
        }
        Some("input_audio") if crypto.aad_mode.is_aci() => {
            let field =
                format!("messages.{message_index}.content.{content_index}.input_audio.data");
            decrypt_content_part_nested(crypto, part, "input_audio", "data", &field)
        }
        _ => Ok(0),
    }
}

/// Decrypt a content part's `text` in place. A null or non-string value
/// passes through.
fn decrypt_string_member(
    crypto: &E2eeFieldCrypto<'_>,
    part: &mut serde_json::Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<usize, E2eeError> {
    let Some(Value::String(ciphertext_hex)) = part.get_mut(key) else {
        return Ok(0);
    };
    let aad = request_field_aad(crypto, field)?;
    let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
    *ciphertext_hex = String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
    Ok(1)
}

/// Decrypt a nested string member (`image_url.url`, `input_audio.data`) of a
/// content part.
fn decrypt_content_part_nested(
    crypto: &E2eeFieldCrypto<'_>,
    part: &mut serde_json::Map<String, Value>,
    wrapper_key: &str,
    inner_key: &str,
    field: &str,
) -> Result<usize, E2eeError> {
    let Some(Value::Object(wrapper)) = part.get_mut(wrapper_key) else {
        return Ok(0);
    };
    let Some(Value::String(ciphertext_hex)) = wrapper.get_mut(inner_key) else {
        return Ok(0);
    };
    let aad = request_field_aad(crypto, field)?;
    let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
    *ciphertext_hex = String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
    Ok(1)
}

/// AAD for an E2EE v2 request field (§6): the JCS canonicalization bound to the
/// field path (§5), and none on the legacy (no-AAD) path.
fn request_field_aad(
    crypto: &E2eeFieldCrypto<'_>,
    field: &str,
) -> Result<Option<Vec<u8>>, E2eeError> {
    match crypto.aad_mode {
        E2eeAadMode::AciV2 | E2eeAadMode::AciV2_1 => {
            let (purpose, _) = aci_purposes(crypto.aad_mode)?;
            let (nonce, timestamp) = crypto.nonce_ts()?;
            Ok(Some(request_aad(
                purpose,
                crypto.algo,
                crypto.model,
                field,
                nonce,
                timestamp,
            )?))
        }
        E2eeAadMode::LegacyV1 => Ok(None),
    }
}

pub(super) fn decrypt_e2ee_field(
    crypto: &E2eeFieldCrypto<'_>,
    ciphertext_hex: &str,
    aad: Option<&[u8]>,
) -> Result<Vec<u8>, E2eeError> {
    match crypto.decryptor {
        E2eeDecryptor::Aci { key_id, kdf } => {
            let aad = aad.ok_or(E2eeError::DecryptionFailed)?;
            crypto
                .keys
                .decrypt_e2ee(key_id, ciphertext_hex, aad, kdf)
                .map_err(|_| E2eeError::DecryptionFailed)
        }
        E2eeDecryptor::Legacy { signing_algo } => crypto
            .keys
            .decrypt_legacy_e2ee(signing_algo, ciphertext_hex, aad)
            .map_err(|_| E2eeError::DecryptionFailed),
    }
}

pub(super) fn decrypted_chat_content_value(plaintext: String) -> Value {
    match serde_json::from_str::<Value>(&plaintext) {
        Ok(Value::Array(items)) => Value::Array(items),
        _ => Value::String(plaintext),
    }
}

pub(super) fn encrypt_e2ee_response_body(
    cleartext_body: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Vec<u8>, E2eeError> {
    let mut payload: Value =
        serde_json::from_slice(cleartext_body).map_err(|_| E2eeError::EncryptionFailed)?;
    if ctx.aad_mode == E2eeAadMode::AciV2_1 {
        seal_v2_1_response(&mut payload, ctx, endpoint_path, None)?;
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    }
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if endpoint_path == EMBEDDINGS_PATH {
        encrypt_embedding_data(&mut payload, ctx, &response_id)?;
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    }

    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    };

    for (position, choice) in choices.iter_mut().enumerate() {
        let choice_index = choice
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(position as u64);
        let Some(choice) = choice.as_object_mut() else {
            continue;
        };
        if endpoint_path == COMPLETIONS_PATH {
            encrypt_response_field(
                choice,
                "text",
                &format!("choices.{choice_index}.text"),
                ctx,
                &response_id,
            )?;
        } else if let Some(Value::Object(message)) = choice.get_mut("message") {
            encrypt_response_field(
                message,
                "content",
                &format!("choices.{choice_index}.message.content"),
                ctx,
                &response_id,
            )?;
            encrypt_response_field(
                message,
                "reasoning",
                &format!("choices.{choice_index}.message.reasoning"),
                ctx,
                &response_id,
            )?;
            encrypt_response_field(
                message,
                "reasoning_content",
                &format!("choices.{choice_index}.message.reasoning_content"),
                ctx,
                &response_id,
            )?;
            // Response audio (`message.audio.data`, E2EE v2 §5) is extension-only.
            if ctx.aad_mode.is_aci() {
                encrypt_message_audio_data(message, ctx, &response_id, choice_index)?;
            }
        }
    }

    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

/// The member carrying the v2.1 response MAC.
pub(super) const RESPONSE_MAC_MEMBER: &str = "e2ee_mac";
/// The member carrying a v2.1 stream event's position, from 0.
pub(super) const STREAM_SEQ_MEMBER: &str = "e2ee_seq";

/// v2.1: add an HMAC over the JCS form of a response object (a buffered body
/// or one SSE event), keyed from the request secret, so its readable members
/// cannot be changed either. A stream event first gets its position `seq`,
/// so the MAC also fixes the event order.
fn seal_response_object(
    payload: &mut Value,
    ctx: &E2eeRequestContext,
    seq: Option<u64>,
) -> Result<(), E2eeError> {
    let Some(secret) = &ctx.request_secret else {
        return Ok(());
    };
    let object = payload.as_object_mut().ok_or(E2eeError::EncryptionFailed)?;
    object.remove(RESPONSE_MAC_MEMBER);
    object.remove(STREAM_SEQ_MEMBER);
    if let Some(seq) = seq {
        object.insert(STREAM_SEQ_MEMBER.to_string(), Value::from(seq));
    }
    let bytes =
        serde_json_canonicalizer::to_vec(&*object).map_err(|_| E2eeError::EncryptionFailed)?;
    let mac = crate::aci::e2ee::v2_1_response_mac(&secret.0, &bytes);
    object.insert(
        RESPONSE_MAC_MEMBER.to_string(),
        Value::String(hex::encode(mac)),
    );
    Ok(())
}

pub(super) fn encrypt_embedding_data(
    payload: &mut Value,
    ctx: &E2eeRequestContext,
    response_id: &str,
) -> Result<(), E2eeError> {
    let Some(items) = payload.get_mut("data").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (position, item) in items.iter_mut().enumerate() {
        let data_index = item
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(position as u64);
        let Some(entry) = item.as_object_mut() else {
            continue;
        };
        let Some(embedding) = entry.get_mut("embedding") else {
            continue;
        };
        // OpenAI emits `embedding` as a float array by default and as a
        // base64 string when the client passes `encoding_format=base64`.
        // We serialize to compact JSON before encryption so the
        // decrypted plaintext round-trips through `serde_json` back to
        // the original type, mirroring how chat content arrays are
        // recovered.
        let plaintext = serde_json::to_vec(embedding).map_err(|_| E2eeError::EncryptionFailed)?;
        let aad = embedding_response_aad_for_context(ctx, response_id, data_index)?;
        let ciphertext_hex = encrypt_response_plaintext(ctx, &plaintext, aad.as_deref())?;
        *embedding = Value::String(ciphertext_hex);
    }
    Ok(())
}

fn embedding_response_aad_for_context(
    ctx: &E2eeRequestContext,
    response_id: &str,
    data_index: u64,
) -> Result<Option<Vec<u8>>, E2eeError> {
    match ctx.aad_mode {
        E2eeAadMode::AciV2 | E2eeAadMode::AciV2_1 => {
            let (_, purpose) = aci_purposes(ctx.aad_mode)?;
            let (nonce, timestamp) = response_nonce_ts(ctx)?;
            let field = format!("data.{data_index}.embedding");
            Ok(Some(response_aad(
                purpose,
                &ctx.algo,
                &ctx.request_model,
                response_id,
                &field,
                nonce,
                timestamp,
            )?))
        }
        E2eeAadMode::LegacyV1 => Ok(None),
    }
}

pub(super) fn encrypt_e2ee_final_response(
    cleartext_body: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
    is_sse: bool,
) -> Result<Vec<u8>, ServiceError> {
    if !is_sse {
        return encrypt_e2ee_response_body(cleartext_body, ctx, endpoint_path).map_err(Into::into);
    }
    super::streaming::finalize_sse_body(cleartext_body, Some(ctx), endpoint_path)
        .map(|(bytes, _)| bytes)
}

pub fn is_sse_content_type(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// Encrypt one SSE record. `seq` counts the JSON events of this stream: the
/// next one's position, advanced for each event encrypted.
pub(super) fn encrypt_e2ee_record(
    record: &mut crate::sse::Record,
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
    seq: &mut u64,
) -> Result<(), E2eeError> {
    let Some(event) = record.event() else {
        return Ok(());
    };
    if event.meaning.kind == crate::sse_protocol::EventKind::Done {
        return Ok(());
    }
    let Some(event) = record.event_mut() else {
        return Ok(());
    };
    let crate::sse::Payload::Json(value) = &mut event.payload else {
        return Err(E2eeError::EncryptionFailed);
    };
    let value = value.take();
    record.set_encrypted(encrypt_e2ee_stream_value(value, ctx, endpoint_path, *seq)?);
    *seq += 1;
    Ok(())
}

pub(super) fn encrypt_e2ee_stream_value(
    mut payload: Value,
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
    seq: u64,
) -> Result<Value, E2eeError> {
    if endpoint_path == EMBEDDINGS_PATH {
        // Embeddings are buffered-only. A streaming embedding is an internal
        // inconsistency: fail closed rather than send unencrypted vectors.
        return Err(E2eeError::EncryptionFailed);
    }
    if ctx.aad_mode == E2eeAadMode::AciV2_1 {
        drop_empty_delta_content(&mut payload);
        seal_v2_1_response(&mut payload, ctx, endpoint_path, Some(seq))?;
        return Ok(payload);
    }
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return Ok(payload);
    };

    for (position, choice) in choices.iter_mut().enumerate() {
        let choice_index = choice
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(position as u64);
        let Some(choice) = choice.as_object_mut() else {
            continue;
        };
        if endpoint_path == COMPLETIONS_PATH {
            encrypt_response_field(
                choice,
                "text",
                &format!("choices.{choice_index}.text"),
                ctx,
                &response_id,
            )?;
        } else if let Some(Value::Object(delta)) = choice.get_mut("delta") {
            if delta.get("content").and_then(Value::as_str) == Some("") {
                delta.remove("content");
            }
            encrypt_response_field(
                delta,
                "content",
                &format!("choices.{choice_index}.delta.content"),
                ctx,
                &response_id,
            )?;
            encrypt_response_field(
                delta,
                "reasoning",
                &format!("choices.{choice_index}.delta.reasoning"),
                ctx,
                &response_id,
            )?;
            encrypt_response_field(
                delta,
                "reasoning_content",
                &format!("choices.{choice_index}.delta.reasoning_content"),
                ctx,
                &response_id,
            )?;
        }
    }

    Ok(payload)
}

/// An empty streamed `delta.content` carries nothing; drop it rather than
/// encrypt it.
fn drop_empty_delta_content(payload: &mut Value) {
    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for choice in choices {
        if let Some(Value::Object(delta)) = choice.get_mut("delta") {
            if delta.get("content").and_then(Value::as_str) == Some("") {
                delta.remove("content");
            }
        }
    }
}

pub(super) fn encrypt_response_field(
    container: &mut serde_json::Map<String, Value>,
    field_name: &str,
    aci_field: &str,
    ctx: &E2eeRequestContext,
    response_id: &str,
) -> Result<(), E2eeError> {
    let Some(value) = container.get_mut(field_name) else {
        return Ok(());
    };
    let Some(plaintext) = optional_response_string(value)? else {
        return Ok(());
    };
    let aad = response_aad_for_context(ctx, response_id, aci_field)?;
    *plaintext = encrypt_response_plaintext(ctx, plaintext.as_bytes(), aad.as_deref())?;
    Ok(())
}

fn optional_response_string(value: &mut Value) -> Result<Option<&mut String>, E2eeError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value)),
        _ => Err(E2eeError::EncryptionFailed),
    }
}

/// Encrypt `message.audio.data` in place under the `choices.{i}.message.audio.data`
/// field path. ACI-only; the caller gates on [`E2eeAadMode::is_aci`].
fn encrypt_message_audio_data(
    message: &mut serde_json::Map<String, Value>,
    ctx: &E2eeRequestContext,
    response_id: &str,
    choice_index: u64,
) -> Result<(), E2eeError> {
    let Some(Value::Object(audio)) = message.get_mut("audio") else {
        return Ok(());
    };
    let Some(Value::String(plaintext)) = audio.get_mut("data") else {
        return Ok(());
    };
    let (_, purpose) = aci_purposes(ctx.aad_mode)?;
    let (nonce, timestamp) = response_nonce_ts(ctx)?;
    let field = format!("choices.{choice_index}.message.audio.data");
    let aad = response_aad(
        purpose,
        &ctx.algo,
        &ctx.request_model,
        response_id,
        &field,
        nonce,
        timestamp,
    )?;
    *plaintext = encrypt_response_plaintext(ctx, plaintext.as_bytes(), Some(aad.as_slice()))?;
    Ok(())
}

fn response_aad_for_context(
    ctx: &E2eeRequestContext,
    response_id: &str,
    aci_field: &str,
) -> Result<Option<Vec<u8>>, E2eeError> {
    match ctx.aad_mode {
        E2eeAadMode::AciV2 | E2eeAadMode::AciV2_1 => {
            let (_, purpose) = aci_purposes(ctx.aad_mode)?;
            let (nonce, timestamp) = response_nonce_ts(ctx)?;
            Ok(Some(response_aad(
                purpose,
                &ctx.algo,
                &ctx.request_model,
                response_id,
                aci_field,
                nonce,
                timestamp,
            )?))
        }
        E2eeAadMode::LegacyV1 => Ok(None),
    }
}

/// The request nonce and timestamp carried into the response AAD.
fn response_nonce_ts(ctx: &E2eeRequestContext) -> Result<(&str, u64), E2eeError> {
    let nonce = ctx.nonce.as_deref().ok_or(E2eeError::EncryptionFailed)?;
    let timestamp = ctx.timestamp.ok_or(E2eeError::EncryptionFailed)?;
    Ok((nonce, timestamp))
}

pub(super) fn encrypt_response_plaintext(
    ctx: &E2eeRequestContext,
    plaintext: &[u8],
    aad: Option<&[u8]>,
) -> Result<String, E2eeError> {
    match ctx.aad_mode {
        E2eeAadMode::AciV2 | E2eeAadMode::AciV2_1 => {
            // The response suite is the request's selected service E2EE key
            // algo; `client_public_key_hex` is already normalized for it.
            let aad = aad.ok_or(E2eeError::EncryptionFailed)?;
            encrypt_aci_e2ee_for_public_key(
                &ctx.algo,
                &ctx.client_public_key_hex,
                plaintext,
                aad,
                ctx.response_kdf()?,
            )
            .map_err(|_| E2eeError::EncryptionFailed)
        }
        E2eeAadMode::LegacyV1 => {
            encrypt_legacy_for_public_key(&ctx.algo, &ctx.client_public_key_hex, plaintext, aad)
                .map_err(|_| E2eeError::EncryptionFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{aci_request_aad, aci_response_aad, entry_index, optional_response_string};
    use serde_json::json;

    #[test]
    fn entry_index_reads_an_integral_index_as_a_javascript_client_does() {
        let index = |entry: serde_json::Value| entry_index(&entry, 7);
        assert_eq!(index(json!({"index": 1})), 1);
        assert_eq!(index(json!({"index": 1.0})), 1);
        assert_eq!(
            index(json!({"index": 9_007_199_254_740_991.0_f64})),
            9_007_199_254_740_991
        );
        assert_eq!(index(json!({})), 7);
        for not_an_index in [
            json!(1.5),
            json!(-1),
            json!(-1.0),
            json!(9_007_199_254_740_992.0_f64),
            json!(9_007_199_254_740_993_u64),
            json!("1"),
        ] {
            assert_eq!(index(json!({"index": not_an_index})), 7);
        }
    }

    // Byte-exact expected AAD from spec/test-vectors.md §5 (X25519 suite).
    const NONCE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const REQUEST_AAD: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.request.v2","ts":1750000000}"#;
    const RESPONSE_AAD: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.response.v2","ts":1750000000}"#;

    #[test]
    fn request_aad_matches_spec_test_vector() {
        let aad = aci_request_aad(
            "x25519-aes-256-gcm-hkdf-sha256",
            "demo-model",
            "messages.0.content",
            NONCE,
            1_750_000_000,
        )
        .unwrap();
        assert_eq!(aad, REQUEST_AAD.as_bytes());
    }

    #[test]
    fn response_aad_matches_spec_test_vector() {
        let aad = aci_response_aad(
            "x25519-aes-256-gcm-hkdf-sha256",
            "demo-model",
            "chatcmpl-123",
            "choices.0.message.content",
            NONCE,
            1_750_000_000,
        )
        .unwrap();
        assert_eq!(aad, RESPONSE_AAD.as_bytes());
    }

    #[test]
    fn response_fields_reject_non_string_values() {
        assert!(optional_response_string(&mut json!({"text": "secret"})).is_err());
    }

    #[test]
    fn nonce_validation_requires_64_hex() {
        assert!(super::validate_aci_e2ee_nonce(NONCE).is_ok());
        // Either case is accepted (E2EE v2 §7).
        assert!(super::validate_aci_e2ee_nonce(&"A".repeat(64)).is_ok());
        assert!(super::validate_aci_e2ee_nonce("").is_err());
        assert!(super::validate_aci_e2ee_nonce("nonce-1").is_err());
        assert!(super::validate_aci_e2ee_nonce(&"a".repeat(63)).is_err());
        assert!(super::validate_aci_e2ee_nonce(&"a".repeat(65)).is_err());
        // Right length, non-hex character.
        assert!(super::validate_aci_e2ee_nonce(&format!("{}g", "0".repeat(63))).is_err());
    }
}
