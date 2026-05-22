//! Axum app exposing the ACI endpoints.
//!
//! Endpoints:
//!
//! * `GET  /v1/attestation/report` - service-scoped report; an
//!   optional `?nonce=...` query parameter is bound into
//!   `report_data` (URL-decoded UTF-8 string, or JSON `null` when
//!   absent).
//! * `POST /v1/chat/completions` - OpenAI-shaped chat-completion
//!   forwarding with ACI-side hashing and receipt signing. An
//!   optional `Authorization: Bearer <token>` is recorded on the
//!   receipt so later lookups can authenticate the original requester.
//! * `POST /v1/completions` - compatibility surface. The aggregator
//!   forwards legacy prompt completions through the same ACI receipt
//!   path as chat completions. ACI E2EE is an optional add-on here;
//!   plaintext OpenAI-compatible requests remain unchanged.
//! * `GET  /v1/models` - proxy the upstream OpenAI-compatible model list.
//! * `GET  /v1/metrics` - expose aggregator-owned Prometheus metrics.
//! * `GET  /v1/admin/upstreams` - authenticated admin view of the
//!   current upstream config, with secrets redacted.
//! * `PUT  /v1/admin/upstreams` - authenticated admin replacement of
//!   the single upstream config file.
//! * `GET  /v1/receipt/{chat_id}` - fetch the canonical receipt
//!   response. The top-level fields match dstack-vllm-proxy's legacy
//!   signature response; the signed ACI receipt is carried in
//!   `receipt`.
//! * `GET  /v1/signature/{chat_id}` - alias of `/v1/receipt/{chat_id}`.
//! * `GET  /v1/receipt/{chat_id}/body` - fetch the retained
//!   post-rewrite request body. Returns `receipt_body_not_retained`
//!   when retention is zero or the entry expired.
//!
//! The router installs a middleware that emits `X-ACI-Version`,
//! `X-ACI-Identity`, and `X-ACI-Keyset-Digest` on every response,
//! including error paths.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::aci::e2ee::{
    E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519, E2EE_ALGO_SECP256K1_AESGCM,
};
use crate::aci::keys::{
    ethereum_address_from_uncompressed_public_key, KeyError, LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519,
};
use crate::aci::types::AttestationReport;
use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{
    AciService, ChatCompletionRequest, E2eeError, E2eeRequestContext, E2eeRequestParts,
    E2eeResponseInfo, GatewayRequestContext, ReceiptOwner, ServiceError, StreamingForwardResult,
    UpstreamVerificationError, CHAT_COMPLETIONS_PATH, COMPLETIONS_PATH,
};
use crate::aggregator::upstream_config::{
    parse_config_text, UpstreamConfigError, UpstreamConfigManager,
};

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<AciService>,
    pub upstream_config: Option<Arc<UpstreamConfigManager>>,
    pub admin_token: Option<String>,
    middleware: Option<HttpMiddleware>,
    request_store: GatewayRequestStore,
}

#[derive(Clone)]
struct HttpMiddleware {
    base_url: String,
    client: reqwest::Client,
}

#[derive(Clone)]
pub struct GatewayRequestStore {
    inner: Arc<Mutex<HashMap<String, PendingGatewayRequest>>>,
    ttl: Duration,
}

#[derive(Clone)]
struct PendingGatewayRequest {
    expires_at: Instant,
    request: StoredGatewayRequest,
}

#[derive(Clone)]
pub struct StoredGatewayRequest {
    pub endpoint_path: &'static str,
    pub received_body: Vec<u8>,
    pub upstream_required: bool,
    pub requester: Option<ReceiptOwner>,
    pub e2ee: Option<E2eeRequestContext>,
    pub user_model: Option<String>,
}

impl GatewayRequestStore {
    const DEFAULT_TTL: Duration = Duration::from_secs(300);

    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    pub fn insert(&self, request_id: String, request: StoredGatewayRequest) {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("gateway request store poisoned");
        inner.retain(|_, pending| pending.expires_at > now);
        inner.insert(
            request_id,
            PendingGatewayRequest {
                expires_at: now + self.ttl,
                request,
            },
        );
    }

