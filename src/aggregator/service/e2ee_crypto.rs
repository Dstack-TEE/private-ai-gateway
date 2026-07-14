//! Body sealing/unsealing behind the service seam.
//!
//! ACI E2EE v3 (§7) seals whole bodies (or whole SSE event payloads) with the
//! core construction in [`crate::aci::e2ee`]. The inherited §13 legacy
//! `X-Signing-Algo` mode keeps its per-field hex ciphertext walk with no AAD.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};

use super::streaming::E2eeSseTransformer;
use super::wire::E2eeMode;
use super::{E2eeError, E2eeRequestContext, COMPLETIONS_PATH, EMBEDDINGS_PATH};
use crate::aci::e2ee::{
    encrypt_legacy_for_public_key, normalize_secp256k1_public_key_hex, seal_v3,
    x25519_public_key_from_hex, E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519,
    E2EE_CONTEXT_RESPONSE,
};
use crate::aci::keys::KeyProvider;

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

pub(super) fn normalize_legacy_public_key(
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

/// The request `model`: present and a string (needed for routing and, in v3,
/// bound into the AAD).
pub(super) fn validate_payload_model(payload: &Value) -> Result<String, E2eeError> {
    payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(E2eeError::DecryptionFailed)
}

// ---------- §13 legacy per-field request decryption ----------

pub(super) fn decrypt_legacy_request_payload(
    keys: &dyn KeyProvider,
    signing_algo: &str,
    endpoint_path: &str,
    payload: &mut Value,
) -> Result<(), E2eeError> {
    if endpoint_path == COMPLETIONS_PATH {
        return decrypt_legacy_string_or_array_field(keys, signing_algo, payload, "prompt");
    }
    if endpoint_path == EMBEDDINGS_PATH {
        return decrypt_legacy_string_or_array_field(keys, signing_algo, payload, "input");
    }

    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return Err(E2eeError::DecryptionFailed);
    };
    let mut decrypted_count = 0usize;
    for message in messages.iter_mut() {
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        decrypted_count += decrypt_legacy_content_value(keys, signing_algo, content)?;
    }

    if decrypted_count == 0 {
        return Err(E2eeError::DecryptionFailed);
    }
    Ok(())
}

