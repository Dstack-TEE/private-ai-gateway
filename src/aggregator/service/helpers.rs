use futures_util::StreamExt;
use rand::RngCore;

use super::ServiceError;
use crate::aci::receipt::{SignedReceipt, EVENT_REQUEST_RECEIVED, EVENT_RESPONSE_RETURNED};
use crate::aci::upstream::UpstreamBodyStream;

pub(super) async fn collect_upstream_body(
    mut body: UpstreamBodyStream,
) -> Result<Vec<u8>, ServiceError> {
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

pub(super) fn generate_receipt_id() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut bytes = [0u8; 12];
    rng.fill_bytes(&mut bytes);
    format!("rcpt-{}", hex::encode(bytes))
}

pub(super) fn generate_upstream_request_id() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut bytes = [0u8; 16];
    rng.fill_bytes(&mut bytes);
    format!("ureq_{}", hex::encode(bytes))
}

pub(super) fn provider_request_id(
    headers: &std::collections::HashMap<String, String>,
) -> Option<String> {
    ["x-request-id", "request-id", "x-amzn-requestid", "cf-ray"]
        .iter()
        .find_map(|name| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        })
}

pub(super) fn trace_upstream_attempt(
    request_id: &str,
    attempt_index: u32,
    route_id: &str,
    status: u16,
    upstream_request_id: &str,
    provider_request_id: Option<&str>,
) {
    tracing::info!(
        request_id,
        attempt_index,
        route_id,
        status,
        upstream_request_id,
        provider_request_id = provider_request_id.unwrap_or(""),
        "upstream attempt completed"
    );
}

pub(super) fn extract_chat_id(body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let trimmed = body.iter().position(|b| !b.is_ascii_whitespace())?;
    if body[trimmed] != b'{' {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let id = parsed.get("id")?.as_str()?;
    Some(id.to_string())
}

pub(super) fn accepted_response_model(status_code: u16, body: &[u8]) -> Option<String> {
    if !(200..=299).contains(&status_code) || body.is_empty() {
        return None;
    }
    let trimmed = body.iter().position(|b| !b.is_ascii_whitespace())?;
    if body[trimmed] != b'{' {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    parsed.get("model")?.as_str().map(str::to_string)
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