    fn take(&self, request_id: &str) -> Option<StoredGatewayRequest> {
        let pending = self
            .inner
            .lock()
            .expect("gateway request store poisoned")
            .remove(request_id)?;
        (pending.expires_at > Instant::now()).then_some(pending.request)
    }
}

impl Default for GatewayRequestStore {
    fn default() -> Self {
        Self::new(Self::DEFAULT_TTL)
    }
}

#[derive(Clone)]
struct InternalBackendState {
    service: Arc<AciService>,
    request_store: GatewayRequestStore,
}

pub fn build_router(service: Arc<AciService>) -> Router {
    build_router_inner(service, None, None, None, GatewayRequestStore::default())
}

pub fn build_router_with_admin(
    service: Arc<AciService>,
    upstream_config: Arc<UpstreamConfigManager>,
    admin_token: Option<String>,
) -> Router {
    build_router_inner(
        service,
        Some(upstream_config),
        admin_token,
        None,
        GatewayRequestStore::default(),
    )
}

pub fn build_router_with_admin_and_http_middleware(
    service: Arc<AciService>,
    upstream_config: Arc<UpstreamConfigManager>,
    admin_token: Option<String>,
    request_store: GatewayRequestStore,
    middleware_url: impl Into<String>,
) -> Router {
    build_router_inner(
        service,
        Some(upstream_config),
        admin_token,
        Some(http_middleware(middleware_url)),
        request_store,
    )
}

pub fn build_router_with_http_middleware(
    service: Arc<AciService>,
    request_store: GatewayRequestStore,
    middleware_url: impl Into<String>,
) -> Router {
    build_router_inner(
        service,
        None,
        None,
        Some(http_middleware(middleware_url)),
        request_store,
    )
}

fn http_middleware(middleware_url: impl Into<String>) -> HttpMiddleware {
    let mut base_url = middleware_url.into();
    while base_url.ends_with('/') {
        base_url.pop();
    }
    HttpMiddleware {
        base_url,
        client: reqwest::Client::new(),
    }
}

fn build_router_inner(
    service: Arc<AciService>,
    upstream_config: Option<Arc<UpstreamConfigManager>>,
    admin_token: Option<String>,
    middleware: Option<HttpMiddleware>,
    request_store: GatewayRequestStore,
) -> Router {
    let state = AppState {
        service,
        upstream_config,
        admin_token,
        middleware,
        request_store,
    };
    Router::new()
        .route("/", get(root))
        .route("/v1/models", get(models))
        .route("/v1/metrics", get(metrics))
        .route(
            "/v1/admin/upstreams",
            get(admin_get_upstreams).put(admin_put_upstreams),
        )
        .route("/v1/attestation/report", get(attestation_report))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/receipt/:chat_id", get(receipt_by_chat_id))
        .route("/v1/signature/:chat_id", get(receipt_by_chat_id))
        .route("/v1/receipt/:chat_id/body", get(get_receipt_body))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            aci_headers_middleware,
        ))
        .with_state(state)
}

pub fn build_internal_backend_router(
    service: Arc<AciService>,
    request_store: GatewayRequestStore,
) -> Router {
    Router::new()
        .route("/internal/forward", post(internal_forward))
        .with_state(InternalBackendState {
            service,
            request_store,
        })
}

#[derive(Deserialize)]
struct AttestationQuery {
    nonce: Option<String>,
    signing_algo: Option<String>,
}

#[derive(Deserialize)]
struct SignatureQuery {
    signing_algo: Option<String>,
}

async fn root(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "api_version": "aci/1",
        "workload_id": state.service.workload_id(),
        "workload_keyset_digest": state.service.workload_keyset_digest(),
    }))
}

async fn models(State(state): State<AppState>) -> Response {
    if let Some(middleware) = state.middleware.clone() {
        return get_from_middleware(middleware, "/v1/models").await;
    }
    match state.service.upstream().models().await {
        Ok(upstream) => upstream_direct_response(upstream, "application/json"),
        Err(err) => upstream_proxy_error_response(err),
    }
}

