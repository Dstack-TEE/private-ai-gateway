//! The gateway's optional middleware.
//!
//! When the gateway's optional `middleware` config section is present, the
//! gateway consults the control plane, applies request/response transforms, and
//! forwards completions through the service in-process, and it relays model
//! catalogs from the control plane.

pub mod completion;
pub mod config;
pub mod control;
pub mod errors;
pub mod pricing;
pub mod reasoning;
pub mod request_features;
pub mod request_transform;
pub mod response_transform;
pub mod sse;
pub mod stream_transform;
pub mod types;

use std::collections::HashSet;

use axum::{
    http::{header::CONTENT_TYPE, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

pub use completion::CompletionInput;
pub use config::MiddlewareConfig;
pub use control::{hash_api_key, ControlClient};

use crate::aggregator::service::AciService;
use errors::Surface;

/// Middleware handle held by the gateway's app state.
pub struct Middleware {
    control: ControlClient,
    sse_keepalive_ms: Option<u64>,
    /// See `MiddlewareConfig::send_request_features`; `None` means on.
    send_request_features: bool,
    /// See `MiddlewareConfig::prefix_hash_secret`.
    prefix_hash_secret: Option<String>,
    /// Normalized (lowercased) TEE-only host set; see `MiddlewareConfig::tee_only_domains`.
    tee_only_domains: HashSet<String>,
}

impl Middleware {
    pub fn new(config: &MiddlewareConfig) -> Result<Self, String> {
        // A weak key would silently void the documented guarantee ("control
        // cannot dictionary-test"): HMAC with an empty or guessable secret is
        // as computable as the plain hash. Refuse to start rather than run
        // with a promise the configuration cannot keep.
        if let Some(secret) = &config.prefix_hash_secret {
            if secret.trim().len() < 32 {
                return Err(format!(
                    "middleware.prefix_hash_secret is {} bytes after trimming; a keyed \
                     prefix hash needs a random secret of at least 32 bytes — unset it \
                     entirely for the (documented) plain-SHA-256 fallback",
                    secret.trim().len()
                ));
            }
        }
        Ok(Self {
            control: ControlClient::new(config)?,
            sse_keepalive_ms: config.sse_keepalive_ms,
            send_request_features: config.send_request_features.unwrap_or(true),
            prefix_hash_secret: config.prefix_hash_secret.clone(),
            tee_only_domains: config
                .tee_only_domains
                .iter()
                .map(|d| d.trim().to_ascii_lowercase())
                .filter(|d| !d.is_empty())
                .collect(),
        })
    }

    /// Whether `host` (an already-normalized `Host` domain from
    /// `request_host_domain`) is a TEE-only host. On these hosts the catalog is
    /// forced to `?tee=true` and completions are forced to attested serving.
    pub fn is_tee_only_domain(&self, host: &str) -> bool {
        self.tee_only_domains.contains(host)
    }

    /// Whether any TEE-only host is configured. A request whose host cannot
    /// be resolved must not bypass those hosts (§1.2 fail closed).
    pub fn has_tee_only_domains(&self) -> bool {
        !self.tee_only_domains.is_empty()
    }

    /// Relay a `/v1/...` catalog request to the control plane, which serves
    /// catalogs without the `/v1` prefix. The control body is returned verbatim
    /// with its status and a forced JSON content type.
    pub async fn handle_catalog(&self, v1_path: &str) -> Response {
        let control_path = v1_path.strip_prefix("/v1").unwrap_or(v1_path);
        match self.control.catalog_get(control_path).await {
            Ok(catalog) => {
                let status =
                    StatusCode::from_u16(catalog.status).unwrap_or(StatusCode::BAD_GATEWAY);
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                (status, headers, catalog.body).into_response()
            }
            Err(err) => {
                tracing::error!(error = %err, path = control_path, "control catalog request failed");
                errors::error_response(
                    Surface::Openai,
                    502,
                    errors::error_type(Surface::Openai, 502),
                    "control plane unavailable",
                    None,
                )
            }
        }
    }

    /// Run the completion flow: consult the control plane, shape
    /// candidate bodies, forward through the service, and finalize the response.
    pub async fn handle_completion(
        &self,
        service: &AciService,
        input: CompletionInput,
    ) -> Response {
        completion::run(
            &self.control,
            service,
            self.sse_keepalive_ms,
            self.send_request_features,
            self.prefix_hash_secret.as_deref(),
            input,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, routing::get, Json, Router};
    use serde_json::json;
    use tokio::net::TcpListener;

    // Spawn a minimal stub control plane and return its base URL.
    async fn spawn_stub_control() -> String {
        let app = Router::new().route(
            "/models",
            get(|| async { Json(json!({ "data": ["alpha", "beta"] })) }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[test]
    fn refuses_a_prefix_hash_secret_too_weak_to_key_anything() {
        let config = |secret: Option<&str>| MiddlewareConfig {
            control_url: "http://control.example".to_string(),
            control_token: None,
            control_timeout_ms: None,
            control_post_timeout_ms: None,
            sse_keepalive_ms: None,
            send_request_features: None,
            prefix_hash_secret: secret.map(str::to_string),
            tee_only_domains: Vec::new(),
        };
        // Empty and short secrets would make the HMAC as computable as the
        // plain hash while the docs promise otherwise — starting up would be
        // running with a broken promise.
        for weak in ["", "   ", "test", "0123456789012345678901234567890"] {
            assert!(Middleware::new(&config(Some(weak))).is_err(), "{weak:?}");
        }
        assert!(Middleware::new(&config(Some(&"x".repeat(32)))).is_ok());
        assert!(Middleware::new(&config(None)).is_ok());
    }

    #[tokio::test]
    async fn handle_catalog_relays_control_response() {
        let base_url = spawn_stub_control().await;
        let middleware = Middleware::new(&MiddlewareConfig {
            control_url: base_url,
            control_token: None,
            control_timeout_ms: Some(2_000),
            control_post_timeout_ms: Some(2_000),
            sse_keepalive_ms: None,
            send_request_features: None,
            prefix_hash_secret: None,
            tee_only_domains: Vec::new(),
        })
        .unwrap();

        let response = middleware.handle_catalog("/v1/models").await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({ "data": ["alpha", "beta"] }));
    }

    #[tokio::test]
    async fn handle_catalog_reports_control_unavailable() {
        let middleware = Middleware::new(&MiddlewareConfig {
            control_url: "http://127.0.0.1:1".to_string(),
            control_token: None,
            control_timeout_ms: Some(200),
            control_post_timeout_ms: Some(200),
            sse_keepalive_ms: None,
            send_request_features: None,
            prefix_hash_secret: None,
            tee_only_domains: Vec::new(),
        })
        .unwrap();

        let response = middleware.handle_catalog("/v1/models").await;
        assert_eq!(response.status().as_u16(), 502);
    }

    #[test]
    fn is_tee_only_domain_matches_normalized_hosts() {
        let middleware = Middleware::new(&MiddlewareConfig {
            control_url: "http://control.invalid".to_string(),
            control_token: None,
            control_timeout_ms: Some(200),
            control_post_timeout_ms: Some(200),
            sse_keepalive_ms: None,
            send_request_features: None,
            prefix_hash_secret: None,
            tee_only_domains: vec!["Tee.Example.com".to_string(), "  ".to_string()],
        })
        .unwrap();
        // Config entries are lowercased on load; the lookup key is an
        // already-normalized host from `request_host_domain`.
        assert!(middleware.is_tee_only_domain("tee.example.com"));
        assert!(!middleware.is_tee_only_domain("api.example.com"));
        // Blank entries are dropped, so an empty host never matches.
        assert!(!middleware.is_tee_only_domain(""));
    }
}
