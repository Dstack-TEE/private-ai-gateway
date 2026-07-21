//! Client-facing error payloads, shaped per downstream API surface.
//!
//! The envelope a surface's SDK parses. HTTP response construction stays in
//! `middleware::errors`, which re-exports these; the definitions live here
//! because the service layer builds them too, when it ends a broken response
//! stream with an in-protocol error.

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

pub(crate) fn envelope(
    surface: Surface,
    error_type: &str,
    message: &str,
    request_id: Option<&str>,
) -> Value {
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
