//! Client-facing error payloads, shaped per downstream API surface.
//!
//! The envelope a surface's SDK parses. HTTP response construction stays in
//! `middleware::errors`, which re-exports these; the definitions live here
//! because the service layer builds them too, when it ends a broken response
//! stream with an in-protocol error.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How upstream-authored error prose is exposed to callers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicErrorPolicy {
    #[default]
    Relay,
    Opaque,
}

/// Only standard request-field paths can be retained in opaque errors. Free
/// text and application-defined schema keys may contain upstream identifiers.
pub(crate) fn public_error_param(param: &str) -> Option<&str> {
    const FIELDS: &[&str] = &[
        "model",
        "messages",
        "role",
        "content",
        "type",
        "text",
        "image_url",
        "url",
        "detail",
        "input",
        "input_audio",
        "data",
        "format",
        "prompt",
        "max_tokens",
        "max_completion_tokens",
        "max_output_tokens",
        "temperature",
        "top_p",
        "top_k",
        "n",
        "stream",
        "stream_options",
        "include_usage",
        "stop",
        "seed",
        "tools",
        "tool_choice",
        "tool_calls",
        "tool_call_id",
        "parallel_tool_calls",
        "function",
        "name",
        "description",
        "parameters",
        "response_format",
        "json_schema",
        "schema",
        "strict",
        "frequency_penalty",
        "presence_penalty",
        "logprobs",
        "top_logprobs",
        "logit_bias",
        "user",
        "reasoning",
        "effort",
        "reasoning_effort",
        "thinking",
        "budget_tokens",
        "dimensions",
        "encoding_format",
        "instructions",
    ];
    (!param.is_empty()
        && param.len() <= 256
        && param
            .split(['.', '[', ']'])
            .filter(|s| !s.is_empty())
            .all(|part| FIELDS.contains(&part) || part.bytes().all(|c| c.is_ascii_digit()))
        && param.bytes().any(|c| c.is_ascii_alphabetic()))
    .then_some(param)
}

/// Fixed prose for known request errors, with at most a standard parameter
/// path. No upstream-authored text is interpolated into the message.
pub(crate) fn opaque_error_message(status: u16, kind: Option<&str>, param: Option<&str>) -> String {
    let message = match kind {
        Some("context_length_exceeded") => "The request exceeds the model's context window",
        Some("missing_required_parameter") => "A required request parameter is missing",
        Some("unknown_parameter" | "unsupported_parameter") => {
            "A request parameter is not supported"
        }
        Some("invalid_value" | "invalid_type" | "unsupported_value") => {
            "A request parameter has an invalid value"
        }
        Some("string_above_max_length") => "A request value exceeds the maximum length",
        Some("content_filter" | "content_policy_violation" | "image_content_policy_violation") => {
            "The request was rejected by the content policy"
        }
        Some(
            "invalid_image"
            | "invalid_image_format"
            | "invalid_image_mode"
            | "invalid_base64_image"
            | "image_parse_error"
            | "unsupported_image_media_type"
            | "empty_image_file",
        ) => "The request contains an invalid or unsupported image",
        Some("invalid_image_url" | "image_file_not_found" | "failed_to_download_image") => {
            "The image at the provided URL could not be fetched"
        }
        Some("image_too_large" | "image_file_too_large") => "The request image is too large",
        Some("image_too_small") => "The request image is too small",
        _ => upstream_message(status),
    };
    match param.and_then(public_error_param) {
        Some(param) => format!("{message} (parameter: {param})"),
        None => message.to_string(),
    }
}

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
            413 => "request_too_large",
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
            // No 413 arm: `request_too_large` is the Anthropic surface's word,
            // and this surface has none — an oversized request is an invalid
            // one here, which the fallthrough already says.
            429 => "rate_limit_error",
            503 => "service_unavailable",
            504 => "timeout_error",
            s if s >= 500 => "upstream_error",
            _ => "invalid_request_error",
        },
    }
}

/// Generic sanitized message for a non-actionable upstream status.
///
/// The 4xx arms matter as much as the 5xx ones: they are what a caller reads
/// when their own request was rejected but the provider's wording could not be
/// relayed, and the generic upstream line would send them to retry a request
/// that cannot succeed.
pub fn upstream_message(upstream_status: u16) -> &'static str {
    match upstream_status {
        400 | 422 => "The request was rejected as invalid",
        401..=403 => "The upstream provider is currently unavailable",
        404 => "The requested model or resource was not found",
        413 => "The request is too large",
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
        Surface::Openai => json!({
            "error": { "message": message, "type": error_type, "code": null, "param": null }
        }),
    }
}
