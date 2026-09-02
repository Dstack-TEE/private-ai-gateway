//! Axum route handlers and their query types.

use std::collections::HashSet;

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::aci::e2ee::{E2EE_ALGO_LEGACY_ECDSA, E2EE_ALGO_LEGACY_ED25519};
use crate::aci::keys::{
    ethereum_address_from_uncompressed_public_key, KeyError, LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519,
};
use crate::aci::types::{
    AttestationReport, KeyedPublicKey, PROVIDER_ACI_SESSION_IDS, PROVIDER_ACI_VERIFIED,
};
use crate::aggregator::service::{
    E2eeRequestParts, GatewayRequestContext, ReceiptOwner, ServiceError, CHAT_COMPLETIONS_PATH,
    COMPLETIONS_PATH, EMBEDDINGS_PATH, MESSAGES_PATH, RESPONSES_PATH,
};
use crate::aggregator::session_store::sort_sessions_newest_first;
use crate::aggregator::upstream_config::{parse_config_text, UpstreamProvider};

use super::backend::{
    fetch_upstream_nvidia_payload, forward_to_backend, generate_request_id,
    inbound_aci_forward_depth, strip_empty_tool_calls, upstream_direct_response,
    upstream_proxy_error_response, BackendForwardInput,
};
use super::error_responses::{
    admin_not_found_response, e2ee_error_response, error_response, insert_str_header,
    internal_error_response, invalid_signing_algo_response, unknown_downstream_host_response,
    unsupported_e2ee_response, upstream_config_error_response,
};
use super::util::{
    enforce_admin, enforce_owner, extract_bearer, force_tee_true, has_e2ee_headers, header_str,
    request_host_domain,
};
use super::AppState;
use crate::middleware::errors::Surface;
use crate::middleware::request_transform::Endpoint;
use crate::middleware::{hash_api_key, CompletionInput, Middleware};

#[derive(Deserialize)]
pub(super) struct AttestationQuery {
    nonce: Option<String>,
    signing_algo: Option<String>,
    model: Option<String>,
    version: Option<u32>,
}

#[derive(Deserialize)]
pub(super) struct SignatureQuery {
    signing_algo: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SessionListQuery {
    upstream_name: Option<String>,
    model: Option<String>,
}

// Liveness probe for load balancers and orchestrators. Unauthenticated and
// version-independent: it reports only that the process is serving requests.
pub(super) async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub(super) async fn root(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "api_version": "aci/1",
        "workload_keyset_digest": state.service.workload_keyset_digest(),
    }))
}

// Catalog filters (e.g. `?zdr=true`) are the control plane's to interpret, so
// the query string is relayed verbatim rather than parsed here. Without this the
// gateway would silently drop it and serve an unfiltered catalog.
fn catalog_path(base: &str, query: Option<String>) -> String {
    match query.as_deref().filter(|q| !q.is_empty()) {
        Some(q) => format!("{base}?{q}"),
        None => base.to_string(),
    }
}

/// On a TEE-only host, rewrite the relayed catalog query to force `?tee=true`
/// (see `force_tee_true`); otherwise pass the client's query through unchanged.
fn tee_only_catalog_query(
    middleware: &Middleware,
    headers: &HeaderMap,
    query: Option<String>,
) -> Option<String> {
    match request_host_domain(headers) {
        Some(host) if middleware.is_tee_only_domain(&host) => Some(force_tee_true(query)),
        _ => query,
    }
}

pub(super) async fn models(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if let Some(middleware) = state.middleware.clone() {
        let query = tee_only_catalog_query(&middleware, &headers, query);
        return middleware
            .handle_catalog(&catalog_path("/v1/models", query))
            .await;
    }
    match state.service.upstream().models().await {
        Ok(upstream) => upstream_direct_response(upstream, "application/json"),
        Err(err) => upstream_proxy_error_response(err),
    }
}

