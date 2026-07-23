//! Upstream backend abstraction for the aggregator.
//!
//! The aggregator forwards a chat-completion request to an upstream
//! after ACI-side hashing. Different upstream providers (Chutes,
//! Tinfoil, NEAR AI, Privatemode, Phala dstack-vllm-proxy, raw OpenAI-compatible
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

use std::collections::HashMap;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{stream, Stream};

use crate::aci::receipt::UpstreamVerifiedEvent;

mod chutes;
mod openai;
mod privatemode;
mod router;
mod tls;

pub use chutes::{
    ChutesProviderBackend, ChutesSessionStore, ChutesVerifiedDiscovery, ChutesVerifiedInstance,
};
pub use openai::OpenAICompatibleBackend;
pub use privatemode::{
    PrivatemodeDeploymentConfigError, PrivatemodeProviderBackend, PrivatemodeProxyDeployment,
};
pub use router::{ModelRoute, ModelRouterBackend};

use openai::request_model_id;

pub const DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS: u64 = 10;
pub const DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Clone, Default)]
pub struct UpstreamRequest {
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
    pub path: Option<String>,
    pub target_route_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedUpstreamRequest {
    pub request: UpstreamRequest,
    pub upstream_name: String,
    pub url_origin: Option<String>,
    pub model_id: String,
    pub route_id: Option<String>,
    /// Whether the selected route is an attested (TEE) provider.
    /// `Some(true)` = TEE (verification is fail-closed), `Some(false)` =
    /// non-TEE (TLS endpoint bound, never fail-closed, and its
    /// `upstream.verified` event carries no attestation evidence),
    /// `None` = unclassified (the caller's request-level `upstream_required`
    /// decides, preserving the behaviour of routes built directly via
    /// [`ModelRoute::new`]).
    pub is_tee: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct UpstreamResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
    /// The instance that actually served this request, when the backend fronts
    /// several (Chutes: the serving instance id). Lets the receipt cite that
    /// instance's attested session; `None` for single-channel backends.
    pub served_instance_id: Option<String>,
}

pub type UpstreamBodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, UpstreamError>> + Send>>;

pub struct UpstreamStreamResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: UpstreamBodyStream,
    /// See [`UpstreamResponse::served_instance_id`].
    pub served_instance_id: Option<String>,
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
            is_tee: None,
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
            served_instance_id: response.served_instance_id,
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

    /// Legacy multi-instance attestation report for `model`, in the old
    /// dstack/chutes shape. Only the Chutes provider supports it; other
    /// backends fail so callers can fall back to the gateway's own report.
    async fn chutes_attestation_report(
        &self,
        _model: &str,
    ) -> Result<serde_json::Value, UpstreamError> {
        Err(UpstreamError::Routing(format!(
            "backend {} does not produce a chutes attestation report",
            self.name()
        )))
    }
}
