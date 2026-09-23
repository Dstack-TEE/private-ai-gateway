use std::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Method as HttpMethod, Request, StatusCode},
    middleware::{self, Next},
    response::{sse::Event as SseEvent, IntoResponse, Response, Sse},
    routing::{get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    runtime::Handle,
    sync::{broadcast, watch},
};
use tokio_util::sync::CancellationToken;

use super::Auth;
use crate::{
    contracts::GatewayState,
    controller::DesktopRuntime,
    protocol::{Command, RpcError, BUILD_VERSION},
    ui_api::{self, Backend, Event, Host, Method, StateEventProjection},
};

/// Built by `npm run build:web`; a missing bundle only disables the web UI.
#[derive(RustEmbed)]
#[folder = "web-dist/"]
#[allow_missing = true]
struct WebAssets;

const EXPIRED_LINK: &str =
    "This sign-in link has expired or was already used. Run `pap app open --web` for a new link.";
const SESSION_CHECK: Duration = Duration::from_secs(15);

/// Runs management commands through the same admission and dispatch as the IPC endpoint.
#[derive(Clone)]
struct ServiceBackend(Arc<DesktopRuntime>);

impl Backend for ServiceBackend {
    async fn execute(&self, command: Command) -> Result<Value, String> {
        let runtime = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let admission = runtime.admission();
            crate::server::execute(&runtime, &admission, &Handle::current(), command)
        })
        .await
        .map_err(|_| "Management request task failed".to_string())?
        // Match the IPC client's rendering so both transports report identical errors.
        .map_err(|error| format!("{}: {}", error.code, error.message))
    }
}

#[derive(Clone)]
struct WebHost {
    events: broadcast::Sender<Event>,
}

impl Host for WebHost {
    fn emit(&self, event: Event) -> Result<(), String> {
        let _ = self.events.send(event);
        Ok(())
    }
}

#[derive(Clone)]
struct WebState<B> {
    backend: B,
    host: WebHost,
    auth: Arc<Auth>,
    states: watch::Receiver<GatewayState>,
    port: u16,
    shutdown: CancellationToken,
}

pub(super) fn start(
    runtime: Arc<DesktopRuntime>,
    port: u16,
    reopening: bool,
    auth: Arc<Auth>,
    shutdown: CancellationToken,
    handle: &Handle,
) -> Result<String, String> {
    if WebAssets::get("index.html").is_none() {
        return Err("Web UI assets are not built. Run `npm run build:web` in apps/desktop and rebuild the service.".into());
    }
    let listener = bind(port, reopening)?;
    let state = WebState {
        backend: ServiceBackend(runtime.clone()),
        host: WebHost {
            events: broadcast::channel(64).0,
        },
        auth,
        states: runtime.subscribe(),
        port,
        shutdown: shutdown.clone(),
    };
    handle.spawn(async move {
        let result = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => axum::serve(listener, router(state))
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .map_err(|_| ()),
            Err(_) => Err(()),
        };
        if result.is_err() {
            crate::diagnostic(format_args!("The web UI listener on port {port} stopped"));
        }
    });
    Ok(format!("http://127.0.0.1:{port}"))
}

fn bind(port: u16, reopening: bool) -> Result<std::net::TcpListener, String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    // A listener closed by the same toggle may take a moment to release its port.
    let mut retries = if reopening { 20 } else { 0 };
    loop {
        match std::net::TcpListener::bind(address) {
            Ok(listener) => {
                listener
                    .set_nonblocking(true)
                    .map_err(|_| format!("Cannot configure the web UI listener on port {port}"))?;
                return Ok(listener);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && retries > 0 => {
                retries -= 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(match error.kind() {
                    std::io::ErrorKind::AddrInUse => {
                        format!("Port {port} is already in use on 127.0.0.1")
                    }
                    std::io::ErrorKind::PermissionDenied => {
                        format!("Port {port} requires elevated privileges; choose another port")
                    }
                    _ => format!("Cannot listen on 127.0.0.1:{port}"),
                })
            }
        }
    }
}

fn router<B: Backend>(state: WebState<B>) -> Router {
    Router::new()
        .route("/api/session", post(session::<B>))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/events", get(events::<B>))
        .route("/api/rpc/{method}", post(rpc::<B>))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(state.clone(), security::<B>))
        .with_state(state)
}

