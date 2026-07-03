use super::wire::{E2eeAadMode, E2eeDecryptor};

use serde_json::{json, Value};

use super::streaming::E2eeSseTransformer;
use super::{E2eeError, E2eeRequestContext, COMPLETIONS_PATH, EMBEDDINGS_PATH};
use crate::aci::canonical::canonicalize;
use crate::aci::e2ee::{
    encrypt_aci_e2ee_for_public_key, encrypt_legacy_for_public_key,
    normalize_secp256k1_public_key_hex, E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519,
};
use crate::aci::keys::KeyProvider;

/// ACI v2 nonce (§7.5): reject only an empty nonce. With JCS AAD (§7.3) the
/// old `|`/CR/LF ambiguity checks are unnecessary.
pub(super) fn validate_aci_e2ee_nonce(nonce: &str) -> Result<(), E2eeError> {
    if nonce.is_empty() {
        return Err(E2eeError::InvalidNonce);
    }
    Ok(())
}

/// Legacy X-Signing-Algo nonce: empty or `|`/CR/LF is rejected because the
/// legacy AAD is pipe-delimited. Frozen compatibility surface.
fn validate_e2ee_nonce(nonce: &str) -> Result<(), E2eeError> {
    if nonce.is_empty() || aad_component_is_ambiguous(nonce) {
        return Err(E2eeError::InvalidNonce);
    }
    Ok(())
}

pub(super) fn validate_legacy_e2ee_nonce(nonce: &str) -> Result<(), E2eeError> {
    validate_e2ee_nonce(nonce)?;
    if nonce.len() < 16 {
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

/// ACI v2 model (§7.3): present and a string. No ambiguity check — the JCS
/// AAD needs no escaping.
pub(super) fn validate_aci_payload_model(payload: &Value) -> Result<String, E2eeError> {
    payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(E2eeError::InvalidPayloadModel)
}

/// Legacy model: present, a string, and free of `|`/CR/LF. Frozen.
pub(super) fn validate_payload_model(payload: &Value) -> Result<String, E2eeError> {
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return Err(E2eeError::InvalidPayloadModel);
    };
    if aad_component_is_ambiguous(model) {
        return Err(E2eeError::InvalidPayloadModel);
    }
    Ok(model.to_string())
}

fn aad_component_is_ambiguous(value: &str) -> bool {
    value.contains('|') || value.contains('\r') || value.contains('\n')
}

/// ACI v2 request AAD (§7.3): the JCS canonicalization of the purpose-tagged
/// object. `field` is the encrypted location's field path (§7.2).
fn aci_request_aad(
    algo: &str,
    model: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
) -> Result<Vec<u8>, E2eeError> {
    canonicalize(&json!({
        "purpose": "aci.e2ee.request.v2",
        "algo": algo,
        "model": model,
        "field": field,
        "nonce": nonce,
        "ts": timestamp,
    }))
    .map_err(|_| E2eeError::DecryptionFailed)
}

/// ACI v2 response AAD (§7.3): like the request AAD but tagged
/// `aci.e2ee.response.v2` and additionally binding the response `id`.
fn aci_response_aad(
    algo: &str,
    model: &str,
    response_id: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
) -> Result<Vec<u8>, E2eeError> {
    canonicalize(&json!({
        "purpose": "aci.e2ee.response.v2",
        "algo": algo,
        "model": model,
        "id": response_id,
        "field": field,
        "nonce": nonce,
        "ts": timestamp,
    }))
    .map_err(|_| E2eeError::EncryptionFailed)
}

/// Legacy pipe-delimited request AAD for chat message content. Frozen.
fn legacy_request_aad(
    algo: &str,
    model: &str,
    message_index: usize,
    content_index: Option<usize>,
    nonce: &str,
    timestamp: u64,
) -> String {
    let content_index = content_index
        .map(|idx| idx.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "v2|req|algo={algo}|model={model}|m={message_index}|c={content_index}|n={nonce}|ts={timestamp}"
    )
}

/// Legacy pipe-delimited request AAD for prompt / embedding-input fields. Frozen.
fn legacy_completion_request_aad(
    algo: &str,
    model: &str,
    field_name: &str,
    nonce: &str,
    timestamp: u64,
) -> String {
    format!("v2|req|algo={algo}|model={model}|field={field_name}|n={nonce}|ts={timestamp}")
}

/// Legacy pipe-delimited response AAD for embedding data. Frozen.
fn legacy_embedding_response_aad(
    algo: &str,
    model: &str,
    response_id: &str,
    data_index: u64,
    field_name: &str,
    nonce: &str,
    timestamp: u64,
) -> String {
    format!(
        "v2|resp|algo={algo}|model={model}|id={response_id}|data={data_index}|field={field_name}|n={nonce}|ts={timestamp}"
    )
}

/// Legacy pipe-delimited response AAD for chat / completion fields. Frozen.
fn legacy_response_aad(
    algo: &str,
    model: &str,
    response_id: &str,
    choice_index: u64,
    field_name: &str,
    nonce: &str,
    timestamp: u64,
) -> String {
    format!(
        "v2|resp|algo={algo}|model={model}|id={response_id}|choice={choice_index}|field={field_name}|n={nonce}|ts={timestamp}"
    )
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
            let aad = completion_request_aad_for_crypto(crypto, "prompt")?;
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
                let aad = completion_request_aad_for_crypto(crypto, &field_name)?;
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
            let aad = completion_request_aad_for_crypto(crypto, "input")?;
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
                let aad = completion_request_aad_for_crypto(crypto, &field_name)?;
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
            let aad = content_request_aad(crypto, message_index, None, &field)?;
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

/// Decrypt one structured content part in place. `text` parts are decrypted in
/// every mode; the per-part `image_url.url` and `input_audio.data` locations
/// (§7.2) are ACI-only — the legacy compatibility modes never defined field
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
            decrypt_content_part_field(crypto, message_index, content_index, part, "text", &field)
        }
        Some("image_url") if crypto.aad_mode.is_aci() => {
            let field = format!("messages.{message_index}.content.{content_index}.image_url.url");
            decrypt_content_part_nested(
                crypto,
                message_index,
                content_index,
                part,
                "image_url",
                "url",
                &field,
            )
        }
        Some("input_audio") if crypto.aad_mode.is_aci() => {
            let field =
                format!("messages.{message_index}.content.{content_index}.input_audio.data");
            decrypt_content_part_nested(
                crypto,
                message_index,
                content_index,
                part,
                "input_audio",
                "data",
                &field,
            )
        }
        _ => Ok(0),
    }
}