async fn metrics(State(state): State<AppState>) -> Response {
    match state.service.metrics() {
        Ok(snapshot) => {
            let mut headers = HeaderMap::new();
            insert_str_header(&mut headers, "content-type", &snapshot.content_type);
            (StatusCode::OK, headers, snapshot.body).into_response()
        }
        Err(err) => internal_error_response(err),
    }
}

async fn admin_get_upstreams(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = enforce_admin(&state, &headers) {
        return resp;
    }
    let Some(manager) = &state.upstream_config else {
        return admin_not_found_response();
    };
    Json(manager.snapshot()).into_response()
}

async fn admin_put_upstreams(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(resp) = enforce_admin(&state, &headers) {
        return resp;
    }
    let Some(manager) = &state.upstream_config else {
        return admin_not_found_response();
    };
    let text = match std::str::from_utf8(&body) {
        Ok(text) => text,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_upstream_config",
                format!("upstream config body must be UTF-8 JSON: {e}"),
            );
        }
    };
    let config = match parse_config_text(text) {
        Ok(config) => config,
        Err(e) => return upstream_config_error_response(e),
    };
    match manager.replace(config) {
        Ok(snapshot) => {
            let manager = manager.clone();
            tokio::spawn(async move {
                let results = manager.prewarm_upstream_verification().await;
                for result in results {
                    match result.reason {
                        Some(reason) => tracing::warn!(
                            upstream = %result.upstream_name,
                            model = %result.model_id,
                            origin = ?result.url_origin,
                            verifier = %result.verifier_id,
                            result = %result.result,
                            reason = %reason,
                            "upstream verification prewarm finished"
                        ),
                        None => tracing::info!(
                            upstream = %result.upstream_name,
                            model = %result.model_id,
                            origin = ?result.url_origin,
                            verifier = %result.verifier_id,
                            result = %result.result,
                            "upstream verification prewarm finished"
                        ),
                    }
                }
            });
            Json(snapshot).into_response()
        }
        Err(e) => upstream_config_error_response(e),
    }
}

async fn attestation_report(
    State(state): State<AppState>,
    Query(q): Query<AttestationQuery>,
) -> Response {
    match state.service.attestation_report(q.nonce).await {
        Ok(report) => {
            match report_with_legacy_attestation_fields(report, q.signing_algo.as_deref()) {
                Ok(value) => Json(value).into_response(),
                Err(e) => internal_error_response(e),
            }
        }
        Err(e) => internal_error_response(e),
    }
}

fn report_with_legacy_attestation_fields(
    report: AttestationReport,
    signing_algo: Option<&str>,
) -> Result<Value, ServiceError> {
    let mut value = serde_json::to_value(report)
        .map_err(|e| ServiceError::Key(KeyError::Crypto(format!("serialize report: {e}"))))?;
    let Some(obj) = value.as_object_mut() else {
        return Ok(value);
    };

    let signing_algo = signing_algo
        .unwrap_or(LEGACY_ALGO_ECDSA)
        .to_ascii_lowercase();
    let legacy_e2ee = obj
        .get("attestation")
        .and_then(|v| v.get("workload_keyset"))
        .and_then(|v| v.get("e2ee_public_keys"))
        .and_then(Value::as_array)
        .and_then(|keys| {
            keys.iter().find_map(|key| {
                let e2ee_key = key.as_object()?;
                let algo = e2ee_key.get("algo").and_then(Value::as_str)?;
                let public_key = e2ee_key.get("public_key").and_then(Value::as_str)?;
                let matches = match signing_algo.as_str() {
                    LEGACY_ALGO_ECDSA => {
                        algo == E2EE_ALGO_LEGACY_ECDSA || algo == E2EE_ALGO_SECP256K1_AESGCM
                    }
                    LEGACY_ALGO_ED25519 => algo == E2EE_ALGO_LEGACY_ED25519,
                    _ => false,
                };
                matches.then(|| public_key.to_string())
            })
        });

    if let Some(public_key) = legacy_e2ee {
        let signing_address = if signing_algo == LEGACY_ALGO_ED25519 {
            public_key.clone()
        } else {
            ethereum_address_from_uncompressed_public_key(&public_key)?
        };
        obj.insert("signing_public_key".to_string(), Value::String(public_key));
        obj.insert("signing_algo".to_string(), Value::String(signing_algo));
        obj.insert(
            "signing_address".to_string(),
            Value::String(signing_address),
        );
    } else if !matches!(
        signing_algo.as_str(),
        LEGACY_ALGO_ECDSA | LEGACY_ALGO_ED25519
    ) {
        return Err(ServiceError::Key(KeyError::UnsupportedAlgo(signing_algo)));
    } else {
        let legacy_e2ee = obj
            .get("attestation")
            .and_then(|v| v.get("workload_keyset"))
            .and_then(|v| v.get("e2ee_public_keys"))
            .and_then(Value::as_array)
            .and_then(|keys| keys.first())
            .and_then(Value::as_object)
            .and_then(|e2ee_key| {
                let algo = e2ee_key.get("algo").and_then(Value::as_str)?;
                let public_key = e2ee_key.get("public_key").and_then(Value::as_str)?;
                (algo == E2EE_ALGO_SECP256K1_AESGCM).then(|| public_key.to_string())
            });
        if let Some(public_key) = legacy_e2ee {
            let signing_address = ethereum_address_from_uncompressed_public_key(&public_key)?;
            obj.insert("signing_public_key".to_string(), Value::String(public_key));
            obj.insert(
                "signing_algo".to_string(),
                Value::String(LEGACY_ALGO_ECDSA.to_string()),
            );
            obj.insert(
                "signing_address".to_string(),
                Value::String(signing_address),
            );
        }
    }

    let mut legacy_attestation = obj.clone();
    legacy_attestation.remove("all_attestations");
    obj.insert(
        "all_attestations".to_string(),
        Value::Array(vec![Value::Object(legacy_attestation)]),
    );
    Ok(value)
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    openai_completion_endpoint(state, headers, body, CHAT_COMPLETIONS_PATH).await
}

