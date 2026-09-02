//! The loopback application proxy on the stable local endpoint, served over
//! TLS with the installation's identity. It admits only requests that carry an
//! issued agent token for that agent's surfaces, forwards only while a
//! verified session (identity + catalog, one generation and epoch) is
//! published and the service declares the surface, and swaps the agent token
//! for the RedPill key on the way to the sidecar. Request bodies are buffered
//! (bounded, with a read timeout) so the model can be checked against the
//! verified catalog; responses stream through. Credentials and the session
//! are re-validated after the body is read, right before anything leaves the
//! process. The proxy adds one attribution header the sidecar copies into its
//! receipt event and strips before forwarding.

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, PoisonError, RwLock, RwLockReadGuard,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::mpsc, sync::Semaphore};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::{
    catalog::{Catalog, Surface},
    tokens::{agent_allows, TokenSet},
};

/// Request bodies are buffered up to this size; the sidecar applies the same
/// limit. Responses stream.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IN_FLIGHT: usize = 64;
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(60);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Idle limit between upstream bytes; matches Claude Code's stream watchdog.
const UPSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(300);
/// Added on the way to the sidecar, which copies it into the receipt event.
pub const TAG_HEADER: &str = "x-aci-tag";
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The verified sidecar session the proxy may forward to. Identity, catalog,
/// generation (per sidecar start) and epoch (per identity/catalog read) are
/// published together; anything from another generation or epoch is stale.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub generation: u64,
    pub epoch: u64,
    pub base_url: Option<String>,
    pub verified: bool,
    pub catalog: Option<Catalog>,
}

/// A request the proxy answered itself (rejected or failed before a receipt).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEvent {
    pub agent: Option<String>,
    pub method: String,
    pub path: String,
    pub model: Option<String>,
    pub status: u16,
    pub detail: String,
    pub at: u64,
}

#[derive(Default)]
struct Credentials {
    tokens: TokenSet,
    api_key: Option<String>,
}

pub struct ProxyState {
    session: RwLock<Session>,
    credentials: RwLock<Credentials>,
    /// Bumped on every token or key change; a request admitted under an
    /// older epoch is refused before it is sent.
    credential_epoch: AtomicU64,
    /// Delivery gate: every revocation (credentials or session) replaces and
    /// cancels this token, so a request that already passed its final checks
    /// but has not started sending is stopped instead of delivered.
    gate: RwLock<CancellationToken>,
    client: reqwest::Client,
    events: mpsc::Sender<ProxyEvent>,
    in_flight: Arc<Semaphore>,
    #[cfg(test)]
    pause: std::sync::Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}

impl ProxyState {
    /// `events` is bounded; when it is full, low-value rejection events are
    /// dropped rather than blocking a request.
    pub fn new(events: mpsc::Sender<ProxyEvent>) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
            .read_timeout(UPSTREAM_READ_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Arc::new(Self {
            session: RwLock::new(Session::default()),
            credentials: RwLock::new(Credentials::default()),
            credential_epoch: AtomicU64::new(1),
            gate: RwLock::new(CancellationToken::new()),
            client,
            events,
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            #[cfg(test)]
            pause: std::sync::Mutex::new(None),
        })
    }

    /// Cancel every delivery admitted so far; called after any state change
    /// that invalidates earlier admissions.
    fn revoke_deliveries(&self) {
        let previous = std::mem::replace(&mut *write(&self.gate), CancellationToken::new());
        previous.cancel();
    }

    // State is guarded by std locks: writers are the desktop shell's event
    // handlers (synchronous), readers never hold a guard across an await.
    /// Publish a session atomically; replaces whatever was there and stops
    /// deliveries admitted under the previous one.
    pub fn publish(&self, session: Session) {
        *write(&self.session) = session;
        self.revoke_deliveries();
    }

    pub fn session(&self) -> Session {
        read(&self.session).clone()
    }

    /// Replace the key in memory. Revocation is immediate: the epoch moves
    /// and admitted-but-unsent deliveries are cancelled before this returns.
    pub fn set_api_key(&self, key: Option<String>) {
        write(&self.credentials).api_key = key;
        self.credential_epoch.fetch_add(1, Ordering::SeqCst);
        self.revoke_deliveries();
    }

    pub fn set_tokens(&self, tokens: TokenSet) {
        write(&self.credentials).tokens = tokens;
        self.credential_epoch.fetch_add(1, Ordering::SeqCst);
        self.revoke_deliveries();
    }

