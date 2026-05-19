//! Upstream backend abstraction for the aggregator.
//!
//! The aggregator forwards a chat-completion request to an upstream
//! after ACI-side hashing. Different upstream providers (Chutes,
//! Tinfoil, NEAR AI, Phala dstack-vllm-proxy, raw OpenAI-compatible
//! endpoints) speak slightly different dialects on top of the OpenAI
//! base. We isolate that with the small trait defined here so:
//!
//! * the per-request flow in the service layer never special-cases a
//!   provider;
//! * future provider adapters (ACI §1.2 "aggregator MUST verify
//!   upstreams inside attested code") plug in by name without touching
//!   the hot path.
//!
//! The first concrete backend is [`OpenAICompatibleBackend`]: it
//! speaks the bare OpenAI `POST /v1/chat/completions` surface. That
//! is enough to front a stock vLLM, a dstack-vllm-proxy in
//! trust-this-only mode, or any OpenAI-shaped endpoint, and is the
//! simplest thing the aggregator can forward to today.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use chacha20poly1305::{
    aead::{Aead, KeyInit as AeadKeyInit},
    ChaCha20Poly1305, Nonce,
};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use futures_util::{stream, Stream, StreamExt};
use ml_kem::{
    kem::{Decapsulate, Encapsulate, Kem, KeyExport, TryKeyInit},
    ml_kem_768::{
        Ciphertext as MlKemCiphertext768, DecapsulationKey as MlKemDecapsulationKey768,
        EncapsulationKey as MlKemEncapsulationKey768,
    },
    MlKem768,
};
use rand::RngCore;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use x509_parser::prelude::parse_x509_certificate;

use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};

pub const DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS: u64 = 10;
pub const DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Clone, Default)]
pub struct UpstreamRequest {
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedUpstreamRequest {
    pub request: UpstreamRequest,
    pub upstream_name: String,
    pub url_origin: Option<String>,
    pub model_id: String,
    pub route_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpstreamResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
}

pub type UpstreamBodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, UpstreamError>> + Send>>;

pub struct UpstreamStreamResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: UpstreamBodyStream,
}

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("upstream routing error: {0}")]
    Routing(String),
    #[error("upstream transport error: {0}")]
    Transport(String),
    #[error("upstream channel binding mismatch: {0}")]
    ChannelBindingMismatch(String),
    #[error("upstream rejected request with status {status}: {body}")]
    Upstream { status: u16, body: String },
}

/// Forward an OpenAI-compatible request to one upstream.
#[async_trait]
pub trait UpstreamBackend: Send + Sync {
    /// Stable identifier (e.g. `"openai-compatible"`, `"chutes"`).
    fn name(&self) -> &str;

    /// Origin (scheme + host + port) recorded in receipts.
    fn url_origin(&self) -> Option<&str>;

    /// Prepare an upstream request before verification and receipt
    /// hashing. Routers use this phase to select the concrete upstream
    /// and rewrite request bytes such as model aliases. Plain backends
    /// leave the request untouched.
    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let model_id = request_model_id(&req.body).unwrap_or_default();
        Ok(PreparedUpstreamRequest {
            request: req,
            upstream_name: self.name().to_string(),
            url_origin: self.url_origin().map(str::to_string),
            model_id,
            route_id: None,
        })
    }

    /// Forward `req` to the upstream and return the response.
    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError>;

    /// Forward a request after [`Self::prepare`] has selected and
    /// normalized the upstream request bytes.
    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward(req.request).await
    }

    /// Forward a verified request. Backends that cannot enforce the
    /// verifier's channel bindings must fail closed.
    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        if !event.channel_bindings.is_empty() {
            return Err(UpstreamError::Transport(format!(
                "backend {} cannot enforce upstream channel bindings",
                self.name()
            )));
        }
        self.forward_prepared(req).await
    }

    /// Return the upstream's OpenAI-compatible model list.
    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Transport(
            "upstream backend does not implement /v1/models".to_string(),
        ))
    }

    /// Forward `req` to the upstream and return an ordered byte stream.
    ///
    /// Implementations that cannot stream may use the default buffered
    /// adapter. Real OpenAI-compatible providers should override this
    /// so SSE chunks are forwarded as they arrive.
    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let response = self.forward(req).await?;
        let body = Bytes::from(response.body);
        Ok(UpstreamStreamResponse {
            status_code: response.status_code,
            headers: response.headers,
            body: Box::pin(stream::once(async move { Ok(body) })),
        })
    }

    /// Stream a request after [`Self::prepare`] has selected and
    /// normalized the upstream request bytes.
    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.forward_stream(req.request).await
    }

    /// Streaming variant of [`Self::forward_verified_prepared`].
    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        if !event.channel_bindings.is_empty() {
            return Err(UpstreamError::Transport(format!(
                "backend {} cannot enforce upstream channel bindings",
                self.name()
            )));
        }
        self.forward_stream_prepared(req).await
    }
}

pub struct ModelRoute {
    pub public_model_id: String,
    pub upstream_model_id: String,
    pub upstream: Arc<dyn UpstreamBackend>,
    pub route_id: String,
}

