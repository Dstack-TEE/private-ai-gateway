//! Gateway adapters around shared TLS SPKI verification primitives.

use std::collections::HashMap;
use std::sync::Arc;

pub use aci_verify::tls::SpkiObservations;

use super::UpstreamError;

pub(super) fn response_headers(resp: &reqwest::Response) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for (key, value) in resp.headers().iter() {
        if let Ok(value) = value.to_str() {
            headers.insert(key.to_string(), value.to_string());
        }
    }
    headers
}

pub(super) fn pinned_spki_client(
    accepted_spkis: Vec<String>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, UpstreamError> {
    aci_verify::tls::pinned_spki_client(
        accepted_spkis,
        connect_timeout_seconds,
        read_timeout_seconds,
    )
    .map_err(UpstreamError::Transport)
}

pub fn observing_spki_client(
    observations: Arc<SpkiObservations>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, UpstreamError> {
    aci_verify::tls::observing_spki_client(
        observations,
        connect_timeout_seconds,
        read_timeout_seconds,
    )
    .map_err(UpstreamError::Transport)
}
