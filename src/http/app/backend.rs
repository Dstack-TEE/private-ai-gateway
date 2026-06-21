//! Direct-to-backend forwarding path and the internal forwarder used by the
//! middleware split, plus upstream response shaping.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use rand::RngCore;
use serde_json::Value;

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{
    AciService, ChatCompletionRequest, E2eeRequestContext, E2eeResponseInfo, GatewayRequestContext,
    MiddlewareForwardResult, ReceiptOwner, ServiceError, StreamingForwardResult,
};
use crate::aggregator::upstream_config::{AttestationUpstreamTarget, UpstreamProvider};

use super::error_responses::{
    e2ee_error_response, error_response, insert_str_header, internal_error_response,
    upstream_verification_error_response,
};
use super::util::{build_forward_candidates, header_str, insert_attribution_headers};
use super::InternalBackendState;

pub(super) struct BackendForwardInput {
    pub(super) context: GatewayRequestContext,
    pub(super) endpoint_path: &'static str,
    pub(super) received_body: Vec<u8>,
    pub(super) forwarded_body: Option<Vec<u8>>,
    pub(super) upstream_required: bool,
    pub(super) requester: Option<ReceiptOwner>,
    pub(super) e2ee: Option<E2eeRequestContext>,
    pub(super) stream: bool,
}

