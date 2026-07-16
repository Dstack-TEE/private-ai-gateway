use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::Value;

use crate::aggregator::service::AciService;

use super::{
    backend::MiddlewareBackend,
    completion::{
        forward_selected, CompletionInput, InternalForwardInput, InternalForwardRequest,
        PendingCompletion,
    },
    errors::{self, Surface},
    request_transform,
    types::ProviderFormat,
};

const DEFAULT_PROXY_TIMEOUT_MS: u64 = 1_800_000;
const REQUEST_ID_HEADER: &str = "x-private-ai-gateway-request-id";
const INTERNAL_TOKEN_HEADER: &str = "x-private-ai-gateway-internal-token";
const USER_TIER_HEADER: &str = "x-user-tier";

#[derive(Clone, Default)]
struct PendingStore {
    inner: Arc<Mutex<HashMap<String, PendingEntry>>>,
}

struct PendingEntry {
    pending: PendingCompletion,
    expires_at: Instant,
}

impl PendingStore {
    fn insert(&self, pending: PendingCompletion, ttl: Duration) {
        let mut inner = self
            .inner
            .lock()
            .expect("pending middleware store poisoned");
        let now = Instant::now();
        inner.retain(|_, entry| entry.expires_at > now);
        inner.insert(
            pending.request_id.clone(),
            PendingEntry {
                pending,
                expires_at: now + ttl,
            },
        );
    }

    fn take(&self, request_id: &str) -> Option<PendingCompletion> {
        let mut inner = self
            .inner
            .lock()
            .expect("pending middleware store poisoned");
        let entry = inner.remove(request_id)?;
        if entry.expires_at <= Instant::now() {
            return None;
        }
        Some(entry.pending)
    }

    fn remove(&self, request_id: &str) {
        self.inner
            .lock()
            .expect("pending middleware store poisoned")
            .remove(request_id);
    }
}

pub struct ProxyBackend {
    client: reqwest::Client,
    base_url: String,
    internal_token: String,
    timeout: Duration,
    sse_keepalive_ms: Option<u64>,
    pending: PendingStore,
}

impl ProxyBackend {
    pub fn new(config: &super::config::MiddlewareConfig) -> Result<Self, String> {
        let base_url = config
            .proxy_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "middleware.proxy_url is required in proxy mode".to_string())?;
        let internal_token = config
            .internal_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "middleware.internal_token is required in proxy mode".to_string())?;
        let timeout =
            Duration::from_millis(config.proxy_timeout_ms.unwrap_or(DEFAULT_PROXY_TIMEOUT_MS));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|err| format!("failed to build proxy HTTP client: {err}"))?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            internal_token: internal_token.to_string(),
            timeout,
            sse_keepalive_ms: config.sse_keepalive_ms,
            pending: PendingStore::default(),
        })
    }

    fn endpoint_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