async fn openai_completion_endpoint(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
    endpoint_path: &'static str,
) -> Response {
    let has_e2ee = has_e2ee_headers(&headers);
    if has_e2ee && state.service.supported_e2ee_versions().is_empty() {
        return unsupported_e2ee_response();
    }

    let (service_body, e2ee) = if has_e2ee {
        match state.service.prepare_e2ee_v2_request(
            E2eeRequestParts {
                signing_algo: header_str(&headers, "x-signing-algo"),
                client_public_key: header_str(&headers, "x-client-pub-key"),
                model_public_key: header_str(&headers, "x-model-pub-key"),
                version: header_str(&headers, "x-e2ee-version"),
                nonce: header_str(&headers, "x-e2ee-nonce"),
                timestamp: header_str(&headers, "x-e2ee-timestamp"),
            },
            body.as_ref(),
            endpoint_path,
        ) {
            Ok(prepared) => (prepared.decrypted_body, Some(prepared.context)),
            Err(err) => return e2ee_error_response(err),
        }
    } else {
        (body.to_vec(), None)
    };

    // Surface obviously-broken bodies early; we still hash exactly
    // the bytes visible after TLS / E2EE termination.
    let parsed = match serde_json::from_slice::<Value>(&service_body) {
        Ok(value) => value,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("invalid json: {e}"),
            );
        }
    };
    let (parsed, forwarded_body) = match strip_empty_tool_calls(parsed) {
        (normalized, true) => match serde_json::to_vec(&normalized) {
            Ok(bytes) => (normalized, Some(bytes)),
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize normalized request");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "failed to serialize normalized request",
                );
            }
        },
        (normalized, false) => (normalized, None),
    };

    let upstream_required = match headers
        .get("x-upstream-verification")
        .and_then(|v| v.to_str().ok())
    {
        None | Some("required") => true,
        Some("none") => false,
        Some(other) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("invalid X-Upstream-Verification: {other}"),
            );
        }
    };

    let requester = extract_bearer(&headers)
        .as_deref()
        .map(ReceiptOwner::from_bearer);
    let context = GatewayRequestContext {
        request_id: generate_request_id(),
        user_model: parsed
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        target_route_id: None,
    };

    let stream = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(middleware) = state.middleware.clone() {
        let forwarded_body = forwarded_body.unwrap_or_else(|| service_body.clone());
        let request_id = context.request_id.clone();
        state.request_store.insert(
            request_id.clone(),
            StoredGatewayRequest {
                endpoint_path,
                received_body: service_body,
                upstream_required,
                requester,
                e2ee,
                user_model: context.user_model.clone(),
            },
        );
        let response =
            forward_to_middleware(middleware, endpoint_path, context, forwarded_body).await;
        state.request_store.take(&request_id);
        return response;
    }

    forward_to_backend(
        state.service,
        BackendForwardInput {
            context,
            endpoint_path,
            received_body: service_body,
            forwarded_body,
            upstream_required,
            requester,
            e2ee,
            stream,
        },
    )
    .await
}