// Relay every /v1/models/<sub> sub-catalog to the middleware, which owns the
// real routing (namespace, providers, ...). matchit 0.7.3 forbids a param and
// a static sibling at the same position, so we relay the whole subtree rather
// than enumerate routes here. Only meaningful in the middleware topology.
pub(super) async fn models_subpath(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(rest): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let Some(middleware) = state.middleware.clone() else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "model sub-catalogs are not available in direct-upstream mode",
        );
    };
    let query = tee_only_catalog_query(&middleware, &headers, query);
    middleware
        .handle_catalog(&catalog_path(&format!("/v1/models/{rest}"), query))
        .await
}

// Embedding model catalog. Only meaningful in the control-plane middleware
// topology.
pub(super) async fn embeddings_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let Some(middleware) = state.middleware.clone() else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "embedding model catalog is not available in direct-upstream mode",
        );
    };
    let query = tee_only_catalog_query(&middleware, &headers, query);
    middleware
        .handle_catalog(&catalog_path("/v1/embeddings/models", query))
        .await
}

pub(super) async fn metrics(State(state): State<AppState>) -> Response {
    match state.service.metrics() {
        Ok(snapshot) => {
            let mut headers = HeaderMap::new();
            insert_str_header(&mut headers, "content-type", &snapshot.content_type);
            (StatusCode::OK, headers, snapshot.body).into_response()
        }
        Err(err) => internal_error_response(err),
    }
}

pub(super) async fn admin_get_upstreams(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = enforce_admin(&state, &headers) {
        return resp;
    }
    let Some(manager) = &state.upstream_config else {
        return admin_not_found_response();
    };
    Json(manager.snapshot()).into_response()
}

pub(super) async fn admin_put_upstreams(
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

pub(super) async fn attestation_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AttestationQuery>,
) -> Response {
    let domain = request_host_domain(&headers);
    let model = q.model.as_deref().filter(|m| !m.is_empty());

    // Resolve the upstream serving `model` (direct-upstream mode only).
    let target = model.and_then(|m| {
        state
            .upstream_config
            .as_ref()
            .and_then(|mgr| mgr.attestation_upstream_target(m))
    });

    // Chutes serves a self-contained multi-instance report from the upstream,
    // independent of the gateway's own keyset.
    if let (Some(model), Some(target)) = (model, target.as_ref()) {
        if target.provider == UpstreamProvider::Chutes {
            return match state
                .service
                .upstream()
                .chutes_attestation_report(model)
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(e) => upstream_proxy_error_response(e),
            };
        }
    }

    // Otherwise (no model, or a non-Chutes provider): the gateway's own report,
    // enriched with the upstream model node's real GPU evidence when the provider
    // exposes it (PhalaDirect / NearAi).
    //
    // Effective nonce: the client's, or a freshly generated one when omitted —
    // matching dstack-vllm-proxy, which binds a fresh nonce rather than leaving
    // the slot empty. The same nonce is bound into report_data, echoed as
    // request_nonce, and used to fetch the upstream GPU evidence so all three
    // agree.
    let nonce = resolve_report_nonce(q.nonce.as_deref());
    match state
        .service
        .legacy_attestation_report_for_domain(
            q.signing_algo.as_deref(),
            q.version.unwrap_or(1),
            Some(&nonce),
            domain.as_deref(),
        )
        .await
    {
        Ok(report) => {
            let nvidia_payload = match target.as_ref() {
                Some(target) => {
                    fetch_upstream_nvidia_payload(
                        target,
                        &nonce,
                        inbound_aci_forward_depth(&headers),
                    )
                    .await
                }
                None => None,
            }
            .unwrap_or_else(|| empty_nvidia_payload(Some(&nonce)));
            match report_with_legacy_attestation_fields(
                report,
                q.signing_algo.as_deref(),
                nvidia_payload,
                &state.service.legacy_e2ee_keys(),
            ) {
                Ok(value) => Json(value).into_response(),
                Err(e) => internal_error_response(e),
            }
        }
        Err(
            e @ (ServiceError::DownstreamTlsDomainMissing
            | ServiceError::DownstreamTlsDomainUnknown(_)),
        ) => unknown_downstream_host_response(e),
        Err(e) => internal_error_response(e),
    }
}