    pub fn tokens(&self) -> TokenSet {
        read(&self.credentials).tokens.clone()
    }

    /// Read the model list through the sidecar of `generation`; the caller
    /// publishes it under `epoch`. A result for another generation or a newer
    /// epoch is refused here so it can never be published stale.
    pub async fn fetch_catalog(&self, generation: u64, epoch: u64) -> Result<Catalog, String> {
        let base_url = {
            let session = read(&self.session);
            if session.generation != generation || session.epoch != epoch {
                return Err(
                    "The gateway's identity changed while reading the model list".to_string(),
                );
            }
            session
                .base_url
                .clone()
                .ok_or_else(|| "The gateway is not running".to_string())?
        };
        let mut request = self
            .client
            .get(format!("{base_url}/v1/models"))
            .timeout(Duration::from_secs(30));
        if let Some(key) = read(&self.credentials).api_key.as_deref() {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|_| {
            "The verified gateway did not answer the model list request".to_string()
        })?;
        if !response.status().is_success() {
            return Err(format!(
                "The model list request failed with HTTP {}",
                response.status().as_u16()
            ));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|_| "The model list is not valid JSON".to_string())?;
        let catalog = Catalog::from_remote(&body, now_secs())?;
        let session = read(&self.session);
        if session.generation != generation || session.epoch != epoch {
            return Err("The gateway's identity changed while reading the model list".to_string());
        }
        Ok(catalog)
    }

    /// The verified session, or the rejection to send instead.
    fn verified_session(&self) -> Result<Lease, Rejection> {
        let session = read(&self.session);
        match (&session.base_url, session.verified, &session.catalog) {
            (Some(base_url), true, Some(_)) => Ok(Lease {
                generation: session.generation,
                epoch: session.epoch,
                base_url: base_url.clone(),
            }),
            _ => Err(Rejection::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "gateway_not_verified",
                "The gateway is not verified; requests are blocked until verification succeeds",
            )),
        }
    }

    /// Re-check a lease right before sending upstream.
    fn lease_valid(&self, lease: &Lease) -> bool {
        let session = read(&self.session);
        session.verified
            && session.generation == lease.generation
            && session.epoch == lease.epoch
            && session.catalog.is_some()
    }

    fn authorize(&self, headers: &HeaderMap, path: &str) -> Result<Auth, Rejection> {
        let token = presented_token(headers).ok_or_else(|| {
            Rejection::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "This endpoint accepts only agents connected through Private AI Gateway",
            )
        })?;
        let epoch = self.credential_epoch.load(Ordering::SeqCst);
        let agent = read(&self.credentials)
            .tokens
            .agent_for(&token)
            .map(str::to_string)
            .ok_or_else(|| {
                Rejection::new(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "The agent token is not recognized; reconnect the agent in Private AI Gateway",
                )
            })?;
        if !agent_allows(&agent, path) {
            return Err(Rejection::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "This agent's token is not valid for this endpoint",
            ));
        }
        Ok(Auth {
            agent,
            token,
            epoch,
        })
    }

    /// The API key to send, provided the credentials that admitted this
    /// request are still exactly in force.
    fn current_key(&self, auth: &Auth) -> Result<String, Rejection> {
        if self.credential_epoch.load(Ordering::SeqCst) != auth.epoch {
            return Err(Rejection::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "credentials_changed",
                "Credentials changed while the request was being read; send it again",
            ));
        }
        let credentials = read(&self.credentials);
        if credentials.tokens.agent_for(&auth.token) != Some(auth.agent.as_str()) {
            return Err(Rejection::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "The agent token was revoked",
            ));
        }
        credentials.api_key.clone().ok_or_else(|| {
            Rejection::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "api_key_missing",
                "No RedPill API key is saved in Private AI Gateway",
            )
        })
    }

    fn emit(&self, event: ProxyEvent) {
        let _ = self.events.try_send(event);
    }
}

/// Permission to forward one request through a specific verified session.
struct Lease {
    generation: u64,
    epoch: u64,
    base_url: String,
}

