//! HTTPS client for Private AI Proxy's ACI commands.
//!
//! No WebPKI chain validation: ACI's root of trust is the attested keyset
//! (§1.1), not a CA. The handshake records the observed leaf SPKI sha256
//! per hostname (the spec 9.1(6) channel check) and enforces a registered
//! per-hostname pin set, fail closed on mismatch.

use std::sync::{Arc, RwLock};

use crate::aci::tls::{error_chain, observing_spki_client, SpkiObservations};
use desktop_core::endpoint;
use futures_util::StreamExt;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use url::Url;

const CONNECT_TIMEOUT_SECONDS: u64 = 10;
// Generous read timeout: chat responses stream for a while.
const READ_TIMEOUT_SECONDS: u64 = 600;

/// 32 fresh random bytes, hex-encoded — the attestation request nonce.
pub fn random_nonce_hex() -> String {
    let mut nonce = [0u8; 32];
    rand::fill(&mut nonce);
    hex::encode(nonce)
}

/// Strip a trailing `/` so URL joins stay canonical.
pub fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

/// The attestation report's URL, which carries the request nonce.
pub fn attestation_url(base_url: &str, nonce: &str) -> Result<Url, String> {
    let mut url = endpoint(base_url, &["v1", "aci", "attestation"])?;
    url.query_pairs_mut().append_pair("nonce", nonce);
    Ok(url)
}

/// The URL's host in the form the TLS verifier keys pins and observations
/// by (a rustls `ServerName`): a lowercase DNS name, or an IP address without
/// the brackets an IPv6 literal carries in a URL.
pub fn host_of(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL {url:?}: {e}"))?;
    match parsed.host() {
        Some(url::Host::Domain(domain)) => Ok(domain.to_ascii_lowercase()),
        Some(url::Host::Ipv4(ip)) => Ok(ip.to_string()),
        Some(url::Host::Ipv6(ip)) => Ok(ip.to_string()),
        None => Err(format!("URL {url:?} has no host")),
    }
}

/// The largest receipt document read. A §7.3 receipt is a few fixed members
/// plus one flat object per event (a type, a `sha256:` hash or a few
/// identifiers); the spec test vector is under 1 KiB. 64 KiB leaves room for
/// dozens of implementation events (§7.4) while bounding what an audit holds
/// in memory and saves with usage.
pub const MAX_RECEIPT_BYTES: usize = 64 * 1024;

/// Why a bounded GET produced no response.
#[derive(Debug, thiserror::Error)]
pub enum GetError {
    /// The request or its body read failed.
    #[error("{0}")]
    Failed(String),
    /// The body exceeded this many bytes.
    #[error("response body exceeds {} KiB", .0 / 1024)]
    TooLarge(usize),
}

impl From<GetError> for String {
    fn from(error: GetError) -> Self {
        error.to_string()
    }
}

/// Why a service could not be verified at all: its host could not be reached
/// or did not answer as an ACI service. Authored for the user; it names only
/// the host, never a URL, nonce or transport error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error("Cannot reach {host}. Check the service URL and your network connection.")]
    Unreachable { host: String },
    #[error("{host} refused the connection. Check the service URL and port.")]
    Refused { host: String },
    #[error("{host} did not respond in time. Check the service URL and your network connection.")]
    TimedOut { host: String },
    #[error("A secure connection to {host} could not be established. Check the service URL.")]
    Tls { host: String },
    /// It answered without an attestation report: HTTP 404, or another body.
    #[error("{host} is not a Confidential AI service: it has no attestation report. Check the service URL.")]
    NotAci { host: String },
    #[error("{host} is unavailable right now (HTTP {status}). Try again later.")]
    Unavailable { host: String, status: u16 },
    #[error(
        "{host} answered HTTP {status} instead of an attestation report. Check the service URL."
    )]
    Status { host: String, status: u16 },
}

impl ServiceError {
    /// Classifies an attestation request answered with a non-success status.
    pub fn from_status(host: &str, status: u16) -> Self {
        let host = host.to_string();
        match status {
            404 => Self::NotAci { host },
            429 | 500..=599 => Self::Unavailable { host, status },
            _ => Self::Status { host, status },
        }
    }

    /// Classifies a failed request by the error types in its source chain.
    pub fn from_request(error: &reqwest::Error, host: &str) -> Self {
        let host = host.to_string();
        for cause in error_chain(error) {
            if cause.is::<rustls::Error>() {
                return Self::Tls { host };
            }
            match cause
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind)
            {
                Some(std::io::ErrorKind::ConnectionRefused) => return Self::Refused { host },
                Some(std::io::ErrorKind::TimedOut) => return Self::TimedOut { host },
                _ => {}
            }
        }
        if error.is_timeout() {
            Self::TimedOut { host }
        } else {
            Self::Unreachable { host }
        }
    }
}

/// A buffered response with its exact body bytes as read off the wire.
#[derive(Debug)]
pub struct HttpResult {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: Vec<u8>,
}

