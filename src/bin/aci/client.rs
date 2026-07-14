//! HTTPS client for the `aci` CLI: normal WebPKI validation plus per-hostname
//! recording of the observed leaf SPKI sha256 (for the §10.1(6) channel-bound
//! check) and optional per-hostname pin enforcement (fail closed on mismatch).

use std::sync::Arc;

use futures_util::StreamExt;
use private_ai_gateway::aci::upstream::{observing_webpki_client, SpkiObservations};
use rand::RngCore;

const CONNECT_TIMEOUT_SECONDS: u64 = 10;
// Generous read timeout: chat responses stream for a while.
const READ_TIMEOUT_SECONDS: u64 = 600;

/// 32 fresh random bytes, hex-encoded — the attestation request nonce.
pub fn random_nonce_hex() -> String {
    let mut nonce = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

/// Strip a trailing `/` so URL joins stay canonical.
pub fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

pub fn host_of(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL {url:?}: {e}"))?;
    parsed
        .host_str()
        .map(|h| h.to_ascii_lowercase())
        .ok_or_else(|| format!("URL {url:?} has no host"))
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
    http: reqwest::Client,
    observations: Arc<SpkiObservations>,
}

impl AciClient {
    pub fn new() -> Result<Self, String> {
        let observations = Arc::new(SpkiObservations::default());
        let http = observing_webpki_client(
            observations.clone(),
            CONNECT_TIMEOUT_SECONDS,
            READ_TIMEOUT_SECONDS,
        )
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self { http, observations })
    }

    /// The leaf SPKI sha256 (hex) observed on the most recent TLS handshake to
    /// `host`; `None` for hosts never contacted over TLS.
    pub fn observed_spki(&self, host: &str) -> Option<String> {
        self.observations.observed_spki(host)
    }

    /// Enforce `spki_sha256` (hex) on every future TLS handshake to `host`;
    /// a handshake presenting any other key fails closed.
    pub fn pin(&self, host: &str, spki_sha256: &str) {
        self.observations.pin(host, spki_sha256);
    }

    /// A request builder on the pinned/recording transport. The local proxy
    /// (`aci serve`) uses it to forward arbitrary methods and paths upstream so
    /// every hop still enforces the attested SPKI pin.
    pub fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.http.request(method, url)
    }

    pub async fn get(&self, url: &str, bearer: Option<&str>) -> Result<HttpResult, String> {
        let mut req = self.http.get(url);
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("GET {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("GET {url}: failed to read body: {e}"))?
            .to_vec();
        Ok(HttpResult {
            status,
            headers,
            body,
        })
    }

    pub async fn fetch_attestation(
        &self,
        base_url: &str,
        nonce: &str,
    ) -> Result<HttpResult, String> {
        self.get(
            &format!("{base_url}/v1/aci/attestation?nonce={nonce}"),
            None,
        )
        .await
    }

    pub async fn fetch_receipt(
        &self,
        base_url: &str,
        receipt_id: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, String> {
        self.get(&format!("{base_url}/v1/aci/receipts/{receipt_id}"), bearer)
            .await
    }

    /// Fetch one attested session; the path takes the id's 64-hex digest
    /// without the `sha256:` prefix (§9.1).
    pub async fn fetch_session(
        &self,
        base_url: &str,
        session_id: &str,
    ) -> Result<HttpResult, String> {
        let hex_id = session_id.strip_prefix("sha256:").unwrap_or(session_id);
        self.get(&format!("{base_url}/v1/aci/sessions/{hex_id}"), None)
            .await
    }

    pub async fn fetch_models(
        &self,
        base_url: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, String> {
        self.get(&format!("{base_url}/v1/models"), bearer).await
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
        let url = format!("{base_url}/v1/chat/completions");
        let mut req = self
            .http
            .post(&url)
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
    fn host_extraction_lowercases() {
        assert_eq!(
            host_of("https://API.Example.com/v1").unwrap(),
            "api.example.com"
        );
        assert!(host_of("not a url").is_err());
    }

    /// Live-network check that a registered pin is enforced fail-closed:
    /// a handshake presenting any other key must abort the connection.
    /// Run with: cargo test --bin aci -- --ignored pin_mismatch
    #[tokio::test]
    #[ignore]
    async fn pin_mismatch_fails_closed_live() {
        let client = AciClient::new().unwrap();
        client.pin("api.redpill.ai", &"00".repeat(32));
        let err = client
            .get("https://api.redpill.ai/health", None)
            .await
            .expect_err("wrong pin must abort the handshake");
        assert!(err.contains("failed"), "unexpected error: {err}");
    }
}