/// Who a request was admitted as, and under which credential epoch.
struct Auth {
    agent: String,
    token: String,
    epoch: u64,
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

struct Rejection {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl Rejection {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

pub fn router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/responses", post(responses))
        .route("/v1/responses/compact", post(responses_compact))
        .fallback(not_found)
        .with_state(state)
}

/// Bind the local endpoint synchronously and exclusively. A busy port surfaces
/// here, before the app can start or connect anything.
pub fn bind_std(addr: SocketAddr) -> Result<std::net::TcpListener, String> {
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|error| format!("Cannot listen on {addr}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Cannot configure the listener on {addr}: {error}"))?;
    Ok(listener)
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PENDING_HANDSHAKES: usize = 64;

/// Serve the proxy over TLS on an already-bound listener until it fails. Each
/// connection must complete a handshake against the installation identity
/// before any HTTP is read; handshakes are bounded and timed so idle or slow
/// connections release their permit and leak no task or socket.
pub async fn serve_tls(
    state: Arc<ProxyState>,
    listener: std::net::TcpListener,
    server_config: Arc<rustls::ServerConfig>,
) -> Result<(), String> {
    serve_tls_with(
        state,
        listener,
        server_config,
        HANDSHAKE_TIMEOUT,
        MAX_PENDING_HANDSHAKES,
    )
    .await
}

async fn serve_tls_with(
    state: Arc<ProxyState>,
    listener: std::net::TcpListener,
    server_config: Arc<rustls::ServerConfig>,
    handshake_timeout: Duration,
    max_pending_handshakes: usize,
) -> Result<(), String> {
    let listener = TcpListener::from_std(listener)
        .map_err(|error| format!("Cannot use the local listener: {error}"))?;
    let acceptor = TlsAcceptor::from(server_config);
    let service = TowerToHyperService::new(router(state));
    let permits = Arc::new(Semaphore::new(max_pending_handshakes));
    loop {
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "The local gateway stopped".to_string())?;
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("The local gateway stopped: {error}"))?;
        let acceptor = acceptor.clone();
        let service = service.clone();
        tokio::spawn(async move {
            let tls = {
                let _permit = permit;
                match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                    Ok(Ok(tls)) => tls,
                    // Timeout or bad handshake: the stream and permit drop here.
                    _ => return,
                }
            };
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tls), service)
                .await;
        });
    }
}

async fn health(State(state): State<Arc<ProxyState>>) -> Response {
    let session = state.session();
    let models = session
        .catalog
        .as_ref()
        .map_or(0, |catalog| catalog.models.len());
    Json(json!({ "status": "ok", "verified": session.verified, "models": models })).into_response()
}

async fn models(State(state): State<Arc<ProxyState>>, headers: HeaderMap) -> Response {
    let surface = Surface::ChatCompletions;
    if let Err(rejection) = state.authorize(&headers, "/v1/models") {
        return reject(&state, None, "GET", "/v1/models", None, surface, rejection);
    }
    let session = state.session();
    match (session.verified, session.catalog) {
        (true, Some(catalog)) => Json(catalog.openai_list()).into_response(),
        _ => reject(
            &state,
            None,
            "GET",
            "/v1/models",
            None,
            surface,
            Rejection::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "gateway_not_verified",
                "The verified model list is not available until verification succeeds",
            ),
        ),
    }
}

async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        body,
        Surface::ChatCompletions,
        "/v1/chat/completions",
    )
    .await
}

async fn messages(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    relay(state, headers, body, Surface::Messages, "/v1/messages").await
}

async fn responses(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    relay(state, headers, body, Surface::Responses, "/v1/responses").await
}

/// Helper endpoints belong to their surface and are gated exactly like
/// inference: agent scope, verified session, declared surface, and a model
/// present in the catalog (both protocols require `model`).
async fn count_tokens(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        body,
        Surface::Messages,
        "/v1/messages/count_tokens",
    )
    .await
}

async fn responses_compact(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        body,
        Surface::Responses,
        "/v1/responses/compact",
    )
    .await
}

async fn not_found() -> Response {
    error_response(
        Surface::ChatCompletions,
        StatusCode::NOT_FOUND,
        "not_found",
        "Private AI Gateway serves /v1/models, /v1/chat/completions, /v1/messages, and /v1/responses",
    )
}

