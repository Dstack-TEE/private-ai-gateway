use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{sse::Event, IntoResponse, Response, Sse},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use rust_embed::RustEmbed;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{
    agent_access,
    client::Client,
    contracts::{
        AccountBalanceTarget, ConfidentialProfileInput, ConnectOptions, LocalApiConfig,
        StartGatewayConfig,
    },
    maintenance::ProfileBackup,
    preferences::{Appearance, NotificationPreferences},
    protocol::{Preference, RpcError, BUILD_VERSION},
    usage::UsageQuery,
};

#[derive(RustEmbed)]
#[folder = "web-dist/"]
struct WebAssets;

#[derive(Clone)]
struct AppState {
    client: Arc<Client>,
    token: Arc<[u8]>,
    port: u16,
    events: broadcast::Sender<WebEvent>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebEvent {
    event: &'static str,
    payload: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListenAddress {
    address: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppearanceParams {
    appearance: Appearance,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LaunchPreferenceParams {
    name: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartParams {
    config: StartGatewayConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProfileIdParams {
    profile_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveConfigurationParams {
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoginIdParams {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteLoginParams {
    id: String,
    callback_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginLoginParams {
    profile: ConfidentialProfileInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveLoginParams {
    id: String,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    workspace_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BalanceParams {
    target: AccountBalanceTarget,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrganizationParams {
    organization_slug: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TopUpParams {
    provider: crate::contracts::ServiceProvider,
    scope_slug: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalApiParams {
    config: LocalApiConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportParams {
    backup: ProfileBackup,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotificationsParams {
    config: NotificationPreferences,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UsageParams {
    query: UsageQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UsageRecordParams {
    record_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentChangeParams {
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
    revision: Option<String>,
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
        token: Arc::from(token.as_bytes()),
        port,
        events,
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

/// Explicit browser-management allowlist. The web surface never forwards an
/// arbitrary protocol method to the local service.
const RPC_METHODS: &[&str] = &[
    "startBackendService",
    "getState",
    "start",
    "stop",
    "activateProfile",
    "deleteProfile",
    "saveConfiguration",
    "completeAccountLogin",
    "beginAccountLogin",
    "pollAccountLogin",
    "saveAccountLogin",
    "getAccountDetails",
    "getAccountBalance",
    "getOrganizationUrl",
    "getTopUpUrl",
    "cancelAccountLogin",
    "getClientKey",
    "rotateClientKey",
    "saveLocalApiConfig",
    "listListenAddresses",
    "importProfiles",
    "exportProfilesContent",
    "exportDiagnosticsContent",
    "queryUsage",
    "getUsageRecord",
    "listAgents",
    "getAgentAccess",
    "requestAgentAccess",
    "previewAgent",
    "applyAgent",
    "getAppearance",
    "setAppearance",
    "getLaunchPreferences",
    "setLaunchPreference",
    "getNotificationSettings",
    "saveNotificationSettings",
    "resetSettings",
];

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
        if request.method() == Method::POST
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
    // Browsers omit Origin on same-origin GET requests. Referer is also
    // browser-controlled and is accepted only for the exact loopback origin.
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
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let mut receiver = state.events.subscribe();
    let initial = WebEvent {
        event: "gateway://state",
        payload: serde_json::to_value(state.client.cached_state()).unwrap_or(Value::Null),
    };
    let stream = async_stream::stream! {
        yield Ok(Event::default().json_data(initial).unwrap_or_else(|_| Event::default().data("{}")));
        loop {
            match receiver.recv().await {
                Ok(event) => yield Ok(Event::default().json_data(event).unwrap_or_else(|_| Event::default().data("{}"))),
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
    match dispatch(&state, &method, params).await {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response(),
    }
}

async fn dispatch(state: &AppState, method: &str, value: Value) -> Result<Value, RpcError> {
    if !RPC_METHODS.contains(&method) {
        return Err(RpcError::new(
            "method_not_found",
            "Unknown management method",
        ));
    }
    let client = state.client.clone();
    match method {
        "startBackendService" => {
            blocking(move || {
                Client::ensure_service()?;
                client.state()
            })
            .await
        }
        "getState" => blocking(move || client.state_or_cached()).await,
        "start" => {
            let params: StartParams = params(value)?;
            blocking(move || client.start(params.config)).await
        }
        "stop" => blocking(move || client.stop()).await,
        "activateProfile" => {
            let params: ProfileIdParams = params(value)?;
            blocking(move || client.activate_profile(params.profile_id)).await
        }
        "deleteProfile" => {
            let params: ProfileIdParams = params(value)?;
            blocking(move || client.delete_profile(params.profile_id)).await
        }
        "saveConfiguration" => {
            let params: SaveConfigurationParams = params(value)?;
            value_result(
                client
                    .save_configuration(params.profile, params.require_production_os, params.key)
                    .await,
            )
        }
        "completeAccountLogin" => {
            let params: CompleteLoginParams = params(value)?;
            value_result(
                client
                    .complete_account_login(params.id, params.callback_url)
                    .await,
            )
        }
        "beginAccountLogin" => {
            let params: BeginLoginParams = params(value)?;
            value_result(client.begin_account_login(params.profile).await)
        }
        "pollAccountLogin" => {
            let params: LoginIdParams = params(value)?;
            value_result(client.poll_account_login(params.id).await)
        }
        "saveAccountLogin" => {
            let params: SaveLoginParams = params(value)?;
            value_result(
                client
                    .save_account_login(
                        params.id,
                        params.profile,
                        params.require_production_os,
                        params.workspace_id,
                    )
                    .await,
            )
        }
        "getAccountDetails" => {
            let params: ProfileIdParams = params(value)?;
            value_result(client.account_details(params.profile_id).await)
        }
        "getAccountBalance" => {
            let params: BalanceParams = params(value)?;
            value_result(client.account_balance(params.target).await)
        }
        "getOrganizationUrl" => {
            let params: OrganizationParams = params(value)?;
            value_result(crate::account_login::organization_url(Some(
                &params.organization_slug,
            )))
        }
        "getTopUpUrl" => {
            let params: TopUpParams = params(value)?;
            value_result(crate::account_login::top_up_url(
                &params.provider,
                params.scope_slug.as_deref(),
            ))
        }
        "cancelAccountLogin" => {
            let params: LoginIdParams = params(value)?;
            value_result(client.cancel_account_login(params.id).await)
        }
        "getClientKey" => blocking(move || client.client_key()).await,
        "rotateClientKey" => blocking(move || client.rotate_client_key()).await,
        "saveLocalApiConfig" => {
            let params: LocalApiParams = params(value)?;
            value_result(client.save_local_api_config(params.config).await)
        }
        "listListenAddresses" => blocking(list_listen_addresses).await,
        "importProfiles" => {
            let params: ImportParams = params(value)?;
            blocking(move || client.import_profiles(params.backup)).await
        }
        "exportProfilesContent" => blocking(move || client.export_profiles_content()).await,
        "exportDiagnosticsContent" => blocking(move || client.export_diagnostics_content()).await,
        "queryUsage" => {
            let params: UsageParams = params(value)?;
            blocking(move || client.query_usage(params.query)).await
        }
        "getUsageRecord" => {
            let params: UsageRecordParams = params(value)?;
            blocking(move || {
                client
                    .usage_record(&params.record_id)?
                    .ok_or_else(|| "Usage record not found".to_string())
            })
            .await
        }
        "listAgents" => blocking(move || client.list_agents()).await,
        "getAgentAccess" | "requestAgentAccess" => value_result(Ok(agent_access::status())),
        "previewAgent" => {
            let params: AgentChangeParams = params(value)?;
            blocking(move || client.preview_agent(params.agent_id, params.connect, params.options))
                .await
        }
        "applyAgent" => {
            let params: AgentChangeParams = params(value)?;
            let revision = params
                .revision
                .ok_or_else(|| RpcError::new("invalid_request", "Missing agent revision"))?;
            blocking(move || {
                client.apply_agent(params.agent_id, params.connect, revision, params.options)
            })
            .await
        }
        "getAppearance" => blocking(move || Ok(client.preferences()?.appearance)).await,
        "setAppearance" => {
            let params: AppearanceParams = params(value)?;
            let appearance = params.appearance;
            let result = blocking(move || {
                client.set_preference(Preference::Appearance(appearance))?;
                Ok(())
            })
            .await;
            if result.is_ok() {
                emit(state, "gateway://appearance", json!(appearance));
            }
            result
        }
        "getLaunchPreferences" => launch_preferences(client).await,
        "setLaunchPreference" => {
            let params: LaunchPreferenceParams = params(value)?;
            if params.name == "openAtLogin" {
                return Err(RpcError::new(
                    "unsupported",
                    "Open at Login is unavailable in the web UI",
                ));
            }
            if params.name != "connectOnLaunch" {
                return Err(RpcError::new(
                    "invalid_request",
                    "Unknown startup preference",
                ));
            }
            let enabled = params.enabled;
            let result = blocking(move || {
                client.set_preference(Preference::ConnectOnLaunch(enabled))?;
                Ok(json!({"openAtLogin": false, "connectOnLaunch": enabled}))
            })
            .await;
            if let Ok(payload) = &result {
                emit(state, "gateway://launch-preferences", payload.clone());
            }
            result
        }
        "getNotificationSettings" => notification_settings(client).await,
        "saveNotificationSettings" => {
            let params: NotificationsParams = params(value)?;
            blocking(move || {
                client.set_preference(Preference::Notifications(params.config))?;
                Ok(())
            })
            .await
        }
        "resetSettings" => {
            let result = blocking(move || client.reset_settings()).await;
            if result.is_ok() {
                emit(state, "gateway://settings-reset", Value::Null);
            }
            result
        }
        _ => Err(RpcError::new(
            "method_not_found",
            "Unknown management method",
        )),
    }
}

async fn launch_preferences(client: Arc<Client>) -> Result<Value, RpcError> {
    blocking(move || {
        Ok(json!({
            "openAtLogin": false,
            "connectOnLaunch": client.preferences()?.connect_on_launch
        }))
    })
    .await
}

async fn notification_settings(client: Arc<Client>) -> Result<Value, RpcError> {
    blocking(move || {
        Ok(json!({
            "preferences": client.preferences()?.notifications,
            "permission": "unsupported",
            "alertsEnabled": false
        }))
    })
    .await
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value)
        .map_err(|_| RpcError::new("invalid_request", "Invalid management request"))
}

async fn blocking<T, F>(operation: F) -> Result<Value, RpcError>
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| RpcError::new("internal_error", "Management request failed"))?;
    value_result(result)
}

fn value_result<T: Serialize>(result: Result<T, String>) -> Result<Value, RpcError> {
    let value = result.map_err(|error| RpcError::operation(&error))?;
    serde_json::to_value(value)
        .map_err(|_| RpcError::new("internal_error", "Management response failed"))
}

fn emit(state: &AppState, event: &'static str, payload: Value) {
    let _ = state.events.send(WebEvent { event, payload });
}

fn publish_state_events(state: AppState) {
    tokio::spawn(async move {
        let mut states = state.client.subscribe();
        let initial = states.borrow().clone();
        let mut client_key_revision = initial.client_key_revision;
        let mut backend_instance = initial.backend_instance;
        loop {
            if states.changed().await.is_err() {
                break;
            }
            let snapshot = states.borrow_and_update().clone();
            emit(
                &state,
                "gateway://state",
                serde_json::to_value(&snapshot).unwrap_or(Value::Null),
            );
            if snapshot.client_key_revision != client_key_revision
                || snapshot.backend_instance != backend_instance
            {
                client_key_revision = snapshot.client_key_revision;
                backend_instance = snapshot.backend_instance.clone();
                emit(
                    &state,
                    "gateway://client-key-changed",
                    json!(snapshot.client_key_available.unwrap_or(true)),
                );
            }
        }
    });
}

fn list_listen_addresses() -> Result<Vec<ListenAddress>, String> {
    let interfaces = if_addrs::get_if_addrs().map_err(|_| {
        "Could not read network interfaces. Enter an IP address manually.".to_string()
    })?;
    let mut addresses: Vec<_> = interfaces
        .into_iter()
        .filter(|interface| interface.is_oper_up())
        .filter(|interface| {
            !matches!(interface.ip(), std::net::IpAddr::V6(ip) if ip.is_unicast_link_local())
        })
        .map(|interface| ListenAddress {
            address: interface.ip().to_string(),
            name: interface.name,
        })
        .collect();
    addresses.sort_by(|left, right| {
        left.address
            .cmp(&right.address)
            .then(left.name.cmp(&right.name))
    });
    addresses.dedup_by(|left, right| left.address == right.address);
    Ok(addresses)
}

async fn asset(request: Request<Body>) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
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
        router(AppState {
            client: Arc::new(Client::new()),
            token: Arc::from(&b"token"[..]),
            port: 3210,
            events: broadcast::channel(1).0,
        })
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
                .method(Method::GET)
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
        let routes = test_router();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/rpc/stop")
            .header(header::HOST, "127.0.0.1:3210")
            .header(header::ORIGIN, "http://127.0.0.1:3210")
            .header(header::AUTHORIZATION, "Bearer token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            routes.oneshot(request).await.unwrap().status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