impl HttpResult {
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("invalid JSON response: {e}"))
    }

    pub fn error_for_status(&self, what: &str) -> Result<(), String> {
        if (200..300).contains(&self.status) {
            return Ok(());
        }
        Err(format!(
            "{what} returned HTTP {}: {}",
            self.status,
            self.summarize_body()
        ))
    }

    /// A one-line body summary for error messages: HTML pages collapse to a
    /// type+size note, other bodies are trimmed to a short prefix, so pointing
    /// the CLI at the wrong URL does not dump a wall of markup.
    fn summarize_body(&self) -> String {
        const MAX_CHARS: usize = 200;
        let content_type = self
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if content_type.contains("text/html") {
            return format!("(text/html body, {} bytes)", self.body.len());
        }
        let text = String::from_utf8_lossy(&self.body);
        let text = text.trim();
        if text.chars().count() <= MAX_CHARS {
            text.to_string()
        } else {
            let prefix: String = text.chars().take(MAX_CHARS).collect();
            format!("{prefix}… ({} bytes total)", self.body.len())
        }
    }
}

pub struct AciClient {
    /// Replaced on every re-pin: a reqwest `Client` owns its connection pool,
    /// so a fresh one never reuses a keep-alive connection whose key the new
    /// pin set no longer allows.
    http: RwLock<reqwest::Client>,
    observations: Arc<SpkiObservations>,
}

impl AciClient {
    pub fn new() -> Result<Self, String> {
        let observations = Arc::new(SpkiObservations::default());
        let http = RwLock::new(build_http(&observations)?);
        Ok(Self { http, observations })
    }

    fn http(&self) -> reqwest::Client {
        self.http.read().expect("HTTP client lock poisoned").clone()
    }

    /// The leaf SPKI sha256 (hex) observed on the most recent TLS handshake to
    /// `host`; `None` for hosts never contacted over TLS.
    pub fn observed_spki(&self, host: &str) -> Option<String> {
        self.observations.observed_spki(host)
    }

    /// Enforce the pin set `spkis` (sha256 hex) on every future TLS handshake
    /// to `host`; a handshake presenting any other key fails closed. Pooled
    /// connections are dropped with the previous client, so no later request
    /// rides a connection established under the old pin set.
    /// An empty set is refused: leaving the host unpinned would accept any key.
    pub fn pin(&self, host: &str, spkis: &[String]) -> Result<(), String> {
        if spkis.is_empty() {
            return Err(format!(
                "no attested TLS key to pin for {host}; refusing to connect unpinned (fail closed)"
            ));
        }
        let http = build_http(&self.observations)?;
        self.observations.pin(host, spkis);
        *self.http.write().expect("HTTP client lock poisoned") = http;
        Ok(())
    }

    /// The pin set currently enforced for `host`; empty when unpinned.
    pub fn pinned_spkis(&self, host: &str) -> Vec<String> {
        self.observations.pinned_spkis(host)
    }

