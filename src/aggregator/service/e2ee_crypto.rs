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

/// E2EE v2.1 body digest: SHA-256 of the RFC 8785 JCS form of the request
/// body as received, ciphertext fields in place. The body must be I-JSON:
/// JCS writes numbers as doubles, so an integer beyond ±2^53 would hash like
/// its neighbours while its exact digits are forwarded.
pub(super) fn request_body_digest(payload: &Value) -> Result<[u8; 32], E2eeError> {
    use sha2::{Digest, Sha256};
    if !is_i_json_numbers(payload) {
        return Err(E2eeError::BodyMismatch);
    }
    let bytes = serde_json_canonicalizer::to_vec(payload).map_err(|_| E2eeError::BodyMismatch)?;
    Ok(Sha256::digest(bytes).into())
}

fn is_i_json_numbers(value: &Value) -> bool {
    const MAX_SAFE: u64 = (1 << 53) - 1;
    match value {
        Value::Number(n) => {
            n.as_u64().is_none_or(|v| v <= MAX_SAFE)
                && n.as_i64().is_none_or(|v| v.unsigned_abs() <= MAX_SAFE)
        }
        Value::Array(items) => items.iter().all(is_i_json_numbers),
        Value::Object(map) => map.values().all(is_i_json_numbers),
        _ => true,
    }
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
    if endpoint_path == COMPLETIONS_PATH {
        return decrypt_completion_prompt(crypto, payload);
    }
    if endpoint_path == EMBEDDINGS_PATH {
        return decrypt_embedding_input(crypto, payload);
    }

    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return Err(E2eeError::DecryptionFailed);
    };
    let mut decrypted_count = 0usize;
    for (message_index, message) in messages.iter_mut().enumerate() {
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        if crypto.aad_mode == E2eeAadMode::AciV2_1 {
            for key in ["reasoning", "reasoning_content"] {
                let field = format!("messages.{message_index}.{key}");
                decrypted_count += decrypt_content_part_field(crypto, message, key, &field)?;
            }
            decrypted_count += decrypt_tool_call_arguments(crypto, message_index, message)?;
        }
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        decrypted_count += decrypt_content_value(crypto, message_index, content)?;
    }

    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
}

pub(super) fn decrypt_completion_prompt(
    crypto: &E2eeFieldCrypto<'_>,
    payload: &mut Value,
) -> Result<(), E2eeError> {
    let Some(prompt) = payload.get_mut("prompt") else {
        return Err(E2eeError::DecryptionFailed);
    };

    let decrypted_count = match prompt {
        Value::String(ciphertext_hex) => {
            let aad = request_field_aad(crypto, "prompt")?;
            let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
            *ciphertext_hex =
                String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
            1
        }
        Value::Array(items) => {
            let mut decrypted_count = 0usize;
            for (index, item) in items.iter_mut().enumerate() {
                let Value::String(ciphertext_hex) = item else {
                    continue;
                };
                let field_name = format!("prompt.{index}");
                let aad = request_field_aad(crypto, &field_name)?;
                let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
                *ciphertext_hex =
                    String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
                decrypted_count += 1;
            }
            decrypted_count
        }
        _ => 0,
    };

    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
}