impl ModelRoute {
    pub fn new(
        public_model_id: impl Into<String>,
        upstream_model_id: impl Into<String>,
        upstream: Arc<dyn UpstreamBackend>,
        route_id: impl Into<String>,
    ) -> Result<Self, UpstreamError> {
        let public_model_id = public_model_id.into();
        let upstream_model_id = upstream_model_id.into();
        let route_id = route_id.into();
        if public_model_id.trim().is_empty() {
            return Err(UpstreamError::Routing(
                "public model id must not be empty".to_string(),
            ));
        }
        if upstream_model_id.trim().is_empty() {
            return Err(UpstreamError::Routing(
                "upstream model id must not be empty".to_string(),
            ));
        }
        if route_id.trim().is_empty() {
            return Err(UpstreamError::Routing(
                "route id must not be empty".to_string(),
            ));
        }
        Ok(Self {
            public_model_id,
            upstream_model_id,
            upstream,
            route_id,
        })
    }
}

/// Model-id router for OpenAI-compatible request bodies.
///
/// A route maps one public model id to one concrete upstream and one
/// upstream-accepted model id. The rewrite happens in [`Self::prepare`],
/// before upstream verification and receipt hashing, so the receipt
/// covers the exact bytes sent to the selected upstream.
pub struct ModelRouterBackend {
    name: String,
    routes: HashMap<String, ModelRoute>,
    order: Vec<String>,
}

impl ModelRouterBackend {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            routes: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn add_route(&mut self, route: ModelRoute) -> Result<(), UpstreamError> {
        if self.routes.contains_key(&route.public_model_id) {
            return Err(UpstreamError::Routing(format!(
                "duplicate public model id {:?}",
                route.public_model_id
            )));
        }
        if self
            .routes
            .values()
            .any(|existing| existing.route_id == route.route_id)
        {
            return Err(UpstreamError::Routing(format!(
                "duplicate route id {:?}",
                route.route_id
            )));
        }
        self.order.push(route.public_model_id.clone());
        self.routes.insert(route.public_model_id.clone(), route);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    fn route_for(&self, public_model_id: &str) -> Result<&ModelRoute, UpstreamError> {
        self.routes.get(public_model_id).ok_or_else(|| {
            UpstreamError::Routing(format!("no upstream route for model {public_model_id:?}"))
        })
    }

    fn route_from_prepared(
        &self,
        req: &PreparedUpstreamRequest,
    ) -> Result<&ModelRoute, UpstreamError> {
        let route_id = req.route_id.as_deref().ok_or_else(|| {
            UpstreamError::Routing("prepared router request is missing route id".to_string())
        })?;
        let public_model_id = self
            .order
            .iter()
            .find(|public| {
                self.routes
                    .get(public.as_str())
                    .is_some_and(|route| route.route_id == route_id)
            })
            .ok_or_else(|| {
                UpstreamError::Routing(format!("prepared router route id {route_id:?} is unknown"))
            })?;
        self.route_for(public_model_id)
    }
}

#[async_trait]
impl UpstreamBackend for ModelRouterBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn url_origin(&self) -> Option<&str> {
        None
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let public_model_id = request_model_id(&req.body).ok_or_else(|| {
            UpstreamError::Routing("request body must contain a string model field".to_string())
        })?;
        let route = self.route_for(&public_model_id)?;
        let mut request = req;
        request.body = rewrite_request_model(&request.body, &route.upstream_model_id)?;
        Ok(PreparedUpstreamRequest {
            request,
            upstream_name: route.upstream.name().to_string(),
            url_origin: route.upstream.url_origin().map(str::to_string),
            model_id: route.upstream_model_id.clone(),
            route_id: Some(route.route_id.clone()),
        })
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let prepared = self.prepare(req)?;
        self.forward_prepared(prepared).await
    }

    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route.upstream.forward(req.request).await
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route.upstream.forward_verified_prepared(req, event).await
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let prepared = self.prepare(req)?;
        self.forward_stream_prepared(prepared).await
    }

    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route.upstream.forward_stream(req.request).await
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route
            .upstream
            .forward_stream_verified_prepared(req, event)
            .await
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        let data = self
            .order
            .iter()
            .filter_map(|public| self.routes.get(public))
            .map(|route| {
                json!({
                    "id": route.public_model_id,
                    "object": "model",
                    "owned_by": self.name.as_str(),
                })
            })
            .collect::<Vec<_>>();
        let body = serde_json::to_vec(&json!({
            "object": "list",
            "data": data,
        }))
        .map_err(|e| UpstreamError::Routing(e.to_string()))?;
        Ok(UpstreamResponse {
            status_code: 200,
            body,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
        })
    }
}

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

fn request_model_id(body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_slice(body).ok()?;
    parsed.get("model")?.as_str().map(str::to_string)
}