async fn forward_to_middleware(
    middleware: HttpMiddleware,
    endpoint_path: &'static str,
    context: GatewayRequestContext,
    body: Vec<u8>,
) -> Response {
    let url = format!("{}{}", middleware.base_url, endpoint_path);
    let mut builder = middleware
        .client
        .post(url)
        .header("content-type", "application/json")
        .header("x-private-ai-gateway-request-id", context.request_id);
    if let Some(user_model) = context.user_model {
        builder = builder.header("x-private-ai-gateway-user-model", user_model);
    }
    middleware_response(builder.body(body).send().await).await
}

async fn get_from_middleware(middleware: HttpMiddleware, path: &'static str) -> Response {
    let url = format!("{}{}", middleware.base_url, path);
    middleware_response(middleware.client.get(url).send().await).await
}

async fn middleware_response(result: Result<reqwest::Response, reqwest::Error>) -> Response {
    match result {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let headers = reqwest_response_headers(resp.headers());
            match resp.bytes().await {
                Ok(body) => (status, headers, body.to_vec()).into_response(),
                Err(err) => error_response(
                    StatusCode::BAD_GATEWAY,
                    "middleware_error",
                    format!("middleware response read failed: {err}"),
                ),
            }
        }
        Err(err) => error_response(
            StatusCode::BAD_GATEWAY,
            "middleware_error",
            format!("middleware request failed: {err}"),
        ),
    }
}

struct BackendForwardInput {
    context: GatewayRequestContext,
    endpoint_path: &'static str,
    received_body: Vec<u8>,
    forwarded_body: Option<Vec<u8>>,
    upstream_required: bool,
    requester: Option<ReceiptOwner>,
    e2ee: Option<E2eeRequestContext>,
    stream: bool,
}

async fn forward_to_backend(service: Arc<AciService>, input: BackendForwardInput) -> Response {
    if input.stream {
        let result = service
            .forward_chat_completion_stream_request(ChatCompletionRequest {
                context: input.context,
                endpoint_path: input.endpoint_path,
                received_body: &input.received_body,
                forwarded_body: input.forwarded_body,
                upstream_required: Some(input.upstream_required),
                upstream_verification_event: None,
                requester: input.requester,
                e2ee: input.e2ee,
            })
            .await;
        return match result {
            Ok(StreamingForwardResult::Stream(forward)) => {
                let mut resp_headers = chat_response_headers(
                    &forward.receipt_id,
                    &forward.upstream_headers,
                    "text/event-stream",
                    forward.e2ee.as_ref(),
                );
                resp_headers.insert(
                    HeaderName::from_static("x-accel-buffering"),
                    HeaderValue::from_static("no"),
                );
                resp_headers.insert(
                    HeaderName::from_static("cache-control"),
                    HeaderValue::from_static("no-cache"),
                );
                let status =
                    StatusCode::from_u16(forward.upstream_status).unwrap_or(StatusCode::OK);
                let body = Body::from_stream(
                    forward
                        .body
                        .map(|chunk| chunk.map_err(|e| std::io::Error::other(e.to_string()))),
                );
                (status, resp_headers, body).into_response()
            }
            Ok(StreamingForwardResult::UpstreamError(forward)) => {
                let status =
                    StatusCode::from_u16(forward.upstream_status).unwrap_or(StatusCode::OK);
                let resp_headers = upstream_direct_response_headers(&forward.upstream_headers);
                (status, resp_headers, forward.upstream_body).into_response()
            }
            Err(ServiceError::UpstreamVerification(uv)) => upstream_verification_error_response(uv),
            Err(ServiceError::E2ee(err)) => e2ee_error_response(err),
            Err(ServiceError::Upstream(UpstreamError::Routing(message))) => {
                routing_error_response(message)
            }
            Err(other) => internal_error_response(other),
        };
    }

    let result = service
        .forward_chat_completion_request(ChatCompletionRequest {
            context: input.context,
            endpoint_path: input.endpoint_path,
            received_body: &input.received_body,
            forwarded_body: input.forwarded_body,
            upstream_required: Some(input.upstream_required),
            upstream_verification_event: None,
            requester: input.requester,
            e2ee: input.e2ee,
        })
        .await;
    match result {
        Ok(forward) => {
            let resp_headers = chat_response_headers(
                &forward.receipt.receipt_id,
                &forward.upstream_headers,
                "application/json",
                forward.e2ee.as_ref(),
            );

            let status = StatusCode::from_u16(forward.upstream_status).unwrap_or(StatusCode::OK);
            (status, resp_headers, forward.upstream_body).into_response()
        }
        Err(ServiceError::UpstreamVerification(uv)) => upstream_verification_error_response(uv),
        Err(ServiceError::E2ee(err)) => e2ee_error_response(err),
        Err(ServiceError::Upstream(UpstreamError::Routing(message))) => {
            routing_error_response(message)
        }
        Err(other) => internal_error_response(other),
    }
}