pub(super) fn decrypt_embedding_input(
    crypto: &E2eeFieldCrypto<'_>,
    payload: &mut Value,
) -> Result<(), E2eeError> {
    let Some(input) = payload.get_mut("input") else {
        return Err(E2eeError::DecryptionFailed);
    };

    let decrypted_count = match input {
        Value::String(ciphertext_hex) => {
            let aad = request_field_aad(crypto, "input")?;
            let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
            *ciphertext_hex =
                String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
            1
        }
        Value::Array(items) => {
            // OpenAI accepts string arrays AND integer token-id arrays
            // for `input`. Only encrypted strings carry E2EE field
            // ciphertext; numeric arrays pass through.
            let mut decrypted_count = 0usize;
            for (index, item) in items.iter_mut().enumerate() {
                let Value::String(ciphertext_hex) = item else {
                    continue;
                };
                let field_name = format!("input.{index}");
                let aad = request_field_aad(crypto, &field_name)?;
                let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
                *ciphertext_hex =
                    String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
                decrypted_count += 1;
            }
            decrypted_count
        }
        _ => 0,
    };

    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
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
            *content = match crypto.aad_mode {
                E2eeAadMode::AciV2_1 => v2_1_content_value(&plaintext)?,
                _ => decrypted_chat_content_value(plaintext),
            };
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

/// v2.1: decrypt `messages.{m}.tool_calls.{t}.function.arguments` in place.
fn decrypt_tool_call_arguments(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    message: &mut serde_json::Map<String, Value>,
) -> Result<usize, E2eeError> {
    let Some(Value::Array(calls)) = message.get_mut("tool_calls") else {
        return Ok(0);
    };
    let mut decrypted_count = 0usize;
    for (position, call) in calls.iter_mut().enumerate() {
        let call_index = entry_index(call, position);
        let Some(Value::String(ciphertext_hex)) = call
            .get_mut("function")
            .and_then(|function| function.get_mut("arguments"))
        else {
            continue;
        };
        let field = format!("messages.{message_index}.tool_calls.{call_index}.function.arguments");
        let aad = request_field_aad(crypto, &field)?;
        let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
        *ciphertext_hex = String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
        decrypted_count += 1;
    }
    Ok(decrypted_count)
}

/// An array entry's path index: its `index` member when it is a
/// non-negative safe integer by JSON number value (so `1.0` is `1`, as a
/// JavaScript client reads it), its array position otherwise (§5).
fn entry_index(entry: &Value, position: usize) -> u64 {
    const MAX_SAFE: f64 = ((1u64 << 53) - 1) as f64;
    entry
        .get("index")
        .and_then(Value::as_f64)
        .filter(|index| index.fract() == 0.0 && (0.0..=MAX_SAFE).contains(index))
        .map_or(position as u64, |index| index as u64)
}

/// A response `choices` or `data` entry's path index: by JSON number value
/// under v2.1 ([`entry_index`]); v2 and legacy keep reading integers only.
fn response_entry_index(entry: &Value, position: usize, ctx: &E2eeRequestContext) -> u64 {
    match ctx.aad_mode {
        E2eeAadMode::AciV2_1 => entry_index(entry, position),
        _ => entry
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(position as u64),
    }
}

/// v2.1: encrypt `{prefix}.tool_calls.{t}.function.arguments` in place.
fn encrypt_tool_call_arguments(
    container: &mut serde_json::Map<String, Value>,
    prefix: &str,
    ctx: &E2eeRequestContext,
    response_id: &str,
) -> Result<(), E2eeError> {
    if ctx.aad_mode != E2eeAadMode::AciV2_1 {
        return Ok(());
    }
    let Some(Value::Array(calls)) = container.get_mut("tool_calls") else {
        return Ok(());
    };
    for (position, call) in calls.iter_mut().enumerate() {
        let call_index = entry_index(call, position);
        let Some(Value::Object(function)) = call.get_mut("function") else {
            continue;
        };
        encrypt_response_field(
            function,
            "arguments",
            &format!("{prefix}.tool_calls.{call_index}.function.arguments"),
            ctx,
            response_id,
        )?;
    }
    Ok(())
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
            decrypt_content_part_field(crypto, part, "text", &field)
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

/// Decrypt a direct string member (`text`) of a content part.
fn decrypt_content_part_field(
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

/// v2.1 whole content: the plaintext is the JSON encoding of the content
/// value, a string or a structured array, so text is never mistaken for parts.
fn v2_1_content_value(plaintext: &str) -> Result<Value, E2eeError> {
    match serde_json::from_str::<Value>(plaintext) {
        Ok(value @ (Value::String(_) | Value::Array(_))) => Ok(value),
        _ => Err(E2eeError::DecryptionFailed),
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
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if endpoint_path == EMBEDDINGS_PATH {
        encrypt_embedding_data(&mut payload, ctx, &response_id)?;
        seal_response_object(&mut payload, ctx)?;
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    }

    let choices = match payload.get_mut("choices").and_then(Value::as_array_mut) {
        Some(choices) => choices.iter_mut(),
        None => [].iter_mut(),
    };
    for (position, choice) in choices.enumerate() {
        let choice_index = response_entry_index(choice, position, ctx);
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
            encrypt_tool_call_arguments(
                message,
                &format!("choices.{choice_index}.message"),
                ctx,
                &response_id,
            )?;
        }
        let part = (endpoint_path != COMPLETIONS_PATH).then_some("message");
        encrypt_v2_1_choice_extras(choice, part, choice_index, ctx, &response_id)?;
    }

    seal_response_object(&mut payload, ctx)?;
    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

/// v2.1 encrypts two members v2 leaves readable: `refusal` and the whole
/// `logprobs` value (as its compact JSON), which repeats the generated tokens.
fn encrypt_v2_1_choice_extras(
    choice: &mut serde_json::Map<String, Value>,
    part: Option<&str>,
    choice_index: u64,
    ctx: &E2eeRequestContext,
    response_id: &str,
) -> Result<(), E2eeError> {
    if ctx.aad_mode != E2eeAadMode::AciV2_1 {
        return Ok(());
    }
    if let Some(part) = part {
        if let Some(Value::Object(container)) = choice.get_mut(part) {
            let field = format!("choices.{choice_index}.{part}.refusal");
            encrypt_response_field(container, "refusal", &field, ctx, response_id)?;
        }
    }
    if let Some(logprobs) = choice.get_mut("logprobs").filter(|value| !value.is_null()) {
        let plaintext = serde_json::to_vec(logprobs).map_err(|_| E2eeError::EncryptionFailed)?;
        let field = format!("choices.{choice_index}.logprobs");
        let aad = response_aad_for_context(ctx, response_id, &field)?;
        *logprobs = Value::String(encrypt_response_plaintext(ctx, &plaintext, aad.as_deref())?);
    }
    Ok(())
}

/// The member carrying the v2.1 response MAC.
pub(super) const RESPONSE_MAC_MEMBER: &str = "e2ee_mac";

/// v2.1: add an HMAC over the JCS form of a response object (a buffered body
/// or one SSE event), keyed from the request secret, so its readable members
/// cannot be changed either.
fn seal_response_object(payload: &mut Value, ctx: &E2eeRequestContext) -> Result<(), E2eeError> {
    let Some(secret) = &ctx.request_secret else {
        return Ok(());
    };
    let object = payload.as_object_mut().ok_or(E2eeError::EncryptionFailed)?;
    object.remove(RESPONSE_MAC_MEMBER);
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
        let data_index = response_entry_index(item, position, ctx);
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

pub(super) fn encrypt_e2ee_record(
    record: &mut crate::sse::Record,
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
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
    record.set_encrypted(encrypt_e2ee_stream_value(value, ctx, endpoint_path)?);
    Ok(())
}

pub(super) fn encrypt_e2ee_stream_value(
    mut payload: Value,
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Value, E2eeError> {
    if endpoint_path == EMBEDDINGS_PATH {
        // Embeddings are buffered-only. A streaming embedding is an internal
        // inconsistency: fail closed rather than send unencrypted vectors.
        return Err(E2eeError::EncryptionFailed);
    }
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let choices = match payload.get_mut("choices").and_then(Value::as_array_mut) {
        Some(choices) => choices.iter_mut(),
        None => [].iter_mut(),
    };
    for (position, choice) in choices.enumerate() {
        let choice_index = response_entry_index(choice, position, ctx);
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
            encrypt_tool_call_arguments(
                delta,
                &format!("choices.{choice_index}.delta"),
                ctx,
                &response_id,
            )?;
        }
        let part = (endpoint_path != COMPLETIONS_PATH).then_some("delta");
        encrypt_v2_1_choice_extras(choice, part, choice_index, ctx, &response_id)?;
    }

    seal_response_object(&mut payload, ctx)?;
    Ok(payload)
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
    fn entry_index_reads_json_number_value() {
        assert_eq!(entry_index(&json!({"index": 1}), 7), 1);
        assert_eq!(entry_index(&json!({"index": 1.0}), 7), 1);
        for index in [
            json!(1.5),
            json!(-1),
            json!(9_007_199_254_740_992u64),
            json!("1"),
        ] {
            assert_eq!(entry_index(&json!({ "index": index }), 7), 7);
        }
        assert_eq!(entry_index(&json!({}), 7), 7);
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
