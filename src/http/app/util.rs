//! Small request/response helpers: header parsing, host normalization,
//! forward-candidate construction, owner/admin guards, and error responses.

use axum::{
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use serde_json::{json, Value};

use super::*;

use crate::aggregator::service::{
    E2eeError, ForwardCandidate, ReceiptOwner, ServiceError, UpstreamVerificationError,
};
use crate::aggregator::upstream_config::UpstreamConfigError;

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
    attempts: usize,
    session_id: Option<&str>,
) {
    if let Ok(value) = HeaderValue::from_str(selected_route) {
        headers.insert(
            HeaderName::from_static("x-private-ai-gateway-selected-route"),
            value,
        );
    }
    if let Ok(value) = HeaderValue::from_str(&attempts.to_string()) {
        headers.insert(
            HeaderName::from_static("x-private-ai-gateway-attempts"),
            value,
        );
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

pub(super) fn unsupported_e2ee_response() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "e2ee_invalid_version",
        "ACI E2EE is not supported by this service",
    )
}

pub(super) fn invalid_signing_algo_response() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_signing_algo",
        "Invalid signing algorithm. Must be 'ed25519' or 'ecdsa'",
    )
}

pub(super) fn e2ee_error_response(err: E2eeError) -> Response {
    match err {
        E2eeError::EncryptionFailed => internal_error_response(ServiceError::E2ee(err)),
        E2eeError::HeaderMissing => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_header_missing",
            err.to_string(),
        ),
        E2eeError::InvalidSigningAlgo => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_signing_algo",
            err.to_string(),
        ),
        E2eeError::InvalidVersion => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_version",
            err.to_string(),
        ),
        E2eeError::InvalidPublicKey => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_public_key",
            err.to_string(),
        ),
        E2eeError::ModelKeyMismatch => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_model_key_mismatch",
            err.to_string(),
        ),
        E2eeError::InvalidNonce => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_nonce",
            err.to_string(),
        ),
        E2eeError::ReplayDetected => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_replay_detected",
            err.to_string(),
        ),
        E2eeError::InvalidTimestamp => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_timestamp",
            err.to_string(),
        ),
        E2eeError::InvalidPayloadModel => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_payload_model",
            err.to_string(),
        ),
        E2eeError::DecryptionFailed => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_decryption_failed",
            err.to_string(),
        ),
    }
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

pub(super) fn insert_str_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(HeaderName::from_static(name), v);
    }
}

pub(super) fn admin_not_found_response() -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "not_found",
        "admin upstream config endpoint is not enabled",
    )
}

pub(super) fn upstream_config_error_response(err: UpstreamConfigError) -> Response {
    match err {
        UpstreamConfigError::InvalidConfig(message) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_upstream_config", message)
        }
        other => {
            tracing::error!(error = %other, "upstream config admin operation failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                other.to_string(),
            )
        }
    }
}

pub(super) fn upstream_verification_error_response(err: UpstreamVerificationError) -> Response {
    let message = err.to_string();
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "upstream_verification_failed",
        message,
    )
}

pub(super) fn error_response(
    status: StatusCode,
    error_type: &str,
    message: impl Into<String>,
) -> Response {
    let body = json!({
        "error": {
            "message": message.into(),
            "type": error_type,
            "code": Value::Null,
            "param": Value::Null,
        }
    });
    (status, Json(body)).into_response()
}

pub(super) fn internal_error_response(err: ServiceError) -> Response {
    tracing::error!(error = %err, "aci service internal error");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        err.to_string(),
    )
}