async fn security<B: Backend>(
    State(state): State<WebState<B>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let headers = request.headers();
    if !valid_host(headers, state.port) {
        return secure_response(status(StatusCode::FORBIDDEN, "Invalid request host"));
    }
    let path = request.uri().path();
    if path.starts_with("/api/") {
        if !valid_origin(headers, state.port) {
            return secure_response(status(StatusCode::FORBIDDEN, "Invalid request origin"));
        }
        // Only the code exchange is reachable without a session.
        if path != "/api/session"
            && !bearer(headers).is_some_and(|token| state.auth.authorize(token))
        {
            return secure_response(status(StatusCode::UNAUTHORIZED, EXPIRED_LINK));
        }
        if request.method() == HttpMethod::POST
            && headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_none_or(|value| !value.eq_ignore_ascii_case("application/json"))
        {
            return secure_response(status(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "JSON body required",
            ));
        }
    }
    secure_response(next.run(request).await)
}

fn secure_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; connect-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// Only the IPv4 loopback name the server binds; `localhost` may resolve elsewhere.
fn valid_host(headers: &HeaderMap, port: u16) -> bool {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("127.0.0.1:{port}"))
}

fn valid_origin(headers: &HeaderMap, port: u16) -> bool {
    let origin = format!("http://127.0.0.1:{port}");
    if let Some(actual) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        return actual == origin;
    }
    headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|referer| referer == format!("{origin}/"))
        || headers
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "same-origin")
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    code: String,
}

async fn session<B: Backend>(
    State(state): State<WebState<B>>,
    Json(request): Json<SessionRequest>,
) -> Response {
    match state.auth.exchange(&request.code) {
        Some(token) => Json(json!({ "token": token })).into_response(),
        None => status(StatusCode::UNAUTHORIZED, EXPIRED_LINK),
    }
}

async fn bootstrap() -> Json<Value> {
    Json(json!({
        "version": BUILD_VERSION,
        "distribution": {
            "channel": "web",
            "nativeUpdates": false,
            "cliRegistration": false,
            "accountPortalLinks": true,
            "sandboxHomeAccess": false,
            "launchAtLogin": false,
            "notifications": false,
            "webUi": true
        }
    }))
}

/// Each connection starts with a full snapshot, so a client that falls behind
/// or reconnects resynchronizes without replaying missed events.
async fn events<B: Backend>(
    State(state): State<WebState<B>>,
    headers: HeaderMap,
) -> Sse<impl futures_core::Stream<Item = Result<SseEvent, Infallible>>> {
    let token = bearer(&headers).unwrap_or_default().to_string();
    let mut states = state.states.clone();
    let initial = states.borrow_and_update().clone();
    let mut projection = StateEventProjection::new(&initial);
    let mut receiver = state.host.events.subscribe();
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
    if let Ok((_, events)) = ui_api::preference_events(&state.backend, &state.host).await {
        snapshot.extend(events);
    }
    let shutdown = state.shutdown.clone();
    let auth = state.auth.clone();
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
                _ = session.tick() => if !auth.authorize(&token) {
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

async fn rpc<B: Backend>(
    State(state): State<WebState<B>>,
    Path(method): Path<String>,
    Json(params): Json<Value>,
) -> Response {
    let Some(method) = Method::from_name(&method) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": RpcError::new("method_not_found", "Unknown management method") }),
            ),
        )
            .into_response();
    };
    match ui_api::invoke(&state.backend, &state.host, method, params).await {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": error.rpc() })),
        )
            .into_response(),
    }
}

async fn asset(request: Request<Body>) -> Response {
    if request.method() != HttpMethod::GET && request.method() != HttpMethod::HEAD {
        return status(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    let path = request.uri().path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return status(StatusCode::NOT_FOUND, "Unknown API endpoint");
    }
    // Unknown client routes render the single-page app.
    let path = if WebAssets::get(path).is_some() {
        path
    } else {
        "index.html"
    };
    let Some(asset) = WebAssets::get(path) else {
        return status(StatusCode::NOT_FOUND, "Web UI not found");
    };
    let content_type = match path.rsplit('.').next() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json") => "application/json",
        _ => "text/html; charset=utf-8",
    };
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        asset.data,
    )
        .into_response()
}