async fn relay(
    state: Arc<ProxyState>,
    headers: HeaderMap,
    body: Body,
    surface: Surface,
    path: &'static str,
) -> Response {
    let auth = match state.authorize(&headers, path) {
        Ok(auth) => auth,
        Err(rejection) => return reject(&state, None, "POST", path, None, surface, rejection),
    };
    let agent = auth.agent.clone();
    let Ok(_permit) = state.in_flight.clone().try_acquire_owned() else {
        let rejection = Rejection::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many requests are in flight through the local gateway; retry shortly",
        );
        return reject(&state, Some(agent), "POST", path, None, surface, rejection);
    };
    let lease = match state.verified_session() {
        Ok(lease) => lease,
        Err(rejection) => {
            return reject(&state, Some(agent), "POST", path, None, surface, rejection)
        }
    };
    if let Err(rejection) = state.current_key(&auth) {
        return reject(&state, Some(agent), "POST", path, None, surface, rejection);
    }
    let bytes = match tokio::time::timeout(BODY_READ_TIMEOUT, to_bytes(body, MAX_BODY_BYTES)).await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            let rejection = Rejection::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "The request body exceeds the local gateway limit",
            );
            return reject(&state, Some(agent), "POST", path, None, surface, rejection);
        }
        Err(_) => {
            let rejection = Rejection::new(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "The request body was not received in time",
            );
            return reject(&state, Some(agent), "POST", path, None, surface, rejection);
        }
    };
    let model = model_of(&bytes);
    let decision = match check_catalog(&state, surface, model.as_deref()) {
        Ok(label) => label,
        Err(rejection) => {
            return reject(&state, Some(agent), "POST", path, model, surface, rejection)
        }
    };
    // Take the delivery token first, then re-validate session and credentials:
    // a revocation after this point cancels the token, and the send below is
    // raced against it, so nothing admitted here can leave once revoked.
    let delivery = read(&state.gate).clone();
    if !state.lease_valid(&lease) {
        let rejection = Rejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_not_verified",
            "The gateway's verification changed while the request was being read",
        );
        return reject(&state, Some(agent), "POST", path, model, surface, rejection);
    }
    let key = match state.current_key(&auth) {
        Ok(key) => key,
        Err(rejection) => {
            return reject(&state, Some(agent), "POST", path, model, surface, rejection)
        }
    };
    #[cfg(test)]
    {
        let pause = state.pause.lock().ok().and_then(|slot| slot.clone());
        if let Some((reached, resume)) = pause {
            reached.notify_one();
            resume.notified().await;
        }
    }
    forward(
        &state,
        &lease.base_url,
        &key,
        &agent,
        surface,
        path,
        decision,
        &headers,
        bytes,
        model,
        delivery,
    )
    .await
}