/// The nonce a legacy report binds: the client's when supplied (and non-empty),
/// otherwise a freshly generated 32-byte hex nonce — matching dstack-vllm-proxy,
/// which never leaves the report-data nonce slot empty.
fn resolve_report_nonce(client_nonce: Option<&str>) -> String {
    match client_nonce.filter(|n| !n.is_empty()) {
        Some(nonce) => nonce.to_string(),
        None => {
            let mut bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut bytes);
            hex::encode(bytes)
        }
    }
}

/// Empty legacy `nvidia_payload` (a JSON string), used when no real upstream GPU
/// evidence is available. Field shape stays stable for old clients; the empty
/// `evidence_list` honestly signals "no GPU evidence".
fn empty_nvidia_payload(nonce: Option<&str>) -> Value {
    Value::String(
        json!({
            "nonce": nonce.unwrap_or_default(),
            "evidence_list": [],
            "arch": "HOPPER",
        })
        .to_string(),
    )
}

/// Canonical ACI attestation report — the bare report, no legacy
/// dstack-vllm-proxy compatibility fields injected.
pub(super) async fn aci_attestation_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AttestationQuery>,
) -> Response {
    let domain = request_host_domain(&headers);
    match state
        .service
        .attestation_report_for_domain(q.nonce, domain.as_deref())
        .await
    {
        Ok(report) => Json(report).into_response(),
        Err(ServiceError::InvalidNonce(e)) => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            e.to_string(),
        ),
        Err(
            e @ (ServiceError::DownstreamTlsDomainMissing
            | ServiceError::DownstreamTlsDomainUnknown(_)),
        ) => unknown_downstream_host_response(e),
        Err(e) => internal_error_response(e),
    }
}

/// Place the legacy dstack-vllm-proxy compatibility fields on a gateway
/// attestation report. `nvidia_payload` is supplied by the caller — the
/// handler decides whether it carries real upstream GPU evidence or an empty
/// placeholder. `legacy_keys` are the legacy keys from the key provider
/// (the ACI keyset no longer lists them).
pub(super) fn report_with_legacy_attestation_fields(
    report: AttestationReport,
    signing_algo: Option<&str>,
    nvidia_payload: Value,
    legacy_keys: &[KeyedPublicKey],
) -> Result<Value, ServiceError> {
    let mut value = serde_json::to_value(report)
        .map_err(|e| ServiceError::Key(KeyError::Crypto(format!("serialize report: {e}"))))?;
    let Some(obj) = value.as_object_mut() else {
        return Ok(value);
    };

    let signing_algo = signing_algo
        .unwrap_or(LEGACY_ALGO_ECDSA)
        .to_ascii_lowercase();
    let legacy_algo = match signing_algo.as_str() {
        LEGACY_ALGO_ECDSA => E2EE_ALGO_LEGACY_ECDSA,
        LEGACY_ALGO_ED25519 => E2EE_ALGO_LEGACY_ED25519,
        _ => return Err(ServiceError::Key(KeyError::UnsupportedAlgo(signing_algo))),
    };

    if let Some(key) = legacy_keys.iter().find(|key| key.algo == legacy_algo) {
        // The pre-ACI report served the 65-byte `04`-prefixed SEC1 form;
        // compatibility surfaces keep their wire shape (Appendix B).
        let public_key = if signing_algo == LEGACY_ALGO_ECDSA && key.public_key_hex.len() == 128 {
            format!("04{}", key.public_key_hex)
        } else {
            key.public_key_hex.clone()
        };
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
    }

    // Legacy dstack-vllm-proxy compatibility fields. Old clients read these from
    // the top level (and from each `all_attestations` entry), so inject them
    // before the clone below.
    if let Some(intel_quote) = obj
        .get("attestation")
        .and_then(|v| v.get("evidence"))
        .and_then(|v| v.get("quote"))
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        obj.insert("intel_quote".to_string(), Value::String(intel_quote));
    }
    obj.insert("nvidia_payload".to_string(), nvidia_payload);

    let mut legacy_attestation = obj.clone();
    legacy_attestation.remove("all_attestations");
    obj.insert(
        "all_attestations".to_string(),
        Value::Array(vec![Value::Object(legacy_attestation)]),
    );
    Ok(value)
}