pub(super) async fn forward_to_backend(
    service: Arc<AciService>,
    input: BackendForwardInput,
) -> Response {
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

pub(super) async fn internal_forward(
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
    let (candidates, stream) = match build_forward_candidates(&headers, &body, &parsed) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };

    let journal = stored.receipt_journal;
    let received_body = stored.received_body;
    let result = state
        .service
        .forward_chat_completion_for_middleware(
            ChatCompletionRequest {
                context: GatewayRequestContext {
                    request_id,
                    user_model: stored.user_model,
                    target_route_id: None,
                    user_tier: header_str(&headers, "x-user-tier").map(str::to_string),
                },
                endpoint_path: stored.endpoint_path,
                received_body: &received_body,
                forwarded_body: None,
                upstream_required: Some(stored.upstream_required),
                upstream_verification_event: None,
                requester: stored.requester,
                e2ee: stored.e2ee,
            },
            candidates,
            stream,
            journal.clone(),
        )
        .await;
    match result {
        Ok(MiddlewareForwardResult::Forwarded(forward)) => {
            let forward = *forward;
            journal.set(forward.receipt);
            let default_content_type = if stream {
                "text/event-stream"
            } else {
                "application/json"
            };
            let mut resp_headers = chat_response_headers(
                &forward.receipt_id,
                &forward.upstream_headers,
                default_content_type,
                None,
            );
            if stream {
                resp_headers.insert(
                    HeaderName::from_static("x-accel-buffering"),
                    HeaderValue::from_static("no"),
                );
                resp_headers.insert(
                    HeaderName::from_static("cache-control"),
                    HeaderValue::from_static("no-cache"),
                );
            }
            insert_attribution_headers(
                &mut resp_headers,
                &forward.selected_route,
                &forward.failed_attempts,
                forward.session_id.as_deref(),
            );
            let status = StatusCode::from_u16(forward.upstream_status).unwrap_or(StatusCode::OK);
            (status, resp_headers, forward.upstream_body).into_response()
        }
        Ok(MiddlewareForwardResult::Stream(forward)) => {
            let forward = *forward;
            let mut resp_headers = chat_response_headers(
                &forward.receipt_id,
                &forward.upstream_headers,
                "text/event-stream",
                None,
            );
            resp_headers.insert(
                HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            );
            resp_headers.insert(
                HeaderName::from_static("cache-control"),
                HeaderValue::from_static("no-cache"),
            );
            insert_attribution_headers(
                &mut resp_headers,
                &forward.selected_route,
                &forward.failed_attempts,
                forward.session_id.as_deref(),
            );
            let status = StatusCode::from_u16(forward.upstream_status).unwrap_or(StatusCode::OK);
            let body = Body::from_stream(
                forward
                    .body
                    .map(|chunk| chunk.map_err(|e| std::io::Error::other(e.to_string()))),
            );
            (status, resp_headers, body).into_response()
        }
        Ok(MiddlewareForwardResult::UpstreamError(forward)) => {
            let status = StatusCode::from_u16(forward.upstream_status).unwrap_or(StatusCode::OK);
            let resp_headers = upstream_direct_response_headers(&forward.upstream_headers);
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

pub(super) fn strip_empty_tool_calls(mut payload: Value) -> (Value, bool) {
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

pub(super) fn generate_request_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("req_{}", hex::encode(bytes))
}

pub(super) fn chat_response_headers(
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

pub(super) fn upstream_direct_response_headers(
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

/// Fetch the upstream model node's real `nvidia_payload` GPU evidence, bound to
/// the client's `nonce`, so a model-scoped attestation report can carry GPU
/// evidence the gateway (CPU-only) cannot produce itself. Dispatches per
/// provider; returns `None` on any failure or for providers that do not expose
/// `nvidia_payload` via `/v1/attestation/report` (caller falls back to an empty
/// placeholder).
pub(super) async fn fetch_upstream_nvidia_payload(
    target: &AttestationUpstreamTarget,
    nonce: &str,
) -> Option<Value> {
    let query = match target.provider {
        UpstreamProvider::PhalaDirect => format!("signing_algo=ecdsa&nonce={nonce}&version=2"),
        UpstreamProvider::NearAi => format!(
            "signing_algo=ecdsa&nonce={nonce}&include_tls_fingerprint=true&model={}",
            target.upstream_model_id
        ),
        // Chutes / Tinfoil expose GPU evidence through other mechanisms.
        _ => return None,
    };
    fetch_report_nvidia_payload(target, &query).await
}

/// GET `{base_url}/v1/attestation/report?{query}` and return its top-level
/// `nvidia_payload` field verbatim. `None` on any error.
async fn fetch_report_nvidia_payload(
    target: &AttestationUpstreamTarget,
    query: &str,
) -> Option<Value> {
    let url = format!("{}/v1/attestation/report?{query}", target.base_url);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(target.connect_timeout_seconds))
        .read_timeout(std::time::Duration::from_secs(target.read_timeout_seconds))
        .build()
        .map_err(|e| tracing::warn!(upstream = %target.upstream_name, error = %e, "build attestation client"))
        .ok()?;
    let mut req = client.get(&url).header("accept", "application/json");
    if let Some(token) = &target.bearer_token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| tracing::warn!(upstream = %target.upstream_name, error = %e, "fetch upstream nvidia_payload"))
        .ok()?;
    if !resp.status().is_success() {
        tracing::warn!(upstream = %target.upstream_name, status = %resp.status(), "upstream attestation report non-2xx");
        return None;
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| tracing::warn!(upstream = %target.upstream_name, error = %e, "parse upstream attestation report"))
        .ok()?;
    match target.provider {
        UpstreamProvider::NearAi => nearai_nvidia_payload(&body, target),
        _ => body.get("nvidia_payload").cloned(),
    }
}

/// near-ai nests GPU evidence under `model_attestations[]`, one entry per model.
/// Return the entry whose `model_name` matches the requested model; `None` (not
/// the first entry) when none matches, so a substituted model's evidence is never
/// attached.
fn nearai_nvidia_payload(body: &Value, target: &AttestationUpstreamTarget) -> Option<Value> {
    let entries = body.get("model_attestations").and_then(Value::as_array)?;
    let matched = entries.iter().find(|entry| {
        entry.get("model_name").and_then(Value::as_str) == Some(target.upstream_model_id.as_str())
    });
    match matched {
        Some(entry) => entry.get("nvidia_payload").cloned(),
        None => {
            tracing::warn!(
                upstream = %target.upstream_name,
                model = %target.upstream_model_id,
                "near-ai report has no model_attestations entry for the requested model"
            );
            None
        }
    }
}

pub(super) fn reqwest_response_headers(upstream_headers: &reqwest::header::HeaderMap) -> HeaderMap {
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

pub(super) fn upstream_direct_response(
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

pub(super) fn upstream_proxy_error_response(err: crate::aci::upstream::UpstreamError) -> Response {
    tracing::warn!(error = %err, "upstream proxy request failed");
    error_response(StatusCode::BAD_GATEWAY, "upstream_error", err.to_string())
}

pub(super) fn routing_error_response(message: String) -> Response {
    error_response(StatusCode::BAD_REQUEST, "model_routing_error", message)
}