fn status(code: StatusCode, message: &'static str) -> Response {
    (
        code,
        Json(json!({ "error": { "code": code.as_u16(), "message": message } })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    const HOST: &str = "127.0.0.1:3210";
    const ORIGIN: &str = "http://127.0.0.1:3210";

    #[derive(Clone)]
    struct FakeBackend;

    impl Backend for FakeBackend {
        async fn execute(&self, command: Command) -> Result<Value, String> {
            match command {
                Command::Stop => Ok(serde_json::to_value(GatewayState::default()).unwrap()),
                _ => Err("invalid_state: Stop protection before changing this".into()),
            }
        }
    }

    struct Fixture {
        router: Router,
        auth: Arc<Auth>,
        shutdown: CancellationToken,
        _states: watch::Sender<GatewayState>,
    }

    fn fixture() -> Fixture {
        let auth = Arc::new(Auth::default());
        let shutdown = CancellationToken::new();
        let (states, receiver) = watch::channel(GatewayState::default());
        let router = router(WebState {
            backend: FakeBackend,
            host: WebHost {
                events: broadcast::channel(4).0,
            },
            auth: auth.clone(),
            states: receiver,
            port: 3210,
            shutdown: shutdown.clone(),
        });
        Fixture {
            router,
            auth,
            shutdown,
            _states: states,
        }
    }

    fn request(method: HttpMethod, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, ORIGIN)
    }

    async fn send(router: &Router, request: Request<Body>) -> Response {
        router.clone().oneshot(request).await.unwrap()
    }

    async fn exchange(router: &Router, code: &str) -> Response {
        send(
            router,
            request(HttpMethod::POST, "/api/session")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "code": code }).to_string()))
                .unwrap(),
        )
        .await
    }

    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    async fn stop(router: &Router, token: &str) -> Response {
        send(
            router,
            request(HttpMethod::POST, "/api/rpc/stop")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
    }

    #[tokio::test]
    async fn login_code_opens_one_session_and_sessions_are_required() {
        let fixture = fixture();
        let code = fixture.auth.mint_code();
        let response = exchange(&fixture.router, &code).await;
        assert_eq!(response.status(), StatusCode::OK);
        let token = json_body(response).await["token"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            exchange(&fixture.router, &code).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(stop(&fixture.router, &token).await.status(), StatusCode::OK);
        assert_eq!(
            stop(&fixture.router, "wrong").await.status(),
            StatusCode::UNAUTHORIZED
        );
        let missing = send(
            &fixture.router,
            request(HttpMethod::GET, "/api/bootstrap")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        fixture.auth.revoke_all();
        assert_eq!(
            stop(&fixture.router, &token).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn rejects_foreign_hosts_origins_and_non_json_mutations() {
        let fixture = fixture();
        let token = fixture.auth.exchange(&fixture.auth.mint_code()).unwrap();
        let bearer = format!("Bearer {token}");
        for (host, origin, expected) in [
            ("localhost:3210", ORIGIN, StatusCode::FORBIDDEN),
            ("attacker.example:3210", ORIGIN, StatusCode::FORBIDDEN),
            (HOST, "http://localhost:3210", StatusCode::FORBIDDEN),
            (HOST, "https://attacker.example", StatusCode::FORBIDDEN),
            (HOST, ORIGIN, StatusCode::OK),
        ] {
            let response = send(
                &fixture.router,
                Request::builder()
                    .uri("/api/bootstrap")
                    .header(header::HOST, host)
                    .header(header::ORIGIN, origin)
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(response.status(), expected, "{host} {origin}");
            assert_eq!(
                response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
                Some(&HeaderValue::from_static("nosniff"))
            );
        }
        let get_mutation = send(
            &fixture.router,
            request(HttpMethod::GET, "/api/rpc/stop")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(get_mutation.status(), StatusCode::METHOD_NOT_ALLOWED);
        let form = send(
            &fixture.router,
            request(HttpMethod::POST, "/api/session")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(form.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let unknown = send(
            &fixture.router,
            request(HttpMethod::GET, "/api/unknown")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn operation_errors_match_the_desktop_transport() {
        let fixture = fixture();
        let token = fixture.auth.exchange(&fixture.auth.mint_code()).unwrap();
        let response = send(
            &fixture.router,
            request(HttpMethod::POST, "/api/rpc/activateProfile")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "profileId": "missing" }).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(response).await["error"]["message"],
            "invalid_state: Stop protection before changing this"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn event_streams_end_on_shutdown_and_revocation() {
        for revoke in [false, true] {
            let fixture = fixture();
            let token = fixture.auth.exchange(&fixture.auth.mint_code()).unwrap();
            let response = send(
                &fixture.router,
                request(HttpMethod::GET, "/api/events")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            if revoke {
                fixture.auth.revoke_all();
            } else {
                fixture.shutdown.cancel();
            }
            let body = tokio::time::timeout(
                SESSION_CHECK * 2,
                axum::body::to_bytes(response.into_body(), usize::MAX),
            )
            .await
            .expect("event stream should end")
            .unwrap();
            assert!(body.starts_with(b"data: "));
        }
    }
}