fn rewrite_request_model(body: &[u8], upstream_model_id: &str) -> Result<Vec<u8>, UpstreamError> {
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

const CHUTES_DEFAULT_E2EE_API_BASE: &str = "https://api.chutes.ai";
const CHUTES_MLKEM_768_ALGORITHM: &str = "chutes-ml-kem-768";
const CHUTES_MLKEM_CT_SIZE: usize = 1088;
const CHUTES_TAG_SIZE: usize = 16;
const CHUTES_INFO_REQ: &[u8] = b"e2e-req-v1";
const CHUTES_INFO_RESP: &[u8] = b"e2e-resp-v1";
const CHUTES_INFO_STREAM: &[u8] = b"e2e-stream-v1";
const CHUTES_MODEL_CACHE_TTL_SECONDS: u64 = 300;
const CHUTES_DEFAULT_NONCE_TTL_SECONDS: u64 = 55;

/// Chutes provider adapter.
///
/// Chutes binds upstream attestation to an application E2EE public key, so
/// this backend never forwards model requests as plaintext. It fetches a
/// single-use nonce and the live E2EE public key from Chutes, checks that the
/// key matches the verifier-provided channel binding, then sends the request
/// through Chutes' `/e2e/invoke` transport.
pub struct ChutesProviderBackend {
    inner: OpenAICompatibleBackend,
    e2ee_api_base: String,
    api_key: Option<String>,
    chute_ids: HashMap<String, String>,
    client: reqwest::Client,
    session_store: Arc<ChutesSessionStore>,
}

impl ChutesProviderBackend {
    pub fn new_with_timeouts(
        base_url: impl Into<String>,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .read_timeout(Duration::from_secs(read_timeout_seconds))
            .build()
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        Ok(Self {
            inner: OpenAICompatibleBackend::new_with_timeouts(
                base_url,
                connect_timeout_seconds,
                read_timeout_seconds,
            )?
            .with_name("chutes"),
            e2ee_api_base: CHUTES_DEFAULT_E2EE_API_BASE.to_string(),
            api_key: None,
            chute_ids: HashMap::new(),
            client,
            session_store: Arc::new(ChutesSessionStore::new()),
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.with_name(name);
        self
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into();
        self.api_key = Some(token.clone());
        self.inner = self.inner.with_bearer_token(token);
        self
    }

    pub fn with_e2ee_api_base(mut self, base_url: impl Into<String>) -> Self {
        self.e2ee_api_base = base_url.into().trim().trim_end_matches('/').to_string();
        self
    }

    pub fn with_chute_ids<I, K, V>(mut self, chute_ids: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.chute_ids = chute_ids
            .into_iter()
            .map(|(model_id, chute_id)| (model_id.into(), chute_id.into()))
            .collect();
        self
    }

    pub fn with_session_store(mut self, session_store: Arc<ChutesSessionStore>) -> Self {
        self.session_store = session_store;
        self
    }

    fn api_key(&self) -> Result<String, UpstreamError> {
        let token = self.api_key.clone().unwrap_or_default();
        if token.trim().is_empty() {
            return Err(UpstreamError::Transport(
                "Chutes E2EE transport requires bearer_token in upstream config".to_string(),
            ));
        }
        Ok(token)
    }

    fn e2ee_requires_verified_binding(&self) -> UpstreamError {
        UpstreamError::Transport(
            "Chutes E2EE transport requires a verified chutes-ml-kem-768 public key binding"
                .to_string(),
        )
    }

    async fn invoke_verified(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
        stream: bool,
    ) -> Result<ChutesInvokeResponse, UpstreamError> {
        if event.result != VerificationResult::Verified {
            return Err(self.e2ee_requires_verified_binding());
        }
        let api_key = self.api_key()?;
        let payload: Value = serde_json::from_slice(&req.request.body)
            .map_err(|e| UpstreamError::Routing(format!("invalid JSON request body: {e}")))?;
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                UpstreamError::Routing("request body must contain a string model field".to_string())
            })?;
        let chute_id = self.resolve_chute_id(model, &api_key).await?;
        let accepted = chutes_accepted_bindings(event)?;
        let selected = self
            .acquire_verified_chutes_session(&chute_id, &api_key, &accepted)
            .await?;
        let encrypted = build_chutes_e2ee_request(&selected.e2e_pubkey, payload)?;
        let headers = chutes_invoke_headers(
            &api_key,
            &chute_id,
            &selected.instance_id,
            &selected.nonce,
            stream,
            req.request
                .path
                .as_deref()
                .unwrap_or("/v1/chat/completions"),
        );
        let url = format!("{}/e2e/invoke", self.e2ee_api_base);
        let mut builder = self.client.post(url).body(encrypted.blob);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status_code = resp.status().as_u16();
        let headers = response_headers(&resp);
        Ok(ChutesInvokeResponse {
            status_code,
            headers,
            response_sk: encrypted.response_sk,
            response: resp,
        })
    }

    async fn resolve_chute_id(&self, model: &str, api_key: &str) -> Result<String, UpstreamError> {
        if looks_like_uuid(model) {
            return Ok(model.to_string());
        }
        if let Some(chute_id) = self.chute_ids.get(model) {
            return Ok(chute_id.clone());
        }
        if let Some(chute_id) = self.session_store.cached_chute_id(model) {
            return Ok(chute_id);
        }
        let url = format!("{}/chutes/", self.e2ee_api_base);
        let resp = self
            .client
            .get(url)
            .query(&[("include_public", "true"), ("name", model)])
            .header("authorization", format!("Bearer {api_key}"))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(UpstreamError::Upstream {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        let chutes: ChutesLookupResponse =
            serde_json::from_slice(&body).map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let chute_id = chutes
            .items
            .iter()
            .find(|entry| entry.name.as_deref() == Some(model) && entry.chute_id.is_some())
            .and_then(|entry| entry.chute_id.clone())
            .ok_or_else(|| {
                UpstreamError::Routing(format!(
                    "Chutes /chutes lookup did not return an exact chute_id match for model {model:?}"
                ))
            })?;
        self.session_store.cache_chute_id(model, &chute_id);
        Ok(chute_id)
    }

    async fn fetch_instances(
        &self,
        chute_id: &str,
        api_key: &str,
    ) -> Result<ChutesInstancesResponse, UpstreamError> {
        let url = format!("{}/e2e/instances/{chute_id}", self.e2ee_api_base);
        let resp = self
            .client
            .get(url)
            .header("authorization", format!("Bearer {api_key}"))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(UpstreamError::Upstream {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        serde_json::from_slice(&body).map_err(|e| UpstreamError::Transport(e.to_string()))
    }

    async fn acquire_verified_chutes_session(
        &self,
        chute_id: &str,
        api_key: &str,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<SelectedChutesInstance, UpstreamError> {
        if let Some(selected) = self.pop_verified_chutes_nonce(chute_id, accepted)? {
            return Ok(selected);
        }

        let _refill_guard = self.session_store.refill_lock.lock().await;
        if let Some(selected) = self.pop_verified_chutes_nonce(chute_id, accepted)? {
            return Ok(selected);
        }
        let discovery = self.fetch_instances(chute_id, api_key).await?;
        self.cache_verified_chutes_nonces(chute_id, discovery, accepted)?;
        self.pop_verified_chutes_nonce(chute_id, accepted)?
            .ok_or_else(|| {
                UpstreamError::ChannelBindingMismatch(
                    "Chutes did not return an E2EE key matching the verified binding".to_string(),
                )
            })
    }

    fn pop_verified_chutes_nonce(
        &self,
        chute_id: &str,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<Option<SelectedChutesInstance>, UpstreamError> {
        self.session_store.pop_verified_nonce(chute_id, accepted)
    }

    fn cache_verified_chutes_nonces(
        &self,
        chute_id: &str,
        discovery: ChutesInstancesResponse,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<usize, UpstreamError> {
        let verified = verified_discovery_from_response(chute_id, discovery, Some(accepted))?;
        self.session_store.record_verified_discovery(verified)
    }

    pub async fn refresh_verified_sessions_for_model(
        &self,
        model: &str,
        event: &UpstreamVerifiedEvent,
    ) -> Result<usize, UpstreamError> {
        if event.result != VerificationResult::Verified {
            return Err(self.e2ee_requires_verified_binding());
        }
        let api_key = self.api_key()?;
        let chute_id = self.resolve_chute_id(model, &api_key).await?;
        let accepted = chutes_accepted_bindings(event)?;
        let discovery = self.fetch_instances(&chute_id, &api_key).await?;
        self.cache_verified_chutes_nonces(&chute_id, discovery, &accepted)
    }
}

#[async_trait]
impl UpstreamBackend for ChutesProviderBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn url_origin(&self) -> Option<&str> {
        self.inner.url_origin()
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        self.inner.prepare(req)
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(self.e2ee_requires_verified_binding())
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let invoke = self.invoke_verified(req, event, false).await?;
        let status_code = invoke.status_code;
        let headers = invoke.headers;
        let body = invoke
            .response
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?
            .to_vec();
        if status_code != 200 {
            return Ok(UpstreamResponse {
                status_code,
                body,
                headers,
            });
        }
        let body = decrypt_chutes_response(&body, &invoke.response_sk)?;
        Ok(UpstreamResponse {
            status_code,
            body,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
        })
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(self.e2ee_requires_verified_binding())
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let invoke = self.invoke_verified(req, event, true).await?;
        let status_code = invoke.status_code;
        let mut headers = invoke.headers;
        let raw_body = invoke
            .response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| UpstreamError::Transport(e.to_string())));
        let body: UpstreamBodyStream = if status_code == 200 {
            headers.insert("content-type".to_string(), "text/event-stream".to_string());
            Box::pin(ChutesE2eeDecryptingStream::new(
                Box::pin(raw_body),
                invoke.response_sk,
            ))
        } else {
            Box::pin(raw_body)
        };
        Ok(UpstreamStreamResponse {
            status_code,
            headers,
            body,
        })
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.inner.models().await
    }
}

struct ChutesInvokeResponse {
    status_code: u16,
    headers: HashMap<String, String>,
    response_sk: MlKemDecapsulationKey768,
    response: reqwest::Response,
}

#[derive(Debug, Deserialize)]
struct ChutesLookupResponse {
    items: Vec<ChutesLookupEntry>,
}

#[derive(Debug, Deserialize)]
struct ChutesLookupEntry {
    name: Option<String>,
    chute_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChutesInstancesResponse {
    instances: Vec<ChutesInstanceInfo>,
    #[serde(default)]
    nonce_expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ChutesInstanceInfo {
    instance_id: String,
    e2e_pubkey: String,
    nonces: Vec<String>,
}

#[derive(Debug)]
struct SelectedChutesInstance {
    instance_id: String,
    e2e_pubkey: String,
    nonce: String,
}

#[derive(Debug)]
pub struct ChutesSessionStore {
    cache: Mutex<ChutesSessionCache>,
    refill_lock: tokio::sync::Mutex<()>,
}

impl ChutesSessionStore {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(ChutesSessionCache::default()),
            refill_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn cached_chute_id(&self, model: &str) -> Option<String> {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        match cache.model_map.get(model) {
            Some(entry) if entry.expires_at > now => Some(entry.chute_id.clone()),
            Some(_) => {
                cache.model_map.remove(model);
                None
            }
            None => None,
        }
    }

    pub fn cache_chute_id(&self, model: &str, chute_id: &str) {
        let expires_at = Instant::now() + Duration::from_secs(CHUTES_MODEL_CACHE_TTL_SECONDS);
        self.cache.lock().unwrap().model_map.insert(
            model.to_string(),
            CachedChuteId {
                chute_id: chute_id.to_string(),
                expires_at,
            },
        );
    }

    pub fn record_verified_discovery(
        &self,
        discovery: ChutesVerifiedDiscovery,
    ) -> Result<usize, UpstreamError> {
        let nonce_ttl = discovery
            .nonce_expires_in
            .unwrap_or(CHUTES_DEFAULT_NONCE_TTL_SECONDS);
        if nonce_ttl == 0 {
            return Ok(0);
        }
        let candidates = discovery
            .instances
            .into_iter()
            .filter(|instance| !instance.nonces.is_empty())
            .map(|instance| ChutesNonceCandidate {
                instance_id: instance.instance_id,
                e2e_pubkey: instance.e2e_pubkey,
                public_key_sha256: instance.public_key_sha256,
                nonces: instance.nonces,
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err(UpstreamError::Transport(
                "verified Chutes E2EE instances did not include fresh nonces".to_string(),
            ));
        }
        Ok(self.record_nonce_candidates(&discovery.chute_id, nonce_ttl, candidates))
    }

    fn pop_verified_nonce(
        &self,
        chute_id: &str,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<Option<SelectedChutesInstance>, UpstreamError> {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        let Some(pool) = cache.nonce_pools.get_mut(chute_id) else {
            return Ok(None);
        };

        let mut retained = VecDeque::with_capacity(pool.len());
        let mut selected = None;
        while let Some(nonce) = pool.pop_front() {
            if nonce.expires_at <= now {
                continue;
            }
            if !chutes_binding_matches(accepted, &nonce.instance_id, &nonce.public_key_sha256) {
                continue;
            }
            if selected.is_none() {
                selected = Some(SelectedChutesInstance {
                    instance_id: nonce.instance_id,
                    e2e_pubkey: nonce.e2e_pubkey,
                    nonce: nonce.nonce,
                });
                continue;
            }
            retained.push_back(nonce);
        }
        *pool = retained;
        Ok(selected)
    }

    fn record_nonce_candidates(
        &self,
        chute_id: &str,
        nonce_ttl: u64,
        candidates: Vec<ChutesNonceCandidate>,
    ) -> usize {
        let expires_at = Instant::now() + Duration::from_secs(nonce_ttl);
        let mut cache = self.cache.lock().unwrap();
        let pool = cache.nonce_pools.entry(chute_id.to_string()).or_default();
        let mut existing = pool
            .iter()
            .map(|nonce| (nonce.instance_id.clone(), nonce.nonce.clone()))
            .collect::<HashSet<_>>();
        let max_nonces = candidates
            .iter()
            .map(|candidate| candidate.nonces.len())
            .max()
            .unwrap_or(0);
        let mut added = 0;
        for nonce_index in 0..max_nonces {
            for candidate in &candidates {
                let Some(nonce) = candidate.nonces.get(nonce_index) else {
                    continue;
                };
                if existing.insert((candidate.instance_id.clone(), nonce.clone())) {
                    added += 1;
                    pool.push_back(ChutesPooledNonce {
                        instance_id: candidate.instance_id.clone(),
                        e2e_pubkey: candidate.e2e_pubkey.clone(),
                        public_key_sha256: candidate.public_key_sha256.clone(),
                        nonce: nonce.clone(),
                        expires_at,
                    });
                }
            }
        }
        added
    }

    #[cfg(test)]
    pub(crate) fn pooled_nonce_count(&self, chute_id: &str) -> usize {
        let now = Instant::now();
        self.cache
            .lock()
            .unwrap()
            .nonce_pools
            .get(chute_id)
            .map(|pool| pool.iter().filter(|nonce| nonce.expires_at > now).count())
            .unwrap_or(0)
    }
}

impl Default for ChutesSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default, Debug)]
struct ChutesSessionCache {
    model_map: HashMap<String, CachedChuteId>,
    nonce_pools: HashMap<String, VecDeque<ChutesPooledNonce>>,
}

#[derive(Debug)]
struct CachedChuteId {
    chute_id: String,
    expires_at: Instant,
}

#[derive(Debug)]
struct ChutesPooledNonce {
    instance_id: String,
    e2e_pubkey: String,
    public_key_sha256: String,
    nonce: String,
    expires_at: Instant,
}

struct ChutesNonceCandidate {
    instance_id: String,
    e2e_pubkey: String,
    public_key_sha256: String,
    nonces: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChutesVerifiedDiscovery {
    pub chute_id: String,
    #[serde(default)]
    pub nonce_expires_in: Option<u64>,
    #[serde(default)]
    pub instances: Vec<ChutesVerifiedInstance>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChutesVerifiedInstance {
    pub instance_id: String,
    pub e2e_pubkey: String,
    pub public_key_sha256: String,
    #[serde(default)]
    pub nonces: Vec<String>,
}

#[derive(Debug)]
struct ChutesAcceptedBinding {
    key_id: Option<String>,
    public_key_sha256: String,
}

struct ChutesE2eeRequest {
    blob: Vec<u8>,
    response_sk: MlKemDecapsulationKey768,
}

fn looks_like_uuid(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() == 5
        && value.len() == 36
        && value.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

fn chutes_invoke_headers(
    api_key: &str,
    chute_id: &str,
    instance_id: &str,
    nonce: &str,
    stream: bool,
    e2e_path: &str,
) -> HashMap<&'static str, String> {
    HashMap::from([
        ("authorization", format!("Bearer {api_key}")),
        ("x-chute-id", chute_id.to_string()),
        ("x-instance-id", instance_id.to_string()),
        ("x-e2e-nonce", nonce.to_string()),
        ("x-e2e-stream", stream.to_string()),
        ("x-e2e-path", e2e_path.to_string()),
        ("content-type", "application/octet-stream".to_string()),
    ])
}

fn verified_discovery_from_response(
    chute_id: &str,
    discovery: ChutesInstancesResponse,
    accepted: Option<&[ChutesAcceptedBinding]>,
) -> Result<ChutesVerifiedDiscovery, UpstreamError> {
    let mut matched_verified_key = false;
    let mut instances = Vec::new();
    for instance in discovery.instances {
        let public_key_sha256 = chutes_e2ee_pubkey_sha256(&instance.e2e_pubkey)?;
        let accepted = accepted
            .map(|accepted| {
                chutes_binding_matches(accepted, &instance.instance_id, &public_key_sha256)
            })
            .unwrap_or(true);
        if accepted {
            matched_verified_key = true;
            instances.push(ChutesVerifiedInstance {
                instance_id: instance.instance_id,
                e2e_pubkey: instance.e2e_pubkey,
                public_key_sha256,
                nonces: instance.nonces,
            });
        }
    }
    if !matched_verified_key {
        return Err(UpstreamError::ChannelBindingMismatch(
            "Chutes did not return an E2EE key matching the verified binding".to_string(),
        ));
    }
    Ok(ChutesVerifiedDiscovery {
        chute_id: chute_id.to_string(),
        nonce_expires_in: discovery.nonce_expires_in,
        instances,
    })
}

fn chutes_accepted_bindings(
    event: &UpstreamVerifiedEvent,
) -> Result<Vec<ChutesAcceptedBinding>, UpstreamError> {
    let accepted = event
        .channel_bindings
        .iter()
        .filter_map(|binding| match binding {
            ChannelBinding::E2eePublicKeySha256 {
                provider,
                key_id,
                algorithm,
                public_key_sha256,
            } if provider == "chutes" && algorithm == CHUTES_MLKEM_768_ALGORITHM => {
                Some(ChutesAcceptedBinding {
                    key_id: key_id.clone(),
                    public_key_sha256: public_key_sha256.clone(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if accepted.is_empty() {
        return Err(UpstreamError::Transport(
            "verified Chutes event did not include an E2EE key binding".to_string(),
        ));
    }
    Ok(accepted)
}

fn chutes_binding_matches(
    accepted: &[ChutesAcceptedBinding],
    instance_id: &str,
    public_key_sha256: &str,
) -> bool {
    accepted.iter().any(|binding| {
        binding
            .key_id
            .as_deref()
            .is_none_or(|key_id| key_id == instance_id)
            && binding
                .public_key_sha256
                .eq_ignore_ascii_case(public_key_sha256)
    })
}

fn chutes_e2ee_pubkey_sha256(e2e_pubkey_b64: &str) -> Result<String, UpstreamError> {
    let pubkey = BASE64
        .decode(e2e_pubkey_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes E2EE public key: {e}")))?;
    Ok(hex::encode(Sha256::digest(&pubkey)))
}

fn build_chutes_e2ee_request(
    e2e_pubkey_b64: &str,
    payload: Value,
) -> Result<ChutesE2eeRequest, UpstreamError> {
    let (response_sk, response_pk) = MlKem768::generate_keypair();
    let e2e_pubkey = BASE64
        .decode(e2e_pubkey_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes E2EE public key: {e}")))?;
    let e2e_pubkey = MlKemEncapsulationKey768::new_from_slice(&e2e_pubkey)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes ML-KEM public key: {e}")))?;
    let (mlkem_ct, shared_secret) = e2e_pubkey.encapsulate();
    let sym_key = chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_REQ,
    )?;
    let mut payload = payload;
    let Some(obj) = payload.as_object_mut() else {
        return Err(UpstreamError::Routing(
            "request body must be a JSON object".to_string(),
        ));
    };
    obj.insert(
        "e2e_response_pk".to_string(),
        Value::String(BASE64.encode(response_pk.to_bytes().as_slice())),
    );
    let payload =
        serde_json::to_vec(&payload).map_err(|e| UpstreamError::Transport(e.to_string()))?;
    let compressed = gzip_compress(&payload)?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let encrypted = chacha_encrypt(&sym_key, &nonce, &compressed)?;

    let mut blob = Vec::with_capacity(mlkem_ct.as_slice().len() + nonce.len() + encrypted.len());
    blob.extend_from_slice(mlkem_ct.as_slice());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&encrypted);
    Ok(ChutesE2eeRequest { blob, response_sk })
}

fn decrypt_chutes_response(
    response_blob: &[u8],
    response_sk: &MlKemDecapsulationKey768,
) -> Result<Vec<u8>, UpstreamError> {
    if response_blob.len() <= CHUTES_MLKEM_CT_SIZE + 12 + CHUTES_TAG_SIZE {
        return Err(UpstreamError::Transport(
            "Chutes E2EE response blob is too short".to_string(),
        ));
    }
    let mlkem_ct = MlKemCiphertext768::try_from(&response_blob[..CHUTES_MLKEM_CT_SIZE])
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes response ML-KEM CT: {e}")))?;
    let nonce = &response_blob[CHUTES_MLKEM_CT_SIZE..CHUTES_MLKEM_CT_SIZE + 12];
    let ciphertext = &response_blob[CHUTES_MLKEM_CT_SIZE + 12..];
    let shared_secret = response_sk.decapsulate(&mlkem_ct);
    let sym_key = chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_RESP,
    )?;
    let plaintext = chacha_decrypt(&sym_key, nonce, ciphertext)?;
    gzip_decompress(&plaintext)
}

fn decrypt_chutes_stream_init(
    response_sk: &MlKemDecapsulationKey768,
    mlkem_ct_b64: &str,
) -> Result<Vec<u8>, UpstreamError> {
    let mlkem_ct = BASE64
        .decode(mlkem_ct_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes stream init: {e}")))?;
    let mlkem_ct = MlKemCiphertext768::try_from(mlkem_ct.as_slice())
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes stream ML-KEM CT: {e}")))?;
    let shared_secret = response_sk.decapsulate(&mlkem_ct);
    chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_STREAM,
    )
}

fn decrypt_chutes_stream_chunk(
    stream_key: &[u8],
    chunk_b64: &str,
) -> Result<Vec<u8>, UpstreamError> {
    let raw = BASE64
        .decode(chunk_b64)
        .map_err(|e| UpstreamError::Transport(format!("invalid Chutes stream chunk: {e}")))?;
    if raw.len() <= 12 + CHUTES_TAG_SIZE {
        return Err(UpstreamError::Transport(
            "Chutes E2EE stream chunk is too short".to_string(),
        ));
    }
    chacha_decrypt(stream_key, &raw[..12], &raw[12..])
}

fn chutes_derive_key(
    shared_secret: &[u8],
    mlkem_ct: &[u8],
    info: &[u8],
) -> Result<Vec<u8>, UpstreamError> {
    let salt = mlkem_ct.get(..16).ok_or_else(|| {
        UpstreamError::Transport("Chutes ML-KEM ciphertext is too short".to_string())
    })?;
    let hkdf = hkdf::Hkdf::<Sha256>::new(Some(salt), shared_secret);
    let mut key = [0u8; 32];
    hkdf.expand(info, &mut key)
        .map_err(|_| UpstreamError::Transport("Chutes HKDF failed".to_string()))?;
    Ok(key.to_vec())
}

#[allow(deprecated)]
fn chacha_encrypt(key: &[u8], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| UpstreamError::Transport("invalid Chutes ChaCha20 key".to_string()))?;
    cipher
        .encrypt(Nonce::from_slice(nonce), plaintext)
        .map_err(|_| UpstreamError::Transport("Chutes E2EE encryption failed".to_string()))
}

#[allow(deprecated)]
fn chacha_decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| UpstreamError::Transport("invalid Chutes ChaCha20 key".to_string()))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| UpstreamError::Transport("Chutes E2EE decryption failed".to_string()))
}

fn gzip_compress(plaintext: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(plaintext)
        .map_err(|e| UpstreamError::Transport(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| UpstreamError::Transport(e.to_string()))
}

fn gzip_decompress(compressed: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut decoder = GzDecoder::new(compressed);
    let mut plaintext = Vec::new();
    decoder
        .read_to_end(&mut plaintext)
        .map_err(|e| UpstreamError::Transport(e.to_string()))?;
    Ok(plaintext)
}

struct ChutesE2eeDecryptingStream {
    inner: UpstreamBodyStream,
    response_sk: MlKemDecapsulationKey768,
    stream_key: Option<Vec<u8>>,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    finished: bool,
}

impl ChutesE2eeDecryptingStream {
    fn new(inner: UpstreamBodyStream, response_sk: MlKemDecapsulationKey768) -> Self {
        Self {
            inner,
            response_sk,
            stream_key: None,
            buffer: Vec::new(),
            pending: VecDeque::new(),
            finished: false,
        }
    }

    fn process_buffer(&mut self) -> Result<(), UpstreamError> {
        while let Some(pos) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=pos).collect::<Vec<_>>();
            if line.ends_with(b"\n") {
                line.pop();
            }
            if line.ends_with(b"\r") {
                line.pop();
            }
            self.process_sse_line(&line)?;
        }
        Ok(())
    }

    fn process_sse_line(&mut self, line: &[u8]) -> Result<(), UpstreamError> {
        let Some(data) = line.strip_prefix(b"data: ") else {
            return Ok(());
        };
        let raw = String::from_utf8(data.to_vec()).map_err(|_| {
            UpstreamError::Transport("Chutes E2EE stream line is not UTF-8".to_string())
        })?;
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(());
        }
        if raw == "[DONE]" {
            self.pending
                .push_back(Bytes::from_static(b"data: [DONE]\n\n"));
            return Ok(());
        }
        let event: Value = serde_json::from_str(raw)
            .map_err(|e| UpstreamError::Transport(format!("invalid Chutes E2EE SSE event: {e}")))?;
        if let Some(init) = event.get("e2e_init").and_then(Value::as_str) {
            self.stream_key = Some(decrypt_chutes_stream_init(&self.response_sk, init)?);
            return Ok(());
        }
        if let Some(chunk) = event.get("e2e").and_then(Value::as_str) {
            let stream_key = self.stream_key.as_deref().ok_or_else(|| {
                UpstreamError::Transport(
                    "received Chutes E2EE stream chunk before e2e_init".to_string(),
                )
            })?;
            let mut plaintext = decrypt_chutes_stream_chunk(stream_key, chunk)?;
            plaintext.extend_from_slice(b"\n\n");
            self.pending.push_back(Bytes::from(plaintext));
            return Ok(());
        }
        if event.get("usage").is_some() {
            let mut line = Vec::with_capacity(raw.len() + 8);
            line.extend_from_slice(b"data: ");
            line.extend_from_slice(raw.as_bytes());
            line.extend_from_slice(b"\n\n");
            self.pending.push_back(Bytes::from(line));
            return Ok(());
        }
        if let Some(error) = event.get("e2e_error") {
            let body = serde_json::to_vec(&json!({ "error": error }))
                .map_err(|e| UpstreamError::Transport(e.to_string()))?;
            let mut line = Vec::with_capacity(body.len() + 8);
            line.extend_from_slice(b"data: ");
            line.extend_from_slice(&body);
            line.extend_from_slice(b"\n\n");
            self.pending.push_back(Bytes::from(line));
        }
        Ok(())
    }
}

impl Unpin for ChutesE2eeDecryptingStream {}

impl Stream for ChutesE2eeDecryptingStream {
    type Item = Result<Bytes, UpstreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(chunk) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(chunk)));
        }
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend_from_slice(&chunk);
                    if let Err(err) = this.process_buffer() {
                        this.finished = true;
                        return Poll::Ready(Some(Err(err)));
                    }
                    if let Some(chunk) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                }
                Poll::Ready(Some(Err(err))) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(None) => {
                    if !this.buffer.is_empty() {
                        let line = std::mem::take(&mut this.buffer);
                        if let Err(err) = this.process_sse_line(&line) {
                            this.finished = true;
                            return Poll::Ready(Some(Err(err)));
                        }
                    }
                    this.finished = true;
                    if let Some(chunk) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                    return Poll::Ready(None);
                }
            }
        }
    }
}

fn response_headers(resp: &reqwest::Response) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for (k, v) in resp.headers().iter() {
        if let Ok(value) = v.to_str() {
            headers.insert(k.to_string(), value.to_string());
        }
    }
    headers
}

