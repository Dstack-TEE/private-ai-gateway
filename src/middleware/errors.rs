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

// Re-exported so call sites keep importing client-facing error shapes from one
// place; the definitions are shared because the service layer builds them too.
pub use crate::error_payload::{error_type, upstream_message, Surface};
pub use crate::sse_protocol::{sse_protocol, stream_error_tail, SseProtocol};

use crate::error_payload::envelope;

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

/// The host component of an `http(s)://` URL (`https://a.example/x?y` -> `a.example`).
/// Used to correlate an upstream error message with a request image URL when the
/// message names only the host (e.g. a DNS failure) rather than the full URL.
fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', ':', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    (!host.is_empty()).then_some(host)
}

/// Whether `message` names `host` as a standalone host token. Two guards keep this
/// from misfiring on an unrelated provider error: the host must be domain-like (have
/// a dot), so a bare single-label host such as `internal` can't match the word
/// "internal" in a generic message; and the token must match whole (splitting the
/// message on non-host characters), so `a.co` doesn't match inside `banana.com`.
fn message_references_host(message: &str, host: &str) -> bool {
    host.contains('.')
        && message
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
            .any(|token| token == host)
}

/// The fetchable remote URL of a single content part, across the request shapes this
/// gateway serves on one code path: OpenAI chat `{"type":"image_url","image_url":{"url"}}`,
/// Responses `{"type":"input_image","image_url":"<url>"}` (image_url may be a bare
/// string or an object), and Anthropic `{"type":"image","source":{"type":"url","url"}}`.
/// Data-URI (`data:`) sources carry no fetchable URL and yield `None`.
fn image_part_url(part: &Value) -> Option<&str> {
    match part.get("type").and_then(Value::as_str) {
        Some("image_url") | Some("input_image") => {
            let image_url = part.get("image_url")?;
            image_url
                .as_str()
                .or_else(|| image_url.get("url").and_then(Value::as_str))
        }
        Some("image") => part
            .get("source")
            .filter(|source| source.get("type").and_then(Value::as_str) == Some("url"))
            .and_then(|source| source.get("url"))
            .and_then(Value::as_str),
        _ => None,
    }
}

/// Collect the remote (`http`/`https`) image URLs a request asks the upstream to
/// fetch. Covers every surface served by the completion path — OpenAI chat and
/// Anthropic messages (`messages[].content[]`) and Responses (`input[]`, whose image
/// parts may sit directly in the array or nested under `content`). A cheap substring
/// guard skips the JSON parse entirely when the body has no image content at all.
fn remote_image_urls(request_body: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(request_body) else {
        return Vec::new();
    };
    if !text.contains("image") {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    for key in ["messages", "input"] {
        let Some(items) = value.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            // Chat/messages nest image parts under `content`; Responses may also
            // place a part directly in the `input` array, so check the item itself.
            let nested = item.get("content").and_then(Value::as_array);
            for part in std::iter::once(item).chain(nested.into_iter().flatten()) {
                if let Some(url) = image_part_url(part) {
                    if url.starts_with("https://") || url.starts_with("http://") {
                        urls.push(url.to_string());
                    }
                }
            }
        }
    }
    urls
}

/// When a non-2xx upstream error was caused by a remote image URL in the client's
/// request that the upstream could not fetch, return a normalized, client-facing
/// message and let the caller treat it as a 400. Detection is URL-correlation
/// based: the request carried a remote image URL AND the upstream error message
/// names that URL (or its host). This keeps false positives out — an unrelated 5xx
/// never matches. Returns `None` when it is not an image-input error (caller keeps
/// the existing mapping). The status check runs first, so success responses never
/// touch the request body.
pub fn classify_image_input_error(
    received_body: &[u8],
    upstream_status: u16,
    upstream_body: &[u8],
) -> Option<String> {
    if (200..300).contains(&upstream_status) {
        return None;
    }
    let message = extract_error_message(upstream_body)?;
    let matched = remote_image_urls(received_body).into_iter().find(|url| {
        message.contains(url.as_str())
            || url_host(url).is_some_and(|host| message_references_host(&message, host))
    })?;
    Some(format!(
        "Failed to fetch the image at the provided URL: {matched}. \
         Ensure the URL is correct and publicly accessible."
    ))
}

/// The error-envelope surface for an endpoint path: `/v1/messages` is
/// Anthropic-shaped, everything else OpenAI-shaped.
pub fn surface_for_path(endpoint_path: &str) -> Surface {
    if endpoint_path == crate::aggregator::service::MESSAGES_PATH {
        Surface::Anthropic
    } else {
        Surface::Openai
    }
}