/// Decrypt a top-level field (`prompt` / `input`) that is a hex ciphertext
/// string or an array of them. Non-string array items (e.g. token-id arrays
/// for embeddings) pass through untouched.
fn decrypt_legacy_string_or_array_field(
    keys: &dyn KeyProvider,
    signing_algo: &str,
    payload: &mut Value,
    field: &str,
) -> Result<(), E2eeError> {
    let Some(target) = payload.get_mut(field) else {
        return Err(E2eeError::DecryptionFailed);
    };

    let decrypted_count = match target {
        Value::String(ciphertext_hex) => {
            *ciphertext_hex = decrypt_legacy_field_to_string(keys, signing_algo, ciphertext_hex)?;
            1
        }
        Value::Array(items) => {
            let mut decrypted_count = 0usize;
            for item in items.iter_mut() {
                let Value::String(ciphertext_hex) = item else {
                    continue;
                };
                *ciphertext_hex =
                    decrypt_legacy_field_to_string(keys, signing_algo, ciphertext_hex)?;
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

fn decrypt_legacy_content_value(
    keys: &dyn KeyProvider,
    signing_algo: &str,
    content: &mut Value,
) -> Result<usize, E2eeError> {
    match content {
        // Whole-content encryption, any modality.
        Value::String(ciphertext_hex) => {
            let plaintext = decrypt_legacy_field_to_string(keys, signing_algo, ciphertext_hex)?;
            *content = decrypted_chat_content_value(plaintext);
            Ok(1)
        }
        Value::Array(items) => {
            let mut decrypted_count = 0usize;
            for item in items.iter_mut() {
                let Some(part) = item.as_object_mut() else {
                    continue;
                };
                // Only `text` parts ever carried legacy field ciphertext; the
                // legacy modes never defined per-part multimodal paths.
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(Value::String(ciphertext_hex)) = part.get_mut("text") else {
                    continue;
                };
                *ciphertext_hex =
                    decrypt_legacy_field_to_string(keys, signing_algo, ciphertext_hex)?;
                decrypted_count += 1;
            }
            Ok(decrypted_count)
        }
        _ => Ok(0),
    }
}

fn decrypt_legacy_field_to_string(
    keys: &dyn KeyProvider,
    signing_algo: &str,
    ciphertext_hex: &str,
) -> Result<String, E2eeError> {
    let plaintext = keys
        .decrypt_legacy_e2ee(signing_algo, ciphertext_hex, None)
        .map_err(|_| E2eeError::DecryptionFailed)?;
    String::from_utf8(plaintext).map_err(|_| E2eeError::DecryptionFailed)
}

pub(super) fn decrypted_chat_content_value(plaintext: String) -> Value {
    match serde_json::from_str::<Value>(&plaintext) {
        Ok(Value::Array(items)) => Value::Array(items),
        _ => Value::String(plaintext),
    }
}

// ---------- Response sealing ----------

/// Seal one whole plaintext unit (a buffered body or one SSE event payload)
/// to the client per §7.3, producing the `{"sealed_b64": …}` wire body.
fn seal_v3_response_unit(plaintext: &[u8], ctx: &E2eeRequestContext) -> Result<Vec<u8>, E2eeError> {
    let recipient = x25519_public_key_from_hex(&ctx.client_public_key_hex)
        .map_err(|_| E2eeError::EncryptionFailed)?;
    let sealed = seal_v3(
        &recipient,
        E2EE_CONTEXT_RESPONSE,
        &ctx.request_model,
        None,
        plaintext,
    )
    .map_err(|_| E2eeError::EncryptionFailed)?;
    serde_json::to_vec(&json!({ "sealed_b64": BASE64.encode(sealed) }))
        .map_err(|_| E2eeError::EncryptionFailed)
}

pub(super) fn encrypt_e2ee_response_body(
    cleartext_body: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Vec<u8>, E2eeError> {
    match ctx.mode {
        E2eeMode::V3 => seal_v3_response_unit(cleartext_body, ctx),
        E2eeMode::LegacyV1 => legacy_encrypt_response_body(cleartext_body, ctx, endpoint_path),
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

/// Seal one SSE event's data payload (§7.3 streaming): the whole event JSON
/// becomes one sealed unit in v3; the legacy mode re-encrypts its per-field
/// locations.
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
    match ctx.mode {
        E2eeMode::V3 => seal_v3_response_unit(cleartext_payload, ctx),
        E2eeMode::LegacyV1 => legacy_encrypt_stream_payload(cleartext_payload, ctx, endpoint_path),
    }
}

// ---------- §13 legacy per-field response encryption ----------

fn legacy_encrypt_response_body(
    cleartext_body: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Vec<u8>, E2eeError> {
    let mut payload: Value =
        serde_json::from_slice(cleartext_body).map_err(|_| E2eeError::EncryptionFailed)?;

    if endpoint_path == EMBEDDINGS_PATH {
        legacy_encrypt_embedding_data(&mut payload, ctx)?;
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    }

    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    };

    for choice in choices.iter_mut() {
        let Some(choice) = choice.as_object_mut() else {
            continue;
        };
        if endpoint_path == COMPLETIONS_PATH {
            legacy_encrypt_field(choice, "text", ctx)?;
        } else if let Some(Value::Object(message)) = choice.get_mut("message") {
            legacy_encrypt_field(message, "content", ctx)?;
            legacy_encrypt_field(message, "reasoning_content", ctx)?;
        }
    }

    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

fn legacy_encrypt_embedding_data(
    payload: &mut Value,
    ctx: &E2eeRequestContext,
) -> Result<(), E2eeError> {
    let Some(items) = payload.get_mut("data").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for item in items.iter_mut() {
        let Some(entry) = item.as_object_mut() else {
            continue;
        };
        let Some(embedding) = entry.get_mut("embedding") else {
            continue;
        };
        // OpenAI emits `embedding` as a float array by default and as a
        // base64 string when the client passes `encoding_format=base64`.
        // We serialize to compact JSON before encryption so the decrypted
        // plaintext round-trips through `serde_json` back to the original
        // type, mirroring how chat content arrays are recovered.
        let plaintext = serde_json::to_vec(embedding).map_err(|_| E2eeError::EncryptionFailed)?;
        let ciphertext_hex = legacy_encrypt_plaintext(ctx, &plaintext)?;
        *embedding = Value::String(ciphertext_hex);
    }
    Ok(())
}

fn legacy_encrypt_stream_payload(
    cleartext_payload: &[u8],
    ctx: &E2eeRequestContext,
    endpoint_path: &str,
) -> Result<Vec<u8>, E2eeError> {
    let mut payload: Value =
        serde_json::from_slice(cleartext_payload).map_err(|_| E2eeError::EncryptionFailed)?;
    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed);
    };

    for choice in choices.iter_mut() {
        let Some(choice) = choice.as_object_mut() else {
            continue;
        };
        if endpoint_path == COMPLETIONS_PATH {
            legacy_encrypt_field(choice, "text", ctx)?;
        } else if let Some(Value::Object(delta)) = choice.get_mut("delta") {
            if delta.get("content").and_then(Value::as_str) == Some("") {
                delta.remove("content");
            }
            legacy_encrypt_field(delta, "content", ctx)?;
            legacy_encrypt_field(delta, "reasoning_content", ctx)?;
        }
    }

    serde_json::to_vec(&payload).map_err(|_| E2eeError::EncryptionFailed)
}

fn legacy_encrypt_field(
    container: &mut serde_json::Map<String, Value>,
    field_name: &str,
    ctx: &E2eeRequestContext,
) -> Result<(), E2eeError> {
    let Some(Value::String(plaintext)) = container.get_mut(field_name) else {
        return Ok(());
    };
    *plaintext = legacy_encrypt_plaintext(ctx, plaintext.as_bytes())?;
    Ok(())
}

fn legacy_encrypt_plaintext(
    ctx: &E2eeRequestContext,
    plaintext: &[u8],
) -> Result<String, E2eeError> {
    encrypt_legacy_for_public_key(&ctx.algo, &ctx.client_public_key_hex, plaintext, None)
        .map_err(|_| E2eeError::EncryptionFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM;
    use crate::aci::e2ee::{unseal_v3, x25519_public_key_hex, x25519_secret_key_from_bytes};

    #[test]
    fn v3_response_body_is_one_sealed_envelope_bound_to_the_model() {
        let secret = x25519_secret_key_from_bytes(&[0x51u8; 32]).unwrap();
        let ctx = E2eeRequestContext {
            algo: E2EE_ALGO_X25519_AESGCM.to_string(),
            mode: E2eeMode::V3,
            request_model: "demo-model".to_string(),
            client_public_key_hex: x25519_public_key_hex(&secret),
        };
        let body = br#"{"id":"chatcmpl-1","choices":[]}"#;
        let wire = encrypt_e2ee_response_body(body, &ctx, "/v1/chat/completions").unwrap();

        let envelope: Value = serde_json::from_slice(&wire).unwrap();
        let sealed = BASE64
            .decode(envelope["sealed_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            unseal_v3(&secret, E2EE_CONTEXT_RESPONSE, "demo-model", None, &sealed).unwrap(),
            body
        );
        // A different model in the AAD fails authentication.
        assert!(unseal_v3(&secret, E2EE_CONTEXT_RESPONSE, "other-model", None, &sealed).is_err());
    }

    #[test]
    fn v3_stream_payload_seals_each_event_whole() {
        let secret = x25519_secret_key_from_bytes(&[0x52u8; 32]).unwrap();
        let ctx = E2eeRequestContext {
            algo: E2EE_ALGO_X25519_AESGCM.to_string(),
            mode: E2eeMode::V3,
            request_model: "demo-model".to_string(),
            client_public_key_hex: x25519_public_key_hex(&secret),
        };
        let event = br#"{"choices":[{"delta":{"content":"hi"}}]}"#;
        let wire = encrypt_e2ee_stream_payload(event, &ctx, "/v1/chat/completions").unwrap();
        let envelope: Value = serde_json::from_slice(&wire).unwrap();
        let sealed = BASE64
            .decode(envelope["sealed_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            unseal_v3(&secret, E2EE_CONTEXT_RESPONSE, "demo-model", None, &sealed).unwrap(),
            event
        );
    }
}
