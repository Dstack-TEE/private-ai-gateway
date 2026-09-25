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
    sync::{broadcast, watch, Semaphore},
};
use tokio_util::sync::CancellationToken;
use tower_http::timeout::RequestBodyTimeoutLayer;

use crate::controller::DesktopRuntime;
use desktop_core::{
    client::CallError,
    contracts::AppState,
    protocol::{self, Command, Version, API_VERSION, API_VERSION_HEADER, BUILD_VERSION},
    ui_api::{self, Backend, Event, Host, Method, StateEventProjection},
};

/// How often an event stream of a browser checks that its session lives.
#[cfg_attr(not(feature = "web-ui"), allow(dead_code))]
pub(crate) const SESSION_CHECK: Duration = Duration::from_secs(15);

/// Event streams one listener serves at once; more are refused as busy.
const MAX_EVENT_STREAMS: usize = 32;
/// A client must finish sending a request body within this time.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

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
    Browser {
        session: String,
        peer: std::net::IpAddr,
    },
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
        .route(protocol::EVENTS_PATH, get(events::<B>))
        .route(
            &format!("{}{{command}}", protocol::RPC_PATH),
            post(rpc::<B>),
        );
    // The process identity (PID, executable path) is for local clients only;
    // the web UI reads its version from `/api/bootstrap`.
    let routes = match &api.listener {
        Listener::Local => routes.route(protocol::VERSION_PATH, get(version_handler)),
        #[cfg(feature = "web-ui")]
        Listener::Web(_) => crate::web_ui::routes(routes),
    };
    routes
        .layer(middleware::from_fn_with_state(api.clone(), authorize::<B>))
        .layer(Extension(EventStreams(Arc::new(Semaphore::new(
            MAX_EVENT_STREAMS,
        )))))
        .layer(RequestBodyTimeoutLayer::new(BODY_READ_TIMEOUT))
        .with_state(api)
}

/// The event stream slots of one listener.
#[derive(Clone)]
struct EventStreams(Arc<Semaphore>);

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