/// Decrypt a direct string member (`text`) of a content part.
fn decrypt_content_part_field(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content_index: usize,
    part: &mut serde_json::Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<usize, E2eeError> {
    let Some(Value::String(ciphertext_hex)) = part.get_mut(key) else {
        return Ok(0);
    };
    let aad = content_request_aad(crypto, message_index, Some(content_index), field)?;
    let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
    *ciphertext_hex = String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
    Ok(1)
}

/// Decrypt a nested string member (`image_url.url`, `input_audio.data`) of a
/// content part.
fn decrypt_content_part_nested(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content_index: usize,
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
    let aad = content_request_aad(crypto, message_index, Some(content_index), field)?;
    let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
    *ciphertext_hex = String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
    Ok(1)
}

/// AAD for a chat message-content location. ACI uses the JCS field path;
/// the legacy modes keep the pipe-delimited `m`/`c` form.
fn content_request_aad(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content_index: Option<usize>,
    aci_field: &str,
) -> Result<Option<Vec<u8>>, E2eeError> {
    match crypto.aad_mode {
        E2eeAadMode::AciV2 => {
            let (nonce, timestamp) = crypto.nonce_ts()?;
            Ok(Some(aci_request_aad(
                crypto.algo,
                crypto.model,
                aci_field,
                nonce,
                timestamp,
            )?))
        }
        E2eeAadMode::LegacyV2 => {
            let (nonce, timestamp) = crypto.nonce_ts()?;
            Ok(Some(
                legacy_request_aad(
                    crypto.algo,
                    crypto.model,
                    message_index,
                    content_index,
                    nonce,
                    timestamp,
                )
                .into_bytes(),
            ))
        }
        E2eeAadMode::LegacyV1 => Ok(None),
    }
}

/// AAD for a prompt / embedding-input field, whose field path (`prompt`,
/// `prompt.{i}`, `input`, `input.{i}`) is already the field-path component.
fn completion_request_aad_for_crypto(
    crypto: &E2eeFieldCrypto<'_>,
    field: &str,
) -> Result<Option<Vec<u8>>, E2eeError> {
    match crypto.aad_mode {
        E2eeAadMode::AciV2 => {
            let (nonce, timestamp) = crypto.nonce_ts()?;
            Ok(Some(aci_request_aad(
                crypto.algo,
                crypto.model,
                field,
                nonce,
                timestamp,
            )?))
        }
        E2eeAadMode::LegacyV2 => {
            let (nonce, timestamp) = crypto.nonce_ts()?;
            Ok(Some(
                legacy_completion_request_aad(crypto.algo, crypto.model, field, nonce, timestamp)
                    .into_bytes(),
            ))
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
        E2eeDecryptor::AciV2 { key_id } => {
            let aad = aad.ok_or(E2eeError::DecryptionFailed)?;
            crypto
                .keys
                .decrypt_e2ee(key_id, ciphertext_hex, aad)
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
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let response_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ctx.aad_mode == E2eeAadMode::LegacyV2 && aad_component_is_ambiguous(&response_id) {
        return Err(E2eeError::EncryptionFailed);
    }

    if endpoint_path == EMBEDDINGS_PATH {
        encrypt_embedding_data(&mut payload, ctx, &response_model, &response_id)?;
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
                &response_model,
                &response_id,
                choice_index,
            )?;
        } else if let Some(Value::Object(message)) = choice.get_mut("message") {
            encrypt_response_field(
                message,
                "content",
                &format!("choices.{choice_index}.message.content"),
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
            encrypt_response_field(
                message,
                "reasoning_content",
                &format!("choices.{choice_index}.message.reasoning_content"),
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
            // Response audio (`message.audio.data`, §7.2) is ACI-only.
            if ctx.aad_mode.is_aci() {
                encrypt_message_audio_data(message, ctx, &response_id, choice_index)?;
            }
        }
    }

    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

pub(super) fn encrypt_embedding_data(
    payload: &mut Value,
    ctx: &E2eeRequestContext,
    response_model: &str,
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
        let aad = embedding_response_aad_for_context(ctx, response_model, response_id, data_index)?;
        let ciphertext_hex = encrypt_response_plaintext(ctx, &plaintext, aad.as_deref())?;
        *embedding = Value::String(ciphertext_hex);
    }
    Ok(())
}

fn embedding_response_aad_for_context(
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    data_index: u64,
) -> Result<Option<Vec<u8>>, E2eeError> {
    match ctx.aad_mode {
        E2eeAadMode::AciV2 => {
            let (nonce, timestamp) = response_nonce_ts(ctx)?;
            let field = format!("data.{data_index}.embedding");
            Ok(Some(aci_response_aad(
                &ctx.algo,
                &ctx.request_model,
                response_id,
                &field,
                nonce,
                timestamp,
            )?))
        }
        E2eeAadMode::LegacyV2 => {
            if aad_component_is_ambiguous(response_model) {
                return Err(E2eeError::EncryptionFailed);
            }
            let (nonce, timestamp) = response_nonce_ts(ctx)?;
            Ok(Some(
                legacy_embedding_response_aad(
                    &ctx.algo,
                    response_model,
                    response_id,
                    data_index,
                    "embedding",
                    nonce,
                    timestamp,
                )
                .into_bytes(),
            ))
        }
        E2eeAadMode::LegacyV1 => Ok(None),
    }
}

pub(super) fn encrypt_e2ee_final_response(
    cleartext_body: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
    is_sse: bool,
) -> Result<Vec<u8>, E2eeError> {
    if !is_sse {
        return encrypt_e2ee_response_body(cleartext_body, ctx, endpoint_path);
    }
    let mut transformer = E2eeSseTransformer::new(ctx.clone(), endpoint_path.to_string());
    let mut out = transformer.push_chunk(cleartext_body)?;
    out.extend(transformer.finish()?);
    Ok(out)
}

pub(super) fn is_sse_content_type(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

pub(super) fn encrypt_e2ee_stream_payload(
    cleartext_payload: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Vec<u8>, E2eeError> {
    if endpoint_path == EMBEDDINGS_PATH {
        // OpenAI's embeddings endpoint is buffered-only; the router
        // forces stream=false, so reaching here means an internal
        // inconsistency that we fail closed on.
        return Err(E2eeError::EncryptionFailed);
    }
    let mut payload: Value =
        serde_json::from_slice(cleartext_payload).map_err(|_| E2eeError::EncryptionFailed)?;
    let response_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let response_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ctx.aad_mode == E2eeAadMode::LegacyV2 && aad_component_is_ambiguous(&response_id) {
        return Err(E2eeError::EncryptionFailed);
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
                &response_model,
                &response_id,
                choice_index,
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
                &response_model,
                &response_id,
                choice_index,
            )?;
            encrypt_response_field(
                delta,
                "reasoning_content",
                &format!("choices.{choice_index}.delta.reasoning_content"),
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
        }
    }

    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

pub(super) fn encrypt_response_field(
    container: &mut serde_json::Map<String, Value>,
    field_name: &str,
    aci_field: &str,
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    choice_index: u64,
) -> Result<(), E2eeError> {
    let Some(Value::String(plaintext)) = container.get_mut(field_name) else {
        return Ok(());
    };
    let aad = response_aad_for_context(
        ctx,
        response_model,
        response_id,
        choice_index,
        field_name,
        aci_field,
    )?;
    *plaintext = encrypt_response_plaintext(ctx, plaintext.as_bytes(), aad.as_deref())?;
    Ok(())
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
    let (nonce, timestamp) = response_nonce_ts(ctx)?;
    let field = format!("choices.{choice_index}.message.audio.data");
    let aad = aci_response_aad(
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
    response_model: &str,
    response_id: &str,
    choice_index: u64,
    field_name: &str,
    aci_field: &str,
) -> Result<Option<Vec<u8>>, E2eeError> {
    match ctx.aad_mode {
        E2eeAadMode::AciV2 => {
            let (nonce, timestamp) = response_nonce_ts(ctx)?;
            Ok(Some(aci_response_aad(
                &ctx.algo,
                &ctx.request_model,
                response_id,
                aci_field,
                nonce,
                timestamp,
            )?))
        }
        E2eeAadMode::LegacyV2 => {
            if aad_component_is_ambiguous(field_name) || aad_component_is_ambiguous(response_model)
            {
                return Err(E2eeError::EncryptionFailed);
            }
            let (nonce, timestamp) = response_nonce_ts(ctx)?;
            Ok(Some(
                legacy_response_aad(
                    &ctx.algo,
                    response_model,
                    response_id,
                    choice_index,
                    field_name,
                    nonce,
                    timestamp,
                )
                .into_bytes(),
            ))
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
        E2eeAadMode::AciV2 => {
            // The response suite is the request's selected service E2EE key
            // algo; `client_public_key_hex` is already normalized for it.
            let aad = aad.ok_or(E2eeError::EncryptionFailed)?;
            encrypt_aci_e2ee_for_public_key(&ctx.algo, &ctx.client_public_key_hex, plaintext, aad)
                .map_err(|_| E2eeError::EncryptionFailed)
        }
        E2eeAadMode::LegacyV1 | E2eeAadMode::LegacyV2 => {
            encrypt_legacy_for_public_key(&ctx.algo, &ctx.client_public_key_hex, plaintext, aad)
                .map_err(|_| E2eeError::EncryptionFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{aci_request_aad, aci_response_aad};

    // Byte-exact expected AAD from spec/test-vectors.md §7 (X25519 suite).
    const REQUEST_AAD: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content","model":"demo-model","nonce":"6e6f6e63652d31323334","purpose":"aci.e2ee.request.v2","ts":1750000000}"#;
    const RESPONSE_AAD: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"6e6f6e63652d31323334","purpose":"aci.e2ee.response.v2","ts":1750000000}"#;

    #[test]
    fn request_aad_matches_spec_test_vector() {
        let aad = aci_request_aad(
            "x25519-aes-256-gcm-hkdf-sha256",
            "demo-model",
            "messages.0.content",
            "6e6f6e63652d31323334",
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
            "6e6f6e63652d31323334",
            1_750_000_000,
        )
        .unwrap();
        assert_eq!(aad, RESPONSE_AAD.as_bytes());
    }
}