fn pinned_spki_client(
    accepted_spkis: Vec<String>,
    accepted_certificates: Vec<String>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, UpstreamError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let inner = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| UpstreamError::Transport(format!("failed to build TLS verifier: {e}")))?;
    let verifier = Arc::new(SpkiPinVerifier {
        inner,
        accepted: accepted_spkis.into_iter().collect(),
        accepted_certificates: accepted_certificates.into_iter().collect(),
    });
    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_seconds))
        .read_timeout(Duration::from_secs(read_timeout_seconds))
        .use_preconfigured_tls(tls)
        .build()
        .map_err(|e| UpstreamError::Transport(e.to_string()))
}

struct SpkiPinVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    accepted: HashSet<String>,
    accepted_certificates: HashSet<String>,
}

impl fmt::Debug for SpkiPinVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpkiPinVerifier")
            .field("accepted_count", &self.accepted.len())
            .field(
                "accepted_certificate_count",
                &self.accepted_certificates.len(),
            )
            .finish()
    }
}

impl ServerCertVerifier for SpkiPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let verified = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        if !self.accepted_certificates.is_empty() {
            let digest = hex::encode(Sha256::digest(end_entity.as_ref()));
            if !self.accepted_certificates.contains(&digest) {
                return Err(RustlsError::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ));
            }
        }
        let (_, cert) = parse_x509_certificate(end_entity.as_ref())
            .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        let digest = Sha256::digest(cert.public_key().raw);
        let digest = hex::encode(digest);
        if self.accepted.is_empty() || self.accepted.contains(&digest) {
            Ok(verified)
        } else {
            Err(RustlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
