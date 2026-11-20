//! Error/diagnostic HTTP response constructors shared across handlers.

use axum::{
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::aggregator::service::{E2eeError, ServiceError};
use crate::aggregator::upstream_config::UpstreamConfigError;
use crate::middleware::errors::{service_error_response, Surface};

pub(super) fn unsupported_e2ee_response() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "e2ee_invalid_version",
        "E2EE v2 is not supported by this service",
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
    internal_error_response(ServiceError::E2ee(err))
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

pub(super) fn unknown_downstream_host_response(err: ServiceError) -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", err.to_string())
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

/// The response for a service failure on an endpoint that speaks the §10
/// (OpenAI) error shape and has no request id.
pub(super) fn internal_error_response(err: ServiceError) -> Response {
    service_error_response(Surface::Openai, &err, None)
}
