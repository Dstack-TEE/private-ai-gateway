//! Small request/response helpers: header parsing, host normalization,
//! owner/admin guards, and error responses.

use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};

use crate::aggregator::service::ReceiptOwner;

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

pub(super) fn enforce_api(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = state.api_token.as_deref() else {
        return None;
    };
    let Some(token) = extract_bearer(headers) else {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "api bearer token required",
        ));
    };
    if token == expected {
        None
    } else {
        Some(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "invalid api bearer token",
        ))
    }
}
