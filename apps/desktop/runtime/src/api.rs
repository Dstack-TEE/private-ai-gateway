//! The management API (`desktop_core::protocol`), one router served on two
//! listeners as Docker Engine serves one API on `unix://` and `tcp://`: the
//! private local endpoint, whose peers were authenticated by their OS user
//! when accepted (Tailscale's LocalAPI over `safesocket`), and the web UI's
//! TCP listener, whose browsers need a session. One middleware authorizes
//! each request by its listener; browsers may call only the renderer methods
//! (`ui_api::Method`), as LocalAPI grants each handler by the peer's access.

use std::{
    convert::Infallible,
    sync::{Arc, OnceLock},
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{sse::Event as SseEvent, IntoResponse, Response, Sse},
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::{json, Value};
use tokio::{
    runtime::Handle,
    sync::{broadcast, watch},
};
use tokio_util::sync::CancellationToken;

use crate::controller::DesktopRuntime;
use desktop_core::{
    client::CallError,
    contracts::AppState,
    protocol::{self, Command, Version, API_VERSION, API_VERSION_HEADER, BUILD_VERSION},
    ui_api::{self, Backend, Event, Host, Method, StateEventProjection},
};

/// How often an event stream of a browser checks that its session lives.
#[cfg_attr(not(feature = "web-ui"), allow(dead_code))]
const SESSION_CHECK: Duration = Duration::from_secs(15);

/// This process as `GET /api/version` reports it.
pub(crate) fn version() -> &'static Version {
    static VERSION: OnceLock<Version> = OnceLock::new();
    VERSION.get_or_init(|| {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        Version {
            api_version: API_VERSION,
            product: desktop_core::brand::APP_IDENTIFIER.into(),
            version: BUILD_VERSION.into(),
            instance_id: format!("{}-{started}", std::process::id()),
            process_id: std::process::id(),
            executable: std::env::current_exe()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    })
}

/// Runs commands through the service's admission and dispatch.
#[derive(Clone)]
pub(crate) struct ServiceBackend(pub(crate) Arc<DesktopRuntime>);

impl Backend for ServiceBackend {
    async fn execute(&self, command: Command) -> Result<Value, CallError> {
        let runtime = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let admission = runtime.admission();
            crate::server::execute(&runtime, &admission, &Handle::current(), command)
        })
        .await
        .map_err(|_| CallError::Api(protocol::Error::internal()))?
        .map_err(CallError::Api)
    }
}

/// Events renderer methods emit reach this listener's event streams.
#[derive(Clone)]
pub(crate) struct ServiceHost {
    pub(crate) events: broadcast::Sender<Event>,
}

impl Default for ServiceHost {
    fn default() -> Self {
        Self {
            events: broadcast::channel(64).0,
        }
    }
}

impl Host for ServiceHost {
    fn emit(&self, event: Event) -> Result<(), String> {
        let _ = self.events.send(event);
        Ok(())
    }
}

/// Which listener a router serves, and so how it authorizes requests.
#[derive(Clone)]
pub(crate) enum Listener {
    /// The local endpoint: peers were authenticated when accepted.
    Local,
    /// The web UI: browsers with a session, on an allowed host and origin.
    #[cfg(feature = "web-ui")]
    Web(Arc<crate::web_ui::Gate>),
}

/// Who sent a request, as the listener's authorization established.
#[derive(Clone)]
pub(crate) enum Caller {
    /// The same OS user over the local endpoint: may run every command.
    Owner,
    /// A signed-in browser: renderer methods only.
    #[cfg_attr(not(feature = "web-ui"), allow(dead_code))]
    Browser { session: String },
}

#[derive(Clone)]
pub(crate) struct Api<B> {
    pub(crate) backend: B,
    pub(crate) host: ServiceHost,
    pub(crate) states: watch::Receiver<AppState>,
    /// Ends this listener's event streams.
    pub(crate) shutdown: CancellationToken,
    pub(crate) listener: Listener,
}

pub(crate) fn router<B: Backend>(api: Api<B>) -> Router {
    let routes = Router::new()
        .route(protocol::VERSION_PATH, get(version_handler))
        .route(protocol::EVENTS_PATH, get(events::<B>))
        .route(&format!("{}{{command}}", protocol::RPC_PATH), post(rpc::<B>));
    #[cfg(feature = "web-ui")]
    let routes = match &api.listener {
        Listener::Web(_) => crate::web_ui::routes(routes),
        Listener::Local => routes,
    };
    routes
        .layer(middleware::from_fn_with_state(api.clone(), authorize::<B>))
        .with_state(api)
}

