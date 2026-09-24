use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, Method as HttpMethod, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use tokio::{runtime::Handle, sync::Semaphore};
use tokio_util::sync::CancellationToken;

use super::{auth::SESSION_LIFETIME, throttle::THROTTLE_REFILL, Auth, Throttle};
use crate::{
    api::{self, Api, Caller, ServiceBackend, ServiceHost},
    controller::DesktopRuntime,
};
use desktop_core::{
    contracts::{DistributionCapabilities, DistributionChannel, WebBootstrap},
    listen::{self, url_host, ResolvedListen},
    protocol::{self, ErrorCode, BUILD_VERSION},
    ui_api::{self, Backend, Method},
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
/// At most this many password checks run at once, across all clients. Each
/// Argon2 verification holds 19 MiB, and the per-client throttle alone admits
/// a burst from every address.
const CONCURRENT_VERIFICATIONS: usize = 2;
static VERIFICATIONS: Semaphore = Semaphore::const_new(CONCURRENT_VERIFICATIONS);

/// How the web UI listener admits browsers.
pub(crate) struct Gate {
    auth: Arc<Auth>,
    throttle: Arc<Throttle>,
    /// Accepted `Host` values; see [`allowed_hosts`].
    hosts: Vec<String>,
    /// Session cookie name; see [`cookie_name`].
    cookie: String,
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
    let router = api::router(Api {
        backend: ServiceBackend(runtime.clone()),
        host: ServiceHost::default(),
        states: runtime.subscribe(),
        shutdown: shutdown.clone(),
        listener: api::Listener::Web(Arc::new(Gate {
            auth,
            throttle,
            hosts: allowed_hosts(listen),
            cookie: cookie_name(listen.bind.port()),
        })),
    });
    handle.spawn(async move {
        let result = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .map_err(|_| ()),
            Err(_) => Err(()),
        };
        if result.is_err() {
            tracing::warn!("The web UI listener on {address} stopped");
        }
    });
    Ok(())
}

/// `Host` values the listener answers to: the bound address, the client host,
/// and loopback when bound to every interface. A listener that loopback
/// reaches also answers to `localhost`, as Syncthing's GUI does: browsers
/// resolve that name only to loopback (RFC 6761 section 6.3), so no page can
/// rebind it. Any other name may be a DNS-rebinding page and is refused.
fn allowed_hosts(listen: &ResolvedListen) -> Vec<String> {
    let port = listen.bind.port();
    let bound = match listen.bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let localhost = bound.is_loopback().then(|| "localhost".to_string());
    let mut hosts = Vec::new();
    for host in std::iter::once(bound.to_string())
        .chain(localhost)
        .chain(listen.config.client_host.clone())
    {
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

/// The web UI's own routes around the shared API: sign-in, the bootstrap, a
/// password change that proves the current password, and the page.
pub(crate) fn routes<B: Backend>(routes: Router<Api<B>>) -> Router<Api<B>> {
    routes
        .route("/api/session", post(session).delete(sign_out))
        .route("/api/bootstrap", get(bootstrap))
        .route(
            &format!("{}{}", protocol::RPC_PATH, Method::SetWebUiPassword.name()),
            post(change_password::<B>),
        )
        .fallback(asset)
}

impl Gate {
    /// Admits a browser request: an allowed host, the page's origin, a live
    /// session (only sign-in goes without), JSON mutations; every answer gets
    /// the security headers.
    pub(crate) async fn authorize(
        self: &Arc<Self>,
        mut request: Request<Body>,
        next: Next,
    ) -> Response {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());
        let headers = request.headers();
        let Some(host) = valid_host(headers, &self.hosts) else {
            return secure_response(status(ErrorCode::Forbidden, "Invalid request host"));
        };
        let path = request.uri().path();
        let mut session = String::new();
        if path.starts_with("/api/") {
            if !valid_origin(headers, host, request.method()) {
                return secure_response(status(ErrorCode::Forbidden, "Invalid request origin"));
            }
            // Only sign-in is reachable without a session, and both it and rejected
            // requests draw from the client's throttle budget.
            if path == "/api/session" && request.method() == HttpMethod::POST {
                if !self.throttle.allow(peer) {
                    return secure_response(throttled());
                }
            } else {
                let jar = CookieJar::from_headers(headers);
                match session_token(&jar, &self.cookie).filter(|token| self.auth.authorize(token)) {
                    Some(token) => session = token.to_string(),
                    None => {
                        let response = if self.throttle.allow(peer) {
                            status(ErrorCode::Unauthorized, SESSION_ENDED)
                        } else {
                            throttled()
                        };
                        // Expire a cookie whose session ended, however it ended.
                        return secure_response(
                            (expire_session(jar, &self.cookie), response).into_response(),
                        );
                    }
                }
            }
            if request.method() == HttpMethod::POST
                && headers
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_none_or(|value| !value.eq_ignore_ascii_case("application/json"))
            {
                return secure_response(status(
                    ErrorCode::UnsupportedMediaType,
                    "JSON body required",
                ));
            }
        }
        request.extensions_mut().insert(Caller::Browser { session });
        request.extensions_mut().insert(self.clone());
        secure_response(next.run(request).await)
    }

    /// Whether a browser's session still lives; an open page counts as activity.
    pub(crate) fn session_live(&self, session: &str) -> bool {
        self.auth.authorize(session)
    }
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

fn session_token<'a>(jar: &'a CookieJar, name: &str) -> Option<&'a str> {
    jar.get(name).map(Cookie::value)
}

