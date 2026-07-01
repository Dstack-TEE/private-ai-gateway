//! Client-facing error responses, shaped per downstream API surface.
//!
//! Two surfaces are served: an OpenAI-compatible surface (chat/completions,
//! completions, embeddings, responses) and an Anthropic-compatible surface
//! (messages). Success responses are converted per surface elsewhere; these
//! builders do the same for errors so each SDK gets a parseable envelope.
//!
//! Upstream error detail is never passed through raw: status, body, and headers
//! are always rebuilt here so provider internals cannot leak.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    http::{header::CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

/// Downstream API surface that shapes the error envelope and `error.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Openai,
    Anthropic,
}

/// Map an HTTP status to the surface's `error.type`. Only covers statuses this
/// gateway actually emits.
pub fn error_type(surface: Surface, status: u16) -> &'static str {
    match surface {
        Surface::Anthropic => match status {
            400 => "invalid_request_error",
            401 => "authentication_error",
            402 => "billing_error",
            403 => "permission_error",
            404 => "not_found_error",
            429 => "rate_limit_error",
            504 => "timeout_error",
            s if s >= 500 => "api_error",
            _ => "invalid_request_error",
        },
        Surface::Openai => match status {
            401 => "authentication_error",
            402 => "insufficient_quota",
            403 => "permission_error",
            404 => "not_found_error",
            429 => "rate_limit_error",
            503 => "service_unavailable",
            504 => "timeout_error",
            s if s >= 500 => "upstream_error",
            _ => "invalid_request_error",
        },
    }
}

/// Flatten an upstream status to the client-facing status. The mapping is uniform
/// across surfaces; only the envelope and `error.type` are surface-aware.
pub fn map_upstream_status(status: u16) -> u16 {
    match status {
        400 | 404 | 422 => status,
        429 => 429,
        503 => 503,
        504 => 504,
        _ => 502,
    }
}

/// 4xx other than auth/billing/rate-limit (401/402/403/429) describe a problem
/// with the caller's own request, so the provider's message is worth surfacing
/// (always re-wrapped in our envelope, never the raw upstream response).
pub fn is_actionable_client_error(status: u16) -> bool {
    (400..500).contains(&status) && !matches!(status, 401..=403 | 429)
}

/// Generic sanitized message for a non-actionable upstream status.
pub fn upstream_message(upstream_status: u16) -> &'static str {
    match upstream_status {
        401..=403 => "The upstream provider is currently unavailable",
        429 => "Rate limit exceeded. Please retry after some time.",
        503 => "The model is currently unavailable. Please try again later.",
        504 => "The upstream provider timed out",
        _ => "The upstream provider returned an error",
    }
}

fn envelope(surface: Surface, error_type: &str, message: &str, request_id: Option<&str>) -> Value {
    match surface {
        Surface::Anthropic => {
            let mut value = json!({
                "type": "error",
                "error": { "type": error_type, "message": message },
            });
            if let Some(request_id) = request_id {
                value["request_id"] = json!(request_id);
            }
            value
        }
        Surface::Openai => json!({ "error": { "message": message, "type": error_type } }),
    }
}

/// Serialize the surface error envelope to bytes (for the E2EE generated path).
pub fn envelope_bytes(
    surface: Surface,
    error_type: &str,
    message: &str,
    request_id: Option<&str>,
) -> Vec<u8> {
    serde_json::to_vec(&envelope(surface, error_type, message, request_id)).unwrap_or_default()
}

fn rate_limit_envelope(surface: Surface, message: &str, request_id: Option<&str>) -> Value {
    let mut body = envelope(surface, "rate_limit_error", message, request_id);
    // OpenAI clients expect a string error code on rate limits.
    if surface == Surface::Openai {
        body["error"]["code"] = json!("rate_limit_exceeded");
    }
    body
}

/// Serialize the rate-limit envelope to bytes (for the E2EE generated path).
pub fn rate_limit_envelope_bytes(
    surface: Surface,
    message: &str,
    request_id: Option<&str>,
) -> Vec<u8> {
    serde_json::to_vec(&rate_limit_envelope(surface, message, request_id)).unwrap_or_default()
}

/// The standard rate-limit response headers (`X-RateLimit-*`, `Retry-After`).
pub fn rate_limit_headers(limit: i64, reset_at: i64) -> Vec<(&'static str, String)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let retry_after = (reset_at - now).max(1);
    vec![
        ("X-RateLimit-Limit", limit.to_string()),
        ("X-RateLimit-Remaining", "0".to_string()),
        ("X-RateLimit-Reset", reset_at.to_string()),
        ("Retry-After", retry_after.to_string()),
    ]
}

fn json_response(body: &Value, status: u16, extra_headers: &[(&str, String)]) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    for (name, value) in extra_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    (
        status,
        headers,
        serde_json::to_vec(body).unwrap_or_default(),
    )
        .into_response()
}