async fn internal_forward(
    State(state): State<InternalBackendState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(request_id) = header_str(&headers, "x-private-ai-gateway-request-id") else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_internal_request",
            "missing X-Private-AI-Gateway-Request-Id",
        );
    };
    let request_id = request_id.to_string();
    let Some(target_route_id) = header_str(&headers, "x-private-ai-gateway-target") else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_internal_request",
            "missing X-Private-AI-Gateway-Target",
        );
    };
    if target_route_id.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_internal_request",
            "empty X-Private-AI-Gateway-Target",
        );
    }
    let target_route_id = target_route_id.to_string();
    let Some(stored) = state.request_store.take(&request_id) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_internal_request",
            "unknown or expired request id",
        );
    };

    let parsed = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("invalid json: {e}"),
            );
        }
    };
    let stream = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    forward_to_backend(
        state.service,
        BackendForwardInput {
            context: GatewayRequestContext {
                request_id,
                user_model: stored.user_model,
                target_route_id: Some(target_route_id),
            },
            endpoint_path: stored.endpoint_path,
            received_body: stored.received_body,
            forwarded_body: Some(body.to_vec()),
            upstream_required: stored.upstream_required,
            requester: stored.requester,
            e2ee: stored.e2ee,
            stream,
        },
    )
    .await
}

fn strip_empty_tool_calls(mut payload: Value) -> (Value, bool) {
    let mut changed = false;
    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return (payload, changed);
    };

    for message in messages {
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        if message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            message.remove("tool_calls");
            changed = true;
        }
    }

    (payload, changed)
}

fn generate_request_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("req_{}", hex::encode(bytes))
}

fn chat_response_headers(
    receipt_id: &str,
    upstream_headers: &std::collections::HashMap<String, String>,
    default_content_type: &'static str,
    e2ee: Option<&E2eeResponseInfo>,
) -> HeaderMap {
    let mut resp_headers = HeaderMap::new();
    insert_str_header(&mut resp_headers, "x-receipt-id", receipt_id);
    match e2ee {
        Some(info) => {
            resp_headers.insert(
                HeaderName::from_static("x-e2ee-applied"),
                HeaderValue::from_static("true"),
            );
            insert_str_header(&mut resp_headers, "x-e2ee-version", &info.version);
            insert_str_header(&mut resp_headers, "x-e2ee-algo", &info.algo);
        }
        None => {
            resp_headers.insert(
                HeaderName::from_static("x-e2ee-applied"),
                HeaderValue::from_static("false"),
            );
        }
    }

    let content_type = upstream_headers
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| default_content_type.to_string());
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        resp_headers.insert(axum::http::header::CONTENT_TYPE, value);
    }
    resp_headers
}

