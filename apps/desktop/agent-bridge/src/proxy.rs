//! The loopback application proxy on the stable local HTTP endpoint. It admits
//! only requests that carry an issued agent token for that agent's paths,
//! forwards only while a verified session (identity + catalog, one generation
//! and epoch) is published, and swaps the agent token for the provider key on
//! the way to the in-process verifier. It relays method, path, query, body,
//! status, and stream unchanged.
//! Request bodies are buffered (bounded, with a read timeout) only so the
//! `model` can be checked against the verified catalog; responses stream
//! through. Credentials and the session are re-validated after the body is
//! read, right before anything leaves the process. Request attribution is
//! passed directly to the verifier and never appears on an HTTP hop.

mod routes;
#[cfg(test)]
use routes::models;
pub use routes::router;
mod usage;
use usage::*;

use std::{
    collections::HashSet,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, PoisonError, RwLock, RwLockReadGuard,
    },
    time::Duration,
};

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{RawQuery, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{FutureExt, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::mpsc, sync::Semaphore};
use tokio_util::sync::CancellationToken;

use desktop_core::{
    agents::Agent,
    brand::{PRODUCT_NAME, SERVICE_NAME},
    now_secs,
    sse::DataLines,
};

use crate::{
    agents::AgentIntegration,
    catalog::{Catalog, Surface},
    tokens::{agent_allows, TokenSet},
};

/// Request bodies are buffered up to this size; the verifier applies the same
/// limit. Responses stream.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IN_FLIGHT: usize = 64;
/// Connections the Local API holds open at once; later ones wait to be accepted.
const MAX_CONNECTIONS: usize = 128;
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(not(test))]
const VERIFIER_CALL_TIMEOUT: Duration = Duration::from_secs(660);
#[cfg(test)]
const VERIFIER_CALL_TIMEOUT: Duration = Duration::from_millis(100);
// Match the gateway's SSE limit: Responses terminal events repeat the full output.
const MAX_USAGE_CAPTURE_BYTES: usize = desktop_core::sse::MAX_LINE_BYTES;
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

/// The verified in-process session the proxy may forward to. Identity, catalog,
/// generation (per verifier start) and epoch (per identity/catalog read) are
/// published together; anything from another generation or epoch is stale.
#[derive(Clone, Default)]
pub struct Session {
    pub generation: u64,
    pub epoch: u64,
    pub session_id: Option<String>,
    pub service: Option<Arc<dyn VerifiedService>>,
    pub verified: bool,
    pub catalog: Option<Catalog>,
}

/// Stable attribution carried directly into the verifier and its asynchronous
/// receipt audit. This replaces the private `x-aci-tag` wire protocol.
#[derive(Clone, Debug)]
pub struct ForwardContext {
    pub generation: u64,
    pub request_id: String,
    pub session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub at: u64,
}

pub type VerifiedResponse = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

/// An already-attested forwarding service. Implementations must retain the
/// verifier's TLS pinning, policy and receipt-audit behavior.
pub trait VerifiedService: Send + Sync {
    fn call(
        self: Arc<Self>,
        request: Request<Body>,
        context: Option<ForwardContext>,
    ) -> VerifiedResponse;
}

/// One stage of a request observed by the local proxy. Forwarded requests can
/// emit an initial response event and a final usage event before the verifier's
/// receipt verdict is merged through the same event stream.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEvent {
    pub generation: u64,
    pub request_id: String,
    pub session_id: String,
    pub agent: Option<String>,
    pub method: String,
    pub path: String,
    pub model: Option<String>,
    pub status: u16,
    pub streamed: bool,
    pub receipt_id: Option<String>,
    /// The receipt document the verifier checked, exactly as the service
    /// returned it.
    pub receipt: Option<String>,
    pub verified: Option<bool>,
    pub detail: String,
    pub at: u64,
    pub local_policy_applied: Option<bool>,
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
    events: mpsc::Sender<ProxyEvent>,
    in_flight: Arc<Semaphore>,
    #[cfg(test)]
    pause: std::sync::Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}