/// The client-facing `(status, envelope bytes)` for an image-input error, or
/// `None` when the upstream error is not one (see
/// [`classify_image_input_error`]). The single place the 400 response for this
/// error is assembled — every serving path applies these parts as-is.
pub fn image_input_error_parts(
    surface: Surface,
    received_body: &[u8],
    upstream_status: u16,
    upstream_body: &[u8],
    request_id: Option<&str>,
) -> Option<(u16, Vec<u8>)> {
    let message = classify_image_input_error(received_body, upstream_status, upstream_body)?;
    Some((
        400,
        envelope_bytes(surface, error_type(surface, 400), &message, request_id),
    ))
}

/// Marker substring an upstream uses to signal it has no available capacity or
/// targets to serve the request. Matched raw against the error body because the
/// message can sit outside the usual `error`/`error.message` fields.
const UPSTREAM_CAPACITY_MARKER: &[u8] = b"exhausted all available targets";

/// The client-facing `(status, envelope bytes)` for an upstream capacity signal,
/// or `None` when the upstream error is not one. An upstream reporting it has no
/// available capacity/targets is busy, not faulty: surface it as `429`
/// (rate-limited) rather than a `5xx` outage, so it is treated as load, not an
/// error. Peer to [`image_input_error_parts`] — both remap a recognized upstream
/// error body to a specific client status.
/// Whether an upstream outcome is a capacity signal: a literal 429, or the
/// recognized 5xx capacity/no-targets body that error normalization also
/// surfaces to clients as 429. The single classification shared by error
/// normalization and the forwarder's capacity-retry targeting — the two must
/// never disagree about what "capacity" means, or a request could be told
/// 429 without ever having been eligible for the retry that 429s get.
pub(crate) fn is_upstream_capacity_signal(status: u16, body: &[u8]) -> bool {
    if status == 429 {
        return true;
    }
    (500..600).contains(&status)
        && body
            .windows(UPSTREAM_CAPACITY_MARKER.len())
            .any(|w| w == UPSTREAM_CAPACITY_MARKER)
}

fn capacity_error_parts(
    surface: Surface,
    upstream_status: u16,
    upstream_body: &[u8],
    request_id: Option<&str>,
) -> Option<(u16, Vec<u8>)> {
    if !(500..600).contains(&upstream_status) {
        return None;
    }
    if !is_upstream_capacity_signal(upstream_status, upstream_body) {
        return None;
    }
    Some((
        429,
        envelope_bytes(
            surface,
            error_type(surface, 429),
            upstream_message(429),
            request_id,
        ),
    ))
}

/// Normalize a non-2xx upstream response into the client-facing status and the
/// surface-shaped error body bytes. For actionable client errors the provider's
/// own message is re-wrapped at the original status; everything else gets a
/// generic sanitized message at the mapped status.
pub fn normalize_upstream_error_parts(
    surface: Surface,
    upstream_status: u16,
    body: &[u8],
    received_body: &[u8],
    request_id: Option<&str>,
) -> (u16, Vec<u8>) {
    // A failed fetch of a client-supplied image URL is the caller's problem, not a
    // provider fault: surface it as a 400 with a message naming the URL.
    if let Some(parts) =
        image_input_error_parts(surface, received_body, upstream_status, body, request_id)
    {
        return parts;
    }
    // A provider signalling it is out of capacity is busy, not broken: 429, not 5xx.
    if let Some(parts) = capacity_error_parts(surface, upstream_status, body, request_id) {
        return parts;
    }
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

/// Wrap `(status, envelope bytes)` parts into a JSON error response.
pub fn parts_response(status: u16, body: Vec<u8>) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    (status, headers, body).into_response()
}

/// Normalize a non-2xx upstream response into a surface-shaped error response.
pub fn normalize_upstream_error(
    surface: Surface,
    upstream_status: u16,
    body: &[u8],
    received_body: &[u8],
    request_id: Option<&str>,
) -> Response {
    let (status, bytes) =
        normalize_upstream_error_parts(surface, upstream_status, body, received_body, request_id);
    parts_response(status, bytes)
}

#[cfg(test)]
mod tests {
    use super::is_upstream_capacity_signal;

