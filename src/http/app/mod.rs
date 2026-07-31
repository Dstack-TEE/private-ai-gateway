//! Axum app exposing the ACI endpoints.
//!
//! Endpoints:
//!
//! * `GET  /v1/attestation/report` - service-scoped report; an
//!   optional `?nonce=...` query parameter is bound into
//!   `report_data` (URL-decoded UTF-8 string, or JSON `null` when
//!   absent).
//! * `POST /v1/chat/completions` - OpenAI-shaped chat-completion
//!   forwarding with ACI-side hashing and receipt signing. When an inference
//!   token digest is configured in direct mode, `Authorization: Bearer <token>`
//!   must match it; the bearer also owns the receipt for later authenticated
//!   retrieval. Middleware mode instead preserves each client's bearer for
//!   control-plane authorization and attribution.
//! * `POST /v1/completions` - compatibility surface. The aggregator
//!   forwards legacy prompt completions through the same ACI receipt
//!   path as chat completions. ACI E2EE is an optional add-on here;
//!   plaintext OpenAI-compatible requests remain unchanged.
//! * `POST /v1/embeddings` - OpenAI-shaped embeddings forwarding.
//!   Buffered-only; any client-sent `stream:true` is forced back to
//!   buffered before forwarding. The aggregator hashes the body and
//!   issues a receipt the same way as `/v1/chat/completions`; ACI
//!   E2EE is supported and operates on the `input` request field and
//!   each `data[].embedding` response field.
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
//! * `POST /v1/admin/revoke-keyset` - authenticated admin revocation of the
//!   current workload keyset (§4.7). Signs and persists the revocation
//!   statement, after which the service stops serving reports/inference under
//!   the revoked keyset.
//!
//! ACI verification artifacts live under the `/v1/aci/` namespace so they do
//! not pollute the OpenAI surface. The id parameter accepts the gateway
//! `receipt_id` (preferred; always on the `x-receipt-id` response header, and
//! the only handle for `/v1/embeddings` receipts which have no chat_id) or the
//! upstream `chat_id`.
//!
//! * `GET  /v1/aci/attestation` - the bare ACI attestation report.
//! * `GET  /v1/aci/receipts/{id}` - the signed ACI receipt (canonical value).
//! * `GET  /v1/aci/sessions/{session_id}` - the attested-session record a
//!   receipt references.
//! * `GET  /v1/aci/sessions?upstream_name=&model=` - list attested sessions.
//! * `GET  /v1/aci/revocations` - all issued keyset revocation statements
//!   (§4.7), a public transparency surface.
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
//! The router installs a middleware that emits `X-ACI-Version`,
//! `X-ACI-Identity`, and `X-ACI-Keyset-Digest` on every response,
//! including error paths.

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderName, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;

/// Request-body cap for the inference surface. Multimodal chat payloads with
/// base64 image parts routinely run 5–20 MB; axum's 2 MB default rejected them
/// with an extractor-level 413 that reached clients as a connection reset.
/// Bodies are buffered in memory, so the cap stays bounded rather than
/// unlimited.
const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;

use crate::aggregator::service::AciService;
use crate::aggregator::upstream_config::UpstreamConfigManager;
use crate::middleware::Middleware;
use util::enforce_inference;

mod backend;
mod error_responses;
mod handlers;
mod util;

use handlers::{
    aci_attestation_report, aci_list_sessions, aci_receipt, aci_revocations, admin_get_upstreams,
    admin_put_upstreams, admin_revoke_keyset, attestation_report, attested_session,
    chat_completions, completions, embeddings, embeddings_models, health, messages, metrics,
    models, models_subpath, receipt_by_chat_id, responses, root,
};

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<AciService>,
    pub upstream_config: Option<Arc<UpstreamConfigManager>>,
    pub admin_token: Option<String>,
    pub inference_token_sha256: Option<[u8; 32]>,
    middleware: Option<Arc<Middleware>>,
}

pub fn build_router(service: Arc<AciService>) -> Router {
    build_router_inner(service, None, None, None, None)
}

pub fn build_router_with_admin(
    service: Arc<AciService>,
    upstream_config: Arc<UpstreamConfigManager>,
    admin_token: Option<String>,
    inference_token_sha256: Option<[u8; 32]>,
) -> Router {
    build_router_inner(
        service,
        Some(upstream_config),
        admin_token,
        inference_token_sha256,
        None,
    )
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
        None,
        Some(middleware),
    )
}

fn build_router_inner(
    service: Arc<AciService>,
    upstream_config: Option<Arc<UpstreamConfigManager>>,
    admin_token: Option<String>,
    inference_token_sha256: Option<[u8; 32]>,
    middleware: Option<Arc<Middleware>>,
) -> Router {
    let state = AppState {
        service,
        upstream_config,
        admin_token,
        inference_token_sha256,
        middleware,
    };
    Router::new()
        // Apply inference authentication before any handler polls a body
        // extractor. `route_layer` affects only the inference routes declared
        // above it; catalogs and operational endpoints remain public.
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            inference_auth_middleware,
        ))
        .route("/", get(root))
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/models/*rest", get(models_subpath))
        .route("/v1/embeddings/models", get(embeddings_models))
        // Gateway operations.
        .route("/v1/metrics", get(metrics))
        .route(
            "/v1/admin/upstreams",
            get(admin_get_upstreams).put(admin_put_upstreams),
        )
        .route("/v1/admin/revoke-keyset", post(admin_revoke_keyset))
        // Canonical ACI verification surface (clean shapes).
        .route("/v1/aci/attestation", get(aci_attestation_report))
        .route("/v1/aci/receipts/:id", get(aci_receipt))
        .route("/v1/aci/sessions", get(aci_list_sessions))
        .route("/v1/aci/sessions/:session_id", get(attested_session))
        .route("/v1/aci/revocations", get(aci_revocations))
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
        // Multimodal chat payloads (base64 image parts) routinely exceed
        // axum's 2 MB default body limit. The default-limit 413 is rejected in
        // the extractor with the client's upload left unread, which hyper
        // turns into a TCP RST — clients see a connection error instead of a
        // 413, and the request never reaches request logging. Raise the cap so
        // legitimate payloads fit.
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

async fn inference_auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(response) = enforce_inference(&state, req.headers()) {
        return response;
    }
    next.run(req).await
}

/// Middleware that stamps `X-ACI-Version`, `X-ACI-Identity`, and
/// `X-ACI-Keyset-Digest` on every response, including errors. A
/// relying party can therefore confirm the workload identity that
/// served any HTTP path, not just the success path of
/// `POST /v1/chat/completions`.
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
    if let Ok(v) = HeaderValue::from_str(state.service.workload_id()) {
        headers.insert(HeaderName::from_static("x-aci-identity"), v);
    }
    if let Ok(v) = HeaderValue::from_str(state.service.workload_keyset_digest()) {
        headers.insert(HeaderName::from_static("x-aci-keyset-digest"), v);
    }
    resp
}
