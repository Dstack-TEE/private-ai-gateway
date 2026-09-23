use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Method as HttpMethod, Request, StatusCode},
    middleware::{self, Next},
    response::{sse::Event as SseEvent, IntoResponse, Response, Sse},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{
    client::Client,
    protocol::{RpcError, BUILD_VERSION},
    ui_api::{self, Event, Host, Method, StateEventProjection},
};

#[derive(RustEmbed)]
#[folder = "web-dist/"]
struct WebAssets;

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
struct AppState {
    client: Arc<Client>,
    host: WebHost,
    token: Arc<[u8]>,
    port: u16,
}

pub(super) fn run(port: u16, no_open: bool) -> Result<(), String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "Cannot initialize the web UI runtime".to_string())?
                    .block_on(serve(port, no_open))
            })
            .join()
            .map_err(|_| "The web UI server stopped unexpectedly".to_string())?
    })
}

async fn serve(port: u16, no_open: bool) -> Result<(), String> {
    if WebAssets::get("index.html").is_none() {
        return Err("The embedded web UI is missing. Run `npm run build:web` in apps/desktop before building the CLI.".into());
    }
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| "Cannot bind the web UI to 127.0.0.1".to_string())?;
    let port = listener
        .local_addr()
        .map_err(|_| "Cannot read the web UI address".to_string())?
        .port();
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let (events, _) = broadcast::channel(64);
    let client = Client::attach(tokio::runtime::Handle::current())?;
    let state = AppState {
        client: client.clone(),
        host: WebHost { events },
        token: Arc::from(token.as_bytes()),
        port,
    };
    publish_state_events(state.clone());
    let app = router(state);
    let url = format!("http://127.0.0.1:{port}/#token={token}");
    println!("Private AI Proxy web UI: {url}");
    eprintln!("Keep this terminal open. Press Ctrl+C to stop the web UI.");
    if !no_open && open_browser(&url).is_err() {
        eprintln!("Cannot open a browser automatically; open the printed URL manually.");
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| "The web UI server failed".to_string())
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/events", get(events))
        .route("/api/rpc/{method}", post(rpc))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(state.clone(), security))
        .with_state(state)
}

async fn security(State(state): State<AppState>, request: Request<Body>, next: Next) -> Response {
    if !valid_host(request.headers(), state.port) {
        return secure_response(status(StatusCode::FORBIDDEN, "Invalid request host"));
    }
    if request.uri().path().starts_with("/api/") {
        if !valid_origin(request.headers(), state.port) {
            return secure_response(status(StatusCode::FORBIDDEN, "Invalid request origin"));
        }
        if !valid_token(request.headers(), &state.token) {
            return secure_response(status(StatusCode::UNAUTHORIZED, "Authentication required"));
        }
        if request.method() == HttpMethod::POST
            && request
                .headers()
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

fn valid_host(headers: &HeaderMap, port: u16) -> bool {
    let expected_ip = format!("127.0.0.1:{port}");
    let expected_name = format!("localhost:{port}");
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected_ip || value.eq_ignore_ascii_case(&expected_name))
}

fn valid_origin(headers: &HeaderMap, port: u16) -> bool {
    let origins = [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ];
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        return origins.iter().any(|expected| origin == expected);
    }
    headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|referer| origins.iter().any(|origin| referer == format!("{origin}/")))
        || headers
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "same-origin")
}

