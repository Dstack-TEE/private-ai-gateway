use super::streaming::*;
use super::wire::{E2eeAadMode, E2eeDecryptor};
use super::*;

use serde_json::Value;

use crate::aci::e2ee::{
    encrypt_for_public_key, encrypt_legacy_for_public_key, normalize_secp256k1_public_key_hex,
    E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519,
};
use crate::aci::keys::KeyProvider;

pub(super) fn validate_e2ee_nonce(nonce: &str) -> Result<(), E2eeError> {
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

pub(super) fn validate_payload_model(payload: &Value) -> Result<String, E2eeError> {
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return Err(E2eeError::InvalidPayloadModel);
    };
    if aad_component_is_ambiguous(model) {
        return Err(E2eeError::InvalidPayloadModel);
    }
    Ok(model.to_string())
}

pub(super) fn aad_component_is_ambiguous(value: &str) -> bool {
    value.contains('|') || value.contains('\r') || value.contains('\n')
}

pub(super) fn request_aad(
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

pub(super) fn completion_request_aad(
    algo: &str,
    model: &str,
    field_name: &str,
    nonce: &str,
    timestamp: u64,
) -> String {
    format!("v2|req|algo={algo}|model={model}|field={field_name}|n={nonce}|ts={timestamp}")
}

pub(super) fn embedding_response_aad(
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

pub(super) fn response_aad(
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
        Value::String(ciphertext_hex) => {
            let aad = request_aad_for_crypto(crypto, message_index, None)?;
            let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
            let plaintext =
                String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
            *content = decrypted_chat_content_value(plaintext);
            Ok(1)
        }
        Value::Array(items) => {
            let mut decrypted_count = 0usize;
            for (content_index, item) in items.iter_mut().enumerate() {
                let Some(item) = item.as_object_mut() else {
                    continue;
                };
                if item.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(Value::String(ciphertext_hex)) = item.get_mut("text") else {
                    continue;
                };
                let aad = request_aad_for_crypto(crypto, message_index, Some(content_index))?;
                let plaintext = decrypt_e2ee_field(crypto, ciphertext_hex, aad.as_deref())?;
                *ciphertext_hex =
                    String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)?;
                decrypted_count += 1;
            }
            Ok(decrypted_count)
        }
        _ => Ok(0),
    }
}

pub(super) fn request_aad_for_crypto(
    crypto: &E2eeFieldCrypto<'_>,
    message_index: usize,
    content_index: Option<usize>,
) -> Result<Option<String>, E2eeError> {
    if !crypto.aad_mode.uses_aad() {
        return Ok(None);
    }
    let nonce = crypto.nonce.ok_or(E2eeError::DecryptionFailed)?;
    let timestamp = crypto.timestamp.ok_or(E2eeError::DecryptionFailed)?;
    Ok(Some(request_aad(
        crypto.algo,
        crypto.model,
        message_index,
        content_index,
        nonce,
        timestamp,
    )))
}

pub(super) fn completion_request_aad_for_crypto(
    crypto: &E2eeFieldCrypto<'_>,
    field_name: &str,
) -> Result<Option<String>, E2eeError> {
    if !crypto.aad_mode.uses_aad() {
        return Ok(None);
    }
    let nonce = crypto.nonce.ok_or(E2eeError::DecryptionFailed)?;
    let timestamp = crypto.timestamp.ok_or(E2eeError::DecryptionFailed)?;
    Ok(Some(completion_request_aad(
        crypto.algo,
        crypto.model,
        field_name,
        nonce,
        timestamp,
    )))
}

