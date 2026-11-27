//! Small request/response helpers: header parsing, host normalization,
//! owner/admin guards, and error responses.

use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};

use crate::aci::e2ee::E2EE_VERSION_V2_1;
use crate::aggregator::service::{E2eeError, E2eeRequestParts, ReceiptOwner};

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

/// Force `tee=true` onto a relayed catalog query string, dropping any
/// client-supplied `tee` value and preserving every other param (e.g. `zdr`).
/// Forcing rather than appending is deliberate: on a TEE-only host a client
/// must not be able to widen the catalog back to the full set via `?tee=false`
/// (nor trip the control plane's strict-`true` 400 on `?tee=anything-else`).
pub(super) fn force_tee_true(query: Option<String>) -> String {
    let mut params: Vec<String> = query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| *p != "tee" && !p.starts_with("tee="))
        .map(str::to_string)
        .collect();
    params.push("tee=true".to_string());
    params.join("&")
}

/// Every request header an E2EE mode reads (legacy, v2, v2.1).
const E2EE_REQUEST_HEADERS: [&str; 7] = [
    "x-signing-algo",
    "x-client-pub-key",
    "x-model-pub-key",
    "x-e2ee-version",
    "x-e2ee-nonce",
    "x-e2ee-timestamp",
    "x-e2ee-request-key",
];

pub(super) fn has_e2ee_headers(headers: &HeaderMap) -> bool {
    E2EE_REQUEST_HEADERS
        .into_iter()
        .any(|name| headers.contains_key(name))
}

/// Parse the E2EE request headers once, into the one value both the version
/// gate and the dispatch read. `None` when the request carries none of them.
///
/// v2 and legacy requests keep their original reading: a value that is not
/// visible ASCII counts as absent and a repeated header yields its first
/// value. A v2.1 request (no `X-Signing-Algo`, `X-E2EE-Version: 2.1`) is
/// re-read strictly: a repeated or non-visible-ASCII header is refused.
pub(super) fn e2ee_request_parts(
    headers: &HeaderMap,
) -> Result<Option<E2eeRequestParts<'_>>, E2eeError> {
    if !has_e2ee_headers(headers) {
        return Ok(None);
    }
    let parts = read_e2ee_headers(|name| Ok(header_str(headers, name)))?;
    if parts.signing_algo.is_some() || parts.version != Some(E2EE_VERSION_V2_1) {
        return Ok(Some(parts));
    }
    read_e2ee_headers(|name| single_header(headers, name)).map(Some)
}

fn read_e2ee_headers<'a>(
    read: impl Fn(&str) -> Result<Option<&'a str>, E2eeError>,
) -> Result<E2eeRequestParts<'a>, E2eeError> {
    Ok(E2eeRequestParts {
        signing_algo: read("x-signing-algo")?,
        client_public_key: read("x-client-pub-key")?,
        model_public_key: read("x-model-pub-key")?,
        version: read("x-e2ee-version")?,
        nonce: read("x-e2ee-nonce")?,
        timestamp: read("x-e2ee-timestamp")?,
        request_key: read("x-e2ee-request-key")?,
    })
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, E2eeError> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(E2eeError::InvalidHeader);
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| E2eeError::InvalidHeader)
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
        // A non-matching credential gets the same answer as a missing
        // receipt: no existence oracle for other tenants' receipts.
        Some(error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "receipt not found",
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

/// Whether an `accept` error is an ordinary per-connection failure (the peer
/// went away between the SYN and the accept) rather than a listener-level
/// problem. Mirrors the same helper in axum: these are skipped silently, while
/// anything else — EMFILE above all — is logged and backed off.
pub(super) fn is_connection_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

#[cfg(test)]
mod tests {
    use super::force_tee_true;

    #[test]
    fn force_tee_true_forces_and_dedupes() {
        // No query at all -> just the forced param.
        assert_eq!(force_tee_true(None), "tee=true");
        assert_eq!(force_tee_true(Some(String::new())), "tee=true");
        // A client `tee` value (including `false`) is dropped, never widening
        // the catalog back to the full set.
        assert_eq!(force_tee_true(Some("tee=false".to_string())), "tee=true");
        assert_eq!(force_tee_true(Some("tee".to_string())), "tee=true");
        // Other params survive, and only the client `tee` is stripped.
        assert_eq!(
            force_tee_true(Some("zdr=true&tee=false".to_string())),
            "zdr=true&tee=true"
        );
        assert_eq!(
            force_tee_true(Some("zdr=true".to_string())),
            "zdr=true&tee=true"
        );
    }
}
