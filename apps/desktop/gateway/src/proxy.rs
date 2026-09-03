//! The loopback application proxy on the stable local HTTP endpoint. It admits
//! only requests that carry an issued agent token for that agent's paths,
//! forwards only while a verified session (identity + catalog, one generation
//! and epoch) is published, and swaps the agent token for the RedPill key on
//! the way to the sidecar. It relays: method, path, query, body, status, and
//! stream reach the sidecar and come back unchanged; nothing is converted.
//! Request bodies are buffered (bounded, with a read timeout) only so the
//! `model` can be checked against the verified catalog; responses stream
//! through. Credentials and the session are re-validated after the body is
//! read, right before anything leaves the process. The proxy adds one
//! attribution header the sidecar copies into its receipt event and strips
//! before forwarding.

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
    extract::{RawQuery, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use rand::RngCore;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::mpsc, sync::Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{
    brand::{PRODUCT_NAME, SERVICE_NAME},
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
const MAX_USAGE_CAPTURE_BYTES: usize = 1024 * 1024;
const MAX_SSE_LINE_BYTES: usize = 128 * 1024;
/// Attribution (the agent id) added on the way to the sidecar, which copies
/// it into the receipt event and strips it before forwarding.
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
    pub session_id: Option<String>,
    pub base_url: Option<String>,
    pub verified: bool,
    pub catalog: Option<Catalog>,
}

/// One stage of a request observed by the local proxy. Forwarded requests can
/// emit an initial response event and a final usage event before the sidecar's
/// receipt verdict is merged by the desktop backend.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEvent {
    pub request_id: String,
    pub session_id: String,
    pub agent: Option<String>,
    pub method: String,
    pub path: String,
    pub model: Option<String>,
    pub status: u16,
    pub streamed: bool,
    pub receipt_id: Option<String>,
    pub verified: Option<bool>,
    pub detail: String,
    pub at: u64,
    pub locally_constrained: Option<bool>,
    pub rewritten: Option<bool>,
    pub left_device: bool,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
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
                session_id: session
                    .session_id
                    .clone()
                    .unwrap_or_else(|| "unscoped".to_string()),
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
                format!("This endpoint accepts only agents connected through {PRODUCT_NAME}"),
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
                    format!(
                        "The agent token is not recognized; reconnect the agent in {PRODUCT_NAME}"
                    ),
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
                format!("No {SERVICE_NAME} API key is saved in {PRODUCT_NAME}"),
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
    session_id: String,
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

pub async fn serve(state: Arc<ProxyState>, listener: std::net::TcpListener) -> Result<(), String> {
    let listener = TcpListener::from_std(listener)
        .map_err(|error| format!("Cannot use the local listener: {error}"))?;
    axum::serve(listener, router(state))
        .await
        .map_err(|error| format!("The local gateway stopped: {error}"))
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
        return error_response(
            surface,
            rejection.status,
            rejection.code,
            &rejection.message,
        );
    }
    let session = state.session();
    match (session.verified, session.catalog) {
        (true, Some(catalog)) => Json(catalog.openai_list()).into_response(),
        _ => error_response(
            surface,
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_not_verified",
            "The verified model list is not available until verification succeeds",
        ),
    }
}

async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::ChatCompletions,
        "/v1/chat/completions",
    )
    .await
}

async fn messages(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::Messages,
        "/v1/messages",
    )
    .await
}

async fn responses(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::Responses,
        "/v1/responses",
    )
    .await
}

/// Helper endpoints belong to their surface and are gated exactly like
/// inference: agent scope, verified session, and a model present in the
/// catalog (both protocols require `model`).
async fn count_tokens(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::Messages,
        "/v1/messages/count_tokens",
    )
    .await
}

async fn responses_compact(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
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
        &format!("{PRODUCT_NAME} serves /v1/models, /v1/chat/completions, /v1/messages, and /v1/responses"),
    )
}

async fn relay(
    state: Arc<ProxyState>,
    headers: HeaderMap,
    query: Option<String>,
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
    if let Err(rejection) = check_catalog(&state, model.as_deref()) {
        return reject(&state, Some(agent), "POST", path, model, surface, rejection);
    }
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
        state,
        &lease.base_url,
        &lease.session_id,
        &key,
        &agent,
        surface,
        path,
        query.as_deref(),
        &headers,
        bytes,
        model,
        delivery,
    )
    .await
}

