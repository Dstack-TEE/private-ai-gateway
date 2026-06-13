//! Plain OpenAI-compatible upstream backend.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;

use super::tls::{pinned_spki_client, response_headers};
use super::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse, DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS,
    DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS,
};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent};

/// The minimal forwarder.
///
/// Sends `req.body` as the request body to `base_url + path`. Adds
/// an `Authorization: Bearer <token>` header when configured.
///
/// This backend does *not* do upstream attestation. An aggregator
/// that relies on it MUST run an attested per-upstream verifier
/// elsewhere and emit `upstream.verified` with its result; this
/// object is the forwarding plumbing only.
pub struct OpenAICompatibleBackend {
    name: String,
    base_url: String,
    path: String,
    bearer_token: Option<String>,
    client: reqwest::Client,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
}

impl OpenAICompatibleBackend {
    pub fn new(base_url: impl Into<String>) -> Result<Self, UpstreamError> {
        Self::new_with_timeouts(
            base_url,
            DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS,
            DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS,
        )
    }

    pub fn new_with_timeouts(
        base_url: impl Into<String>,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        let mut base = base_url.into();
        while base.ends_with('/') {
            base.pop();
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .read_timeout(Duration::from_secs(read_timeout_seconds))
            .build()
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        Ok(Self {
            name: "openai-compatible".to_string(),
            base_url: base,
            path: "/v1/chat/completions".to_string(),
            bearer_token: None,
            client,
            connect_timeout_seconds,
            read_timeout_seconds,
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        let mut p = path.into();
        if !p.starts_with('/') {
            p.insert(0, '/');
        }
        self.path = p;
        self
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }
}

pub(super) fn request_model_id(body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_slice(body).ok()?;
    parsed.get("model")?.as_str().map(str::to_string)
}

pub(super) fn rewrite_request_model(
    body: &[u8],
    upstream_model_id: &str,
) -> Result<Vec<u8>, UpstreamError> {
    let mut parsed: Value = serde_json::from_slice(body)
        .map_err(|e| UpstreamError::Routing(format!("invalid JSON request body: {e}")))?;
    let Some(obj) = parsed.as_object_mut() else {
        return Err(UpstreamError::Routing(
            "request body must be a JSON object".to_string(),
        ));
    };
    match obj.get_mut("model") {
        Some(model) if model.is_string() => {
            *model = Value::String(upstream_model_id.to_string());
        }
        _ => {
            return Err(UpstreamError::Routing(
                "request body must contain a string model field".to_string(),
            ));
        }
    }
    serde_json::to_vec(&parsed).map_err(|e| UpstreamError::Routing(e.to_string()))
}

#[async_trait]
impl UpstreamBackend for OpenAICompatibleBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn url_origin(&self) -> Option<&str> {
        Some(&self.base_url)
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let resp = self
            .request_builder(&self.client, &req, "application/json")
            .body(req.body)
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = response_headers(&resp);
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?
            .to_vec();
        Ok(UpstreamResponse {
            status_code: status,
            body,
            headers,
        })
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let resp = self
            .request_builder(&self.client, &req, "text/event-stream")
            .body(req.body)
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = response_headers(&resp);
        let body = resp
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| UpstreamError::Transport(e.to_string())));
        Ok(UpstreamStreamResponse {
            status_code: status,
            headers,
            body: Box::pin(body),
        })
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.get("/v1/models", "application/json").await
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let client = self.client_for_event(event)?;
        let resp = self
            .request_builder(&client, &req.request, "application/json")
            .body(req.request.body)
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = response_headers(&resp);
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?
            .to_vec();
        Ok(UpstreamResponse {
            status_code: status,
            body,
            headers,
        })
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let client = self.client_for_event(event)?;
        let resp = self
            .request_builder(&client, &req.request, "text/event-stream")
            .body(req.request.body)
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = response_headers(&resp);
        let body = resp
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| UpstreamError::Transport(e.to_string())));
        Ok(UpstreamStreamResponse {
            status_code: status,
            headers,
            body: Box::pin(body),
        })
    }
}

impl OpenAICompatibleBackend {
    fn client_for_event(
        &self,
        event: &UpstreamVerifiedEvent,
    ) -> Result<reqwest::Client, UpstreamError> {
        if event.channel_bindings.is_empty() {
            return Ok(self.client.clone());
        }
        let mut accepted_spkis = Vec::new();
        let mut accepted_certificates = Vec::new();
        for binding in &event.channel_bindings {
            match binding {
                ChannelBinding::TlsSpkiSha256 {
                    origin,
                    spki_sha256,
                } if origin == &self.base_url => accepted_spkis.push(spki_sha256.clone()),
                ChannelBinding::TlsSpkiSha256 { origin, .. } => {
                    return Err(UpstreamError::Transport(format!(
                        "verified TLS SPKI binding origin {origin:?} does not match upstream {:?}",
                        self.base_url
                    )));
                }
                ChannelBinding::TlsCertificateSha256 {
                    origin,
                    certificate_sha256,
                } if origin == &self.base_url => {
                    accepted_certificates.push(certificate_sha256.clone())
                }
                ChannelBinding::TlsCertificateSha256 { origin, .. } => {
                    return Err(UpstreamError::Transport(format!(
                        "verified TLS certificate binding origin {origin:?} does not match upstream {:?}",
                        self.base_url
                    )));
                }
                ChannelBinding::E2eePublicKeySha256 {
                    provider,
                    algorithm,
                    ..
                } => {
                    return Err(UpstreamError::Transport(format!(
                        "backend {} cannot enforce {provider} E2EE binding {algorithm:?}",
                        self.name
                    )));
                }
            }
        }
        if !self.base_url.starts_with("https://") {
            return Err(UpstreamError::Transport(
                "TLS channel binding requires an https upstream".to_string(),
            ));
        }
        pinned_spki_client(
            accepted_spkis,
            accepted_certificates,
            self.connect_timeout_seconds,
            self.read_timeout_seconds,
        )
    }

    async fn get(
        &self,
        path: &str,
        accept: &'static str,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let resp = self
            .get_builder(path, accept)
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = response_headers(&resp);
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?
            .to_vec();
        Ok(UpstreamResponse {
            status_code: status,
            body,
            headers,
        })
    }

    fn request_builder(
        &self,
        client: &reqwest::Client,
        req: &UpstreamRequest,
        accept: &'static str,
    ) -> reqwest::RequestBuilder {
        let path = req.path.as_deref().unwrap_or(&self.path);
        let url = format!("{}{}", self.base_url, path);
        let mut builder = client
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", accept);
        for (k, v) in req.headers.iter() {
            builder = builder.header(k, v);
        }
        if let Some(t) = &self.bearer_token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder
    }

    fn get_builder(&self, path: &str, accept: &'static str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut builder = self.client.get(&url).header("accept", accept);
        if let Some(t) = &self.bearer_token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder
    }
}
