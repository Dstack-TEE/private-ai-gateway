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
pub mod request_transform;
pub mod response_transform;
pub mod sse;
pub mod stream_transform;
pub mod types;

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
    timings: completion::StreamTimings,
}

impl Middleware {
    pub fn new(config: &MiddlewareConfig) -> Result<Self, String> {
        Ok(Self {
            control: ControlClient::new(config)?,
            timings: completion::StreamTimings {
                sse_keepalive_ms: config.sse_keepalive_ms,
                stream_commit_grace_ms: config.stream_commit_grace_ms,
                stream_first_byte_timeout_ms: config.stream_first_byte_timeout_ms,
            },
        })
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
    /// Takes the service by `Arc`: on streaming early commit the forward keeps
    /// running inside the response body, beyond this call's borrows.
    pub async fn handle_completion(
        &self,
        service: std::sync::Arc<AciService>,
        input: CompletionInput,
    ) -> Response {
        completion::run(&self.control, service, self.timings, input).await
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

    #[tokio::test]
    async fn handle_catalog_relays_control_response() {
        let base_url = spawn_stub_control().await;
        let middleware = Middleware::new(&MiddlewareConfig {
            control_url: base_url,
            control_token: None,
            control_timeout_ms: Some(2_000),
            control_post_timeout_ms: Some(2_000),
            sse_keepalive_ms: None,
            stream_commit_grace_ms: None,
            stream_first_byte_timeout_ms: None,
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
            stream_commit_grace_ms: None,
            stream_first_byte_timeout_ms: None,
        })
        .unwrap();

        let response = middleware.handle_catalog("/v1/models").await;
        assert_eq!(response.status().as_u16(), 502);
    }
}