#[async_trait]
impl MiddlewareBackend for ProxyBackend {
    fn name(&self) -> &'static str {
        "proxy"
    }

    async fn handle_catalog(&self, v1_path: &str) -> Response {
        match self
            .client
            .get(self.endpoint_url(v1_path))
            .timeout(self.timeout)
            .send()
            .await
        {
            Ok(response) => relay_response(response),
            Err(err) => {
                tracing::error!(error = %err, path = v1_path, "proxy catalog request failed");
                errors::error_response(
                    Surface::Openai,
                    502,
                    errors::error_type(Surface::Openai, 502),
                    "middleware proxy unavailable",
                    None,
                )
            }
        }
    }

    async fn handle_completion(&self, _service: &AciService, input: CompletionInput) -> Response {
        let proxy_request = match request_transform::transform_to_provider_request(
            ProviderFormat::Openai,
            &input.params,
            input.endpoint,
            None,
        ) {
            Ok(body) => body,
            Err(err) => {
                let message = format!("failed to shape middleware proxy request: {err}");
                tracing::error!(error = %err, "failed to shape proxy middleware request");
                return errors::error_response(
                    input.surface,
                    500,
                    errors::error_type(input.surface, 500),
                    &message,
                    Some(&input.request_id),
                );
            }
        };
        let proxy_body = match serde_json::to_vec(&proxy_request) {
            Ok(body) => body,
            Err(err) => {
                tracing::error!(error = %err, "failed to serialize proxy middleware request");
                return errors::error_response(
                    input.surface,
                    500,
                    errors::error_type(input.surface, 500),
                    "failed to serialize middleware request",
                    Some(&input.request_id),
                );
            }
        };
        let request_id = input.request_id.clone();
        let surface = input.surface;
        let endpoint_path = input.endpoint_path;
        let user_tier = input
            .user_tier
            .clone()
            .or_else(|| user_tier_from_params(&input.params));
        let pending = PendingCompletion::from(input);
        self.pending
            .insert(pending, self.timeout + Duration::from_secs(30));

        let mut request = self
            .client
            .post(self.endpoint_url(endpoint_path))
            .timeout(self.timeout)
            .header(CONTENT_TYPE, "application/json")
            .header(REQUEST_ID_HEADER, request_id.as_str())
            .header(INTERNAL_TOKEN_HEADER, self.internal_token.as_str())
            .body(proxy_body);
        if let Some(tier) = &user_tier {
            request = request.header(USER_TIER_HEADER, tier);
        }

        match request.send().await {
            Ok(response) => {
                self.pending.remove(&request_id);
                relay_response(response)
            }
            Err(err) => {
                self.pending.remove(&request_id);
                tracing::error!(error = %err, request_id = %request_id, "proxy middleware request failed");
                errors::error_response(
                    surface,
                    502,
                    errors::error_type(surface, 502),
                    "middleware proxy unavailable",
                    Some(&request_id),
                )
            }
        }
    }

    fn internal_token(&self) -> Option<&str> {
        Some(&self.internal_token)
    }

    async fn handle_internal_forward(
        &self,
        service: &AciService,
        input: InternalForwardRequest,
    ) -> Response {
        let Some(pending) = self.pending.take(&input.request_id) else {
            return errors::error_response(
                Surface::Openai,
                404,
                errors::error_type(Surface::Openai, 404),
                "middleware request context is missing or expired",
                Some(&input.request_id),
            );
        };
        forward_selected(
            service,
            self.sse_keepalive_ms,
            InternalForwardInput {
                pending,
                selected_route: input.selected_route,
                user_tier: input.user_tier,
                body: input.body,
            },
        )
        .await
    }
}

fn user_tier_from_params(params: &Value) -> Option<String> {
    let provider = params.get("provider")?.as_object()?;
    for key in ["tier", "userTier", "user_tier"] {
        let Some(value) = provider.get(key).and_then(Value::as_str) else {
            continue;
        };
        let value = value.trim();
        if value.eq_ignore_ascii_case("premium") {
            return Some("premium".to_string());
        }
        if !value.is_empty() {
            return Some("basic".to_string());
        }
    }
    None
}

fn relay_response(response: reqwest::Response) -> Response {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = relay_headers(response.headers());
    let body = Body::from_stream(
        response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string()))),
    );
    (status, headers, body).into_response()
}

fn relay_headers(src: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in src {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "connection" | "transfer-encoding" | "content-length"
        ) {
            continue;
        }
        let Ok(header_name) = HeaderName::from_bytes(name.as_str().as_bytes()) else {
            continue;
        };
        let Ok(header_value) = HeaderValue::from_bytes(value.as_bytes()) else {
            continue;
        };
        out.insert(header_name, header_value);
    }
    out
}

pub fn parse_target_format(value: Option<&str>) -> ProviderFormat {
    match value
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "anthropic" => ProviderFormat::Anthropic,
        _ => ProviderFormat::Openai,
    }
}