/// The one authorization middleware: the listener decides who is calling.
async fn authorize<B: Backend>(
    State(api): State<Api<B>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let mut response = match &api.listener {
        Listener::Local => {
            request.extensions_mut().insert(Caller::Owner);
            next.run(request).await
        }
        #[cfg(feature = "web-ui")]
        Listener::Web(gate) => gate.authorize(request, next).await,
    };
    response
        .headers_mut()
        .insert(API_VERSION_HEADER, HeaderValue::from(API_VERSION));
    response
}

async fn version_handler() -> Json<&'static Version> {
    Json(version())
}

async fn rpc<B: Backend>(
    State(api): State<Api<B>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    let Some(params) = parameters(&body) else {
        return error(protocol::Error::invalid_request());
    };
    let result = match Method::from_name(&name) {
        Some(method) if method.is_command() => match Command::decode(&name, params) {
            Ok(command) => api.backend.execute(command).await,
            Err(invalid) => Err(CallError::Api(invalid)),
        },
        Some(method) => ui_api::invoke(&api.backend, &api.host, method, params).await,
        None if matches!(caller, Caller::Owner) => match Command::decode(&name, params) {
            Ok(command) => api.backend.execute(command).await,
            Err(invalid) => Err(CallError::Api(invalid)),
        },
        None => Err(CallError::Api(protocol::Error::method_not_found())),
    };
    match result {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(failure) => error(failure.into_api()),
    }
}

/// A command's parameters: a JSON object, or nothing for none.
pub(crate) fn parameters(body: &[u8]) -> Option<Value> {
    if body.is_empty() {
        return Some(json!({}));
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
}

/// `{"error": …}` with the status of its code.
pub(crate) fn error(error: protocol::Error) -> Response {
    let status =
        StatusCode::from_u16(error.code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(json!({ "error": error }))).into_response()
}

/// Each connection starts with a full snapshot, so a client that falls behind
/// or reconnects resynchronizes without replaying missed events.
async fn events<B: Backend>(
    State(api): State<Api<B>>,
    Extension(caller): Extension<Caller>,
) -> Sse<impl futures_core::Stream<Item = Result<SseEvent, Infallible>>> {
    let mut states = api.states.clone();
    let initial = states.borrow_and_update().clone();
    let mut projection = StateEventProjection::new(&initial);
    let mut receiver = api.host.events.subscribe();
    let mut snapshot = vec![
        Event::new(
            ui_api::STATE_EVENT,
            serde_json::to_value(&initial).unwrap_or(Value::Null),
        ),
        Event::new(
            ui_api::CLIENT_KEY_CHANGED_EVENT,
            json!(initial.client_key_available.unwrap_or(true)),
        ),
    ];
    if let Ok((_, events)) = ui_api::preference_events(&api.backend, &api.host).await {
        snapshot.extend(events);
    }
    let session_live = move || match (&api.listener, &caller) {
        #[cfg(feature = "web-ui")]
        (Listener::Web(gate), Caller::Browser { session }) => gate.session_live(session),
        _ => true,
    };
    let (backend, host, shutdown) = (api.backend.clone(), api.host.clone(), api.shutdown.clone());
    let stream = async_stream::stream! {
        for event in snapshot {
            yield Ok(sse(&event));
        }
        let mut session = tokio::time::interval(SESSION_CHECK);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                changed = states.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let current = states.borrow_and_update().clone();
                    if projection.settings_changed(&current) {
                        if let Ok((_, events)) = ui_api::preference_events(&backend, &host).await {
                            for event in events {
                                yield Ok(sse(&event));
                            }
                        }
                    }
                    for event in projection.project(&current) {
                        yield Ok(sse(&event));
                    }
                }
                received = receiver.recv() => match received {
                    Ok(event) => yield Ok(sse(&event)),
                    // End the stream; the client reconnects and receives a fresh snapshot.
                    Err(_) => break,
                },
                // An open page counts as activity; revoked or expired sessions end here.
                _ = session.tick() => if !session_live() {
                    break;
                },
            }
        }
    };
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

fn sse(event: &Event) -> SseEvent {
    SseEvent::default()
        .json_data(event)
        .unwrap_or_else(|_| SseEvent::default().data("{}"))
}