fn check_catalog(
    state: &ProxyState,
    surface: Surface,
    model: Option<&str>,
) -> Result<&'static str, Rejection> {
    let model = model.ok_or_else(|| {
        Rejection::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "The request body must be a JSON object with a string `model`",
        )
    })?;
    let session = read(&state.session);
    let catalog = session.catalog.as_ref().ok_or_else(|| {
        Rejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_not_verified",
            "The verified model list is not available",
        )
    })?;
    let entry = catalog.get(model).ok_or_else(|| {
        Rejection::new(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("`{model}` is not in the verified model list"),
        )
    })?;
    let support = entry.support(surface);
    if support.allows_requests() {
        Ok(support.label())
    } else {
        Err(Rejection::new(
            StatusCode::BAD_REQUEST,
            "surface_undeclared",
            format!(
                "The service does not declare {} support, so `{model}` is not forwarded on it",
                surface.path()
            ),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward(
    state: &ProxyState,
    base_url: &str,
    key: &str,
    agent: &str,
    surface: Surface,
    path: &str,
    decision: &str,
    headers: &HeaderMap,
    body: Bytes,
    model: Option<String>,
    delivery: CancellationToken,
) -> Response {
    let dropped = hop_by_hop_names(headers);
    let mut request = state.client.post(format!("{base_url}{path}"));
    for (name, value) in headers {
        let name = name.as_str();
        if !dropped.contains(name)
            && !matches!(
                name,
                "host" | "content-length" | "authorization" | "x-api-key"
            )
            && name != TAG_HEADER
        {
            request = request.header(name, value.as_bytes());
        }
    }
    let request = request
        .header(header::AUTHORIZATION.as_str(), format!("Bearer {key}"))
        .header(TAG_HEADER, format!("{agent} {decision}"))
        .body(body);
    // The send is raced against revocation: cancelled before it starts means
    // nothing is delivered; cancelled mid-send aborts the connection so the
    // sidecar never completes the request.
    let sent = tokio::select! {
        biased;
        _ = delivery.cancelled() => None,
        result = request.send() => Some(result),
    };
    let Some(sent) = sent else {
        let rejection = Rejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "revoked",
            "Credentials or the verified session were revoked before the request was sent; \
             send it again",
        );
        return reject(
            state,
            Some(agent.to_string()),
            "POST",
            path,
            model,
            surface,
            rejection,
        );
    };
    let upstream = match sent {
        Ok(upstream) => upstream,
        Err(error) => {
            let rejection = if error.is_timeout() {
                Rejection::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    "upstream_timeout",
                    "The verified gateway did not respond in time",
                )
            } else {
                Rejection::new(
                    StatusCode::BAD_GATEWAY,
                    "upstream_unreachable",
                    "The verified gateway did not respond",
                )
            };
            return reject(
                state,
                Some(agent.to_string()),
                "POST",
                path,
                model,
                surface,
                rejection,
            );
        }
    };
    let dropped = hop_by_hop_names(upstream.headers());
    let mut builder = Response::builder().status(upstream.status().as_u16());
    for (name, value) in upstream.headers() {
        if !dropped.contains(name.as_str()) {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    builder
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap_or_else(|_| {
            error_response(
                surface,
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "The verified gateway returned an unusable response",
            )
        })
}

/// Standard hop-by-hop names plus whatever the `Connection` header names.
pub fn hop_by_hop_names(headers: &HeaderMap) -> HashSet<String> {
    let mut names: HashSet<String> = HOP_BY_HOP.iter().map(|name| name.to_string()).collect();
    for value in headers.get_all(header::CONNECTION) {
        if let Ok(value) = value.to_str() {
            names.extend(
                value
                    .split(',')
                    .map(|token| token.trim().to_ascii_lowercase())
                    .filter(|token| !token.is_empty()),
            );
        }
    }
    names
}

#[allow(clippy::too_many_arguments)]
fn reject(
    state: &ProxyState,
    agent: Option<String>,
    method: &str,
    path: &str,
    model: Option<String>,
    surface: Surface,
    rejection: Rejection,
) -> Response {
    state.emit(ProxyEvent {
        agent,
        method: method.to_string(),
        path: path.to_string(),
        model,
        status: rejection.status.as_u16(),
        detail: rejection.message.clone(),
        at: now_secs(),
    });
    error_response(
        surface,
        rejection.status,
        rejection.code,
        &rejection.message,
    )
}

/// An error in the surface's own envelope so the agent can display it.
fn error_response(surface: Surface, status: StatusCode, code: &str, message: &str) -> Response {
    let body = match surface {
        Surface::Messages => {
            json!({ "type": "error", "error": { "type": code, "message": message } })
        }
        _ => json!({ "error": { "message": message, "type": code, "code": code } }),
    };
    let mut response = (status, Json(body)).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"Private AI Gateway\""),
        );
    }
    response
}

