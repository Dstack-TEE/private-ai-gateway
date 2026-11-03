use std::collections::HashMap;

use rand::RngCore;
use serde::Deserialize;

use super::ServiceError;
use crate::aci::receipt::{SignedReceipt, EVENT_REQUEST_RECEIVED, EVENT_RESPONSE_RETURNED};
use crate::aci::upstream::{collect_buffered_upstream_body, UpstreamBodyStream};

pub(super) async fn collect_upstream_body(
    body: UpstreamBodyStream,
    headers: &HashMap<String, String>,
    limit: usize,
) -> Result<Vec<u8>, ServiceError> {
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok());
    Ok(collect_buffered_upstream_body(body, content_length, limit).await?)
}

pub(super) fn generate_receipt_id() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut bytes = [0u8; 12];
    rng.fill_bytes(&mut bytes);
    format!("rcpt-{}", hex::encode(bytes))
}

pub(super) fn extract_chat_id(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ResponseId {
        id: Option<String>,
    }
    let trimmed = body.iter().position(|byte| !byte.is_ascii_whitespace())?;
    if body[trimmed] != b'{' {
        return None;
    }
    serde_json::from_slice::<ResponseId>(body).ok()?.id
}

pub(super) fn accepted_response_model(status_code: u16, body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ResponseModel {
        model: Option<String>,
    }
    if !(200..=299).contains(&status_code) {
        return None;
    }
    let trimmed = body.iter().position(|byte| !byte.is_ascii_whitespace())?;
    if body[trimmed] != b'{' {
        return None;
    }
    serde_json::from_slice::<ResponseModel>(body).ok()?.model
}

pub(super) fn legacy_signature_text(receipt: &SignedReceipt) -> Option<String> {
    let payload = receipt.document_json().ok()?;
    let events = payload.get("event_log")?.as_array()?;
    let body_hash = |event_type: &str| {
        events
            .iter()
            .find(|e| e.get("type").and_then(serde_json::Value::as_str) == Some(event_type))?
            .get("body_hash")?
            .as_str()
            .and_then(strip_sha256_prefix)
            .map(str::to_string)
    };
    let request_hash = body_hash(EVENT_REQUEST_RECEIVED)?;
    let response_hash = body_hash(EVENT_RESPONSE_RETURNED)?;
    Some(format!("{request_hash}:{response_hash}"))
}

pub(super) fn strip_sha256_prefix(value: &str) -> Option<&str> {
    value.strip_prefix("sha256:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_identifiers_ignore_other_fields_and_non_string_siblings() {
        let body = br#" {"id":"chat-1","model":"test-model","data":[{"b64_json":"image"}]} "#;
        assert_eq!(extract_chat_id(body).as_deref(), Some("chat-1"));
        assert_eq!(
            accepted_response_model(200, body).as_deref(),
            Some("test-model")
        );
        assert_eq!(
            extract_chat_id(br#"{"id":"chat-1","model":123}"#).as_deref(),
            Some("chat-1")
        );
        assert_eq!(
            accepted_response_model(200, br#"{"id":123,"model":"test-model"}"#).as_deref(),
            Some("test-model")
        );
        assert_eq!(accepted_response_model(400, body), None);
    }

    #[test]
    fn response_identifiers_return_none_for_absent_or_invalid_fields() {
        for body in [
            b"".as_slice(),
            b" ",
            b"not JSON",
            b"data: [DONE]\n\n",
            b"null",
            b"[]",
            br#"["not-an-object"]"#,
            b"{}",
            br#"{"id":null,"model":null}"#,
            br#"{"id":123,"model":false}"#,
            br#"{"id":"chat-1","model":"test-model"} trailing"#,
        ] {
            assert_eq!(extract_chat_id(body), None);
            assert_eq!(accepted_response_model(200, body), None);
        }
    }
}