    #[test]
    fn capacity_signal_is_429_or_the_marked_5xx_body() {
        assert!(is_upstream_capacity_signal(429, b"anything"));
        assert!(is_upstream_capacity_signal(
            503,
            br#"{"error":"exhausted all available targets"}"#
        ));
        // A plain 5xx is an outage, not capacity.
        assert!(!is_upstream_capacity_signal(503, br#"{"error":"boom"}"#));
        // The marker under a non-5xx status is not the recognized signal.
        assert!(!is_upstream_capacity_signal(
            400,
            b"exhausted all available targets"
        ));
    }

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
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["message"], json!("missing field foo"));

        let (status, body) = response_json(normalize_upstream_error(
            Surface::Openai,
            500,
            br#"{"error":{"message":"upstream secret"}}"#,
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 502);
        assert_eq!(
            body["error"]["message"],
            json!("The upstream provider returned an error")
        );
    }

    #[tokio::test]
    async fn capacity_exhaustion_5xx_remaps_to_429() {
        // An upstream 5xx that reports it has no available capacity/targets is a
        // busy signal (the message sits under `detail`, outside the usual
        // `error` field): the client sees 429 (rate-limited), not a 502 outage.
        let (status, body) = response_json(normalize_upstream_error(
            Surface::Openai,
            500,
            br#"{"detail":"exhausted all available targets to no avail"}"#,
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 429);
        assert_eq!(body["error"]["type"], json!("rate_limit_error"));

        // Regression guard: a plain 500 without the marker still maps to 502.
        let (status, _) = response_json(normalize_upstream_error(
            Surface::Openai,
            500,
            br#"{"error":{"message":"kaboom"}}"#,
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 502);
    }

    #[test]
    fn remote_image_urls_covers_all_shapes_and_skips_data_uris() {
        // OpenAI chat (object form) + data URI skipped.
        let openai = br#"{"messages":[{"role":"user","content":[
            {"type":"text","text":"hi"},
            {"type":"image_url","image_url":{"url":"https://a.example/x.jpg"}},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}
        ]}]}"#;
        assert_eq!(
            remote_image_urls(openai),
            vec!["https://a.example/x.jpg".to_string()]
        );
        // Anthropic native: image `source` of type url; base64 source skipped.
        let anthropic = br#"{"messages":[{"role":"user","content":[
            {"type":"image","source":{"type":"url","url":"https://a.example/anthropic.jpg"}},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}
        ]}]}"#;
        assert_eq!(
            remote_image_urls(anthropic),
            vec!["https://a.example/anthropic.jpg".to_string()]
        );
        // Responses: input[] parts nested under content and directly, image_url as a
        // bare string.
        let responses = br#"{"input":[
            {"role":"user","content":[{"type":"input_image","image_url":"https://a.example/nested.jpg"}]},
            {"type":"input_image","image_url":"https://a.example/direct.jpg"}
        ]}"#;
        assert_eq!(
            remote_image_urls(responses),
            vec![
                "https://a.example/nested.jpg".to_string(),
                "https://a.example/direct.jpg".to_string()
            ]
        );
        // No image content at all -> empty (and the cheap guard skips the parse).
        assert!(remote_image_urls(br#"{"messages":[{"role":"user","content":"hi"}]}"#).is_empty());
    }

    #[test]
    fn classify_image_input_error_matches_url_and_host() {
        // A request carrying one remote image URL.
        fn request(url: &str) -> Vec<u8> {
            serde_json::to_vec(&json!({
                "messages": [{ "role": "user", "content": [
                    { "type": "image_url", "image_url": { "url": url } }
                ]}]
            }))
            .unwrap()
        }
        let req = request("https://halleonard.example/wl/02116757-wl.jpg");
        // Full-URL match (the 403-fetch probe).
        assert!(classify_image_input_error(
            &req,
            500,
            br#"{"error":{"message":"403, message='Forbidden', url='https://halleonard.example/wl/02116757-wl.jpg'"}}"#,
        )
        .is_some());
        // Host-only match (the DNS-failure probe).
        assert!(classify_image_input_error(
            &request("https://files.teleclaw.io/workspace/x.jpg"),
            500,
            br#"{"error":{"message":"Cannot connect to host files.teleclaw.io:443 ssl:default [Name or service not known]"}}"#,
        )
        .is_some());
        // No remote URL in the request (invalid base64 probe) -> not an image-input error.
        assert!(classify_image_input_error(
            &request("data:image/png;base64,bm90YW5pbWFnZQ=="),
            400,
            br#"{"error":{"message":"Failed to load image: cannot identify image file"}}"#,
        )
        .is_none());
        // A 5xx that names an unrelated URL must not be misclassified.
        assert!(classify_image_input_error(
            &req,
            500,
            br#"{"error":{"message":"internal error talking to https://other.example/foo"}}"#,
        )
        .is_none());
        // A bare single-label host must NOT match an unrelated word in a generic
        // provider error (the `contains(host)` false positive).
        assert!(classify_image_input_error(
            &request("https://internal/x.jpg"),
            500,
            br#"{"error":{"message":"internal error"}}"#,
        )
        .is_none());
        // A domain-like host must only match as a whole token, not as a substring of
        // a larger hostname.
        assert!(classify_image_input_error(
            &request("https://a.co/x.jpg"),
            500,
            br#"{"error":{"message":"failed to reach banana.com"}}"#,
        )
        .is_none());
    }
}