pub(super) async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    openai_completion_endpoint(state, headers, body, CHAT_COMPLETIONS_PATH, false).await
}

pub(super) async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // OpenAI embeddings is buffered-only: any client-sent `stream:true`
    // is forced back to buffered so the receipt/E2EE pipeline runs the
    // single non-streaming response path.
    openai_completion_endpoint(state, headers, body, EMBEDDINGS_PATH, true).await
}

pub(super) async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // Native Anthropic-format downstream surface. The frontend treats the body
    // as opaque plaintext: it only extracts `model`/`stream` and forwards to the
    // middleware, which handles Anthropic<->provider conversion. The E2EE v2
    // compatibility protocol does not define Anthropic response content blocks,
    // so fail closed rather than mark an unencrypted response as E2EE-applied.
    if has_e2ee_headers(&headers) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_unsupported_endpoint",
            "E2EE v2 is not supported on /v1/messages",
        );
    }
    openai_completion_endpoint(state, headers, body, MESSAGES_PATH, false).await
}

pub(super) async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // Native OpenAI Responses API passthrough (create only). The frontend treats
    // the body as opaque plaintext (extracts `model`/`stream`); the path flows
    // through to the upstream as `base_url + /v1/responses`. E2EE v2 is not
    // supported on this endpoint yet — its body uses `input`, not `messages` —
    // so reject E2EE requests cleanly instead of failing later in field decryption.
    if has_e2ee_headers(&headers) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "e2ee_unsupported_endpoint",
            "E2EE v2 is not supported on /v1/responses",
        );
    }
    openai_completion_endpoint(state, headers, body, RESPONSES_PATH, false).await
}

#[derive(Debug, Default)]
struct AciConstraint {
    required: bool,
    session_ids: Vec<String>,
}

/// Parse the gateway-owned ACI routing constraints. The rest of `provider` is
/// still forwarded verbatim to the control plane for its own validation.
/// Malformed constraints are rejected rather than silently downgraded.
fn aci_constraint(parsed: &Value) -> Result<AciConstraint, String> {
    let Some(provider) = parsed.get("provider") else {
        return Ok(AciConstraint::default());
    };
    if provider.is_null() {
        return Ok(AciConstraint::default());
    }
    let Some(block) = provider.as_object() else {
        return Err("invalid 'provider' routing block: expected an object".to_string());
    };

    let explicitly_verified = match block.get(PROVIDER_ACI_VERIFIED) {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err("invalid 'provider.aci_verified': expected a boolean".to_string()),
    };

    let mut session_ids = Vec::new();
    let mut seen_session_ids = HashSet::new();
    if let Some(value) = block.get(PROVIDER_ACI_SESSION_IDS) {
        let Some(values) = value.as_array() else {
            return Err("invalid 'provider.aci_session_ids': expected an array".to_string());
        };
        if values.is_empty() {
            return Err("invalid 'provider.aci_session_ids': must not be empty".to_string());
        }
        for value in values {
            // §5.3: ids are bare 64-hex (§8). A malformed id is a 400 —
            // treating it as a membership miss would misreport a client bug
            // as `session_not_accepted`.
            let Some(id) = value.as_str().filter(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }) else {
                return Err(
                    "invalid 'provider.aci_session_ids': expected 64-hex session ids".to_string(),
                );
            };
            if seen_session_ids.insert(id) {
                session_ids.push(id.to_string());
            }
        }
    }

    // §5.3: an unknown `aci_`-prefixed member is rejected, never ignored —
    // silently dropping one would let a client believe it constrained serving
    // when it did not.
    if let Some(unknown) = block.keys().find(|name| {
        name.starts_with("aci_")
            && !matches!(
                name.as_str(),
                PROVIDER_ACI_VERIFIED | PROVIDER_ACI_SESSION_IDS
            )
    }) {
        return Err(format!(
            "unknown ACI serving constraint: provider.{unknown}"
        ));
    }

    if explicitly_verified == Some(false) && !session_ids.is_empty() {
        return Err(
            "invalid provider constraint: 'aci_session_ids' requires 'aci_verified'".to_string(),
        );
    }

    Ok(AciConstraint {
        required: explicitly_verified.unwrap_or(false) || !session_ids.is_empty(),
        session_ids,
    })
}