fn valid_token(headers: &HeaderMap, expected: &[u8]) -> bool {
    let Some(actual) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    constant_time_eq(actual.as_bytes(), expected)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(*left.get(index).unwrap_or(&0) ^ *right.get(index).unwrap_or(&0));
    }
    difference == 0
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
            "nativeDialogs": false,
            "launchAtLogin": false,
            "notifications": false
        }
    }))
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<SseEvent, Infallible>>> {
    let mut receiver = state.host.events.subscribe();
    let initial = Event::new(
        ui_api::STATE_EVENT,
        serde_json::to_value(state.client.cached_state()).unwrap_or(Value::Null),
    );
    let stream = async_stream::stream! {
        yield Ok(SseEvent::default().json_data(initial).unwrap_or_else(|_| SseEvent::default().data("{}")));
        loop {
            match receiver.recv().await {
                Ok(event) => yield Ok(SseEvent::default().json_data(event).unwrap_or_else(|_| SseEvent::default().data("{}"))),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn rpc(
    State(state): State<AppState>,
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
    match ui_api::invoke(state.client, state.host, method, params).await {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": error.rpc() })),
        )
            .into_response(),
    }
}

fn publish_state_events(state: AppState) {
    tokio::spawn(async move {
        let mut states = state.client.subscribe();
        let mut projection = StateEventProjection::new(&states.borrow());
        while states.changed().await.is_ok() {
            let snapshot = states.borrow_and_update().clone();
            for event in projection.project(&snapshot) {
                let _ = state.host.emit(event);
            }
        }
    });
}

async fn asset(request: Request<Body>) -> Response {
    if request.method() != HttpMethod::GET && request.method() != HttpMethod::HEAD {
        return status(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    let path = request.uri().path().trim_start_matches('/');
    let asset = WebAssets::get(if path.is_empty() { "index.html" } else { path })
        .or_else(|| WebAssets::get("index.html"));
    let Some(asset) = asset else {
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

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    let mut child = result.map_err(|_| "Cannot open browser".to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    fn headers(host: &str, origin: Option<&str>, token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
        }
        if let Some(token) = token {
            headers.insert(
                header::AUTHORIZATION,
                format!("Bearer {token}").parse().unwrap(),
            );
        }
        headers
    }

    fn test_router() -> Router {
        let (events, _) = broadcast::channel(1);
        router(AppState {
            client: Arc::new(Client::new()),
            host: WebHost { events },
            token: Arc::from(&b"token"[..]),
            port: 3210,
        })
    }

    #[test]
    fn shared_allowlist_is_explicit_and_round_trips() {
        assert!(Method::ALL.len() > 30);
        for method in Method::ALL {
            assert_eq!(Method::from_name(method.name()), Some(*method));
        }
        assert_eq!(Method::from_name("shutdown"), None);
    }

    #[test]
    fn rejects_missing_and_wrong_tokens() {
        let expected = b"correct";
        assert!(!valid_token(
            &headers("127.0.0.1:3210", None, None),
            expected
        ));
        assert!(!valid_token(
            &headers("127.0.0.1:3210", None, Some("wrong")),
            expected
        ));
        assert!(valid_token(
            &headers("127.0.0.1:3210", None, Some("correct")),
            expected
        ));
    }

    #[test]
    fn rejects_dns_rebinding_and_cross_origin_requests() {
        assert!(!valid_host(
            &headers("attacker.example:3210", None, None),
            3210
        ));
        assert!(valid_host(&headers("localhost:3210", None, None), 3210));
        assert!(!valid_origin(
            &headers("127.0.0.1:3210", Some("https://attacker.example"), None),
            3210
        ));
        assert!(valid_origin(
            &headers("127.0.0.1:3210", Some("http://127.0.0.1:3210"), None),
            3210
        ));
    }

    #[tokio::test]
    async fn middleware_rejects_missing_wrong_and_cross_origin_auth() {
        let cases = [
            (
                "127.0.0.1:3210",
                "http://127.0.0.1:3210",
                None,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "127.0.0.1:3210",
                "http://127.0.0.1:3210",
                Some("wrong"),
                StatusCode::UNAUTHORIZED,
            ),
            (
                "attacker.example:3210",
                "http://127.0.0.1:3210",
                Some("token"),
                StatusCode::FORBIDDEN,
            ),
            (
                "127.0.0.1:3210",
                "https://attacker.example",
                Some("token"),
                StatusCode::FORBIDDEN,
            ),
        ];
        for (host, origin, token, expected) in cases {
            let mut request = Request::builder()
                .method(HttpMethod::GET)
                .uri("/api/bootstrap")
                .header(header::HOST, host)
                .header(header::ORIGIN, origin);
            if let Some(token) = token {
                request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            let response = test_router()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(
                response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
                Some(&HeaderValue::from_static("nosniff"))
            );
        }
    }

    #[tokio::test]
    async fn mutations_are_post_only() {
        let request = Request::builder()
            .method(HttpMethod::GET)
            .uri("/api/rpc/stop")
            .header(header::HOST, "127.0.0.1:3210")
            .header(header::ORIGIN, "http://127.0.0.1:3210")
            .header(header::AUTHORIZATION, "Bearer token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            test_router().oneshot(request).await.unwrap().status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