/// Build a client-facing error response in the right envelope for `surface`.
pub fn error_response(
    surface: Surface,
    status: u16,
    error_type: &str,
    message: &str,
    request_id: Option<&str>,
) -> Response {
    json_response(
        &envelope(surface, error_type, message, request_id),
        status,
        &[],
    )
}

/// A 429 response carrying the standard rate-limit headers.
pub fn rate_limit_response(
    surface: Surface,
    message: &str,
    limit: i64,
    reset_at: i64,
    request_id: Option<&str>,
) -> Response {
    json_response(
        &rate_limit_envelope(surface, message, request_id),
        429,
        &rate_limit_headers(limit, reset_at),
    )
}

fn extract_error_message(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    match value.get("error") {
        Some(Value::String(message)) => Some(message.clone()),
        Some(error) => error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        None => None,
    }
}

/// Normalize a non-2xx upstream response into the client-facing status and the
/// surface-shaped error body bytes. For actionable client errors the provider's
/// own message is re-wrapped at the original status; everything else gets a
/// generic sanitized message at the mapped status.
pub fn normalize_upstream_error_parts(
    surface: Surface,
    upstream_status: u16,
    body: &[u8],
    request_id: Option<&str>,
) -> (u16, Vec<u8>) {
    if is_actionable_client_error(upstream_status) {
        if let Some(message) = extract_error_message(body) {
            return (
                upstream_status,
                envelope_bytes(
                    surface,
                    error_type(surface, upstream_status),
                    &message,
                    request_id,
                ),
            );
        }
    }
    let status = map_upstream_status(upstream_status);
    (
        status,
        envelope_bytes(
            surface,
            error_type(surface, status),
            upstream_message(upstream_status),
            request_id,
        ),
    )
}

/// Normalize a non-2xx upstream response into a surface-shaped error response.
pub fn normalize_upstream_error(
    surface: Surface,
    upstream_status: u16,
    body: &[u8],
    request_id: Option<&str>,
) -> Response {
    if is_actionable_client_error(upstream_status) {
        if let Some(message) = extract_error_message(body) {
            return error_response(
                surface,
                upstream_status,
                error_type(surface, upstream_status),
                &message,
                request_id,
            );
        }
    }
    let status = map_upstream_status(upstream_status);
    error_response(
        surface,
        status,
        error_type(surface, status),
        upstream_message(upstream_status),
        request_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn response_json(response: Response) -> (u16, Value) {
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn openai_envelope_shape() {
        let (status, body) = response_json(error_response(
            Surface::Openai,
            400,
            "invalid_request_error",
            "bad",
            None,
        ))
        .await;
        assert_eq!(status, 400);
        assert_eq!(
            body,
            json!({ "error": { "message": "bad", "type": "invalid_request_error" } })
        );
    }

    #[tokio::test]
    async fn anthropic_envelope_shape_with_request_id() {
        let (status, body) = response_json(error_response(
            Surface::Anthropic,
            404,
            "not_found_error",
            "missing",
            Some("req-1"),
        ))
        .await;
        assert_eq!(status, 404);
        assert_eq!(
            body,
            json!({
                "type": "error",
                "error": { "type": "not_found_error", "message": "missing" },
                "request_id": "req-1",
            })
        );
    }

    #[tokio::test]
    async fn rate_limit_adds_openai_code_and_headers() {
        let response = rate_limit_response(Surface::Openai, "slow down", 100, 4_000_000_000, None);
        assert_eq!(response.status().as_u16(), 429);
        assert_eq!(response.headers().get("x-ratelimit-limit").unwrap(), "100");
        assert_eq!(
            response.headers().get("x-ratelimit-remaining").unwrap(),
            "0"
        );
        let (_, body) = response_json(response).await;
        assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));
    }

    #[test]
    fn status_tables() {
        assert_eq!(error_type(Surface::Anthropic, 402), "billing_error");
        assert_eq!(error_type(Surface::Openai, 402), "insufficient_quota");
        assert_eq!(error_type(Surface::Anthropic, 500), "api_error");
        assert_eq!(error_type(Surface::Openai, 500), "upstream_error");
        assert_eq!(map_upstream_status(401), 502);
        assert_eq!(map_upstream_status(422), 422);
        assert_eq!(map_upstream_status(503), 503);
        assert!(is_actionable_client_error(400));
        assert!(!is_actionable_client_error(401));
        assert!(!is_actionable_client_error(500));
    }

    #[tokio::test]
    async fn normalize_surfaces_actionable_message_and_sanitizes_rest() {
        let (status, body) = response_json(normalize_upstream_error(
            Surface::Openai,
            400,
            br#"{"error":{"message":"missing field foo"}}"#,
            None,
        ))
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["message"], json!("missing field foo"));

        let (status, body) = response_json(normalize_upstream_error(
            Surface::Openai,
            500,
            br#"{"error":{"message":"upstream secret"}}"#,
            None,
        ))
        .await;
        assert_eq!(status, 502);
        assert_eq!(
            body["error"]["message"],
            json!("The upstream provider returned an error")
        );
    }
}
