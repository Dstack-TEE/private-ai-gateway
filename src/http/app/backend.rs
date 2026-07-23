//! Direct-to-backend forwarding path plus upstream response shaping.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use rand::RngCore;
use serde_json::Value;

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{
    AciService, ChatCompletionRequest, E2eeRequestContext, E2eeResponseInfo, GatewayRequestContext,
    ReceiptOwner, ServiceError, StreamingForwardResult,
};
use crate::aggregator::upstream_config::{AttestationUpstreamTarget, UpstreamProvider};

use super::error_responses::{
    e2ee_error_response, error_response, insert_str_header, internal_error_response,
    upstream_verification_error_response,
};

pub(super) struct BackendForwardInput {
    pub(super) context: GatewayRequestContext,
    pub(super) endpoint_path: &'static str,
    pub(super) received_body: Vec<u8>,
    pub(super) forwarded_body: Option<Vec<u8>>,
    pub(super) aci_required: bool,
    pub(super) aci_session_ids: Vec<String>,
    pub(super) requester: Option<ReceiptOwner>,
    pub(super) e2ee: Option<E2eeRequestContext>,
    pub(super) stream: bool,
}

pub(super) async fn forward_to_backend(
    service: Arc<AciService>,
    input: BackendForwardInput,
) -> Response {
    if input.stream {
        let request_id = input.context.request_id.clone();
        let result = service
            .forward_chat_completion_stream_request(ChatCompletionRequest {
                context: input.context,
                endpoint_path: input.endpoint_path,
                received_body: &input.received_body,
                forwarded_body: input.forwarded_body,
                aci_required: input.aci_required,
                aci_session_ids: input.aci_session_ids,
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
                // The finalizer already absorbs a failure of the stream it wraps
                // and tells the client where it safely can. What is left here
                // is the finalizer's own failure — receipt signing, E2EE
                // teardown — after headers have gone out: a body `Err` would
                // make hyper abort the connection with a TCP RST, so it is
                // logged and the body ended instead.
                let body = Body::from_stream(forward.body.scan((), move |_, chunk| {
                    std::future::ready(match chunk {
                        Ok(bytes) => Some(Ok::<_, std::io::Error>(bytes)),
                        Err(err) => {
                            tracing::warn!(
                                target: "stream_abort",
                                request_id = %request_id,
                                error = %err,
                                "response finalization failed; ending the body instead of aborting the connection"
                            );
                            None
                        }
                    })
                }));
                (status, resp_headers, body).into_response()
            }
            Ok(StreamingForwardResult::UpstreamError(forward)) => {
                // The streaming error body is cleartext (E2EE applies to the stream,
                // not this pre-stream error), so classify and remap it here directly.
                match image_input_error_response(
                    input.endpoint_path,
                    &input.received_body,
                    forward.upstream_status,
                    &forward.upstream_body,
                ) {
                    Some(resp) => resp,
                    None => {
                        let status =
                            StatusCode::from_u16(forward.upstream_status).unwrap_or(StatusCode::OK);
                        let resp_headers =
                            upstream_direct_response_headers(&forward.upstream_headers);
                        (status, resp_headers, forward.upstream_body).into_response()
                    }
                }
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
            aci_required: input.aci_required,
            aci_session_ids: input.aci_session_ids,
            upstream_verification_event: None,
            requester: input.requester,
            e2ee: input.e2ee,
        })
        .await;
    match result {
        Ok(forward) => {
            // The service already remapped a client image-URL fetch failure to a
            // surface-correct 400 (with a matching receipt + E2EE wire body), so the
            // buffered response is returned uniformly here.
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

/// Remaining aci-service chaining hops, passed between aggregators so a config
/// cycle can't recurse until the host runs out of resources.
pub(super) const ACI_FORWARD_DEPTH_HEADER: &str = "x-aci-forward-depth";

/// Hop budget for a request without a depth header (the top of a chain).
pub(super) const MAX_ACI_FORWARD_HOPS: u32 = 8;

/// Inbound hop budget, clamped so a caller can only lower it, not raise it.
pub(super) fn inbound_aci_forward_depth(headers: &HeaderMap) -> u32 {
    headers
        .get(ACI_FORWARD_DEPTH_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.min(MAX_ACI_FORWARD_HOPS))
        .unwrap_or(MAX_ACI_FORWARD_HOPS)
}

/// Fetch the upstream node's real `nvidia_payload` GPU evidence for `model`,
/// bound to `nonce` (this gateway is CPU-only and can't produce it). `None` on
/// any failure or for providers that don't expose it; `forward_depth` is the
/// remaining chaining budget.
pub(super) async fn fetch_upstream_nvidia_payload(
    target: &AttestationUpstreamTarget,
    nonce: &str,
    forward_depth: u32,
) -> Option<Value> {
    // `next_depth`: budget for the next hop, `None` for terminal providers.
    let (query, next_depth) = match target.provider {
        UpstreamProvider::PhalaDirect => {
            (format!("signing_algo=ecdsa&nonce={nonce}&version=2"), None)
        }
        UpstreamProvider::NearAi => (
            format!(
                "signing_algo=ecdsa&nonce={nonce}&include_tls_fingerprint=true&model={}",
                target.upstream_model_id
            ),
            None,
        ),
        // Upstream is another aggregator: fetch its legacy report for this
        // model+nonce and pass the GPU evidence through verbatim; `forward_depth`
        // caps the recursion down the chain.
        UpstreamProvider::AciService => {
            if forward_depth == 0 {
                tracing::warn!(
                    upstream = %target.upstream_name,
                    "aci-service attestation-forward depth exhausted; omitting GPU evidence"
                );
                return None;
            }
            (
                format!("model={}&nonce={nonce}", target.upstream_model_id),
                Some(forward_depth - 1),
            )
        }
        // Chutes / Tinfoil expose GPU evidence through other mechanisms.
        _ => return None,
    };
    fetch_report_nvidia_payload(target, &query, next_depth).await
}

/// GET `{base_url}/v1/attestation/report?{query}` and return its top-level
/// `nvidia_payload` verbatim. `None` on any error; `forward_depth` propagates as
/// the hop-budget header.
async fn fetch_report_nvidia_payload(
    target: &AttestationUpstreamTarget,
    query: &str,
    forward_depth: Option<u32>,
) -> Option<Value> {
    let url = format!("{}/v1/attestation/report?{query}", target.base_url);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(target.connect_timeout_seconds))
        .read_timeout(std::time::Duration::from_secs(target.read_timeout_seconds))
        .build()
        .map_err(|e| tracing::warn!(upstream = %target.upstream_name, error = %e, "build attestation client"))
        .ok()?;
    let mut req = client.get(&url).header("accept", "application/json");
    if let Some(depth) = forward_depth {
        req = req.header(ACI_FORWARD_DEPTH_HEADER, depth.to_string());
    }
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

/// The surface-appropriate 400 when the upstream error is a client image-URL
/// fetch failure; `None` for any other error (caller keeps verbatim passthrough).
fn image_input_error_response(
    endpoint_path: &str,
    received_body: &[u8],
    upstream_status: u16,
    upstream_body: &[u8],
) -> Option<Response> {
    use crate::middleware::errors::{image_input_error_parts, parts_response, surface_for_path};
    let (status, body) = image_input_error_parts(
        surface_for_path(endpoint_path),
        received_body,
        upstream_status,
        upstream_body,
        None,
    )?;
    Some(parts_response(status, body))
}

pub(super) fn upstream_proxy_error_response(err: crate::aci::upstream::UpstreamError) -> Response {
    tracing::warn!(error = %err, "upstream proxy request failed");
    error_response(StatusCode::BAD_GATEWAY, "upstream_error", err.to_string())
}

pub(super) fn routing_error_response(message: String) -> Response {
    error_response(StatusCode::BAD_REQUEST, "model_routing_error", message)
}