pub(super) fn decrypt_e2ee_field(
    crypto: &E2eeFieldCrypto<'_>,
    ciphertext_hex: &str,
    aad: Option<&str>,
) -> Result<Vec<u8>, E2eeError> {
    match crypto.decryptor {
        E2eeDecryptor::AciV2 { key_id } => {
            let aad = aad.ok_or(E2eeError::DecryptionFailed)?;
            crypto
                .keys
                .decrypt_e2ee(key_id, ciphertext_hex, aad.as_bytes())
                .map_err(|_| E2eeError::DecryptionFailed)
        }
        E2eeDecryptor::Legacy { signing_algo } => crypto
            .keys
            .decrypt_legacy_e2ee(signing_algo, ciphertext_hex, aad.map(str::as_bytes))
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
    if ctx.aad_mode.uses_aad() && aad_component_is_ambiguous(&response_id) {
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
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
        } else if let Some(Value::Object(message)) = choice.get_mut("message") {
            encrypt_response_field(
                message,
                "content",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
            encrypt_response_field(
                message,
                "reasoning_content",
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
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
        let aad = embedding_response_aad_for_context(
            ctx,
            response_model,
            response_id,
            data_index,
            "embedding",
        )?;
        let ciphertext_hex = encrypt_response_plaintext(ctx, &plaintext, aad.as_deref())?;
        *embedding = Value::String(ciphertext_hex);
    }
    Ok(())
}

pub(super) fn embedding_response_aad_for_context(
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    data_index: u64,
    field_name: &str,
) -> Result<Option<String>, E2eeError> {
    if !ctx.aad_mode.uses_aad() {
        return Ok(None);
    }
    if aad_component_is_ambiguous(field_name) {
        return Err(E2eeError::EncryptionFailed);
    }
    let model = match ctx.aad_mode {
        E2eeAadMode::AciV2 => ctx.request_model.as_str(),
        E2eeAadMode::LegacyV2 => response_model,
        E2eeAadMode::LegacyV1 => return Ok(None),
    };
    if aad_component_is_ambiguous(model) {
        return Err(E2eeError::EncryptionFailed);
    }
    let nonce = ctx.nonce.as_deref().ok_or(E2eeError::EncryptionFailed)?;
    let timestamp = ctx.timestamp.ok_or(E2eeError::EncryptionFailed)?;
    Ok(Some(embedding_response_aad(
        &ctx.algo,
        model,
        response_id,
        data_index,
        field_name,
        nonce,
        timestamp,
    )))
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
    if ctx.aad_mode.uses_aad() && aad_component_is_ambiguous(&response_id) {
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
                ctx,
                &response_model,
                &response_id,
                choice_index,
            )?;
            encrypt_response_field(
                delta,
                "reasoning_content",
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
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    choice_index: u64,
) -> Result<(), E2eeError> {
    if aad_component_is_ambiguous(field_name) {
        return Err(E2eeError::EncryptionFailed);
    }
    let Some(Value::String(plaintext)) = container.get_mut(field_name) else {
        return Ok(());
    };
    let aad = response_aad_for_context(ctx, response_model, response_id, choice_index, field_name)?;
    *plaintext = encrypt_response_plaintext(ctx, plaintext.as_bytes(), aad.as_deref())?;
    Ok(())
}

pub(super) fn response_aad_for_context(
    ctx: &E2eeRequestContext,
    response_model: &str,
    response_id: &str,
    choice_index: u64,
    field_name: &str,
) -> Result<Option<String>, E2eeError> {
    if !ctx.aad_mode.uses_aad() {
        return Ok(None);
    }
    if aad_component_is_ambiguous(field_name) {
        return Err(E2eeError::EncryptionFailed);
    }
    let model = match ctx.aad_mode {
        E2eeAadMode::AciV2 => ctx.request_model.as_str(),
        E2eeAadMode::LegacyV2 => response_model,
        E2eeAadMode::LegacyV1 => return Ok(None),
    };
    if aad_component_is_ambiguous(model) {
        return Err(E2eeError::EncryptionFailed);
    }
    let nonce = ctx.nonce.as_deref().ok_or(E2eeError::EncryptionFailed)?;
    let timestamp = ctx.timestamp.ok_or(E2eeError::EncryptionFailed)?;
    Ok(Some(response_aad(
        &ctx.algo,
        model,
        response_id,
        choice_index,
        field_name,
        nonce,
        timestamp,
    )))
}

pub(super) fn encrypt_response_plaintext(
    ctx: &E2eeRequestContext,
    plaintext: &[u8],
    aad: Option<&str>,
) -> Result<String, E2eeError> {
    match ctx.aad_mode {
        E2eeAadMode::AciV2 => {
            let aad = aad.ok_or(E2eeError::EncryptionFailed)?;
            encrypt_for_public_key(&ctx.client_public_key_hex, plaintext, aad.as_bytes())
                .map_err(|_| E2eeError::EncryptionFailed)
        }
        E2eeAadMode::LegacyV1 | E2eeAadMode::LegacyV2 => encrypt_legacy_for_public_key(
            &ctx.algo,
            &ctx.client_public_key_hex,
            plaintext,
            aad.map(str::as_bytes),
        )
        .map_err(|_| E2eeError::EncryptionFailed),
    }
}
