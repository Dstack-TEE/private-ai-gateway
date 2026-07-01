//! Control-plane HTTP client.
//!
//! Reaches the control plane at `control_url` with an optional bearer token. The
//! pre-request consult gates authorization and fails closed: any failure blocks
//! the request rather than letting it through unauthorized. The post-request
//! report is best-effort and never retried. Connections are kept alive and
//! reused (default pooling); the idempotent consult and catalog requests retry
//! once on a connection-level failure, so a keep-alive connection the control
//! plane closed does not surface as a spurious denial.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::config::MiddlewareConfig;
use super::types::{PostReport, PreConsult};

const DEFAULT_CONTROL_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_CONTROL_POST_TIMEOUT_MS: u64 = 10_000;

/// SHA-256 hex of the bearer key — only the hash crosses to the control plane.
pub fn hash_api_key(api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// A failed control-plane request (transport error, timeout, or read failure).
#[derive(Debug)]
pub struct ControlError(String);

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ControlError {}

/// A relayed control-plane catalog response.
pub struct CatalogResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct ControlClient {
    client: reqwest::Client,
    base_url: String,
    token: Option<String>,
    pre_timeout: Duration,
    post_timeout: Duration,
}

impl ControlClient {
    pub fn new(config: &MiddlewareConfig) -> Result<Self, String> {
        let control_url = config.control_url.trim();
        if control_url.is_empty() {
            return Err("middleware.control_url must not be empty".to_string());
        }
        // Keep control-plane connections alive (reqwest's default pooling) so the
        // hot consult path avoids a fresh TCP + TLS handshake per request. A
        // pooled connection the control plane closed can surface as a broken
        // send; the idempotent consult/catalog requests retry once on a fresh
        // connection (see `send_idempotent`) rather than failing the request.
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| format!("failed to build control HTTP client: {err}"))?;
        Ok(Self {
            client,
            base_url: control_url.trim_end_matches('/').to_string(),
            token: config
                .control_token
                .as_ref()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
            pre_timeout: Duration::from_millis(
                config
                    .control_timeout_ms
                    .unwrap_or(DEFAULT_CONTROL_TIMEOUT_MS),
            ),
            post_timeout: Duration::from_millis(
                config
                    .control_post_timeout_ms
                    .unwrap_or(DEFAULT_CONTROL_POST_TIMEOUT_MS),
            ),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authorize(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => builder.header("authorization", format!("Bearer {token}")),
            None => builder,
        }
    }

    /// Send an idempotent request (the pre-consult decision query or a catalog
    /// GET), rebuilding it per attempt. Retries once on a connection-level
    /// failure: a keep-alive connection the control plane closed surfaces as a
    /// broken send that a fresh connection succeeds on. It does not retry a
    /// timeout (the control plane is genuinely slow; a retry would only double
    /// the wait) or a connect failure (the control plane is unreachable; a retry
    /// cannot help). The consult is a decision query with no persisted side
    /// effect, so repeating it is safe.
    async fn send_idempotent(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> reqwest::Result<reqwest::Response> {
        match build().send().await {
            Ok(response) => Ok(response),
            Err(err) if !err.is_timeout() && !err.is_connect() => {
                tracing::debug!(
                    error = %err,
                    "control request failed on a pooled connection; retrying once"
                );
                build().send().await
            }
            Err(err) => Err(err),
        }
    }

    /// Pre-request consult: `{ apiKeyHash?, model?, provider? }` -> decision.
    /// Fails closed — any non-200, invalid JSON, timeout, or transport error
    /// returns a 503 denial.
    pub async fn consult_pre(
        &self,
        model: Option<&str>,
        api_key_hash: Option<&str>,
        provider: Option<&Value>,
    ) -> PreConsult {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            api_key_hash: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            model: Option<&'a str>,
            // Forwarded verbatim so the control plane validates it (a malformed
            // block must not silently drop the caller's routing restrictions).
            #[serde(skip_serializing_if = "Option::is_none")]
            provider: Option<&'a Value>,
        }

        let body = Body {
            api_key_hash,
            model,
            provider,
        };
        let build = || {
            self.authorize(self.client.post(self.url("/consult/pre")))
                .timeout(self.pre_timeout)
                .json(&body)
        };

        match self.send_idempotent(build).await {
            Ok(response) => {
                let status = response.status().as_u16();
                let text = response.text().await.unwrap_or_default();
                if status != 200 {
                    tracing::error!(
                        status,
                        body = %truncate(&text, 300),
                        "consult_pre returned non-200"
                    );
                    return fail_closed();
                }
                match serde_json::from_str::<PreConsult>(&text) {
                    Ok(consult) => consult,
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            body = %truncate(&text, 300),
                            "consult_pre returned invalid JSON"
                        );
                        fail_closed()
                    }
                }
            }
            Err(err) => {
                tracing::error!(error = %err, "consult_pre request failed");
                fail_closed()
            }
        }
    }

    /// Post-request usage report. Best-effort: a control-plane hiccup must never
    /// fail the already-served response, and the report is never retried.
    pub async fn consult_post(&self, report: &PostReport) {
        let request = self
            .authorize(self.client.post(self.url("/consult/post")))
            .timeout(self.post_timeout)
            .json(report);
        if let Err(err) = request.send().await {
            tracing::error!(error = %err, "consult_post request failed");
        }
    }

    /// Fetch a model catalog from the control plane and relay it verbatim.
    /// Bounded by the consult timeout so a hung control plane cannot stall a
    /// catalog request.
    pub async fn catalog_get(&self, path: &str) -> Result<CatalogResponse, ControlError> {
        let build = || {
            self.authorize(self.client.get(self.url(path)))
                .timeout(self.pre_timeout)
        };
        let response = self
            .send_idempotent(build)
            .await
            .map_err(|err| ControlError(err.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|err| ControlError(err.to_string()))?;
        Ok(CatalogResponse {
            status,
            body: body.to_vec(),
        })
    }
}

fn fail_closed() -> PreConsult {
    PreConsult {
        allow: false,
        status: Some(503),
        message: Some("control plane unavailable".to_string()),
        pricing: None,
        candidates: None,
        user_id: None,
        virtual_key_id: None,
        spend_mode: None,
        user_tier: None,
        rate_limit: None,
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str) -> MiddlewareConfig {
        MiddlewareConfig {
            control_url: url.to_string(),
            control_token: None,
            control_timeout_ms: Some(200),
            control_post_timeout_ms: Some(200),
            sse_keepalive_ms: None,
        }
    }

    #[test]
    fn hash_api_key_matches_known_vector() {
        // SHA-256 of the empty string.
        assert_eq!(
            hash_api_key(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn new_strips_trailing_slash() {
        let client = ControlClient::new(&config("http://control.example/")).unwrap();
        assert_eq!(
            client.url("/consult/pre"),
            "http://control.example/consult/pre"
        );
    }

    #[tokio::test]
    async fn consult_pre_fails_closed_on_transport_error() {
        // Port 1 is unroutable in practice; the request fails fast within the
        // configured timeout and must deny.
        let client = ControlClient::new(&config("http://127.0.0.1:1")).unwrap();
        let consult = client.consult_pre(Some("m"), None, None).await;
        assert!(!consult.allow);
        assert_eq!(consult.status, Some(503));
        assert_eq!(
            consult.message.as_deref(),
            Some("control plane unavailable")
        );
    }
}