fn presented_token(headers: &HeaderMap) -> Option<String> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        });
    let api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok());
    bearer
        .or(api_key)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn model_of(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(bytes)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn spawn(router: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// A stand-in sidecar that echoes what it received and declares the
    /// chat and messages surfaces.
    async fn mock_sidecar() -> String {
        let echo = |headers: HeaderMap, body: Bytes| async move {
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string()
            };
            (
                [("x-receipt-id", "rcpt-1")],
                Json(json!({
                    "authorization": header("authorization"),
                    "x-api-key": header("x-api-key"),
                    "tag": header(TAG_HEADER),
                    "anthropic-beta": header("anthropic-beta"),
                    "proxy-connection": header("proxy-connection"),
                    "body": String::from_utf8_lossy(&body),
                })),
            )
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(echo))
            .route("/v1/messages/count_tokens", post(echo))
            .route(
                "/v1/models",
                get(|| async {
                    Json(json!({
                        "data": [{ "id": "openai/gpt-oss-20b" }],
                        "aci_capabilities": { "version": 1, "surfaces": {
                            "chat_completions": "all", "messages": "all", "responses": "undeclared"
                        }}
                    }))
                }),
            );
        spawn(app).await
    }

    fn state() -> (Arc<ProxyState>, mpsc::Receiver<ProxyEvent>) {
        let (sender, receiver) = mpsc::channel(4);
        (ProxyState::new(sender), receiver)
    }

    fn tokens() -> TokenSet {
        let mut set = TokenSet::default();
        set.insert("codex-token".to_string(), "codex".to_string());
        set.insert("claude-token".to_string(), "claude-code".to_string());
        set.insert("opencode-token".to_string(), "opencode".to_string());
        set
    }

    async fn verified(state: &ProxyState, sidecar: &str, generation: u64, epoch: u64) {
        state.publish(Session {
            generation,
            epoch,
            base_url: Some(sidecar.to_string()),
            verified: false,
            catalog: None,
        });
        let catalog = state.fetch_catalog(generation, epoch).await.unwrap();
        state.publish(Session {
            generation,
            epoch,
            base_url: Some(sidecar.to_string()),
            verified: true,
            catalog: Some(catalog),
        });
    }

    /// Idle sockets cannot exhaust the handshake permits: they time out,
    /// release their permit, and a real TLS client still gets through.
    #[tokio::test]
    async fn handshakes_are_bounded_and_idle_connections_time_out() {
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!("pag-hs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let identity =
            crate::tls::load_or_create(&dir, &crate::secrets::MemoryStore::default()).unwrap();
        let (sender, _receiver) = mpsc::channel(4);
        let state = ProxyState::new(sender);
        let listener = bind_std("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_tls_with(
            state,
            listener,
            identity.server_config.clone(),
            Duration::from_millis(200),
            2,
        ));
        // Two idle raw connections take every permit and send nothing.
        let idle_a = std::net::TcpStream::connect(addr).unwrap();
        let idle_b = std::net::TcpStream::connect(addr).unwrap();
        // A real client still completes: the idle handshakes time out and
        // release their permits.
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .add_root_certificate(
                reqwest::Certificate::from_pem(identity.ca_pem.as_bytes()).unwrap(),
            )
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .get(format!("https://127.0.0.1:{}/health", addr.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        // The idle sockets were closed by the server (read returns EOF).
        for mut idle in [idle_a, idle_b] {
            idle.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut buf = [0u8; 1];
            assert_eq!(idle.read(&mut buf).unwrap_or(0), 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connection_named_headers_are_hop_by_hop() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "connection",
            HeaderValue::from_static("close, X-Secret-Hop"),
        );
        headers.insert("x-secret-hop", HeaderValue::from_static("1"));
        let dropped = hop_by_hop_names(&headers);
        assert!(dropped.contains("x-secret-hop"));
        assert!(dropped.contains("proxy-connection"));
        assert!(dropped.contains("transfer-encoding"));
        assert!(!dropped.contains("anthropic-beta"));
    }

    #[test]
    fn a_squatted_port_is_refused_before_anything_starts() {
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = squatter.local_addr().unwrap();
        let error = bind_std(addr).unwrap_err();
        assert!(error.contains("Cannot listen"));
        drop(squatter);
        assert!(bind_std(addr).is_ok());
    }

    #[tokio::test]
    async fn anonymous_wrong_and_cross_agent_tokens_are_refused() {
        let (state, mut events) = state();
        state.set_tokens(tokens());
        let proxy = spawn(router(state)).await;
        let client = reqwest::Client::new();
        let anonymous = client
            .post(format!("{proxy}/v1/chat/completions"))
            .json(&json!({ "model": "openai/gpt-oss-20b" }))
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status().as_u16(), 401);
        assert!(anonymous.headers().contains_key("www-authenticate"));
        let wrong = client
            .post(format!("{proxy}/v1/messages"))
            .header("x-api-key", "guess")
            .json(&json!({ "model": "openai/gpt-oss-20b" }))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status().as_u16(), 401);
        let body: Value = wrong.json().await.unwrap();
        assert_eq!(body["type"], json!("error"));
        let anonymous_count = client
            .post(format!("{proxy}/v1/messages/count_tokens"))
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous_count.status().as_u16(), 401);
        for path in [
            "/v1/messages",
            "/v1/messages/count_tokens",
            "/v1/chat/completions",
        ] {
            let cross = client
                .post(format!("{proxy}{path}"))
                .bearer_auth("codex-token")
                .json(&json!({ "model": "openai/gpt-oss-20b" }))
                .send()
                .await
                .unwrap();
            assert_eq!(cross.status().as_u16(), 403, "{path}");
        }
        assert_eq!(events.recv().await.unwrap().status, 401);
    }

    #[tokio::test]
    async fn requests_fail_closed_until_a_verified_session_with_a_catalog_and_key() {
        let (state, _events) = state();
        state.set_tokens(tokens());
        let sidecar = mock_sidecar().await;
        let proxy = spawn(router(state.clone())).await;
        let client = reqwest::Client::new();
        let send = |client: reqwest::Client, proxy: String| async move {
            client
                .post(format!("{proxy}/v1/chat/completions"))
                .bearer_auth("opencode-token")
                .json(&json!({ "model": "openai/gpt-oss-20b", "messages": [] }))
                .send()
                .await
                .unwrap()
        };
        assert_eq!(
            send(client.clone(), proxy.clone()).await.status().as_u16(),
            503
        );

        state.publish(Session {
            generation: 1,
            epoch: 1,
            base_url: Some(sidecar.clone()),
            verified: true,
            catalog: None,
        });
        assert_eq!(
            send(client.clone(), proxy.clone()).await.status().as_u16(),
            503
        );

        verified(&state, &sidecar, 1, 1).await;
        let response = send(client.clone(), proxy.clone()).await;
        assert_eq!(response.status().as_u16(), 503);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], json!("api_key_missing"));

        // A new epoch (identity update) or generation (restart) revokes the
        // session, and a fetch started under the old epoch is refused.
        state.set_api_key(Some("sk-real".to_string()));
        state.publish(Session {
            generation: 1,
            epoch: 2,
            base_url: Some(sidecar.clone()),
            verified: false,
            catalog: None,
        });
        assert_eq!(
            send(client.clone(), proxy.clone()).await.status().as_u16(),
            503
        );
        assert!(
            state.fetch_catalog(1, 1).await.is_err(),
            "stale epoch refused"
        );
        assert!(state.fetch_catalog(1, 2).await.is_ok());
    }

    #[tokio::test]
    async fn only_declared_catalog_models_are_forwarded_with_the_real_key() {
        let (state, mut events) = state();
        state.set_tokens(tokens());
        let sidecar = mock_sidecar().await;
        verified(&state, &sidecar, 1, 1).await;
        state.set_api_key(Some("sk-real".to_string()));
        let proxy = spawn(router(state.clone())).await;
        let client = reqwest::Client::new();

        let unknown = client
            .post(format!("{proxy}/v1/chat/completions"))
            .bearer_auth("opencode-token")
            .json(&json!({ "model": "gpt-5", "messages": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status().as_u16(), 404);
        assert_eq!(events.recv().await.unwrap().model.as_deref(), Some("gpt-5"));

        // Responses is undeclared by the service: refused, never guessed.
        let undeclared = client
            .post(format!("{proxy}/v1/responses"))
            .bearer_auth("codex-token")
            .json(&json!({ "model": "openai/gpt-oss-20b", "input": "hi" }))
            .send()
            .await
            .unwrap();
        assert_eq!(undeclared.status().as_u16(), 400);
        let body: Value = undeclared.json().await.unwrap();
        assert_eq!(body["error"]["code"], json!("surface_undeclared"));

        let forwarded = client
            .post(format!("{proxy}/v1/chat/completions"))
            .bearer_auth("opencode-token")
            .header("anthropic-beta", "keep-me")
            .header("proxy-connection", "keep-alive")
            .json(&json!({ "model": "openai/gpt-oss-20b", "messages": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(forwarded.status().as_u16(), 200);
        assert_eq!(forwarded.headers()["x-receipt-id"], "rcpt-1");
        let echo: Value = forwarded.json().await.unwrap();
        assert_eq!(echo["authorization"], json!("Bearer sk-real"));
        assert_eq!(echo["x-api-key"], json!(""));
        assert_eq!(echo["tag"], json!("opencode declared"));
        assert_eq!(echo["anthropic-beta"], json!("keep-me"));
        assert_eq!(echo["proxy-connection"], json!(""));

        let counted = client
            .post(format!("{proxy}/v1/messages/count_tokens"))
            .bearer_auth("claude-token")
            .json(&json!({ "model": "openai/gpt-oss-20b" }))
            .send()
            .await
            .unwrap();
        assert_eq!(counted.status().as_u16(), 200);
        let echo: Value = counted.json().await.unwrap();
        assert_eq!(echo["tag"], json!("claude-code declared"));
    }

    /// A token revoked (or a key removed) while the body is still arriving
    /// must fail the request before it is sent upstream.
    #[tokio::test]
    async fn credentials_revoked_mid_body_fail_before_send() {
        let (state, _events) = state();
        state.set_tokens(tokens());
        let sidecar = mock_sidecar().await;
        verified(&state, &sidecar, 1, 1).await;
        state.set_api_key(Some("sk-real".to_string()));
        let proxy = spawn(router(state.clone())).await;

        // Stream a body slowly: first chunk now, the rest after revocation.
        let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
        tx.send(Ok(Bytes::from_static(
            b"{\"model\":\"openai/gpt-oss-20b\",",
        )))
        .await
        .unwrap();
        let revoke_state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            revoke_state.set_tokens(revoke_state.tokens().without("opencode"));
            tx.send(Ok(Bytes::from_static(b"\"messages\":[]}")))
                .await
                .unwrap();
        });
        let response = reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .bearer_auth("opencode-token")
            .header("content-type", "application/json")
            .body(reqwest::Body::wrap_stream(tokio_stream_from_receiver(rx)))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 503);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], json!("credentials_changed"));
    }

    fn tokio_stream_from_receiver(
        mut rx: mpsc::Receiver<Result<Bytes, std::io::Error>>,
    ) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
        futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
    }

    /// Helpers are gated like inference: unknown or missing model, and a
    /// surface the service stopped declaring, are refused.
    #[tokio::test]
    async fn helper_endpoints_are_capability_gated() {
        let (state, _events) = state();
        state.set_tokens(tokens());
        let sidecar = mock_sidecar().await;
        verified(&state, &sidecar, 1, 1).await;
        state.set_api_key(Some("sk-real".to_string()));
        let proxy = spawn(router(state.clone())).await;
        let client = reqwest::Client::new();
        let count = |body: Value| {
            client
                .post(format!("{proxy}/v1/messages/count_tokens"))
                .bearer_auth("claude-token")
                .json(&body)
                .send()
        };
        assert_eq!(
            count(json!({ "model": "gpt-5" }))
                .await
                .unwrap()
                .status()
                .as_u16(),
            404
        );
        assert_eq!(
            count(json!({ "messages": [] }))
                .await
                .unwrap()
                .status()
                .as_u16(),
            400
        );
        assert_eq!(
            count(json!({ "model": "openai/gpt-oss-20b" }))
                .await
                .unwrap()
                .status()
                .as_u16(),
            200
        );

        // The service (or an older one) no longer declares Messages: refused.
        let mut session = state.session();
        let mut catalog = session.catalog.take().unwrap();
        catalog.capabilities = None;
        for model in &mut catalog.models {
            model.messages = crate::catalog::Support::Undeclared;
        }
        session.catalog = Some(catalog);
        session.epoch += 1;
        state.publish(session);
        let refused = count(json!({ "model": "openai/gpt-oss-20b" }))
            .await
            .unwrap();
        assert_eq!(refused.status().as_u16(), 400);
        let body: Value = refused.json().await.unwrap();
        assert_eq!(body["error"]["type"], json!("surface_undeclared"));
    }

    /// A revocation that lands after the final checks but before the send
    /// must deliver nothing to the sidecar.
    #[tokio::test]
    async fn revocation_after_the_final_check_delivers_nothing() {
        let revocations: [fn(&ProxyState); 3] = [
            |state| state.set_api_key(None),
            |state| state.set_tokens(state.tokens().without("opencode")),
            |state| {
                let mut session = state.session();
                session.verified = false;
                session.catalog = None;
                state.publish(session);
            },
        ];
        for revoke in revocations {
            let (state, _events) = state();
            state.set_tokens(tokens());
            let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = delivered.clone();
            let sidecar = spawn(
                Router::new()
                    .route(
                        "/v1/chat/completions",
                        post(move || {
                            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            async { Json(json!({})) }
                        }),
                    )
                    .route(
                        "/v1/models",
                        get(|| async {
                            Json(json!({
                                "data": [{ "id": "openai/gpt-oss-20b" }],
                                "aci_capabilities": { "version": 1, "surfaces": {
                                    "chat_completions": "all", "messages": "all", "responses": "undeclared"
                                }}
                            }))
                        }),
                    ),
            )
            .await;
            verified(&state, &sidecar, 1, 1).await;
            state.set_api_key(Some("sk-real".to_string()));
            let reached = Arc::new(tokio::sync::Notify::new());
            let resume = Arc::new(tokio::sync::Notify::new());
            *state.pause.lock().unwrap() = Some((reached.clone(), resume.clone()));
            let proxy = spawn(router(state.clone())).await;
            let request = tokio::spawn(async move {
                reqwest::Client::new()
                    .post(format!("{proxy}/v1/chat/completions"))
                    .bearer_auth("opencode-token")
                    .json(&json!({ "model": "openai/gpt-oss-20b", "messages": [] }))
                    .send()
                    .await
                    .unwrap()
            });
            reached.notified().await;
            revoke(&state);
            resume.notify_one();
            let response = request.await.unwrap();
            assert_eq!(response.status().as_u16(), 503);
            assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }
}
