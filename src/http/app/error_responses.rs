//! Error/diagnostic HTTP response constructors shared across handlers.

use axum::{
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::aggregator::service::{E2eeError, ServiceError, UpstreamVerificationError};
use crate::aggregator::upstream_config::UpstreamConfigError;

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

pub(super) fn internal_error_response(err: ServiceError) -> Response {
    tracing::error!(error = %err, "aci service internal error");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        err.to_string(),
    )
}