/// Remove gateway-only ACI controls before direct upstream forwarding. The
/// middleware path already shapes a fresh provider request, so it keeps the
/// original block for the control-plane consult.
fn strip_aci_constraint(mut parsed: Value) -> (Value, bool) {
    let Some(provider) = parsed.get_mut("provider").and_then(Value::as_object_mut) else {
        return (parsed, false);
    };
    let changed = provider.remove(PROVIDER_ACI_VERIFIED).is_some()
        | provider.remove(PROVIDER_ACI_SESSION_IDS).is_some();
    if changed && provider.is_empty() {
        let _ = parsed
            .as_object_mut()
            .and_then(|root| root.remove("provider"));
    }
    (parsed, changed)
}

/// The 413 an oversize inference body earns: the surface's error envelope plus
/// a `request_outcome` line carrying the request id, in place of the
/// extractor's bare connection reset. Only the Anthropic envelope shape has a
/// `request_id` field; the OpenAI envelope deliberately matches the upstream
/// wire shape and carries none — there the id lives on the log line.
fn body_too_large_response(
    surface: crate::middleware::errors::Surface,
    request_id: &str,
) -> Response {
    tracing::info!(
        target: "request_outcome",
        request_id = %request_id,
        model = "",
        route = "",
        attempt = 0u32,
        upstream_status = 0u16,
        status = 413u16,
        outcome = "Generated",
        phase = "body_too_large",
        "request body exceeds the inference-surface limit"
    );
    let body = crate::middleware::errors::envelope_bytes(
        surface,
        crate::middleware::errors::error_type(surface, 413),
        "request body exceeds the 32 MiB limit",
        Some(request_id),
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (StatusCode::PAYLOAD_TOO_LARGE, headers, body).into_response()
}

pub(super) async fn openai_completion_endpoint(
    state: AppState,
    headers: HeaderMap,
    body: Body,
    endpoint_path: &'static str,
    force_buffered: bool,
) -> Response {
    // The request id and surface exist before the body is read so an oversize
    // body is refused with a proper envelope and a `request_outcome` line,
    // rather than the extractor-level 413 (an unread upload hyper turns into a
    // connection reset that never reaches request logging).
    let request_id = generate_request_id();
    let surface = if endpoint_path == MESSAGES_PATH {
        Surface::Anthropic
    } else {
        Surface::Openai
    };
    // Reject on a declared length before reading anything; this is the common
    // case (a client that sets content-length) and avoids buffering the body.
    if let Some(declared) =
        header_str(&headers, "content-length").and_then(|value| value.parse::<usize>().ok())
    {
        if declared > super::MAX_REQUEST_BODY_BYTES {
            return body_too_large_response(surface, &request_id);
        }
    }
    let body = match axum::body::to_bytes(body, super::MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            // `to_bytes` fails either because the body ran past the limit or
            // because the transport broke mid-read. Only the former is a 413;
            // the response is written without draining the rest of the upload,
            // so a client still sending may still see a reset after the 413
            // headers — the content-length check above catches the usual case.
            // The limit error can sit at any depth of the wrapper chain, so
            // walk the whole chain rather than trusting one `source()` level.
            let mut source = std::error::Error::source(&err);
            while let Some(current) = source {
                if current.is::<http_body_util::LengthLimitError>() {
                    return body_too_large_response(surface, &request_id);
                }
                source = current.source();
            }
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "could not read request body",
            );
        }
    };
    let has_e2ee = has_e2ee_headers(&headers);
    // `supported_e2ee_versions` advertises client-facing E2EE extensions (ACI
    // §6). The inherited
    // dstack-vllm-proxy path predates it and is identified by
    // `x-signing-algo`, so an explicit ACI opt-out does not disable that path.
    // A missing version is left to the v2 parser so it returns the precise
    // `e2ee_header_missing` error.
    if has_e2ee
        && headers.get("x-signing-algo").is_none()
        && header_str(&headers, "x-e2ee-version").is_some_and(|requested| {
            !state
                .service
                .supported_e2ee_versions()
                .iter()
                .any(|advertised| advertised == requested)
        })
    {
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
    let (parsed, normalized) = strip_empty_tool_calls(parsed);

    if headers.contains_key("x-upstream-verification") {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "X-Upstream-Verification is no longer supported; use provider.aci_verified",
        );
    }

    let aci = match aci_constraint(&parsed) {
        Ok(constraint) => constraint,
        Err(message) => {
            return error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
        }
    };

    let requester = extract_bearer(&headers)
        .as_deref()
        .map(ReceiptOwner::from_bearer);
    let context = GatewayRequestContext {
        request_id: request_id.clone(),
        // The receipt `model` is the model the client requested: under E2EE
        // the clear top-level `model` bound into E2EE v2 §6 AAD, otherwise the body's.
        user_model: e2ee
            .as_ref()
            .map(|ctx| ctx.request_model().to_string())
            .or_else(|| {
                parsed
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        target_route_id: None,
        // Populated from the x-user-tier header on the internal-forward path.
        user_tier: None,
    };

    let stream = !force_buffered
        && parsed
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if let Some(middleware) = state.middleware.clone() {
        let endpoint = match endpoint_path {
            COMPLETIONS_PATH => Endpoint::Complete,
            EMBEDDINGS_PATH => Endpoint::Embed,
            MESSAGES_PATH => Endpoint::Messages,
            RESPONSES_PATH => Endpoint::CreateModelResponse,
            _ => Endpoint::ChatComplete,
        };
        let api_key_hash = extract_bearer(&headers).as_deref().map(hash_api_key);
        // TEE-only host: force attested serving. The `tee_only` flag is carried
        // to the control plane so a non-TEE model is a 404 before any forward,
        // while `aci_required` makes verification fail closed at serve time.
        // Client `provider.aci_verified:false` is ignored.
        let tee_only = match request_host_domain(&headers).as_deref() {
            Some(host) => middleware.is_tee_only_domain(host),
            // §1.2 fail closed: with TEE-only hosts configured, a request
            // whose host cannot be resolved is treated as TEE-only rather
            // than unrestricted. The component in front must forward the
            // original `Host` (see the deployment guide).
            None => middleware.has_tee_only_domains(),
        };
        let aci_required = aci.required || tee_only;
        let input = CompletionInput {
            endpoint,
            endpoint_path,
            surface,
            params: parsed,
            received_body: service_body,
            api_key_hash,
            requester,
            e2ee,
            aci_required,
            aci_session_ids: aci.session_ids,
            request_id: context.request_id,
            user_model: context.user_model,
            stream,
            tee_only,
        };
        return middleware.handle_completion(&state.service, input).await;
    }

    let (direct_params, aci_stripped) = strip_aci_constraint(parsed);
    let forwarded_body = if normalized || aci_stripped {
        match serde_json::to_vec(&direct_params) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize normalized request");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "failed to serialize normalized request",
                );
            }
        }
    } else {
        None
    };

    forward_to_backend(
        state.service,
        BackendForwardInput {
            context,
            endpoint_path,
            received_body: service_body,
            forwarded_body,
            aci_required: aci.required,
            aci_session_ids: aci.session_ids,
            requester,
            e2ee,
            stream,
        },
    )
    .await
}
pub(super) async fn completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    openai_completion_endpoint(state, headers, body, COMPLETIONS_PATH, false).await
}