fn upstream_direct_response_headers(
    upstream_headers: &std::collections::HashMap<String, String>,
) -> HeaderMap {
    let mut resp_headers = HeaderMap::new();
    for (name, value) in upstream_headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "connection" | "transfer-encoding" | "content-length"
        ) {
            continue;
        }
        let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(header_value) = HeaderValue::from_str(value) else {
            continue;
        };
        resp_headers.insert(header_name, header_value);
    }
    resp_headers
}

fn reqwest_response_headers(upstream_headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut resp_headers = HeaderMap::new();
    for (name, value) in upstream_headers {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "connection" | "transfer-encoding" | "content-length"
        ) {
            continue;
        }
        resp_headers.insert(name.clone(), value.clone());
    }
    resp_headers
}

fn upstream_direct_response(
    upstream: crate::aci::upstream::UpstreamResponse,
    default_content_type: &'static str,
) -> Response {
    let mut headers = upstream_direct_response_headers(&upstream.headers);
    if !headers.contains_key(axum::http::header::CONTENT_TYPE) {
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static(default_content_type),
        );
    }
    let status = StatusCode::from_u16(upstream.status_code).unwrap_or(StatusCode::BAD_GATEWAY);
    (status, headers, upstream.body).into_response()
}

fn upstream_proxy_error_response(err: crate::aci::upstream::UpstreamError) -> Response {
    tracing::warn!(error = %err, "upstream proxy request failed");
    error_response(StatusCode::BAD_GATEWAY, "upstream_error", err.to_string())
}

fn routing_error_response(message: String) -> Response {
    error_response(StatusCode::BAD_REQUEST, "model_routing_error", message)
}

async fn completions(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    openai_completion_endpoint(state, headers, body, COMPLETIONS_PATH).await
}

async fn receipt_by_chat_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat_id): Path<String>,
    Query(q): Query<SignatureQuery>,
) -> Response {
    let Some(receipt) = state.service.get_receipt_by_chat_id(&chat_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "Chat id not found or expired",
        );
    };
    if let Some(resp) = enforce_owner(&state, &headers, &receipt.receipt_id) {
        return resp;
    }
    match state
        .service
        .legacy_signature_for_receipt(&receipt, q.signing_algo.as_deref())
    {
        Ok(sig) => Json(json!({
            "api_version": "aci/1",
            "text": sig.text,
            "signature": sig.signature,
            "signing_address": sig.signing_address,
            "signing_algo": sig.signing_algo,
            "receipt": receipt.to_canonical_value(true),
        }))
        .into_response(),
        Err(ServiceError::Key(KeyError::UnsupportedAlgo(_))) => invalid_signing_algo_response(),
        Err(other) => internal_error_response(other),
    }
}

async fn get_receipt_body(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat_id): Path<String>,
) -> Response {
    let Some(receipt) = state.service.get_receipt_by_chat_id(&chat_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "Chat id not found or expired",
        );
    };
    let receipt_id = receipt.receipt_id.clone();
    if let Some(resp) = enforce_owner(&state, &headers, &receipt_id) {
        return resp;
    }
    if state.service.body_retention_seconds() == 0 {
        return error_response(
            StatusCode::NOT_FOUND,
            "receipt_body_not_retained",
            "receipt body not retained",
        );
    }
    match state.service.get_retained_body(&receipt_id) {
        Some(body) => {
            let mut resp_headers = HeaderMap::new();
            resp_headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            (StatusCode::OK, resp_headers, body).into_response()
        }
        None => error_response(
            StatusCode::NOT_FOUND,
            "receipt_body_not_retained",
            "receipt body not retained",
        ),
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn has_e2ee_headers(headers: &HeaderMap) -> bool {
    [
        "x-signing-algo",
        "x-client-pub-key",
        "x-model-pub-key",
        "x-e2ee-version",
        "x-e2ee-nonce",
        "x-e2ee-timestamp",
    ]
    .into_iter()
    .any(|name| headers.contains_key(name))
}

fn unsupported_e2ee_response() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "e2ee_invalid_version",
        "ACI E2EE is not supported by this service",
    )
}

fn invalid_signing_algo_response() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_signing_algo",
        "Invalid signing algorithm. Must be 'ed25519' or 'ecdsa'",
    )
}

