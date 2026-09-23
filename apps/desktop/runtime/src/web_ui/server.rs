use std::{
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
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

use super::{auth::THROTTLE_REFILL, Auth, Throttle};
use crate::{
    contracts::GatewayState,
    controller::DesktopRuntime,
    listen::{url_host, ResolvedListen},
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
const THROTTLED: &str = "Too many sign-in attempts. Wait a few seconds and try again.";
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
    throttle: Arc<Throttle>,
    states: watch::Receiver<GatewayState>,
    /// Accepted `Host` values; see [`allowed_hosts`].
    hosts: Arc<[String]>,
    shutdown: CancellationToken,
}

pub(super) fn start(
    runtime: Arc<DesktopRuntime>,
    listen: &ResolvedListen,
    reopening: bool,
    auth: Arc<Auth>,
    throttle: Arc<Throttle>,
    shutdown: CancellationToken,
    handle: &Handle,
) -> Result<(), String> {
    if WebAssets::get("index.html").is_none() {
        return Err("Web UI assets are not built. Run `npm run build:web` in apps/desktop and rebuild the service.".into());
    }
    let address = listen.bind;
    let listener = bind(address, reopening)?;
    let state = WebState {
        backend: ServiceBackend(runtime.clone()),
        host: WebHost {
            events: broadcast::channel(64).0,
        },
        auth,
        throttle,
        states: runtime.subscribe(),
        hosts: allowed_hosts(listen).into(),
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
            crate::diagnostic(format_args!("The web UI listener on {address} stopped"));
        }
    });
    Ok(())
}

/// `Host` values the listener answers to: the bound address, the client host,
/// and loopback when bound to every interface. Anything else, including
/// `localhost`, may be a DNS-rebinding page and is refused.
fn allowed_hosts(listen: &ResolvedListen) -> Vec<String> {
    let port = listen.bind.port();
    let bound = match listen.bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let mut hosts = Vec::new();
    for host in std::iter::once(bound.to_string()).chain(listen.config.client_host.clone()) {
        let host = url_host(&host);
        hosts.push(format!("{host}:{port}"));
        // Browsers omit the default port.
        if port == 80 {
            hosts.push(host);
        }
    }
    hosts.dedup();
    hosts
}

fn bind(address: SocketAddr, reopening: bool) -> Result<std::net::TcpListener, String> {
    let (ip, port) = (address.ip(), address.port());
    // A listener closed by the same change may take a moment to release its port.
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
                        format!("Port {port} is already in use on {ip}")
                    }
                    std::io::ErrorKind::PermissionDenied => {
                        format!("Port {port} requires elevated privileges; choose another port")
                    }
                    std::io::ErrorKind::AddrNotAvailable => {
                        format!("Address {ip} is not assigned to this device")
                    }
                    _ => format!("Cannot listen on {address}"),
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
    let Some(host) = valid_host(headers, &state.hosts) else {
        return secure_response(status(StatusCode::FORBIDDEN, "Invalid request host"));
    };
    let path = request.uri().path();
    if path.starts_with("/api/") {
        if !valid_origin(headers, host, request.method()) {
            return secure_response(status(StatusCode::FORBIDDEN, "Invalid request origin"));
        }
        // Only the code exchange is reachable without a session, and both it and
        // rejected requests draw from the throttle.
        if path == "/api/session" {
            if !state.throttle.allow() {
                return secure_response(throttled());
            }
        } else if !bearer(headers).is_some_and(|token| state.auth.authorize(token)) {
            return secure_response(if state.throttle.allow() {
                status(StatusCode::UNAUTHORIZED, EXPIRED_LINK)
            } else {
                throttled()
            });
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

/// Returns the request's `Host` when it is on the allowlist.
fn valid_host<'a>(headers: &'a HeaderMap, allowed: &[String]) -> Option<&'a str> {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            allowed
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(value))
        })
}