/// Canonical ACI receipt — the §7.2 receipt document. `id` accepts the
/// gateway `receipt_id` (preferred; on the `x-receipt-id` header) or the
/// upstream `chat_id`.
pub(super) async fn aci_receipt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(receipt) = state
        .service
        .get_receipt_by_receipt_id(&id)
        .or_else(|| state.service.get_receipt_by_chat_id(&id))
    else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "receipt id (receipt_id or chat_id) not found or expired",
        );
    };
    if let Some(resp) = enforce_owner(&state, &headers, &receipt.receipt_id) {
        return resp;
    }
    // The §7.2 receipt document, served as its stored bytes (any encoding
    // of the same document verifies; ours is the JCS form).
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        receipt.document.clone(),
    )
        .into_response()
}

/// List the attested TEE channels (one per upstream endpoint), optionally
/// filtered by `?upstream_name=` (the operator's upstream config name) and/or
/// `?model=`.
///
/// Sessions are per-TEE-channel, not per-model, so a `?model=` filter is
/// resolved to the upstream(s) that serve that model (via the upstream config)
/// and then matched on `upstream_name`.
///
/// Intentionally unauthenticated (like [`attested_session`]): a session record
/// is a transparency artifact carrying only verification material — upstream
/// name, endpoint, the verified identity (e.g. signing address), channel bindings,
/// claims, and an evidence digest. It holds no request or response content. The
/// list response carries only the evidence **digest**, not the full evidence
/// `data` bundle: fetch a single session by id (`/v1/aci/sessions/{session_id}`) for the
/// bytes. This keeps any larger/raw evidence payload off the broad listing.
pub(super) async fn aci_list_sessions(
    State(state): State<AppState>,
    Query(q): Query<SessionListQuery>,
) -> Response {
    let sessions = match q.model.as_deref() {
        // Resolve the model to the upstream(s) serving it, then list each
        // channel's sessions (honoring an upstream_name filter if both are given).
        Some(model) => {
            let names = state
                .upstream_config
                .as_ref()
                .map(|c| c.upstream_names_for_model(model))
                .unwrap_or_default();
            let mut merged = names
                .iter()
                .filter(|n| q.upstream_name.as_deref().is_none_or(|p| p == n.as_str()))
                .flat_map(|n| state.service.list_attested_sessions(Some(n)))
                .collect::<Vec<_>>();
            // Each per-upstream list is already sorted, but the fan-out just
            // concatenates them — re-sort the merge so it matches the ordering of
            // the single-channel path.
            sort_sessions_newest_first(&mut merged);
            merged
        }
        None => state
            .service
            .list_attested_sessions(q.upstream_name.as_deref()),
    };
    // List entries add a `session_id` member for lookup and keep the digest as
    // the integrity anchor while dropping the raw evidence `data` (§8.1). Only
    // the full record's served bytes hash to the session id.
    let sessions: Vec<Value> = sessions
        .iter()
        .map(|s| {
            // The sealed bytes parsed when the store adopted them, so this
            // cannot fail; parse them rather than re-serializing the struct.
            let mut value: Value =
                serde_json::from_slice(s.bytes()).expect("sealed session bytes are valid JSON");
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "session_id".to_string(),
                    Value::String(s.session_id().to_string()),
                );
                if let Some(evidence) = obj.get_mut("evidence").and_then(Value::as_object_mut) {
                    evidence.remove("data");
                }
            }
            value
        })
        .collect();
    Json(json!({
        "api_version": "aci/1",
        "sessions": sessions,
    }))
    .into_response()
}

pub(super) async fn receipt_by_chat_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<SignatureQuery>,
) -> Response {
    let Some(receipt) = state
        .service
        .get_receipt_by_chat_id(&id)
        .or_else(|| state.service.get_receipt_by_receipt_id(&id))
    else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "receipt id (chat_id or receipt_id) not found or expired",
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
            "receipt": receipt.document_json().unwrap_or(serde_json::Value::Null),
        }))
        .into_response(),
        Err(ServiceError::Key(KeyError::UnsupportedAlgo(_))) => invalid_signing_algo_response(),
        Err(other) => internal_error_response(other),
    }
}

/// Serve one attested session as its **exact sealed bytes** (§8): the client
/// recomputes `sha256:` over the body and compares it to the id a receipt
/// cited. `{session_id}` is the id exactly as receipts cite it
/// (`bare 64-hex`), so the value from a receipt pastes straight in.
pub(super) async fn attested_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    let Some(session) = state.service.get_attested_session(&session_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "attested session not found or expired",
        );
    };
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        session.bytes().to_vec(),
    )
        .into_response()
}