/// Resolves the command from the decoded name, so every spelling of a path
/// reaches the same authorization.
async fn rpc<B: Backend>(
    State(api): State<Api<B>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    #[cfg_attr(not(feature = "web-ui"), allow(unused_variables))] headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let Some(params) = parameters(&body) else {
        return error(protocol::Error::invalid_request());
    };
    let method = Method::from_name(&name);
    // A browser proves the current password; the local owner is the root of trust.
    #[cfg(feature = "web-ui")]
    if let (Listener::Web(gate), Caller::Browser { peer, .. }, Some(Method::SetWebUiPassword)) =
        (&api.listener, &caller, method)
    {
        return crate::web_ui::change_password(&api, gate, *peer, &headers, params).await;
    }
    let result = match method {
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
    Extension(EventStreams(streams)): Extension<EventStreams>,
) -> Response {
    // The slot is held until the stream ends.
    let Ok(slot) = streams.try_acquire_owned() else {
        return error(protocol::Error::busy());
    };
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
        (Listener::Web(gate), Caller::Browser { session, .. }) => gate.session_live(session),
        _ => true,
    };
    let (backend, host, shutdown) = (api.backend.clone(), api.host.clone(), api.shutdown.clone());
    let stream = async_stream::stream! {
        let _slot = slot;
        for event in snapshot {
            yield Ok::<_, Infallible>(sse(&event));
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
    Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

fn sse(event: &Event) -> SseEvent {
    SseEvent::default()
        .json_data(event)
        .unwrap_or_else(|_| SseEvent::default().data("{}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// Records the commands it receives and answers like the service would.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Value>>>);

    impl Backend for Recorder {
        async fn execute(&self, command: Command) -> Result<Value, CallError> {
            let (name, params) = command.encode().map_err(CallError::Api)?;
            self.0
                .lock()
                .unwrap()
                .push(json!({ "command": name, "params": params }));
            match name.as_str() {
                "get_state" => Ok(serde_json::to_value(AppState::default()).unwrap()),
                "export_usage" => Err(CallError::Api(protocol::Error::new(
                    protocol::ErrorCode::Busy,
                    "Another export is in progress.",
                ))),
                _ => Ok(Value::Null),
            }
        }
    }

    fn local() -> (Router, Recorder, watch::Sender<AppState>) {
        let recorder = Recorder::default();
        let (states, receiver) = watch::channel(AppState::default());
        let router = router(Api {
            backend: recorder.clone(),
            host: ServiceHost::default(),
            states: receiver,
            shutdown: CancellationToken::new(),
            listener: Listener::Local,
        });
        (router, recorder, states)
    }

    async fn call(router: &Router, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get(API_VERSION_HEADER),
            Some(&HeaderValue::from(API_VERSION))
        );
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn version_names_this_backend() {
        let (router, _, _) = local();
        let (status, body) = call(&router, "GET", protocol::VERSION_PATH, "").await;
        assert_eq!(status, StatusCode::OK);
        let reported: Version = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(reported.api_version, API_VERSION);
        assert_eq!(reported.version, BUILD_VERSION);
        assert_eq!(reported.process_id, std::process::id());
        for field in [
            "apiVersion",
            "product",
            "version",
            "instanceId",
            "processId",
            "executable",
        ] {
            assert!(body.get(field).is_some(), "{field}");
        }
    }

    #[tokio::test]
    async fn commands_travel_by_name_with_their_parameters() {
        let (router, recorder, _) = local();
        let (status, body) = call(&router, "POST", "/api/rpc/get_state", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["status"], AppState::default().status);
        // The owner may run every command, including ones browsers cannot.
        let (status, body) = call(
            &router,
            "POST",
            "/api/rpc/shutdown",
            r#"{"instanceId":"1-2","mode":"updateRestart"}"#,
        )
        .await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "result": null })));
        // The owner, the root of trust, sets the web UI password without the old one.
        let (status, _) = call(
            &router,
            "POST",
            "/api/rpc/set_web_ui%5Fpassword",
            r#"{"password":null}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            recorder.0.lock().unwrap().as_slice(),
            [
                json!({ "command": "get_state", "params": {} }),
                json!({ "command": "shutdown", "params": { "instanceId": "1-2", "mode": "updateRestart" } }),
                json!({ "command": "set_web_ui_password", "params": { "password": null } }),
            ]
        );
    }

    #[tokio::test]
    async fn failures_answer_typed_errors_with_their_status() {
        let (router, recorder, _) = local();
        for (path, body, status, code) in [
            (
                "/api/rpc/notACommand",
                "{}",
                StatusCode::NOT_FOUND,
                "method_not_found",
            ),
            (
                "/api/rpc/activate_profile",
                r#"{"profile_id":"p"}"#,
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                "/api/rpc/get_state",
                "[]",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                "/api/rpc/get_state",
                "not json",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                "/api/rpc/export_usage",
                r#"{"query":{},"path":"/tmp/usage.csv"}"#,
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
            ),
        ] {
            let (answered, answer) = call(&router, "POST", path, body).await;
            assert_eq!(answered, status, "{path} {body}");
            assert_eq!(answer["error"]["code"], code, "{path} {body}");
            assert!(answer["error"]["message"].is_string());
        }
        // Only the valid export reached the backend.
        assert_eq!(recorder.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn events_start_with_the_state_and_follow_it() {
        let (router, _, states) = local();
        let response = router
            .oneshot(
                Request::builder()
                    .uri(protocol::EVENTS_PATH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut received = String::new();
        while !received.contains(ui_api::STATE_EVENT) {
            let chunk = tokio::time::timeout(Duration::from_secs(5), body.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            received.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        let first = received
            .lines()
            .find(|line| line.starts_with("data: "))
            .unwrap();
        let event: Value = serde_json::from_str(&first["data: ".len()..]).unwrap();
        assert_eq!(event["event"], ui_api::STATE_EVENT);
        states.send_modify(|state| state.status = "running".into());
        while !received.contains(r#""status":"running""#) {
            let chunk = tokio::time::timeout(Duration::from_secs(5), body.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            received.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    }

    #[tokio::test]
    async fn event_streams_beyond_the_cap_are_refused_as_busy() {
        let (router, _, _states) = local();
        let open = || {
            router.clone().oneshot(
                Request::builder()
                    .uri(protocol::EVENTS_PATH)
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let mut streams = Vec::new();
        for _ in 0..MAX_EVENT_STREAMS {
            let response = open().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            streams.push(response);
        }
        assert_eq!(
            open().await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        // A closed stream frees its slot.
        streams.pop();
        assert_eq!(open().await.unwrap().status(), StatusCode::OK);
    }
}
