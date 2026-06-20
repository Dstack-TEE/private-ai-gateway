//! Small request/response helpers: header parsing, host normalization,
//! forward-candidate construction, owner/admin guards, and error responses.

use axum::{
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use serde_json::Value;

use crate::aggregator::service::{ForwardCandidate, ReceiptOwner};

use super::error_responses::{admin_not_found_response, error_response};
use super::AppState;

pub(super) fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

pub(super) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// Parse the `x-private-ai-gateway-targets` header + request body into
/// ordered failover candidates. Supports the simple form (one shared body +
/// the ordered targets header) and the envelope form
/// (`{"candidates":[{"target","body"},...]}`, where each candidate carries
/// its own body). Returns `(candidates, stream_flag)` or an error `Response`.
#[allow(clippy::result_large_err)]
pub(super) fn build_forward_candidates(
    headers: &HeaderMap,
    body: &[u8],
    parsed: &Value,
) -> Result<(Vec<ForwardCandidate>, bool), Response> {
    let header_targets: Vec<String> = header_str(headers, "x-private-ai-gateway-targets")
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // Envelope form: a top-level `candidates` array of {target, body}.
    if let Some(items) = parsed.get("candidates").and_then(Value::as_array) {
        let mut candidates = Vec::with_capacity(items.len());
        let mut envelope_targets = Vec::with_capacity(items.len());
        for item in items {
            let Some(target) = item.get("target").and_then(Value::as_str) else {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_internal_request",
                    "candidate is missing a string target",
                ));
            };
            let target = target.trim();
            if target.is_empty() {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_internal_request",
                    "candidate has an empty target",
                ));
            }
            let Some(body_value) = item.get("body") else {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_internal_request",
                    "candidate is missing a body",
                ));
            };
            let body_bytes = match serde_json::to_vec(body_value) {
                Ok(bytes) => bytes,
                Err(e) => {
                    return Err(error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        format!("invalid candidate body: {e}"),
                    ));
                }
            };
            envelope_targets.push(target.to_string());
            candidates.push(ForwardCandidate {
                route_id: target.to_string(),
                body: body_bytes,
            });
        }
        if candidates.is_empty() {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_internal_request",
                "candidate envelope is empty",
            ));
        }
        if !header_targets.is_empty() && header_targets != envelope_targets {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_internal_request",
                "x-private-ai-gateway-targets does not match the candidate envelope",
            ));
        }
        // The backend picks buffered vs streaming once for the whole
        // failover list, so every candidate must agree on `stream`.
        let stream = items[0]
            .get("body")
            .and_then(|b| b.get("stream"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mismatched_stream = items.iter().any(|item| {
            item.get("body")
                .and_then(|b| b.get("stream"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
                != stream
        });
        if mismatched_stream {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_internal_request",
                "all candidates must agree on the stream flag",
            ));
        }
        return Ok((candidates, stream));
    }

    // Simple form: one shared body forwarded to each ordered target.
    if header_targets.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_internal_request",
            "missing X-Private-AI-Gateway-Targets",
        ));
    }
    let stream = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let candidates = header_targets
        .into_iter()
        .map(|route_id| ForwardCandidate {
            route_id,
            body: body.to_vec(),
        })
        .collect();
    Ok((candidates, stream))
}

/// Set the route-attribution response headers for the caller. These are
/// internal only; the frontend strips any leaked `x-private-ai-gateway-*`
/// before the user sees the response.
pub(super) fn insert_attribution_headers(
    headers: &mut HeaderMap,
    selected_route: &str,
    failed_attempts: &[(String, u16)],
    session_id: Option<&str>,
) {
    if let Ok(value) = HeaderValue::from_str(selected_route) {
        headers.insert(
            HeaderName::from_static("x-private-ai-gateway-selected-route"),
            value,
        );
    }
    // Failed-over candidates as `route_id=status`, comma-separated in the order
    // tried. Route ids and statuses are ASCII; `,`/`=` are valid header bytes.
    if !failed_attempts.is_empty() {
        let encoded = failed_attempts
            .iter()
            .map(|(route, status)| format!("{route}={status}"))
            .collect::<Vec<_>>()
            .join(",");
        if let Ok(value) = HeaderValue::from_str(&encoded) {
            headers.insert(
                HeaderName::from_static("x-private-ai-gateway-failed-attempts"),
                value,
            );
        }
    }
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            headers.insert(
                HeaderName::from_static("x-private-ai-gateway-session-id"),
                value,
            );
        }
    }
}

pub(super) fn request_host_domain(headers: &HeaderMap) -> Option<String> {
    normalize_host_domain(header_str(headers, "host")?)
}

pub(super) fn normalize_host_domain(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let host = if let Some(rest) = raw.strip_prefix('[') {
        let end = rest.find(']')?;
        &rest[..end]
    } else {
        raw.split_once(':').map_or(raw, |(host, _)| host)
    };
    let domain = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.contains('/')
        || domain.contains('=')
        || domain.contains(',')
        || domain.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(domain)
}

pub(super) fn has_e2ee_headers(headers: &HeaderMap) -> bool {
    [
        "x-signing-algo",
        "x-client-pub-key",
        "x-model-pub-key",
        "x-e2ee-version",
        "x-e2ee-nonce",
        "x-e2ee-timestamp",
    ]
    .into_iter()
    .any(|name| headers.contains_key(name))
}

/// Returns `Some(response)` when the caller MUST be rejected; returns
/// `None` to indicate "auth passed (or not required), proceed".
pub(super) fn enforce_owner(
    state: &AppState,
    headers: &HeaderMap,
    receipt_id: &str,
) -> Option<Response> {
    // Anonymous receipts: any caller may retrieve them.
    let recorded_owner = state.service.owner_of_receipt(receipt_id)?;
    let Some(token) = extract_bearer(headers) else {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "this receipt is owned; authenticate with the original bearer token",
        ));
    };
    if ReceiptOwner::from_bearer(&token) == recorded_owner {
        None
    } else {
        Some(error_response(
            StatusCode::FORBIDDEN,
            "redaction_required",
            "the presented credential does not match the receipt owner",
        ))
    }
}

pub(super) fn enforce_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = state.admin_token.as_deref() else {
        return Some(admin_not_found_response());
    };
    let Some(token) = extract_bearer(headers) else {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "admin bearer token required",
        ));
    };
    if token == expected {
        None
    } else {
        Some(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "invalid admin bearer token",
        ))
    }
}
