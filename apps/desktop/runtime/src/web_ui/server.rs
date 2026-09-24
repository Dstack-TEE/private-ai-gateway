use std::{
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Body,
    extract::{ConnectInfo, Path, State},
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

use super::{auth::SESSION_LIFETIME, throttle::THROTTLE_REFILL, Auth, Throttle};
use crate::controller::DesktopRuntime;
use desktop_core::{
    contracts::{AppState, DistributionCapabilities, DistributionChannel, WebBootstrap},
    listen::{self, url_host, ResolvedListen},
    protocol::{Command, RpcError, BUILD_VERSION},
    ui_api::{self, Backend, Event, Host, Method, StateEventProjection},
};

/// Built by `npm run build:web`; a missing bundle only disables the web UI.
#[derive(RustEmbed)]
#[folder = "web-dist/"]
#[allow_missing = true]
struct WebAssets;

const SESSION_ENDED: &str = "This web UI session has ended or expired. Sign in again.";
/// The same answer whether the password is wrong or none is set.
const SIGN_IN_FAILED: &str = "Sign-in failed. Check the password and try again.";
const WRONG_CURRENT_PASSWORD: &str = "The current password is incorrect.";
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
    states: watch::Receiver<AppState>,
    /// Accepted `Host` values; see [`allowed_hosts`].
    hosts: Arc<[String]>,
    /// Session cookie name; see [`cookie_name`].
    cookie: Arc<str>,
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
        cookie: cookie_name(listen.bind.port()).into(),
        shutdown: shutdown.clone(),
    };
    handle.spawn(async move {
        let result = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => axum::serve(
                listener,
                router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .map_err(|_| ()),
            Err(_) => Err(()),
        };
        if result.is_err() {
            desktop_core::diagnostic!("The web UI listener on {address} stopped");
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

/// Cookies are not scoped to a port, so each listener names its own: a local
/// web UI and an SSH-tunneled one on the same host keep separate sessions.
fn cookie_name(port: u16) -> String {
    format!("pap_session_{port}")
}

fn bind(address: SocketAddr, reopening: bool) -> Result<std::net::TcpListener, String> {
    let (ip, port) = (address.ip(), address.port());
    let listener = listen::bind(address, reopening).map_err(|error| match error.kind() {
        std::io::ErrorKind::AddrInUse => format!("Port {port} is already in use on {ip}"),
        std::io::ErrorKind::PermissionDenied => {
            format!("Port {port} requires elevated privileges; choose another port")
        }
        std::io::ErrorKind::AddrNotAvailable => {
            format!("Address {ip} is not assigned to this device")
        }
        _ => format!("Cannot listen on {address}"),
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|_| format!("Cannot configure the web UI listener on port {port}"))?;
    Ok(listener)
}

fn router<B: Backend>(state: WebState<B>) -> Router {
    Router::new()
        .route("/api/session", post(session::<B>).delete(sign_out::<B>))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/events", get(events::<B>))
        .route("/api/rpc/{method}", post(rpc::<B>))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(state.clone(), security::<B>))
        .with_state(state)
}

async fn security<B: Backend>(
    State(state): State<WebState<B>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
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
        // Only sign-in is reachable without a session, and both it and rejected
        // requests draw from the client's throttle budget.
        if path == "/api/session" && request.method() == HttpMethod::POST {
            if !state.throttle.allow(peer.ip()) {
                return secure_response(throttled());
            }
        } else {
            let token = session_token(headers, &state.cookie);
            if !token.is_some_and(|token| state.auth.authorize(token)) {
                let mut response = if state.throttle.allow(peer.ip()) {
                    status(StatusCode::UNAUTHORIZED, SESSION_ENDED)
                } else {
                    throttled()
                };
                // Expire a cookie whose session ended, however it ended.
                if token.is_some() {
                    set_session_cookie(&mut response, &state.cookie, None);
                }
                return secure_response(response);
            }
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

/// API requests must come from the page served on the same allowed host. This
/// is the CSRF defense that `SameSite=Strict` cannot give alone: another port
/// on the same host is the same site.
///
/// Browsers send `Origin` on every request that is not a `GET` or `HEAD`, so a
/// mutation without the exact origin is refused. Same-origin `GET`s, such as the
/// event stream, may carry no origin signal at all: `Sec-Fetch-Site` is only
/// sent to secure contexts, which plain HTTP on a network address is not, and
/// the page sets `no-referrer`. Such reads are accepted; cross-origin pages
/// cannot read their responses because no CORS access is ever granted.
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
    if method != HttpMethod::GET && method != HttpMethod::HEAD {
        return false;
    }
    if let Some(site) = header(header::HeaderName::from_static("sec-fetch-site")) {
        return site == "same-origin";
    }
    if let Some(referer) = header(header::REFERER) {
        return referer.eq_ignore_ascii_case(&format!("{origin}/"));
    }
    true
}

fn session_token<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

/// Sets the session cookie, or expires it when `token` is `None`. It cannot be
/// `Secure`: the listener speaks plain HTTP, even on loopback.
fn set_session_cookie(response: &mut Response, name: &str, token: Option<&str>) {
    let cookie = match token {
        Some(token) => format!(
            "{name}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
            SESSION_LIFETIME.as_secs()
        ),
        None => format!("{name}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    };
    if let Ok(cookie) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    password: String,
}

/// Signs in with the password and sets the session cookie.
async fn session<B: Backend>(
    State(state): State<WebState<B>>,
    headers: HeaderMap,
    Json(request): Json<SessionRequest>,
) -> Response {
    let auth = state.auth.clone();
    let token = tokio::task::spawn_blocking(move || auth.sign_in(&request.password))
        .await
        .ok()
        .flatten();
    let Some(token) = token else {
        return status(StatusCode::UNAUTHORIZED, SIGN_IN_FAILED);
    };
    // A browser signing in again replaces its previous session.
    if let Some(previous) = session_token(&headers, &state.cookie) {
        state.auth.revoke(previous);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    set_session_cookie(&mut response, &state.cookie, Some(&token));
    response
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PasswordChange {
    password: Option<String>,
    current_password: Option<String>,
}

/// A browser must prove the current password before changing or clearing it,
/// and draws from its client's throttle budget to do so. The change ends every
/// session, so the caller receives a fresh session cookie.
async fn change_password<B: Backend>(state: WebState<B>, peer: IpAddr, params: Value) -> Response {
    let Ok(change) = serde_json::from_value::<PasswordChange>(params) else {
        return rpc_error(ui_api::Error::InvalidRequest.rpc());
    };
    if !state.throttle.allow(peer) {
        return throttled();
    }
    if state.auth.has_password() {
        let auth = state.auth.clone();
        let current = change.current_password.unwrap_or_default();
        let verified = tokio::task::spawn_blocking(move || auth.verify_password(&current))
            .await
            .unwrap_or(false);
        if !verified {
            return rpc_error(RpcError::new("invalid_state", WRONG_CURRENT_PASSWORD));
        }
    }
    let params = json!({ "password": change.password });
    match ui_api::invoke(
        &state.backend,
        &state.host,
        Method::SetWebUiPassword,
        params,
    )
    .await
    {
        Ok(result) => {
            let mut response = Json(json!({ "result": result })).into_response();
            set_session_cookie(
                &mut response,
                &state.cookie,
                state.auth.open_session().as_deref(),
            );
            response
        }
        Err(error) => rpc_error(error.rpc()),
    }
}

/// Signs this browser out; other browser sessions stay open.
async fn sign_out<B: Backend>(State(state): State<WebState<B>>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers, &state.cookie) {
        state.auth.revoke(token);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    set_session_cookie(&mut response, &state.cookie, None);
    response
}

async fn bootstrap() -> Json<WebBootstrap> {
    Json(WebBootstrap {
        version: BUILD_VERSION.to_string(),
        distribution: DistributionCapabilities {
            channel: DistributionChannel::Web,
            native_updates: false,
            cli_registration: false,
            account_portal_links: true,
            sandbox_home_access: false,
            launch_at_login: false,
            notifications: false,
            web_ui: true,
        },
    })
}

/// Each connection starts with a full snapshot, so a client that falls behind
/// or reconnects resynchronizes without replaying missed events.
async fn events<B: Backend>(
    State(state): State<WebState<B>>,
    headers: HeaderMap,
) -> Sse<impl futures_core::Stream<Item = Result<SseEvent, Infallible>>> {
    let token = session_token(&headers, &state.cookie)
        .unwrap_or_default()
        .to_string();
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
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
    if method == Method::SetWebUiPassword {
        return change_password(state, peer.ip(), params).await;
    }
    match ui_api::invoke(&state.backend, &state.host, method, params).await {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(error) => rpc_error(error.rpc()),
    }
}

fn rpc_error(error: RpcError) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
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
    use axum::extract::connect_info::MockConnectInfo;
    use tower::ServiceExt;

    const HOST: &str = "127.0.0.1:3210";
    const ORIGIN: &str = "http://127.0.0.1:3210";

    const PASSWORD: &str = "correct horse battery";

    /// Answers `stop` and applies password changes the way the service does.
    #[derive(Clone)]
    struct FakeBackend(Arc<Auth>);

    impl Backend for FakeBackend {
        async fn execute(&self, command: Command) -> Result<Value, String> {
            match command {
                Command::Stop => Ok(serde_json::to_value(AppState::default()).unwrap()),
                Command::SetWebUiPassword { password } => {
                    let hash = password
                        .map(|password| super::super::password::hash(&password))
                        .transpose()?;
                    self.0.set_password(hash);
                    Ok(serde_json::to_value(AppState::default()).unwrap())
                }
                _ => Err("invalid_state: Stop protection before changing this".into()),
            }
        }
    }

    struct Fixture {
        router: Router,
        auth: Arc<Auth>,
        shutdown: CancellationToken,
        _states: watch::Sender<AppState>,
    }

    fn fixture() -> Fixture {
        fixture_on("127.0.0.1", None)
    }

    fn fixture_on(address: &str, client_host: Option<&str>) -> Fixture {
        let listen = desktop_core::listen::resolve(desktop_core::contracts::ListenConfig {
            listen_address: address.into(),
            allow_network_access: true,
            port: 3210,
            client_host: client_host.map(Into::into),
        })
        .unwrap();
        let auth = Arc::new(Auth::default());
        let shutdown = CancellationToken::new();
        let (states, receiver) = watch::channel(AppState::default());
        let router = router(WebState {
            backend: FakeBackend(auth.clone()),
            host: WebHost {
                events: broadcast::channel(4).0,
            },
            auth: auth.clone(),
            throttle: Arc::default(),
            states: receiver,
            hosts: allowed_hosts(&listen).into(),
            cookie: cookie_name(3210).into(),
            shutdown: shutdown.clone(),
        })
        .layer(MockConnectInfo(SocketAddr::from((
            [192, 168, 1, 30],
            50000,
        ))));
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

    async fn sign_in(router: &Router, body: Value) -> Response {
        send(
            router,
            request(HttpMethod::POST, "/api/session")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn change_password(router: &Router, token: &str, body: Value) -> Response {
        send(
            router,
            request(HttpMethod::POST, "/api/rpc/setWebUiPassword")
                .header(header::COOKIE, cookie(token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    fn cookie(token: &str) -> String {
        format!("{}={token}", cookie_name(3210))
    }

    /// The session token a response sets, or `""` when it expires the cookie.
    fn set_cookie(response: &Response) -> Option<String> {
        let value = response.headers().get(header::SET_COOKIE)?.to_str().ok()?;
        let (pair, _) = value.split_once(';')?;
        Some(
            pair.strip_prefix(&format!("{}=", cookie_name(3210)))?
                .to_string(),
        )
    }

    fn set_password(auth: &Auth, password: &str) {
        auth.set_password(Some(super::super::password::hash(password).unwrap()));
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
                .header(header::COOKIE, cookie(token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
    }

    #[tokio::test]
    async fn api_requests_need_a_live_session() {
        let fixture = fixture();
        let token = fixture.auth.open_session().unwrap();
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
        // Without a cookie there is nothing to expire.
        assert_eq!(set_cookie(&missing), None);
        fixture.auth.revoke_all();
        let ended = stop(&fixture.router, &token).await;
        assert_eq!(ended.status(), StatusCode::UNAUTHORIZED);
        // An ended session expires its cookie and answers JSON.
        assert_eq!(set_cookie(&ended).as_deref(), Some(""));
        assert_eq!(json_body(ended).await["error"]["message"], SESSION_ENDED);
    }

    #[tokio::test]
    async fn mutations_need_the_exact_origin_even_with_a_valid_cookie() {
        let fixture = fixture();
        let token = fixture.auth.open_session().unwrap();
        let mutate = |origin: Option<&str>, site: Option<&str>| {
            let mut request = Request::builder()
                .method(HttpMethod::POST)
                .uri("/api/rpc/stop")
                .header(header::HOST, HOST)
                .header(header::COOKIE, cookie(&token))
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            if let Some(site) = site {
                request = request.header("sec-fetch-site", site);
            }
            send(&fixture.router, request.body(Body::from("{}")).unwrap())
        };
        for (origin, site) in [
            (Some("http://127.0.0.1:9999"), None),
            (Some("http://attacker.example"), Some("cross-site")),
            (Some("null"), None),
            (None, Some("same-origin")),
            (None, None),
        ] {
            assert_eq!(
                mutate(origin, site).await.status(),
                StatusCode::FORBIDDEN,
                "{origin:?} {site:?}"
            );
        }
        assert_eq!(mutate(Some(ORIGIN), None).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_page_loads_without_a_session_but_every_api_route_needs_one() {
        let fixture = fixture();
        set_password(&fixture.auth, PASSWORD);
        for path in ["/", "/settings", "/assets/missing.js"] {
            let page = send(
                &fixture.router,
                request(HttpMethod::GET, path).body(Body::empty()).unwrap(),
            )
            .await;
            // Development builds without web assets answer 404; never 401.
            assert!(
                matches!(page.status(), StatusCode::OK | StatusCode::NOT_FOUND),
                "{path}: {}",
                page.status()
            );
        }
        for (method, path) in [
            (HttpMethod::GET, "/api/bootstrap"),
            (HttpMethod::GET, "/api/events"),
            (HttpMethod::POST, "/api/rpc/getState"),
            (HttpMethod::POST, "/api/rpc/setWebUiPassword"),
            (HttpMethod::DELETE, "/api/session"),
        ] {
            let response = send(
                &fixture.router,
                request(method.clone(), path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn the_password_opens_a_session_and_failures_look_alike() {
        let fixture = fixture();
        let unset = sign_in(&fixture.router, json!({ "password": PASSWORD })).await;
        assert_eq!(unset.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(unset).await["error"]["message"], SIGN_IN_FAILED);
        set_password(&fixture.auth, PASSWORD);
        let wrong = sign_in(
            &fixture.router,
            json!({ "password": "wrong horse battery" }),
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(wrong).await["error"]["message"], SIGN_IN_FAILED);
        let response = sign_in(&fixture.router, json!({ "password": PASSWORD })).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let header = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(header.ends_with(&format!(
            "; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
            SESSION_LIFETIME.as_secs()
        )));
        let token = set_cookie(&response).unwrap();
        assert_eq!(stop(&fixture.router, &token).await.status(), StatusCode::OK);
        // Signing in again from the same browser replaces its session.
        let again = send(
            &fixture.router,
            request(HttpMethod::POST, "/api/session")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, cookie(&token))
                .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(again.status(), StatusCode::NO_CONTENT);
        assert!(!fixture.auth.authorize(&token));
        assert!(fixture.auth.authorize(&set_cookie(&again).unwrap()));
    }

    #[tokio::test]
    async fn changing_the_password_needs_the_current_one_and_ends_other_sessions() {
        let fixture = fixture();
        set_password(&fixture.auth, PASSWORD);
        let token = fixture.auth.sign_in(PASSWORD).unwrap();
        let other = fixture.auth.sign_in(PASSWORD).unwrap();
        let next = "another long passphrase";
        for current in [None, Some("wrong horse battery")] {
            let refused = change_password(
                &fixture.router,
                &token,
                json!({ "password": next, "currentPassword": current }),
            )
            .await;
            assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                json_body(refused).await["error"]["message"],
                WRONG_CURRENT_PASSWORD
            );
        }
        let short = change_password(
            &fixture.router,
            &token,
            json!({ "password": "short", "currentPassword": PASSWORD }),
        )
        .await;
        assert_eq!(short.status(), StatusCode::BAD_REQUEST);
        assert!(fixture.auth.authorize(&other));

        let changed = change_password(
            &fixture.router,
            &token,
            json!({ "password": next, "currentPassword": PASSWORD }),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let rotated = set_cookie(&changed).unwrap();
        assert!(!fixture.auth.authorize(&token));
        assert!(!fixture.auth.authorize(&other));
        assert_eq!(
            stop(&fixture.router, &rotated).await.status(),
            StatusCode::OK
        );
        assert_eq!(fixture.auth.sign_in(PASSWORD), None);
        assert!(fixture.auth.sign_in(next).is_some());

        // Clearing it ends every session too.
        let cleared = change_password(
            &fixture.router,
            &rotated,
            json!({ "password": null, "currentPassword": next }),
        )
        .await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert!(!fixture.auth.has_password());
        assert!(!fixture.auth.authorize(&rotated));
    }

    #[tokio::test]
    async fn rejects_foreign_hosts_origins_and_non_json_mutations() {
        let fixture = fixture();
        let token = fixture.auth.open_session().unwrap();
        let bearer = cookie(&token);
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
                    .header(header::COOKIE, &bearer)
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
                .header(header::COOKIE, &bearer)
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
                .header(header::COOKIE, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn client_routes_serve_the_app_document_and_api_misses_stay_json() {
        let fixture = fixture();
        let get = |uri: &str| {
            send(
                &fixture.router,
                request(HttpMethod::GET, uri).body(Body::empty()).unwrap(),
            )
        };
        let root = get("/").await;
        let (status, content_type) = (
            root.status(),
            root.headers().get(header::CONTENT_TYPE).cloned(),
        );
        let document = axum::body::to_bytes(root.into_body(), usize::MAX)
            .await
            .unwrap();
        // The page renders before sign-in, so client routes need no session either.
        for uri in [
            "/usage",
            "/usage?agent=codex&rows=50",
            "/settings",
            "/unknown/page",
        ] {
            let response = get(uri).await;
            assert_eq!(response.status(), status, "{uri}");
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE),
                content_type.as_ref(),
                "{uri}"
            );
            for name in [
                header::CONTENT_SECURITY_POLICY,
                header::X_CONTENT_TYPE_OPTIONS,
                header::CACHE_CONTROL,
                header::REFERRER_POLICY,
            ] {
                assert!(response.headers().contains_key(&name), "{uri} {name}");
            }
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(body, document, "{uri}");
        }
        for uri in ["/api", "/api/unknown"] {
            let token = fixture.auth.open_session().unwrap();
            let response = send(
                &fixture.router,
                request(HttpMethod::GET, uri)
                    .header(header::COOKIE, cookie(&token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(json_body(response).await["error"]["code"], 404, "{uri}");
        }
    }

    async fn bootstrap_from(router: &Router, host: &str, origin: &str, token: &str) -> StatusCode {
        send(
            router,
            Request::builder()
                .uri("/api/bootstrap")
                .header(header::HOST, host)
                .header(header::ORIGIN, origin)
                .header(header::COOKIE, cookie(token))
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
        let token = fixture.auth.open_session().unwrap();
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
        let token = fixture.auth.open_session().unwrap();
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
        let token = fixture.auth.open_session().unwrap();
        let send_with = |method: HttpMethod, uri: &str, headers: &[(&str, &str)]| {
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "192.168.1.20:3210")
                .header(header::COOKIE, cookie(&token))
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
    async fn unauthenticated_requests_are_throttled_per_client_but_sessions_are_not() {
        let fixture = fixture_on("192.168.1.20", None);
        let token = fixture.auth.open_session().unwrap();
        let sign_in = |client: Option<[u8; 4]>, password: &str| {
            let mut request = Request::builder()
                .method(HttpMethod::POST)
                .uri("/api/session")
                .header(header::HOST, "192.168.1.20:3210")
                .header(header::ORIGIN, "http://192.168.1.20:3210")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "password": password }).to_string()))
                .unwrap();
            if let Some(client) = client {
                request
                    .extensions_mut()
                    .insert(ConnectInfo(SocketAddr::from((client, 50000))));
            }
            send(&fixture.router, request)
        };
        for _ in 0..super::super::throttle::THROTTLE_BURST.get() {
            assert_eq!(
                sign_in(None, "guess guess guess").await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let throttled = sign_in(None, "guess guess guess").await;
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
        // Even the right password waits, but the throttled client cannot lock
        // another client out of signing in.
        set_password(&fixture.auth, PASSWORD);
        assert_eq!(
            sign_in(None, PASSWORD).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let other = sign_in(Some([192, 168, 1, 31]), PASSWORD).await;
        assert_eq!(other.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn signing_out_ends_only_that_session() {
        let fixture = fixture();
        let token = fixture.auth.open_session().unwrap();
        let other = fixture.auth.open_session().unwrap();
        let sign_out = |token: &str| {
            send(
                &fixture.router,
                request(HttpMethod::DELETE, "/api/session")
                    .header(header::COOKIE, cookie(token))
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let signed_out = sign_out(&token).await;
        assert_eq!(signed_out.status(), StatusCode::NO_CONTENT);
        assert_eq!(set_cookie(&signed_out).as_deref(), Some(""));
        let ended = stop(&fixture.router, &token).await;
        assert_eq!(ended.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(ended).await["error"]["message"], SESSION_ENDED);
        assert_eq!(stop(&fixture.router, &other).await.status(), StatusCode::OK);
        // Signing out needs the session it ends.
        assert_eq!(sign_out(&token).await.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn operation_errors_match_the_desktop_transport() {
        let fixture = fixture();
        let token = fixture.auth.open_session().unwrap();
        let response = send(
            &fixture.router,
            request(HttpMethod::POST, "/api/rpc/activateProfile")
                .header(header::COOKIE, cookie(&token))
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
            let token = fixture.auth.open_session().unwrap();
            let response = send(
                &fixture.router,
                request(HttpMethod::GET, "/api/events")
                    .header(header::COOKIE, cookie(&token))
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