fn check_catalog(state: &ProxyState, model: Option<&str>) -> Result<(), Rejection> {
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
    catalog.get(model).map(|_| ()).ok_or_else(|| {
        Rejection::new(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("`{model}` is not in the verified model list"),
        )
    })
}

#[allow(clippy::too_many_arguments)]
async fn forward(
    state: Arc<ProxyState>,
    base_url: &str,
    session_id: &str,
    key: &str,
    agent: &str,
    surface: Surface,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
    model: Option<String>,
    delivery: CancellationToken,
) -> Response {
    let request_id = new_id();
    let dropped = hop_by_hop_names(headers);
    let target = match query {
        Some(query) => format!("{base_url}{path}?{query}"),
        None => format!("{base_url}{path}"),
    };
    let mut request = state.client.post(target);
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
        .header(TAG_HEADER, format_tag(&request_id, session_id, agent))
        .body(body);
    if delivery.is_cancelled() {
        let rejection = Rejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "revoked",
            "Credentials or the verified session were revoked before the request was sent; \
             send it again",
        );
        return reject(
            &state,
            Some(agent.to_string()),
            "POST",
            path,
            model,
            surface,
            rejection,
        );
    }
    // Once request.send() is polled, bytes may have reached the sidecar even
    // if revocation, timeout, or a connection failure prevents a response.
    // Those failures must never be described as a local rejection.
    let sent = tokio::select! {
        biased;
        _ = delivery.cancelled() => None,
        result = request.send() => Some(result),
    };
    let Some(sent) = sent else {
        let rejection = Rejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "revoked",
            "Credentials or the verified session were revoked while the request was being sent; \
             its delivery could not be confirmed",
        );
        return reject_after_send(
            &state, request_id, session_id, agent, path, model, surface, rejection,
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
            return reject_after_send(
                &state, request_id, session_id, agent, path, model, surface, rejection,
            );
        }
    };
    let status = upstream.status().as_u16();
    let streamed = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
    let receipt_id = upstream
        .headers()
        .get("x-receipt-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    state.emit(ProxyEvent {
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        agent: Some(agent.to_string()),
        method: "POST".to_string(),
        path: path.to_string(),
        model: model.clone(),
        status,
        streamed,
        receipt_id: receipt_id.clone(),
        verified: None,
        detail: "Awaiting receipt verification".to_string(),
        at: now_secs(),
        locally_constrained: None,
        rewritten: None,
        left_device: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    });
    let dropped = hop_by_hop_names(upstream.headers());
    let mut builder = Response::builder().status(upstream.status().as_u16());
    for (name, value) in upstream.headers() {
        if !dropped.contains(name.as_str()) {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    let mut stream = upstream.bytes_stream();
    let event_state = state.clone();
    let event_request_id = request_id;
    let event_session_id = session_id.to_string();
    let event_agent = agent.to_string();
    let event_path = path.to_string();
    let event_model = model;
    let event_receipt_id = receipt_id;
    let body = async_stream::stream! {
        let mut capture = UsageCapture::new(streamed);
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    capture.push(&bytes);
                    yield Ok::<Bytes, reqwest::Error>(bytes);
                }
                Err(error) => {
                    yield Err(error);
                    break;
                }
            }
        }
        let usage = capture.finish();
        event_state.emit(ProxyEvent {
            request_id: event_request_id,
            session_id: event_session_id,
            agent: Some(event_agent),
            method: "POST".to_string(),
            path: event_path,
            model: event_model,
            status,
            streamed,
            receipt_id: event_receipt_id,
            verified: None,
            detail: String::new(),
            at: now_secs(),
            locally_constrained: None,
            rewritten: None,
            left_device: true,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cost_usd: usage.cost_usd,
        });
    };
    builder.body(Body::from_stream(body)).unwrap_or_else(|_| {
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
    let session_id = state
        .session()
        .session_id
        .unwrap_or_else(|| "unscoped".to_string());
    reject_with_context(
        state,
        new_id(),
        session_id,
        agent,
        method,
        path,
        model,
        surface,
        rejection,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn reject_after_send(
    state: &ProxyState,
    request_id: String,
    session_id: &str,
    agent: &str,
    path: &str,
    model: Option<String>,
    surface: Surface,
    rejection: Rejection,
) -> Response {
    reject_with_context(
        state,
        request_id,
        session_id.to_string(),
        Some(agent.to_string()),
        "POST",
        path,
        model,
        surface,
        rejection,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn reject_with_context(
    state: &ProxyState,
    request_id: String,
    session_id: String,
    agent: Option<String>,
    method: &str,
    path: &str,
    model: Option<String>,
    surface: Surface,
    rejection: Rejection,
    left_device: bool,
) -> Response {
    state.emit(ProxyEvent {
        request_id,
        session_id,
        agent,
        method: method.to_string(),
        path: path.to_string(),
        model,
        status: rejection.status.as_u16(),
        streamed: false,
        receipt_id: None,
        verified: None,
        detail: rejection.message.clone(),
        at: now_secs(),
        locally_constrained: None,
        rewritten: None,
        left_device,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
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
            HeaderValue::from_str(&format!("Bearer realm=\"{PRODUCT_NAME}\""))
                .unwrap_or_else(|_| HeaderValue::from_static("Bearer")),
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

fn new_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn format_tag(request_id: &str, session_id: &str, agent: &str) -> String {
    format!("pag:{request_id}:{session_id}:{agent}")
}

#[derive(Default)]
struct UsageValues {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    cost_usd: Option<f64>,
}

impl UsageValues {
    fn merge(&mut self, newer: Self) {
        if newer.input_tokens.is_some() {
            self.input_tokens = newer.input_tokens;
        }
        if newer.output_tokens.is_some() {
            self.output_tokens = newer.output_tokens;
        }
        if newer.cache_read_tokens.is_some() {
            self.cache_read_tokens = newer.cache_read_tokens;
        }
        if newer.cache_write_tokens.is_some() {
            self.cache_write_tokens = newer.cache_write_tokens;
        }
        if newer.cost_usd.is_some() {
            self.cost_usd = newer.cost_usd;
        }
    }
}

struct UsageCapture {
    streamed: bool,
    body: Vec<u8>,
    line: Vec<u8>,
    latest: UsageValues,
    body_overflow: bool,
}

impl UsageCapture {
    fn new(streamed: bool) -> Self {
        Self {
            streamed,
            body: Vec::new(),
            line: Vec::new(),
            latest: UsageValues::default(),
            body_overflow: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.streamed {
            self.push_sse(bytes);
        } else if !self.body_overflow {
            if self.body.len().saturating_add(bytes.len()) <= MAX_USAGE_CAPTURE_BYTES {
                self.body.extend_from_slice(bytes);
            } else {
                self.body.clear();
                self.body_overflow = true;
            }
        }
    }

    fn push_sse(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if *byte == b'\n' {
                self.parse_sse_line();
                self.line.clear();
            } else if self.line.len() < MAX_SSE_LINE_BYTES {
                self.line.push(*byte);
            }
        }
    }

    fn parse_sse_line(&mut self) {
        let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
        let payload = line
            .strip_prefix(b"data: ")
            .or_else(|| line.strip_prefix(b"data:"))
            .map(trim_ascii_start);
        let Some(payload) = payload else { return };
        if payload == b"[DONE]" {
            return;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(payload) {
            if let Some(usage) = find_usage(&value) {
                self.latest.merge(parse_usage(usage));
            }
        }
    }

    fn finish(mut self) -> UsageValues {
        if self.streamed {
            if !self.line.is_empty() {
                self.parse_sse_line();
            }
            self.latest
        } else if self.body_overflow {
            UsageValues::default()
        } else {
            serde_json::from_slice::<Value>(&self.body)
                .ok()
                .and_then(|value| find_usage(&value).map(parse_usage))
                .unwrap_or_default()
        }
    }
}

fn find_usage(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    if let Some(usage) = value.get("usage").and_then(Value::as_object) {
        return Some(usage);
    }
    value
        .get("response")
        .and_then(|response| response.get("usage"))
        .and_then(Value::as_object)
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
                .and_then(Value::as_object)
        })
}

fn parse_usage(usage: &serde_json::Map<String, Value>) -> UsageValues {
    let cache_read = token(usage, "cache_read_input_tokens")
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(number_u64)
        })
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(number_u64)
        });
    UsageValues {
        input_tokens: token(usage, "prompt_tokens").or_else(|| token(usage, "input_tokens")),
        output_tokens: token(usage, "completion_tokens").or_else(|| token(usage, "output_tokens")),
        cache_read_tokens: cache_read,
        cache_write_tokens: token(usage, "cache_creation_input_tokens"),
        cost_usd: usage.get("cost").and_then(number_f64),
    }
}

fn token(usage: &serde_json::Map<String, Value>, name: &str) -> Option<u64> {
    usage.get(name).and_then(number_u64)
}

fn number_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn number_f64(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn trim_ascii_start(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    value
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

    /// A stand-in sidecar that echoes what it received.
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
            .route("/v1/responses", post(echo))
            .route("/v1/messages/count_tokens", post(echo))
            .route(
                "/v1/models",
                get(|| async {
                    Json(json!({
                        "data": [{ "id": "openai/gpt-oss-20b" }]
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
            session_id: Some("test-session".to_string()),
            base_url: Some(sidecar.to_string()),
            verified: false,
            catalog: None,
        });
        let catalog = state.fetch_catalog(generation, epoch).await.unwrap();
        state.publish(Session {
            generation,
            epoch,
            session_id: Some("test-session".to_string()),
            base_url: Some(sidecar.to_string()),
            verified: true,
            catalog: Some(catalog),
        });
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
        let rebound = (0..20).find_map(|_| match bind_std(addr) {
            Ok(listener) => Some(listener),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
                None
            }
        });
        assert!(rebound.is_some());
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
            session_id: Some("test-session".to_string()),
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
            session_id: Some("test-session".to_string()),
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
    async fn verified_catalog_models_are_forwarded_with_the_real_key() {
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

        let responses = client
            .post(format!("{proxy}/v1/responses"))
            .bearer_auth("codex-token")
            .json(&json!({ "model": "openai/gpt-oss-20b", "input": "hi" }))
            .send()
            .await
            .unwrap();
        assert_eq!(responses.status().as_u16(), 200);

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
        let tag = echo["tag"].as_str().unwrap();
        let parts: Vec<_> = tag.split(':').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "pag");
        assert_eq!(parts[2], "test-session");
        assert_eq!(parts[3], "opencode");
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
        let tag = echo["tag"].as_str().unwrap();
        assert!(tag.starts_with("pag:"));
        assert!(tag.ends_with(":test-session:claude-code"));
    }

    #[tokio::test]
    async fn send_failures_are_not_reported_as_local_rejections() {
        let (state, mut events) = state();
        state.set_tokens(tokens());
        let sidecar = mock_sidecar().await;
        verified(&state, &sidecar, 1, 1).await;
        state.set_api_key(Some("sk-real".to_string()));

        let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unavailable_url = format!("http://{}", unavailable.local_addr().unwrap());
        drop(unavailable);
        state.publish(Session {
            base_url: Some(unavailable_url),
            ..state.session()
        });

        let proxy = spawn(router(state)).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy}/v1/responses"))
            .bearer_auth("codex-token")
            .json(&json!({ "model": "openai/gpt-oss-20b", "input": "hello" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 502);
        let event = events.recv().await.unwrap();
        assert!(event.left_device);
        assert_eq!(event.session_id, "test-session");
        assert_eq!(event.agent.as_deref(), Some("codex"));
        assert_eq!(event.model.as_deref(), Some("openai/gpt-oss-20b"));
    }

    /// The proxy relays: for every inference path the sidecar sees the same
    /// method, path, query, and body bytes the agent sent, and the agent gets
    /// the sidecar's status, content type, and streamed bytes back unchanged.
    #[tokio::test]
    async fn every_path_is_relayed_without_rewriting_request_or_response() {
        let (state, _events) = state();
        state.set_tokens(tokens());
        let echo = |method: axum::http::Method,
                    uri: axum::http::Uri,
                    headers: HeaderMap,
                    body: Bytes| async move {
            let tag = headers
                .get(TAG_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = String::from_utf8_lossy(&body);
            (
                StatusCode::ACCEPTED,
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("event: echo\ndata: {method} {uri} {tag}\n\ndata: {body}\n\n"),
            )
        };
        let sidecar = spawn(
            Router::new()
                .route("/v1/chat/completions", post(echo))
                .route("/v1/messages", post(echo))
                .route("/v1/responses", post(echo))
                .route(
                    "/v1/models",
                    get(|| async { Json(json!({ "data": [{ "id": "openai/gpt-oss-20b" }] })) }),
                ),
        )
        .await;
        verified(&state, &sidecar, 1, 1).await;
        state.set_api_key(Some("sk-real".to_string()));
        let proxy = spawn(router(state.clone())).await;
        let client = reqwest::Client::new();
        // Field order, whitespace, and unknown members are the agent's; the
        // proxy only reads `model`.
        let body =
            r#"{"stream": true, "model":"openai/gpt-oss-20b", "input": [{"x": 1}], "extra": null}"#;
        for (path, token, agent) in [
            ("/v1/chat/completions", "opencode-token", "opencode"),
            ("/v1/messages", "claude-token", "claude-code"),
            ("/v1/responses", "codex-token", "codex"),
        ] {
            let response = client
                .post(format!("{proxy}{path}?beta=true&v=2"))
                .bearer_auth(token)
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 202, "{path}");
            assert_eq!(
                response.headers()["content-type"],
                "text/event-stream",
                "{path}"
            );
            let text = response.text().await.unwrap();
            assert!(
                text.starts_with(&format!(
                    "event: echo\ndata: POST {path}?beta=true&v=2 pag:"
                )),
                "{path}: {text}"
            );
            assert!(
                text.contains(&format!(":test-session:{agent}\n\ndata: {body}\n\n")),
                "{path}: {text}"
            );
        }
    }

    #[test]
    fn usage_capture_parses_streamed_and_json_usage_without_inventing_values() {
        let mut stream = UsageCapture::new(true);
        stream.push(
            b"event: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":12,",
        );
        stream.push(
            b"\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":3},\"cost\":\"0.004\"}}}\n\n",
        );
        let usage = stream.finish();
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.cache_read_tokens, Some(3));
        assert_eq!(usage.cost_usd, Some(0.004));

        let mut json = UsageCapture::new(false);
        json.push(br#"{"usage":{"prompt_tokens":21,"completion_tokens":8,"cache_creation_input_tokens":4,"prompt_tokens_details":{"cached_tokens":7},"cost":-1}}"#);
        let usage = json.finish();
        assert_eq!(usage.input_tokens, Some(21));
        assert_eq!(usage.output_tokens, Some(8));
        assert_eq!(usage.cache_read_tokens, Some(7));
        assert_eq!(usage.cache_write_tokens, Some(4));
        assert_eq!(usage.cost_usd, None);

        let mut messages = UsageCapture::new(true);
        messages.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":144,\"cache_read_input_tokens\":32,\"cache_creation_input_tokens\":8}}}\n\n");
        messages.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":21}}\n\n");
        let usage = messages.finish();
        assert_eq!(usage.input_tokens, Some(144));
        assert_eq!(usage.output_tokens, Some(21));
        assert_eq!(usage.cache_read_tokens, Some(32));
        assert_eq!(usage.cache_write_tokens, Some(8));
    }

    #[test]
    fn oversized_usage_body_stays_unknown() {
        let mut capture = UsageCapture::new(false);
        capture.push(&vec![b'x'; MAX_USAGE_CAPTURE_BYTES + 1]);
        let usage = capture.finish();
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.cost_usd, None);
    }

    #[test]
    fn attribution_tag_contains_request_session_and_agent() {
        assert_eq!(
            format_tag("request-1", "session-2", "pi"),
            "pag:request-1:session-2:pi"
        );
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
    async fn helper_endpoints_require_a_verified_catalog_model() {
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
                                "data": [{ "id": "openai/gpt-oss-20b" }]
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
