//! Axum app exposing the ACI endpoints.
//!
//! Endpoints:
//!
//! * `GET  /v1/attestation/report` - service-scoped report; an
//!   optional `?nonce=...` query parameter is bound into
//!   `report_data` (URL-decoded UTF-8 string, or JSON `null` when
//!   absent).
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
//! * `GET  /v1/aci/attestation` - the bare ACI attestation report.
//! * `GET  /v1/aci/receipts/{id}` - the signed ACI receipt (canonical value).
//! * `GET  /v1/aci/sessions/{session_id}` - the attested-session record a
//!   receipt references.
//! * `GET  /v1/aci/sessions?provider=&model=` - list attested sessions.
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

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderName, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use hyper::body::Incoming;
use hyper::server::conn::http1::Builder as HyperHttp1Builder;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use tokio::net::UnixListener;
use tower::ServiceExt as _;

use crate::aggregator::service::{
    AciService, E2eeRequestContext, MiddlewareReceiptJournal, ReceiptOwner,
};
use crate::aggregator::upstream_config::UpstreamConfigManager;

mod backend;
mod error_responses;
mod handlers;
mod proxy;
mod util;

use backend::internal_forward;
use handlers::{
    aci_attestation_report, aci_list_sessions, aci_receipt, admin_get_upstreams,
    admin_put_upstreams, attestation_report, attested_session, chat_completions, completions,
    embeddings, embeddings_models, messages, metrics, models, models_subpath, receipt_by_chat_id,
    responses, root,
};

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<AciService>,
    pub upstream_config: Option<Arc<UpstreamConfigManager>>,
    pub admin_token: Option<String>,
    middleware: Option<UdsMiddleware>,
    request_store: GatewayRequestStore,
}

#[derive(Clone)]
pub(super) struct UdsMiddleware {
    base_url: String,
    client: reqwest::Client,
}

#[derive(Clone)]
pub struct GatewayRequestStore {
    inner: Arc<Mutex<HashMap<String, PendingGatewayRequest>>>,
    ttl: Duration,
}

#[derive(Clone)]
struct PendingGatewayRequest {
    expires_at: Instant,
    request: StoredGatewayRequest,
}

#[derive(Clone)]
pub struct StoredGatewayRequest {
    pub endpoint_path: &'static str,
    pub received_body: Vec<u8>,
    pub upstream_required: bool,
    pub requester: Option<ReceiptOwner>,
    pub e2ee: Option<E2eeRequestContext>,
    pub user_model: Option<String>,
    pub receipt_journal: MiddlewareReceiptJournal,
}

impl GatewayRequestStore {
    const DEFAULT_TTL: Duration = Duration::from_secs(300);

    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    pub fn insert(&self, request_id: String, request: StoredGatewayRequest) {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("gateway request store poisoned");
        inner.retain(|_, pending| pending.expires_at > now);
        inner.insert(
            request_id,
            PendingGatewayRequest {
                expires_at: now + self.ttl,
                request,
            },
        );
    }

    fn take(&self, request_id: &str) -> Option<StoredGatewayRequest> {
        let pending = self
            .inner
            .lock()
            .expect("gateway request store poisoned")
            .remove(request_id)?;
        (pending.expires_at > Instant::now()).then_some(pending.request)
    }
}

impl Default for GatewayRequestStore {
    fn default() -> Self {
        Self::new(Self::DEFAULT_TTL)
    }
}

#[derive(Clone)]
struct InternalBackendState {
    service: Arc<AciService>,
    request_store: GatewayRequestStore,
}

pub fn build_router(service: Arc<AciService>) -> Router {
    build_router_inner(service, None, None, None, GatewayRequestStore::default())
}

pub fn build_router_with_admin(
    service: Arc<AciService>,
    upstream_config: Arc<UpstreamConfigManager>,
    admin_token: Option<String>,
) -> Router {
    build_router_inner(
        service,
        Some(upstream_config),
        admin_token,
        None,
        GatewayRequestStore::default(),
    )
}

pub fn build_router_with_admin_and_uds_middleware(
    service: Arc<AciService>,
    upstream_config: Arc<UpstreamConfigManager>,
    admin_token: Option<String>,
    request_store: GatewayRequestStore,
    middleware_socket_path: impl Into<PathBuf>,
) -> Router {
    build_router_inner(
        service,
        Some(upstream_config),
        admin_token,
        Some(uds_middleware(middleware_socket_path)),
        request_store,
    )
}

pub fn build_router_with_uds_middleware(
    service: Arc<AciService>,
    request_store: GatewayRequestStore,
    middleware_socket_path: impl Into<PathBuf>,
) -> Router {
    build_router_inner(
        service,
        None,
        None,
        Some(uds_middleware(middleware_socket_path)),
        request_store,
    )
}

fn uds_middleware(middleware_socket_path: impl Into<PathBuf>) -> UdsMiddleware {
    let path = middleware_socket_path.into();
    let client = reqwest::Client::builder()
        .unix_socket(path)
        .build()
        .expect("failed to construct Unix-socket middleware HTTP client");
    UdsMiddleware {
        base_url: "http://private-ai-gateway-middleware".to_string(),
        client,
    }
}

fn build_router_inner(
    service: Arc<AciService>,
    upstream_config: Option<Arc<UpstreamConfigManager>>,
    admin_token: Option<String>,
    middleware: Option<UdsMiddleware>,
    request_store: GatewayRequestStore,
) -> Router {
    let state = AppState {
        service,
        upstream_config,
        admin_token,
        middleware,
        request_store,
    };
    Router::new()
        .route("/", get(root))
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
        .with_state(state)
}

pub fn build_internal_backend_router(
    service: Arc<AciService>,
    request_store: GatewayRequestStore,
) -> Router {
    Router::new()
        .route("/internal/forward", post(internal_forward))
        .with_state(InternalBackendState {
            service,
            request_store,
        })
}

pub async fn serve_unix_router(
    socket_path: impl AsRef<FsPath>,
    app: Router,
) -> Result<(), std::io::Error> {
    let listener = bind_unix_listener(socket_path)?;
    serve_unix_listener(listener, app).await
}

pub fn bind_unix_listener(socket_path: impl AsRef<FsPath>) -> Result<UnixListener, std::io::Error> {
    let socket_path = socket_path.as_ref();
    prepare_unix_socket(socket_path)?;
    UnixListener::bind(socket_path)
}

fn prepare_unix_socket(socket_path: &FsPath) -> Result<(), std::io::Error> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if metadata.file_type().is_socket() {
                    std::fs::remove_file(socket_path)?;
                    return Ok(());
                }
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "refusing to replace non-socket path {}",
                    socket_path.display()
                ),
            ))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub async fn serve_unix_listener(
    listener: UnixListener,
    app: Router,
) -> Result<(), std::io::Error> {
    loop {
        let (stream, _) = listener.accept().await?;
        let service = app
            .clone()
            .map_request(|request: hyper::Request<Incoming>| request.map(Body::new));
        tokio::spawn(async move {
            let hyper_service = TowerToHyperService::new(service);
            if let Err(err) = HyperHttp1Builder::new()
                .serve_connection(TokioIo::new(stream), hyper_service)
                .await
            {
                tracing::debug!(error = %err, "Unix-socket HTTP connection closed");
            }
        });
    }
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