/// The session cookie. It cannot be `Secure`: the listener speaks plain HTTP,
/// even on loopback.
fn session_cookie(name: &str, token: &str) -> Cookie<'static> {
    Cookie::build((name.to_string(), token.to_string()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(time::Duration::try_from(SESSION_LIFETIME).unwrap_or(time::Duration::MAX))
        .build()
}

/// Sets a fresh session cookie, or expires the request's one when `token` is `None`.
fn set_session(jar: CookieJar, name: &str, token: Option<&str>) -> CookieJar {
    match token {
        Some(token) => jar.add(session_cookie(name, token)),
        None => expire_session(jar, name),
    }
}

/// Expires the session cookie the request carried; without one it sets nothing.
fn expire_session(jar: CookieJar, name: &str) -> CookieJar {
    jar.remove(session_cookie(name, ""))
}

/// Checks a password on the blocking pool once a verification slot is free.
/// The slot stays taken until the check ends, even if the client disconnects.
async fn verify<T: Send + 'static>(check: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let permit = VERIFICATIONS.acquire().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        check()
    })
    .await
    .ok()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    password: String,
}

/// Signs in with the password and sets the session cookie.
async fn session(Extension(gate): Extension<Arc<Gate>>, jar: CookieJar, body: Bytes) -> Response {
    let Ok(request) = serde_json::from_slice::<SessionRequest>(&body) else {
        return api::error(protocol::Error::invalid_request());
    };
    let auth = gate.auth.clone();
    let Some(token) = verify(move || auth.sign_in(&request.password))
        .await
        .flatten()
    else {
        return status(ErrorCode::Unauthorized, SIGN_IN_FAILED);
    };
    // A browser signing in again replaces its previous session.
    if let Some(previous) = session_token(&jar, &gate.cookie) {
        gate.auth.revoke(previous);
    }
    (
        jar.add(session_cookie(&gate.cookie, &token)),
        StatusCode::NO_CONTENT,
    )
        .into_response()
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
async fn change_password<B: Backend>(
    State(api): State<Api<B>>,
    Extension(gate): Extension<Arc<Gate>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    body: Bytes,
) -> Response {
    let Some(change) = api::parameters(&body)
        .and_then(|params| serde_json::from_value::<PasswordChange>(params).ok())
    else {
        return api::error(protocol::Error::invalid_request());
    };
    if !gate.throttle.allow(peer.ip()) {
        return throttled();
    }
    if gate.auth.has_password() {
        let auth = gate.auth.clone();
        let current = change.current_password.unwrap_or_default();
        let verified = verify(move || auth.verify_password(&current))
            .await
            .unwrap_or(false);
        if !verified {
            return api::error(protocol::Error::invalid_state(WRONG_CURRENT_PASSWORD));
        }
    }
    let params = json!({ "password": change.password });
    match ui_api::invoke(&api.backend, &api.host, Method::SetWebUiPassword, params).await {
        Ok(result) => {
            let jar = set_session(jar, &gate.cookie, gate.auth.open_session().as_deref());
            (jar, Json(json!({ "result": result }))).into_response()
        }
        Err(error) => api::error(error.into_api()),
    }
}

/// Signs this browser out; other browser sessions stay open.
async fn sign_out(Extension(gate): Extension<Arc<Gate>>, jar: CookieJar) -> Response {
    if let Some(token) = session_token(&jar, &gate.cookie) {
        gate.auth.revoke(token);
    }
    (expire_session(jar, &gate.cookie), StatusCode::NO_CONTENT).into_response()
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

async fn asset(request: Request<Body>) -> Response {
    if request.method() != HttpMethod::GET && request.method() != HttpMethod::HEAD {
        return status(ErrorCode::MethodNotAllowed, "Method not allowed");
    }
    let path = request.uri().path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return status(ErrorCode::NotFound, "Unknown API endpoint");
    }
    // Unknown client routes render the single-page app.
    let path = if WebAssets::get(path).is_some() {
        path
    } else {
        "index.html"
    };
    let Some(asset) = WebAssets::get(path) else {
        return status(ErrorCode::NotFound, "Web UI not found");
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
    let mut response = status(ErrorCode::TooManyRequests, THROTTLED);
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(THROTTLE_REFILL.as_secs()),
    );
    response
}

fn status(code: ErrorCode, message: &'static str) -> Response {
    api::error(protocol::Error::new(code, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::connect_info::MockConnectInfo;
    use desktop_core::{client::CallError, contracts::AppState, protocol::Command};
    use serde_json::Value;
    use tokio::sync::watch;
    use tower::ServiceExt;

    const HOST: &str = "127.0.0.1:3210";
    const ORIGIN: &str = "http://127.0.0.1:3210";

    const PASSWORD: &str = "correct horse battery";

    /// Answers `stop` and applies password changes the way the service does.
    #[derive(Clone)]
    struct FakeBackend(Arc<Auth>);

    impl Backend for FakeBackend {
        async fn execute(&self, command: Command) -> Result<Value, CallError> {
            let invalid = |message: String| CallError::Api(protocol::Error::invalid_state(message));
            match command {
                Command::Stop {} => Ok(serde_json::to_value(AppState::default()).unwrap()),
                Command::SetWebUiPassword { password } => {
                    let hash = password
                        .map(|password| super::super::password::hash(&password))
                        .transpose()
                        .map_err(invalid)?;
                    self.0.set_password(hash);
                    Ok(serde_json::to_value(AppState::default()).unwrap())
                }
                _ => Err(invalid("Stop protection before changing this".into())),
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
        let router = api::router(Api {
            backend: FakeBackend(auth.clone()),
            host: ServiceHost::default(),
            states: receiver,
            shutdown: shutdown.clone(),
            listener: api::Listener::Web(Arc::new(Gate {
                auth: auth.clone(),
                throttle: Arc::default(),
                hosts: allowed_hosts(&listen),
                cookie: cookie_name(3210),
            })),
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
            request(HttpMethod::POST, "/api/rpc/set_web_ui_password")
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

    /// The session cookie a response sets.
    fn session_set_cookie(response: &Response) -> Option<Cookie<'static>> {
        let value = response.headers().get(header::SET_COOKIE)?.to_str().ok()?;
        Cookie::parse(value.to_string())
            .ok()
            .filter(|cookie| cookie.name() == cookie_name(3210))
    }

    /// The session token a response sets, or `""` when it expires the cookie.
    fn set_cookie(response: &Response) -> Option<String> {
        session_set_cookie(response).map(|cookie| cookie.value().to_string())
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
        let expired = session_set_cookie(&ended).unwrap();
        assert_eq!(expired.value(), "");
        assert_eq!(expired.max_age(), Some(time::Duration::ZERO));
        assert_eq!(expired.path(), Some("/"));
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
            (HttpMethod::POST, "/api/rpc/get_state"),
            (HttpMethod::POST, "/api/rpc/set_web_ui_password"),
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
        let set = session_set_cookie(&response).unwrap();
        assert_eq!(set.http_only(), Some(true));
        assert_eq!(set.same_site(), Some(SameSite::Strict));
        assert_eq!(set.path(), Some("/"));
        assert_eq!(set.secure(), None);
        assert_eq!(
            set.max_age().map(|age| age.whole_seconds()),
            Some(SESSION_LIFETIME.as_secs() as i64)
        );
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
            assert_eq!(refused.status(), StatusCode::CONFLICT);
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
        assert_eq!(short.status(), StatusCode::CONFLICT);
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
            ("localhost:3210", "http://localhost:3210", StatusCode::OK),
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
            assert_eq!(
                json_body(response).await["error"]["code"],
                "not_found",
                "{uri}"
            );
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
            &[
                "127.0.0.1:3210",
                "localhost:3210",
                "gateway.lan:3210",
                "Gateway.LAN:3210",
            ],
            &[
                "0.0.0.0:3210",
                "[::1]:3210",
                "localhost",
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
            &[
                "127.0.0.1:3210",
                "localhost:3210",
                "studio.tail1234.ts.net:3210",
            ],
            &["[::1]:3210", "localhost:3211", "studio.tail1234.ts.net"],
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
            &["[::1]:3210", "localhost:3210", "[fd00::20]:3210"],
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
    async fn browsers_get_typed_errors_and_only_renderer_methods() {
        let fixture = fixture();
        let token = fixture.auth.open_session().unwrap();
        let call = |name: &str, body: Value| {
            send(
                &fixture.router,
                request(HttpMethod::POST, &format!("/api/rpc/{name}"))
                    .header(header::COOKIE, cookie(&token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        };
        let response = call("activate_profile", json!({ "profileId": "missing" })).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            json_body(response).await["error"],
            json!({ "code": "invalid_state", "message": "Stop protection before changing this" })
        );
        // Commands only the local endpoint's owner may run are unknown here.
        for name in ["shutdown", "export_profiles", "clear_usage", "notACommand"] {
            let response = call(name, json!({})).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{name}");
            assert_eq!(
                json_body(response).await["error"]["code"],
                "method_not_found"
            );
        }
        let invalid = call("activate_profile", json!({ "profile_id": "p" })).await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(invalid).await["error"]["code"], "invalid_request");
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
                api::SESSION_CHECK * 2,
                axum::body::to_bytes(response.into_body(), usize::MAX),
            )
            .await
            .expect("event stream should end")
            .unwrap();
            assert!(body.starts_with(b"data: "));
        }
    }
}