    /// A request builder on the pinned/recording transport. The local proxy
    /// (`private-ai-proxy serve`) uses it to forward arbitrary methods and paths upstream so
    /// every hop still enforces the attested SPKI pin.
    pub fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.http().request(method, url)
    }

    pub async fn get(&self, url: &str, bearer: Option<&str>) -> Result<HttpResult, String> {
        self.read_get(url, bearer)
            .await
            .map_err(|e| format!("GET {url} failed: {e}"))
    }

    /// A GET read whole, failing with the request's own error.
    async fn read_get(
        &self,
        url: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, reqwest::Error> {
        let resp = self.send_get(url, bearer).await?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp.bytes().await?.to_vec();
        Ok(HttpResult {
            status,
            headers,
            body,
        })
    }

    /// A GET whose body read stops with `GetError::TooLarge` past `limit` bytes.
    async fn get_limited(
        &self,
        url: &str,
        bearer: Option<&str>,
        limit: usize,
    ) -> Result<HttpResult, GetError> {
        let resp = self
            .send_get(url, bearer)
            .await
            .map_err(|e| GetError::Failed(format!("GET {url} failed: {e}")))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = Limited::new(reqwest::Body::from(resp), limit)
            .collect()
            .await
            .map_err(|error| {
                if error.is::<LengthLimitError>() {
                    GetError::TooLarge(limit)
                } else {
                    GetError::Failed(format!("GET {url}: failed to read body: {error}"))
                }
            })?
            .to_bytes()
            .to_vec();
        Ok(HttpResult {
            status,
            headers,
            body,
        })
    }

    async fn send_get(
        &self,
        url: &str,
        bearer: Option<&str>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut req = self.http().get(url);
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        req.send().await
    }

    /// Fetch the attestation report. The caller classifies a failure with
    /// [`ServiceError::from_request`].
    pub async fn fetch_attestation(&self, url: &Url) -> Result<HttpResult, reqwest::Error> {
        self.read_get(url.as_str(), None).await
    }

    pub async fn fetch_receipt(
        &self,
        base_url: &str,
        receipt_id: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, GetError> {
        // The id came from a response header; it becomes a path segment, so
        // constrain it before it can steer the request elsewhere.
        if !is_url_safe_id(receipt_id) {
            return Err(GetError::Failed(format!(
                "receipt id {receipt_id:?} is not a URL-safe id"
            )));
        }
        let url =
            endpoint(base_url, &["v1", "aci", "receipts", receipt_id]).map_err(GetError::Failed)?;
        self.get_limited(url.as_str(), bearer, MAX_RECEIPT_BYTES)
            .await
    }

    /// Fetch one attested session; the path takes the session id exactly as
    /// receipts cite it (bare 64-hex, §8.1).
    pub async fn fetch_session(
        &self,
        base_url: &str,
        session_id: &str,
    ) -> Result<HttpResult, String> {
        if !is_url_safe_id(session_id) {
            return Err(format!("session id {session_id:?} is not a URL-safe id"));
        }
        let url = endpoint(base_url, &["v1", "aci", "sessions", session_id])?;
        self.get(url.as_str(), None).await
    }

    pub async fn fetch_models(
        &self,
        base_url: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, String> {
        let url = endpoint(base_url, &["v1", "models"])?;
        self.get(url.as_str(), bearer).await
    }

    /// POST a chat completion and read the whole body, capturing the exact
    /// wire bytes (streamed and buffered alike). `on_chunk` sees each chunk as
    /// it arrives, in order — the concatenation equals the returned body.
    pub async fn post_chat_captured(
        &self,
        base_url: &str,
        bearer: Option<&str>,
        body: Vec<u8>,
        mut on_chunk: impl FnMut(&[u8]),
    ) -> Result<HttpResult, String> {
        let url = endpoint(base_url, &["v1", "chat", "completions"])?;
        let mut req = self
            .http()
            .post(url.as_str())
            .header("content-type", "application/json")
            .header("accept", "text/event-stream, application/json")
            .body(body);
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("POST {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let mut wire = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("POST {url}: stream error: {e}"))?;
            wire.extend_from_slice(&chunk);
            on_chunk(&chunk);
        }
        Ok(HttpResult {
            status,
            headers,
            body: wire,
        })
    }
}

fn build_http(observations: &Arc<SpkiObservations>) -> Result<reqwest::Client, String> {
    observing_spki_client(
        observations.clone(),
        CONNECT_TIMEOUT_SECONDS,
        READ_TIMEOUT_SECONDS,
    )
    .map_err(|e| format!("failed to build HTTP client: {e}"))
}

/// True for ids safe to embed as one URL path segment (receipt ids, §8.1
/// session ids). Server-supplied values outside this set could otherwise
/// steer the follow-up GET to a different path or query.
fn is_url_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_is_32_random_bytes_hex() {
        let nonce = random_nonce_hex();
        assert_eq!(nonce.len(), 64);
        assert!(nonce.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(nonce, random_nonce_hex());
    }

    #[test]
    fn the_nonce_is_one_encoded_query_value() {
        let url = attestation_url("https://gateway.example/base", "a&nonce=b#c d").unwrap();
        assert_eq!(
            url.as_str(),
            "https://gateway.example/base/v1/aci/attestation?nonce=a%26nonce%3Db%23c+d"
        );
        assert_eq!(url.query_pairs().count(), 1);
        assert_eq!(url.fragment(), None);
        assert_eq!(
            attestation_url("https://[::1]:8443", "00ff")
                .unwrap()
                .as_str(),
            "https://[::1]:8443/v1/aci/attestation?nonce=00ff"
        );
    }

    #[test]
    fn host_extraction_lowercases() {
        assert_eq!(
            host_of("https://API.Example.com/v1").unwrap(),
            "api.example.com"
        );
        assert!(host_of("not a url").is_err());
        // Pins are keyed like rustls server names: no IPv6 brackets.
        assert_eq!(host_of("https://[::1]:8443/v1").unwrap(), "::1");
        assert_eq!(host_of("https://127.0.0.1:8443").unwrap(), "127.0.0.1");
    }

    /// Live-network check that a registered pin is enforced fail-closed:
    /// a handshake presenting any other key must abort the connection.
    /// Run from apps/desktop with: cargo test --package private-ai-proxy --bin private-ai-proxy -- --ignored pin_mismatch
    #[tokio::test]
    #[ignore]
    async fn pin_mismatch_fails_closed_live() {
        let client = AciClient::new().unwrap();
        client.pin("api.redpill.ai", &["00".repeat(32)]).unwrap();
        let err = client
            .get("https://api.redpill.ai/health", None)
            .await
            .expect_err("wrong pin must abort the handshake");
        assert!(err.contains("failed"), "unexpected error: {err}");
    }
}