/// API requests must come from the page served on the same allowed host.
///
/// Browsers always send `Origin` on `POST`. Same-origin `GET`s may carry no
/// origin signal at all: `Sec-Fetch-Site` is only sent to secure contexts, which
/// plain HTTP on a network address is not, and the page sets `no-referrer`.
/// Such reads are accepted; they still need a session token that cross-site
/// pages cannot attach, because no CORS preflight is ever granted.
fn valid_origin(headers: &HeaderMap, host: &str, method: &HttpMethod) -> bool {
    let origin = format!("http://{host}");
    let header = |name| {
        headers
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
    };
    if let Some(actual) = header(header::ORIGIN) {
        return actual.eq_ignore_ascii_case(&origin);
    }
    if let Some(site) = header(header::HeaderName::from_static("sec-fetch-site")) {
        return site == "same-origin";
    }
    if let Some(referer) = header(header::REFERER) {
        return referer.eq_ignore_ascii_case(&format!("{origin}/"));
    }
    method == HttpMethod::GET || method == HttpMethod::HEAD
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

fn throttled() -> Response {
    let mut response = status(StatusCode::TOO_MANY_REQUESTS, THROTTLED);
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(THROTTLE_REFILL.as_secs()),
    );
    response
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
        fixture_on("127.0.0.1", None)
    }

    fn fixture_on(address: &str, client_host: Option<&str>) -> Fixture {
        let listen = crate::listen::resolve(crate::contracts::ListenConfig {
            listen_address: address.into(),
            allow_network_access: true,
            port: 3210,
            client_host: client_host.map(Into::into),
        })
        .unwrap();
        let auth = Arc::new(Auth::default());
        let shutdown = CancellationToken::new();
        let (states, receiver) = watch::channel(GatewayState::default());
        let router = router(WebState {
            backend: FakeBackend,
            host: WebHost {
                events: broadcast::channel(4).0,
            },
            auth: auth.clone(),
            throttle: Arc::default(),
            states: receiver,
            hosts: allowed_hosts(&listen).into(),
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

    async fn bootstrap_from(router: &Router, host: &str, origin: &str, token: &str) -> StatusCode {
        send(
            router,
            Request::builder()
                .uri("/api/bootstrap")
                .header(header::HOST, host)
                .header(header::ORIGIN, origin)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .status()
    }

    async fn assert_hosts(
        address: &str,
        client_host: Option<&str>,
        allowed: &[&str],
        refused: &[&str],
    ) {
        let fixture = fixture_on(address, client_host);
        let token = fixture.auth.exchange(&fixture.auth.mint_code()).unwrap();
        for host in allowed {
            let origin = format!("http://{host}");
            assert_eq!(
                bootstrap_from(&fixture.router, host, &origin, &token).await,
                StatusCode::OK,
                "{address} should accept {host}"
            );
            assert_eq!(
                bootstrap_from(&fixture.router, host, "http://attacker.example", &token).await,
                StatusCode::FORBIDDEN
            );
        }
        for host in refused {
            assert_eq!(
                bootstrap_from(&fixture.router, host, &format!("http://{host}"), &token).await,
                StatusCode::FORBIDDEN,
                "{address} should refuse {host}"
            );
        }
    }

    #[tokio::test]
    async fn network_listeners_accept_only_their_own_hosts_and_origins() {
        assert_hosts(
            "0.0.0.0",
            Some("gateway.lan"),
            &["127.0.0.1:3210", "gateway.lan:3210", "Gateway.LAN:3210"],
            &[
                "0.0.0.0:3210",
                "localhost:3210",
                "192.168.1.20:3210",
                "gateway.lan:3211",
                "gateway.lan",
                "attacker.example:3210",
            ],
        )
        .await;
        // A loopback listener behind a TCP forwarder such as `tailscale serve --tcp`.
        assert_hosts(
            "127.0.0.1",
            Some("studio.tail1234.ts.net"),
            &["127.0.0.1:3210", "studio.tail1234.ts.net:3210"],
            &["localhost:3210", "studio.tail1234.ts.net"],
        )
        .await;
        assert_hosts(
            "192.168.1.20",
            None,
            &["192.168.1.20:3210"],
            &["127.0.0.1:3210", "localhost:3210", "192.168.1.21:3210"],
        )
        .await;
        assert_hosts(
            "::",
            Some("fd00::20"),
            &["[::1]:3210", "[fd00::20]:3210"],
            &["[::]:3210", "127.0.0.1:3210", "fd00::20:3210"],
        )
        .await;
        // The origin must be the page on the requested host, not another allowed one.
        let fixture = fixture_on("0.0.0.0", Some("gateway.lan"));
        let token = fixture.auth.exchange(&fixture.auth.mint_code()).unwrap();
        assert_eq!(
            bootstrap_from(
                &fixture.router,
                "gateway.lan:3210",
                "http://127.0.0.1:3210",
                &token
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn requests_without_origin_signals_may_only_read() {
        let fixture = fixture_on("192.168.1.20", None);
        let token = fixture.auth.exchange(&fixture.auth.mint_code()).unwrap();
        let send_with = |method: HttpMethod, uri: &str, headers: &[(&str, &str)]| {
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "192.168.1.20:3210")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json");
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            send(&fixture.router, request.body(Body::from("{}")).unwrap())
        };
        assert_eq!(
            send_with(HttpMethod::GET, "/api/bootstrap", &[])
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_with(HttpMethod::POST, "/api/rpc/stop", &[])
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        for (name, value) in [
            ("sec-fetch-site", "cross-site"),
            ("referer", "http://attacker.example/"),
        ] {
            assert_eq!(
                send_with(HttpMethod::GET, "/api/bootstrap", &[(name, value)])
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
    }

    #[tokio::test]
    async fn unauthenticated_requests_are_throttled_but_sessions_are_not() {
        let fixture = fixture_on("192.168.1.20", None);
        let token = fixture.auth.exchange(&fixture.auth.mint_code()).unwrap();
        let exchange = |code: &'static str| {
            send(
                &fixture.router,
                Request::builder()
                    .method(HttpMethod::POST)
                    .uri("/api/session")
                    .header(header::HOST, "192.168.1.20:3210")
                    .header(header::ORIGIN, "http://192.168.1.20:3210")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "code": code }).to_string()))
                    .unwrap(),
            )
        };
        for _ in 0..super::super::auth::THROTTLE_BURST {
            assert_eq!(exchange("guess").await.status(), StatusCode::UNAUTHORIZED);
        }
        let throttled = exchange("guess").await;
        assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            throttled.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from(THROTTLE_REFILL.as_secs()))
        );
        assert_eq!(
            bootstrap_from(
                &fixture.router,
                "192.168.1.20:3210",
                "http://192.168.1.20:3210",
                "wrong"
            )
            .await,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            bootstrap_from(
                &fixture.router,
                "192.168.1.20:3210",
                "http://192.168.1.20:3210",
                &token
            )
            .await,
            StatusCode::OK
        );
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