impl ProxyState {
    /// `events` is bounded; when it is full, low-value rejection events are
    /// dropped rather than blocking a request.
    pub fn new(events: mpsc::Sender<ProxyEvent>) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            session: RwLock::new(Session::default()),
            credentials: RwLock::new(Credentials::default()),
            credential_epoch: AtomicU64::new(1),
            gate: RwLock::new(CancellationToken::new()),
            events,
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            #[cfg(test)]
            pause: std::sync::Mutex::new(None),
        }))
    }

    /// Cancel every delivery admitted so far; called after any state change
    /// that invalidates earlier admissions.
    fn revoke_deliveries(&self) {
        let previous = std::mem::replace(&mut *write(&self.gate), CancellationToken::new());
        previous.cancel();
    }

    // State is guarded by std locks: writers are the desktop shell's event
    // handlers (synchronous), readers never hold a guard across an await.
    /// Catalog-only updates affect subsequent admissions, not existing requests.
    /// Identity, credentials and explicit stop retain their revocation barriers.
    pub fn publish(&self, session: Session) {
        let mut current = write(&self.session);
        let revoke = current.generation != session.generation
            || current.epoch != session.epoch
            || current.session_id != session.session_id
            || current.service.is_some() != session.service.is_some()
            || current.verified != session.verified
            || current.catalog.is_some() != session.catalog.is_some();
        *current = session;
        if revoke {
            self.revoke_deliveries();
        }
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
        {
            let mut credentials = write(&self.credentials);
            // Periodic reconciliation republishes the same set. It must not
            // revoke valid requests unless a credential actually changed.
            if credentials.tokens == tokens {
                return;
            }
            credentials.tokens = tokens;
        }
        self.credential_epoch.fetch_add(1, Ordering::SeqCst);
        self.revoke_deliveries();
    }

    pub fn tokens(&self) -> TokenSet {
        read(&self.credentials).tokens.clone()
    }

    /// Read the model list through the verifier of `generation`; the caller
    /// publishes it under `epoch`. A result for another generation or a newer
    /// epoch is refused here so it can never be published stale.
    pub async fn fetch_catalog(&self, generation: u64, epoch: u64) -> Result<Catalog, String> {
        let service = {
            let session = read(&self.session);
            if session.generation != generation || session.epoch != epoch {
                return Err(
                    "The gateway's identity changed while reading the model list".to_string(),
                );
            }
            session
                .service
                .clone()
                .ok_or_else(|| "Protection is not running".to_string())?
        };
        let mut request = Request::builder().method("GET").uri("/v1/models");
        if let Some(key) = read(&self.credentials).api_key.as_deref() {
            request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
        }
        let request = request
            .body(Body::empty())
            .map_err(|_| "Cannot build the model list request".to_string())?;
        let bytes = tokio::time::timeout(Duration::from_secs(30), async {
            let response = service.clone().call(request, None).await;
            if !response.status().is_success() {
                return Err(format!(
                    "The model list request failed with HTTP {}",
                    response.status().as_u16()
                ));
            }
            to_bytes(response.into_body(), MAX_BODY_BYTES)
                .await
                .map_err(|_| "The model list could not be read".to_string())
        })
        .await
        .map_err(|_| "The verified gateway did not answer the model list request".to_string())??;
        let body: Value = serde_json::from_slice(&bytes)
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
        match (&session.service, session.verified, &session.catalog) {
            (Some(service), true, Some(_)) => Ok(Lease {
                generation: session.generation,
                epoch: session.epoch,
                session_id: session
                    .session_id
                    .clone()
                    .unwrap_or_else(|| "unscoped".to_string()),
                service: service.clone(),
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

    pub fn event_sender(&self) -> mpsc::Sender<ProxyEvent> {
        self.events.clone()
    }
}

/// Permission to forward one request through a specific verified session.
struct Lease {
    generation: u64,
    epoch: u64,
    session_id: String,
    service: Arc<dyn VerifiedService>,
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

/// Bind the local endpoint synchronously and exclusively. A busy port surfaces
/// here, before the app can start or connect anything.
pub fn bind_std(addr: SocketAddr) -> Result<std::net::TcpListener, String> {
    std::net::TcpListener::bind(addr).map_err(|error| format!("Cannot listen on {addr}: {error}"))
}

pub async fn serve(state: Arc<ProxyState>, listener: std::net::TcpListener) -> Result<(), String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Cannot configure the local listener: {error}"))?;
    let listener = TcpListener::from_std(listener)
        .map_err(|error| format!("Cannot use the local listener: {error}"))?;
    desktop_core::serve::serve(
        listener,
        router(state),
        MAX_CONNECTIONS,
        std::future::pending(),
    )
    .await
    .await;
    Ok(())
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
            "Too many requests are in flight through the Local API; retry shortly",
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
                "The request body exceeds the Local API limit",
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
    if let Err(rejection) = check_catalog(&state, model.as_deref(), surface) {
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
        lease.service,
        lease.generation,
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

fn check_catalog(
    state: &ProxyState,
    model: Option<&str>,
    surface: Surface,
) -> Result<(), Rejection> {
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
    if !entry.supports(surface) {
        return Err(Rejection::new(
            StatusCode::BAD_REQUEST,
            "model_endpoint_unavailable",
            format!(
                "`{model}` has no confirmed {} support; choose a compatible model",
                surface.path()
            ),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn forward(
    state: Arc<ProxyState>,
    service: Arc<dyn VerifiedService>,
    generation: u64,
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
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    };
    let mut request = Request::builder().method("POST").uri(target);
    for (name, value) in headers {
        let name = name.as_str();
        if !dropped.contains(name)
            && !matches!(
                name,
                "host" | "content-length" | "authorization" | "x-api-key" | "x-aci-tag"
            )
        {
            request = request.header(name, value);
        }
    }
    let request = match request
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(body))
    {
        Ok(request) => request,
        Err(_) => {
            let rejection = Rejection::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "The request path or headers are invalid",
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
    };
    let context = ForwardContext {
        generation,
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        agent: agent.to_string(),
        model: model.clone(),
        at: now_secs(),
    };
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
    // Once the verifier call is polled, bytes may have reached the remote service even
    // if revocation, timeout, or a connection failure prevents a response.
    // Those failures must never be described as a local rejection.
    let call = async move {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.clone().call(request, Some(context))
        })) {
            Ok(future) => std::panic::AssertUnwindSafe(future).catch_unwind().await,
            Err(panic) => Err(panic),
        }
    };
    let sent = tokio::select! {
        biased;
        _ = delivery.cancelled() => None,
        result = tokio::time::timeout(VERIFIER_CALL_TIMEOUT, call) => Some(result),
    };
    let Some(sent) = sent else {
        let rejection = Rejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "revoked",
            "Credentials or the verified session were revoked while the request was being sent; \
             its delivery could not be confirmed",
        );
        return reject_after_send(
            &state, generation, request_id, session_id, agent, path, model, surface, rejection,
        );
    };
    let upstream = match sent {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => {
            let rejection = Rejection::new(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "The verified upstream handler failed before returning a response",
            );
            return reject_after_send(
                &state, generation, request_id, session_id, agent, path, model, surface, rejection,
            );
        }
        Err(_) => {
            let rejection = Rejection::new(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                "The verified upstream did not return a response in time",
            );
            return reject_after_send(
                &state, generation, request_id, session_id, agent, path, model, surface, rejection,
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
        generation,
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        agent: Some(agent.to_string()),
        method: "POST".to_string(),
        path: path.to_string(),
        model: model.clone(),
        status,
        streamed,
        receipt_id: receipt_id.clone(),
        receipt: None,
        verified: None,
        detail: String::new(),
        at: now_secs(),
        local_policy_applied: None,
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
    let mut stream = upstream.into_body().into_data_stream();
    let event_state = state.clone();
    let event_request_id = request_id;
    let event_session_id = session_id.to_string();
    let event_agent = agent.to_string();
    let event_path = path.to_string();
    let event_model = model;
    let event_receipt_id = receipt_id;
    let body = async_stream::stream! {
        let mut report = UsageReport {
            capture: Some(UsageCapture::new(streamed)),
            state: event_state,
            event: ProxyEvent {
                generation,
                request_id: event_request_id,
                session_id: event_session_id,
                agent: Some(event_agent),
                method: "POST".to_string(),
                path: event_path,
                model: event_model,
                status,
                streamed,
                receipt_id: event_receipt_id,
                receipt: None,
                verified: None,
                detail: String::new(),
                at: now_secs(),
                local_policy_applied: None,
                rewritten: None,
                left_device: true,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost_usd: None,
            },
        };
        loop {
            let chunk = tokio::select! {
                biased;
                _ = delivery.cancelled() => {
                    report.event.status = 502;
                    report.event.detail = "Response stream interrupted because protection or credentials were revoked".into();
                    yield Err(std::io::Error::other("Response delivery revoked"));
                    break;
                }
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break; };
            match chunk {
                Ok(bytes) => {
                    if let Some(capture) = &mut report.capture { capture.push(&bytes); }
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
                Err(error) => {
                    report.event.status = 502;
                    report.event.detail = "Upstream response stream interrupted".into();
                    yield Err(std::io::Error::other(error));
                    break;
                }
            }
        }
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
    let session = state.session();
    let generation = session.generation;
    let session_id = session.session_id.unwrap_or_else(|| "unscoped".to_string());
    reject_with_context(
        state,
        generation,
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
    generation: u64,
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
        generation,
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
    generation: u64,
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
        generation,
        request_id,
        session_id,
        agent,
        method: method.to_string(),
        path: path.to_string(),
        model,
        status: rejection.status.as_u16(),
        streamed: false,
        receipt_id: None,
        receipt: None,
        verified: None,
        detail: rejection.message.clone(),
        at: now_secs(),
        local_policy_applied: None,
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
    hex::encode(bytes)
}

#[cfg(test)]
mod tests;
