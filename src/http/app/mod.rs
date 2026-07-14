//! Axum app exposing the ACI endpoints.
//!
//! Endpoints:
//!
//! * `POST /v1/chat/completions` - OpenAI-shaped chat-completion
//!   forwarding with ACI-side hashing and receipt signing. An
//!   optional `Authorization: Bearer <token>` is recorded on the
//!   receipt so later lookups can authenticate the original requester.
//! * `POST /v1/completions` - compatibility surface. The aggregator
//!   forwards legacy prompt completions through the same ACI receipt
//!   path as chat completions. ACI E2EE is an optional add-on here;
//!   plaintext OpenAI-compatible requests remain unchanged.
//! * `POST /v1/embeddings` - OpenAI-shaped embeddings forwarding.
//!   Buffered-only; any client-sent `stream:true` is forced back to
//!   buffered before forwarding. The aggregator hashes the body and
//!   issues a receipt the same way as `/v1/chat/completions`.
//! * `GET  /v1/models` - proxy the upstream OpenAI-compatible model list.
//! * `GET  /v1/models/*` - relay model sub-catalogs to the middleware, which
//!   owns the routing: `/v1/models/:namespace` (alias-prefix catalog) and
//!   `/v1/models/providers/:provider` (provider catalog). Control-plane
//!   middleware only.
//! * `GET  /v1/embeddings/models` - embedding model catalog (control-plane
//!   middleware only).
//! * `GET  /health` - unauthenticated liveness probe for load balancers and
//!   orchestrators; reports only that the process is serving requests.
//! * `GET  /v1/metrics` - expose aggregator-owned Prometheus metrics.
//! * `GET  /v1/admin/upstreams` - authenticated admin view of the
//!   current upstream config, with secrets redacted.
//! * `PUT  /v1/admin/upstreams` - authenticated admin replacement of
//!   the single upstream config file.
//!
//! ACI verification artifacts live under the `/v1/aci/` namespace so they do
//! not pollute the OpenAI surface. The id parameter accepts the gateway
//! `receipt_id` (preferred; always on the `x-receipt-id` response header, and
//! the only handle for `/v1/embeddings` receipts which have no chat_id) or the
//! upstream `chat_id`.
//!
//! * `GET  /v1/aci/attestation` - the ACI attestation report (§5), optionally
//!   nonce-bound (`?nonce=`, §4.2 charset).
//! * `GET  /v1/aci/receipts/{id}` - the §8.2 signed-bytes receipt envelope.
//! * `GET  /v1/aci/sessions/{hex}` - the attested-session record a receipt
//!   cites, served as its exact sealed bytes (§9).
//! * `GET  /v1/aci/sessions?upstream_name=&model=` - list current sessions.
//!
//! Legacy aliases for dstack-vllm-proxy compatibility:
//! * `GET  /v1/attestation/report` - report plus legacy e2ee/`signing_address`
//!   compatibility fields. With `?model=X` it also surfaces the upstream model
//!   node's GPU evidence: PhalaDirect/NearAi upstreams have their real
//!   `nvidia_payload` (bound to the request nonce) merged into the gateway's own
//!   report; Chutes returns the self-contained `attestation_type:"chutes"`
//!   multi-instance report. Without a model (or on upstream failure) the
//!   gateway's own report is returned with an empty `nvidia_payload` placeholder.
//! * `GET  /v1/signature/{id}` - the legacy signature wrapper
//!   (`text`/`signature`/`signing_address`) with the signed ACI receipt
//!   carried in `receipt`.
//!
//! The router installs a middleware that emits `X-ACI-Version` and
//! `X-ACI-Keyset-Digest` on every response, including error paths.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderName, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;

use crate::aggregator::service::AciService;
use crate::aggregator::upstream_config::UpstreamConfigManager;
use crate::middleware::Middleware;

mod backend;
mod error_responses;
mod handlers;
mod util;

use handlers::{
    aci_attestation_report, aci_list_sessions, aci_receipt, admin_get_upstreams,
    admin_put_upstreams, attestation_report, attested_session, chat_completions, completions,
    embeddings, embeddings_models, health, messages, metrics, models, models_subpath,
    receipt_by_chat_id, responses, root,
};

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<AciService>,
    pub upstream_config: Option<Arc<UpstreamConfigManager>>,
    pub admin_token: Option<String>,
    middleware: Option<Arc<Middleware>>,
}

pub fn build_router(service: Arc<AciService>) -> Router {
    build_router_inner(service, None, None, None)
}

pub fn build_router_with_admin(
    service: Arc<AciService>,
    upstream_config: Arc<UpstreamConfigManager>,
    admin_token: Option<String>,
) -> Router {
    build_router_inner(service, Some(upstream_config), admin_token, None)
}

/// Build the gateway router with the middleware, which consults the
/// control plane and calls the service directly (in-process, no extra hop).
pub fn build_router_with_admin_and_middleware(
    service: Arc<AciService>,
    upstream_config: Arc<UpstreamConfigManager>,
    admin_token: Option<String>,
    middleware: Arc<Middleware>,
) -> Router {
    build_router_inner(
        service,
        Some(upstream_config),
        admin_token,
        Some(middleware),
    )
}

fn build_router_inner(
    service: Arc<AciService>,
    upstream_config: Option<Arc<UpstreamConfigManager>>,
    admin_token: Option<String>,
    middleware: Option<Arc<Middleware>>,
) -> Router {
    let state = AppState {
        service,
        upstream_config,
        admin_token,
        middleware,
    };
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        // OpenAI- and Anthropic-compatible inference surface.
        .route("/v1/models", get(models))
        .route("/v1/models/*rest", get(models_subpath))
        .route("/v1/embeddings/models", get(embeddings_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses))
        // Gateway operations.
        .route("/v1/metrics", get(metrics))
        .route(
            "/v1/admin/upstreams",
            get(admin_get_upstreams).put(admin_put_upstreams),
        )
        // Canonical ACI verification surface (clean shapes).
        .route("/v1/aci/attestation", get(aci_attestation_report))
        .route("/v1/aci/receipts/:id", get(aci_receipt))
        .route("/v1/aci/sessions", get(aci_list_sessions))
        .route("/v1/aci/sessions/:session_id", get(attested_session))
        // Legacy dstack-vllm-proxy aliases (vllm-proxy response shapes only;
        // we owe no back-compat to earlier private-ai-gateway paths).
        .route("/v1/attestation/report", get(attestation_report))
        .route("/v1/signature/:chat_id", get(receipt_by_chat_id))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            aci_headers_middleware,
        ))
        // Permissive CORS so browser clients can call the gateway directly.
        // Outermost layer: it answers preflight OPTIONS before routing, which
        // otherwise 405s since the routes only declare GET/POST/PUT.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Middleware that stamps `X-ACI-Version` and `X-ACI-Keyset-Digest` on every
/// response, including errors (§6.2). Unauthenticated routing hints: a
/// changed digest tells the client to re-fetch and re-verify the attestation
/// report.
async fn aci_headers_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert(
        HeaderName::from_static("x-aci-version"),
        HeaderValue::from_static("aci/1"),
    );
    if let Ok(v) = HeaderValue::from_str(state.service.workload_keyset_digest()) {
        headers.insert(HeaderName::from_static("x-aci-keyset-digest"), v);
    }
    resp
}