fn e2ee_error_response(err: E2eeError) -> Response {
    match err {
        E2eeError::EncryptionFailed => internal_error_response(ServiceError::E2ee(err)),
        E2eeError::HeaderMissing => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_header_missing",
            err.to_string(),
        ),
        E2eeError::InvalidSigningAlgo => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_signing_algo",
            err.to_string(),
        ),
        E2eeError::InvalidVersion => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_version",
            err.to_string(),
        ),
        E2eeError::InvalidPublicKey => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_public_key",
            err.to_string(),
        ),
        E2eeError::ModelKeyMismatch => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_model_key_mismatch",
            err.to_string(),
        ),
        E2eeError::InvalidNonce => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_nonce",
            err.to_string(),
        ),
        E2eeError::ReplayDetected => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_replay_detected",
            err.to_string(),
        ),
        E2eeError::InvalidTimestamp => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_timestamp",
            err.to_string(),
        ),
        E2eeError::InvalidPayloadModel => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_invalid_payload_model",
            err.to_string(),
        ),
        E2eeError::DecryptionFailed => error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_decryption_failed",
            err.to_string(),
        ),
    }
}

/// Returns `Some(response)` when the caller MUST be rejected; returns
/// `None` to indicate "auth passed (or not required), proceed".
fn enforce_owner(state: &AppState, headers: &HeaderMap, receipt_id: &str) -> Option<Response> {
    // Anonymous receipts: any caller may retrieve them.
    let recorded_owner = state.service.owner_of_receipt(receipt_id)?;
    let Some(token) = extract_bearer(headers) else {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "this receipt is owned; authenticate with the original bearer token",
        ));
    };
    if ReceiptOwner::from_bearer(&token) == recorded_owner {
        None
    } else {
        Some(error_response(
            StatusCode::FORBIDDEN,
            "redaction_required",
            "the presented credential does not match the receipt owner",
        ))
    }
}

fn enforce_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = state.admin_token.as_deref() else {
        return Some(admin_not_found_response());
    };
    let Some(token) = extract_bearer(headers) else {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "admin bearer token required",
        ));
    };
    if token == expected {
        None
    } else {
        Some(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "invalid admin bearer token",
        ))
    }
}

fn insert_str_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(HeaderName::from_static(name), v);
    }
}

fn admin_not_found_response() -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "not_found",
        "admin upstream config endpoint is not enabled",
    )
}

fn upstream_config_error_response(err: UpstreamConfigError) -> Response {
    match err {
        UpstreamConfigError::InvalidConfig(message) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_upstream_config", message)
        }
        other => {
            tracing::error!(error = %other, "upstream config admin operation failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                other.to_string(),
            )
        }
    }
}

fn upstream_verification_error_response(err: UpstreamVerificationError) -> Response {
    let message = err.to_string();
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "upstream_verification_failed",
        message,
    )
}

fn error_response(status: StatusCode, error_type: &str, message: impl Into<String>) -> Response {
    let body = json!({
        "error": {
            "message": message.into(),
            "type": error_type,
            "code": Value::Null,
            "param": Value::Null,
        }
    });
    (status, Json(body)).into_response()
}

fn internal_error_response(err: ServiceError) -> Response {
    tracing::error!(error = %err, "aci service internal error");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        err.to_string(),
    )
}

/// Middleware that stamps `X-ACI-Version`, `X-ACI-Identity`, and
/// `X-ACI-Keyset-Digest` on every response, including errors. A
/// relying party can therefore confirm the workload identity that
/// served any HTTP path, not just the success path of
/// `POST /v1/chat/completions`.
async fn aci_headers_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert(
        HeaderName::from_static("x-aci-version"),
        HeaderValue::from_static("aci/1"),
    );
    if let Ok(v) = HeaderValue::from_str(state.service.workload_id()) {
        headers.insert(HeaderName::from_static("x-aci-identity"), v);
    }
    if let Ok(v) = HeaderValue::from_str(state.service.workload_keyset_digest()) {
        headers.insert(HeaderName::from_static("x-aci-keyset-digest"), v);
    }
    resp
}
