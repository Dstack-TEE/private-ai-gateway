//! Completion forwarding.
//!
//! Runs the completion flow: consult the control plane, shape one
//! body per candidate, call `AciService::forward_chat_completion_for_middleware`
//! directly, consume the typed result, transform the buffered or streaming
//! response, inject cost, post the usage report, and finalize through the
//! existing receipt/E2EE finalizers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::{Body, Bytes},
    http::{header::CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::Value;

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{
    AciService, ChatCompletionRequest, E2eeRequestContext, E2eeResponseInfo, ForwardCandidate,
    GatewayRequestContext, MiddlewareForwardResult, MiddlewareReceiptJournal, ReceiptOwner,
    ServiceError, ServiceResponseStream,
};

use super::control::ControlClient;
use super::errors::{self, Surface};
use super::request_transform::{build_candidates, Endpoint};
use super::sse::{
    EarlyCommitFuture, EarlyCommitStream, KeepAliveStream, MeterStream, StreamReport,
};
use super::stream_transform::SseTransformStream;
use super::types::{ErrorSource, PostReport, ProviderFormat, RouteCandidate, SpendMode};
use super::{pricing, response_transform, stream_transform};

/// Everything the completion path needs, computed by the HTTP handler after E2EE
/// termination and JSON normalization.
pub struct CompletionInput {
    pub endpoint: Endpoint,
    pub endpoint_path: &'static str,
    pub surface: Surface,
    /// Normalized request body used for routing + transforms.
    pub params: Value,
    /// Exact cleartext bytes the service observed (recorded into the receipt).
    pub received_body: Vec<u8>,
    /// SHA-256 hex of the bearer key, for the pre-consult.
    pub api_key_hash: Option<String>,
    pub requester: Option<ReceiptOwner>,
    pub e2ee: Option<E2eeRequestContext>,
    pub upstream_required: bool,
    pub request_id: String,
    pub user_model: Option<String>,
    pub stream: bool,
}

/// Streaming knobs from `MiddlewareConfig`; `0` disables, unset takes the
/// documented default.
#[derive(Clone, Copy)]
pub struct StreamTimings {
    pub sse_keepalive_ms: Option<u64>,
    pub stream_commit_grace_ms: Option<u64>,
    pub stream_first_byte_timeout_ms: Option<u64>,
}

const DEFAULT_KEEPALIVE_MS: u64 = 10_000;
const DEFAULT_COMMIT_GRACE_MS: u64 = 2_000;
// Off by default: the pre-commit first-byte peek makes the forward resolve at
// the first body byte instead of at the response headers, which would push
// every healthy-but-slow-first-token stream on a multi-candidate route into
// the early-commit path. Deployments enable it deliberately for routes whose
// candidates are known to stall.
const DEFAULT_FIRST_BYTE_TIMEOUT_MS: u64 = 0;

fn resolve_ms(configured: Option<u64>, default_ms: u64) -> Option<Duration> {
    match configured.unwrap_or(default_ms) {
        0 => None,
        ms => Some(Duration::from_millis(ms)),
    }
}

/// Run the completion flow and produce the client response.
pub async fn run(
    control: &ControlClient,
    service: Arc<AciService>,
    timings: StreamTimings,
    input: CompletionInput,
) -> Response {
    let CompletionInput {
        endpoint,
        endpoint_path,
        surface,
        params,
        received_body,
        api_key_hash,
        requester,
        e2ee,
        upstream_required,
        request_id,
        user_model,
        stream,
    } = input;

    let started = Instant::now();
    let model = params.get("model").and_then(Value::as_str);
    // Forward the routing block verbatim; the control plane validates it. Parsing
    // it here would silently drop a caller's restrictions on a malformed field.
    let provider = params.get("provider");

    let consult = control
        .consult_pre(model, api_key_hash.as_deref(), provider)
        .await;

    let meter = Meter {
        control: control.clone(),
        request_id: request_id.clone(),
        endpoint_path,
        request_model: model.unwrap_or("").to_string(),
        pricing: consult.pricing.clone(),
        spend_mode: consult.spend_mode,
        user_id: consult.user_id,
        virtual_key_id: consult.virtual_key_id,
        started,
    };

    // Denial (also the fail-closed control-unavailable path: allow=false, 503).
    if !consult.allow {
        let status = consult.status.unwrap_or(403);
        let message = consult.message.as_deref().unwrap_or("forbidden");
        // Only our-infra 5xx denials are recorded as a gateway failure; user
        // denials (401/402/404/429) are attributable to the caller.
        if status >= 500 {
            meter.gateway_failure(status, ErrorSource::Control, message, stream);
        }
        if status == 429 {
            if let Some(rate_limit) = &consult.rate_limit {
                let body = errors::rate_limit_envelope_bytes(surface, message, Some(&request_id));
                let extra = errors::rate_limit_headers(rate_limit.limit, rate_limit.reset_at);
                return finalize_generated(
                    surface,
                    &service,
                    endpoint_path,
                    429,
                    body,
                    &extra,
                    e2ee,
                );
            }
        }
        let body = errors::envelope_bytes(
            surface,
            errors::error_type(surface, status),
            message,
            Some(&request_id),
        );
        return finalize_generated(surface, &service, endpoint_path, status, body, &[], e2ee);
    }

    let candidates = consult.candidates.clone().unwrap_or_default();
    if candidates.is_empty() {
        let message = format!("no route available for model {}", model.unwrap_or("(none)"));
        let body = errors::envelope_bytes(surface, "model_not_found", &message, Some(&request_id));
        return finalize_generated(surface, &service, endpoint_path, 400, body, &[], e2ee);
    }

    // Shape one body per candidate (typed per-route contract).
    let shaped = match build_candidates(&params, endpoint, &candidates) {
        Ok(shaped) => shaped,
        Err(err) => {
            let message = format!("failed to shape provider request: {err}");
            meter.gateway_failure(500, ErrorSource::Gateway, &message, stream);
            let body = errors::envelope_bytes(
                surface,
                errors::error_type(surface, 500),
                &message,
                Some(&request_id),
            );
            return finalize_generated(surface, &service, endpoint_path, 500, body, &[], e2ee);
        }
    };
    let forward_candidates: Vec<ForwardCandidate> = shaped
        .into_iter()
        .map(|(route_id, body)| ForwardCandidate {
            route_id,
            body: serde_json::to_vec(&body).unwrap_or_default(),
        })
        .collect();

    let context = GatewayRequestContext {
        request_id,
        user_model,
        target_route_id: None,
        user_tier: consult.user_tier.clone(),
    };

    if stream {
        return run_streaming(StreamingRun {
            service,
            meter,
            timings,
            surface,
            endpoint,
            endpoint_path,
            context,
            received_body,
            candidates,
            forward_candidates,
            upstream_required,
            requester,
            e2ee,
        })
        .await;
    }

    // Buffered forward. The journal is only consumed by the streaming
    // finalizer; the buffered result carries its draft inline.
    let journal = MiddlewareReceiptJournal::default();
    let request_id = context.request_id.clone();
    let result = service
        .forward_chat_completion_for_middleware(
            ChatCompletionRequest {
                context,
                endpoint_path,
                received_body: &received_body,
                forwarded_body: None,
                upstream_required: Some(upstream_required),
                upstream_verification_event: None,
                requester: requester.clone(),
                e2ee: e2ee.clone(),
                first_byte_deadline: None,
            },
            forward_candidates,
            false,
            journal.clone(),
        )
        .await;

    match result {
        Ok(MiddlewareForwardResult::Forwarded(forward)) => {
            let upstream_status = forward.upstream_status;
            let attempt_index = candidates
                .iter()
                .position(|c| c.route_id == forward.selected_route)
                .unwrap_or(0) as u32;
            let selected_format = candidates
                .iter()
                .find(|c| c.route_id == forward.selected_route)
                .or_else(|| candidates.first())
                .map(|c| c.format)
                .unwrap_or(ProviderFormat::Openai);

            // The buffered forward commits the candidate even on non-2xx; a
            // non-2xx body is normalized rather than transformed, but the receipt
            // is finalized either way.
            let (client_status, final_body) = if (200..300).contains(&upstream_status) {
                let upstream_json: Value = match serde_json::from_slice(&forward.upstream_body) {
                    Ok(value) => value,
                    Err(_) => {
                        // A malformed 2xx body must not be coerced into a fabricated
                        // success. Attribute it to the upstream (it sent an
                        // unparseable success body) and return 502.
                        let message = "upstream returned a malformed success body";
                        meter.gateway_failure(502, ErrorSource::Upstream, message, false);
                        let body = errors::envelope_bytes(
                            surface,
                            errors::error_type(surface, 502),
                            message,
                            Some(&request_id),
                        );
                        return finalize_generated(
                            surface,
                            &service,
                            endpoint_path,
                            502,
                            body,
                            &[],
                            e2ee,
                        );
                    }
                };
                let mut transformed = response_transform::transform_response(
                    selected_format,
                    endpoint,
                    upstream_json,
                );

                // Raw usage (pre-cost) goes to the report; cost is injected only
                // into the client body's top-level usage.
                let raw_usage = transformed.get("usage").cloned();
                meter.success(
                    upstream_status,
                    attempt_index,
                    Some(&forward.selected_route),
                    raw_usage,
                );
                meter.failed_attempts(&forward.failed_attempts, false);

                if let Some(pricing_config) = consult.pricing.as_ref().filter(|p| !p.is_null()) {
                    if let Some(usage) = transformed.get("usage").cloned() {
                        let cost = pricing::compute_cost(&usage, pricing_config);
                        if let Some(usage_obj) =
                            transformed.get_mut("usage").and_then(Value::as_object_mut)
                        {
                            usage_obj.insert("cost".to_string(), pricing::cost_to_json(cost));
                        }
                    }
                }
                (
                    upstream_status,
                    serde_json::to_vec(&transformed).unwrap_or_default(),
                )
            } else {
                let (mapped, body) = errors::normalize_upstream_error_parts(
                    surface,
                    upstream_status,
                    &forward.upstream_body,
                    &received_body,
                    Some(&request_id),
                );
                meter.success(
                    reported_status(mapped, upstream_status),
                    attempt_index,
                    Some(&forward.selected_route),
                    None,
                );
                meter.failed_attempts(&forward.failed_attempts, false);
                (mapped, body)
            };

            match service.finalize_middleware_receipt(
                forward.receipt,
                &final_body,
                Some("application/json"),
                requester,
                e2ee,
            ) {
                Ok(finalized) => {
                    let status =
                        StatusCode::from_u16(client_status).unwrap_or(StatusCode::BAD_GATEWAY);
                    let mut headers =
                        response_headers(&forward.upstream_headers, "application/json");
                    insert_header(&mut headers, "x-receipt-id", &finalized.receipt.receipt_id);
                    apply_e2ee_headers(&mut headers, finalized.e2ee.as_ref(), true);
                    (status, headers, finalized.wire_body).into_response()
                }
                // The receipt finalizer consumed the E2EE context, so a generated
                // error here is necessarily cleartext.
                Err(err) => {
                    service_error_response(surface, endpoint_path, &service, &request_id, err, None)
                }
            }
        }
        // The streaming variants are only produced for `stream == true`, which
        // `run_streaming` handles above.
        Ok(MiddlewareForwardResult::Stream(_)) | Ok(MiddlewareForwardResult::UpstreamError(_)) => {
            let message = "unexpected streaming result for a buffered request";
            meter.gateway_failure(500, ErrorSource::Gateway, message, false);
            let body = errors::envelope_bytes(
                surface,
                errors::error_type(surface, 500),
                message,
                Some(&request_id),
            );
            finalize_generated(surface, &service, endpoint_path, 500, body, &[], e2ee)
        }
        // All candidates failed. Record an upstream-attributed failure so the
        // request is visible to billing/health (per-attempt rows are not
        // recoverable from the typed error). Client-attributable errors (E2EE/4xx)
        // are not recorded. The E2EE context is still available to encrypt the body.
        Err(err) => {
            let status = forward_error_status(&err);
            if status >= 500 {
                meter.gateway_failure(status, ErrorSource::Upstream, &err.to_string(), false);
            }
            service_error_response(surface, endpoint_path, &service, &request_id, err, e2ee)
        }
    }
}

/// Everything the streaming path owns: on early commit the forward keeps
/// running inside the response body, past the handler borrow's lifetime.
struct StreamingRun {
    service: Arc<AciService>,
    meter: Meter,
    timings: StreamTimings,
    surface: Surface,
    endpoint: Endpoint,
    endpoint_path: &'static str,
    context: GatewayRequestContext,
    received_body: Vec<u8>,
    candidates: Vec<RouteCandidate>,
    forward_candidates: Vec<ForwardCandidate>,
    upstream_required: bool,
    requester: Option<ReceiptOwner>,
    e2ee: Option<E2eeRequestContext>,
}

enum DriveOutcome {
    /// A candidate committed: the metered stream (pre-finalizer), plus the
    /// upstream headers/content type for the full-fidelity response.
    Stream {
        body: ServiceResponseStream,
        upstream_headers: HashMap<String, String>,
        content_type: String,
    },
    /// No candidate served: the surface error envelope with its real status.
    Error { status: u16, body: Vec<u8> },
}

/// Streaming forward with early commit: within the grace window the upstream
/// outcome shapes the whole response (status fidelity, upstream headers pass
/// through). Once the grace expires, a `200 text/event-stream` is committed
/// and heartbeats flow while the forward keeps running behind the live
/// stream; a failure past that point is delivered as a terminal in-band error
/// event. Heartbeats and the error event pass through the receipt finalizer,
/// so `response.returned` still hashes exactly the client-visible bytes.
async fn run_streaming(run: StreamingRun) -> Response {
    let StreamingRun {
        service,
        meter,
        timings,
        surface,
        endpoint,
        endpoint_path,
        context,
        received_body,
        candidates,
        forward_candidates,
        upstream_required,
        requester,
        e2ee,
    } = run;
    let request_id = context.request_id.clone();
    let request_model = meter.request_model.clone();

    let keepalive = resolve_ms(timings.sse_keepalive_ms, DEFAULT_KEEPALIVE_MS);
    let first_byte_deadline = resolve_ms(
        timings.stream_first_byte_timeout_ms,
        DEFAULT_FIRST_BYTE_TIMEOUT_MS,
    );
    // E2EE responses keep the wait-for-upstream behavior: their headers carry
    // finalizer output, and a plaintext heartbeat would sit outside the
    // encrypted SSE framing.
    let grace = if e2ee.is_some() {
        None
    } else {
        resolve_ms(timings.stream_commit_grace_ms, DEFAULT_COMMIT_GRACE_MS)
    };

    // Pre-reserve the receipt id so `x-receipt-id` can be sent before a
    // candidate commits; the committing candidate adopts it. If no candidate
    // ever commits, no receipt is stored and the advertised id stays unused.
    let journal = MiddlewareReceiptJournal::default();
    let receipt_id = service.new_receipt_id();
    journal.reserve_receipt_id(receipt_id.clone());

    let mut drive = Box::pin(drive_streaming(DriveCtx {
        service: service.clone(),
        meter,
        journal: journal.clone(),
        context,
        endpoint,
        endpoint_path,
        surface,
        received_body,
        candidates,
        forward_candidates,
        upstream_required,
        requester: requester.clone(),
        e2ee: e2ee.clone(),
        keepalive,
        first_byte_deadline,
    }));

    let outcome = match grace {
        Some(grace) => tokio::time::timeout(grace, drive.as_mut()).await.ok(),
        None => Some(drive.as_mut().await),
    };

    match outcome {
        Some(DriveOutcome::Stream {
            body,
            upstream_headers,
            content_type,
        }) => {
            match service.finalize_middleware_response_stream(
                journal,
                body,
                endpoint_path,
                Some(&content_type),
                requester,
                e2ee,
            ) {
                Ok(finalized) => {
                    let mut headers = response_headers(&upstream_headers, &content_type);
                    insert_header(&mut headers, "x-receipt-id", &receipt_id);
                    apply_e2ee_headers(&mut headers, finalized.e2ee.as_ref(), true);
                    stream_response(headers, finalized.body)
                }
                Err(err) => {
                    service_error_response(surface, endpoint_path, &service, &request_id, err, None)
                }
            }
        }
        Some(DriveOutcome::Error { status, body }) => {
            finalize_generated(surface, &service, endpoint_path, status, body, &[], e2ee)
        }
        // Grace expired: commit now and heartbeat while the forward keeps
        // running. From here on the status is pinned at 200 and a total
        // failure becomes an in-band error event.
        None => {
            tracing::debug!(
                request_id = %request_id,
                grace_ms = grace.map(|g| g.as_millis() as u64),
                "streaming early commit: response committed before the upstream resolved"
            );
            let fallback_request_id = request_id.clone();
            let forward: EarlyCommitFuture = Box::pin(async move {
                match drive.await {
                    DriveOutcome::Stream { body, .. } => Ok(body),
                    DriveOutcome::Error { status, body } => {
                        let message = errors::extract_error_message(&body)
                            .unwrap_or_else(|| "upstream request failed".to_string());
                        Err(Bytes::from(errors::sse_error_event(
                            surface,
                            endpoint,
                            status,
                            &message,
                            &request_id,
                            &request_model,
                        )))
                    }
                }
            });
            // The outer KeepAliveStream heartbeats the waiting period; once the
            // forward resolves, the delegated stream's own inner keep-alive
            // takes over (an idle tick from either layer is a valid comment).
            let early: ServiceResponseStream = Box::pin(KeepAliveStream::new(
                Box::pin(EarlyCommitStream::new(forward)),
                keepalive,
            ));
            match service.finalize_middleware_response_stream(
                journal,
                early,
                endpoint_path,
                Some("text/event-stream"),
                requester,
                None,
            ) {
                Ok(finalized) => {
                    let mut headers = HeaderMap::new();
                    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                    insert_header(&mut headers, "x-receipt-id", &receipt_id);
                    apply_e2ee_headers(&mut headers, finalized.e2ee.as_ref(), true);
                    stream_response(headers, finalized.body)
                }
                Err(err) => service_error_response(
                    surface,
                    endpoint_path,
                    &service,
                    &fallback_request_id,
                    err,
                    None,
                ),
            }
        }
    }
}

// Assemble a committed streaming response: anti-buffering headers plus the
// finalized body. The status is always 200 by construction — the forward only
// commits a stream for an upstream 200 (non-2xx becomes `UpstreamError`), and
// an early commit is definitionally a 200. Callers with a real error status
// use `finalize_generated` instead.
fn stream_response(mut headers: HeaderMap, body: ServiceResponseStream) -> Response {
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    headers.insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-cache"),
    );
    let body = Body::from_stream(
        body.map(|chunk| chunk.map_err(|e| std::io::Error::other(e.to_string()))),
    );
    (StatusCode::OK, headers, body).into_response()
}

/// The forward + stream-assembly stage shared by both commit paths. Usage
/// metering happens here (it must run even when the caller has already
/// committed the response); presentation is left to the caller.
struct DriveCtx {
    service: Arc<AciService>,
    meter: Meter,
    journal: MiddlewareReceiptJournal,
    context: GatewayRequestContext,
    endpoint: Endpoint,
    endpoint_path: &'static str,
    surface: Surface,
    received_body: Vec<u8>,
    candidates: Vec<RouteCandidate>,
    forward_candidates: Vec<ForwardCandidate>,
    upstream_required: bool,
    requester: Option<ReceiptOwner>,
    e2ee: Option<E2eeRequestContext>,
    keepalive: Option<Duration>,
    first_byte_deadline: Option<Duration>,
}

async fn drive_streaming(ctx: DriveCtx) -> DriveOutcome {
    let DriveCtx {
        service,
        meter,
        journal,
        context,
        endpoint,
        endpoint_path,
        surface,
        received_body,
        candidates,
        forward_candidates,
        upstream_required,
        requester,
        e2ee,
        keepalive,
        first_byte_deadline,
    } = ctx;
    let request_id = context.request_id.clone();

    let result = service
        .forward_chat_completion_for_middleware(
            ChatCompletionRequest {
                context,
                endpoint_path,
                received_body: &received_body,
                forwarded_body: None,
                upstream_required: Some(upstream_required),
                upstream_verification_event: None,
                requester,
                e2ee,
                first_byte_deadline,
            },
            forward_candidates,
            true,
            journal,
        )
        .await;

    match result {
        Ok(MiddlewareForwardResult::Stream(forward)) => {
            let content_type = forward
                .upstream_headers
                .get("content-type")
                .cloned()
                .unwrap_or_else(|| "text/event-stream".to_string());
            let attempt_index = candidates
                .iter()
                .position(|c| c.route_id == forward.selected_route)
                .unwrap_or(0) as u32;
            meter.failed_attempts(&forward.failed_attempts, true);

            let report = StreamReport {
                control: meter.control.clone(),
                request_id: meter.request_id.clone(),
                endpoint: endpoint_path.to_string(),
                request_model: meter.request_model.clone(),
                pricing: meter.pricing.clone(),
                spend_mode: meter.spend_mode,
                user_id: meter.user_id,
                virtual_key_id: meter.virtual_key_id,
                selected_route_id: Some(forward.selected_route.clone()),
                attempt_index,
                upstream_status: forward.upstream_status,
                started: meter.started,
            };
            // Order: provider stream (drafts response.received) -> format
            // transform (if cross-format) -> keep-alive -> meter/cost ->
            // finalizer (hashes response.returned). Same-format streaming is
            // native passthrough (no transform).
            let selected_format = candidates
                .iter()
                .find(|c| c.route_id == forward.selected_route)
                .or_else(|| candidates.first())
                .map(|c| c.format)
                .unwrap_or(ProviderFormat::Openai);
            let transformed: ServiceResponseStream =
                match stream_transform::select_stream_transform(selected_format, endpoint) {
                    Some(transform) => Box::pin(SseTransformStream::new(forward.body, transform)),
                    None => forward.body,
                };
            let kept: ServiceResponseStream =
                Box::pin(KeepAliveStream::new(transformed, keepalive));
            let metered: ServiceResponseStream = Box::pin(MeterStream::new(kept, report));
            DriveOutcome::Stream {
                body: metered,
                upstream_headers: forward.upstream_headers,
                content_type,
            }
        }
        Ok(MiddlewareForwardResult::UpstreamError(forward)) => {
            // Streaming non-2xx: attribution is not carried for the terminal
            // status (recorded with no selected route), but the failed-over
            // attempts that preceded it are.
            meter.failed_attempts(&forward.failed_attempts, true);
            let (status, body) = errors::normalize_upstream_error_parts(
                surface,
                forward.upstream_status,
                &forward.upstream_body,
                &received_body,
                Some(&request_id),
            );
            meter.upstream_error(reported_status(status, forward.upstream_status));
            DriveOutcome::Error { status, body }
        }
        // The buffered variant is unreachable for `stream == true`; fail
        // defensively rather than panicking inside a live response body.
        Ok(MiddlewareForwardResult::Forwarded(_)) => {
            let message = "unexpected buffered result for a streaming request";
            meter.gateway_failure(500, ErrorSource::Gateway, message, true);
            DriveOutcome::Error {
                status: 500,
                body: errors::envelope_bytes(
                    surface,
                    errors::error_type(surface, 500),
                    message,
                    Some(&request_id),
                ),
            }
        }
        // All candidates failed. Record an upstream-attributed failure so the
        // request is visible to billing/health; client-attributable errors
        // (E2EE/4xx) are not recorded.
        Err(err) => {
            let status = forward_error_status(&err);
            if status >= 500 {
                meter.gateway_failure(status, ErrorSource::Upstream, &err.to_string(), true);
            }
            // `err` here is a forward-path error (upstream/verification); the
            // E2EE context is never the failure source on this path, so it is
            // safe for the caller to encrypt the error body with it.
            DriveOutcome::Error {
                status,
                body: errors::envelope_bytes(
                    surface,
                    errors::error_type(surface, status),
                    &err.to_string(),
                    Some(&request_id),
                ),
            }
        }
    }
}

// Posts usage reports to the control plane (fire-and-forget). Buffered reports
// have no TTFT and `is_streaming = false`; the status recorded is the raw upstream
// status, distinct from the client-facing mapped status. Owns its control-client
// clone so the streaming path can carry it into the response body.
struct Meter {
    control: ControlClient,
    request_id: String,
    endpoint_path: &'static str,
    request_model: String,
    pricing: Option<Value>,
    spend_mode: Option<SpendMode>,
    user_id: Option<i64>,
    virtual_key_id: Option<i64>,
    started: Instant,
}

impl Meter {
    fn base(&self) -> PostReport {
        PostReport {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint_path.to_string(),
            status: 0,
            duration_ms: self.started.elapsed().as_millis() as u64,
            ttft_ms: None,
            is_streaming: Some(false),
            attempt_index: Some(0),
            selected_route_id: None,
            request_model: self.request_model.clone(),
            usage: None,
            pricing: self.pricing.clone(),
            spend_mode: self.spend_mode,
            user_id: self.user_id,
            virtual_key_id: self.virtual_key_id,
            error_source: None,
            error_message: None,
        }
    }

    fn success(
        &self,
        status: u16,
        attempt_index: u32,
        selected_route_id: Option<&str>,
        usage: Option<Value>,
    ) {
        self.spawn(PostReport {
            status,
            attempt_index: Some(attempt_index),
            selected_route_id: selected_route_id.map(str::to_string),
            usage,
            ..self.base()
        });
    }

    fn upstream_error(&self, status: u16) {
        self.spawn(PostReport {
            status,
            is_streaming: Some(true),
            ..self.base()
        });
    }

    fn failed_attempts(&self, attempts: &[(String, u16)], is_streaming: bool) {
        for (index, (route_id, status)) in attempts.iter().enumerate() {
            if *status == 0 {
                continue;
            }
            self.spawn(PostReport {
                status: *status,
                duration_ms: 0,
                is_streaming: Some(is_streaming),
                attempt_index: Some(index as u32),
                selected_route_id: Some(route_id.clone()),
                ..self.base()
            });
        }
    }

    fn gateway_failure(&self, status: u16, source: ErrorSource, message: &str, is_streaming: bool) {
        self.spawn(PostReport {
            status,
            is_streaming: Some(is_streaming),
            error_source: Some(source),
            error_message: Some(truncate(message, 500)),
            ..self.base()
        });
    }

    fn spawn(&self, report: PostReport) {
        let control = self.control.clone();
        tokio::spawn(async move {
            control.consult_post(&report).await;
        });
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

// The status reported to the control plane for a normalized upstream error: the
// client-facing status when it is client-attributable (4xx) — a remapped
// image-fetch failure must not count against the provider's health — otherwise
// the raw upstream status, preserving the provider's real code in the logs.
fn reported_status(mapped: u16, upstream_status: u16) -> u16 {
    if (400..500).contains(&mapped) {
        mapped
    } else {
        upstream_status
    }
}

// Map a forward/finalize `ServiceError` to a client-facing generated response.
// E2EE clients still get an encrypted error body, except for `E2ee` errors
// themselves (the E2EE setup failed, so the response cannot be encrypted).
// Client-facing status for a forward/finalize `ServiceError`.
fn forward_error_status(err: &ServiceError) -> u16 {
    match err {
        ServiceError::E2ee(_) => 400,
        ServiceError::UpstreamVerification(_) => 503,
        ServiceError::Upstream(UpstreamError::Routing(_)) => 404,
        _ => 502,
    }
}

fn service_error_response(
    surface: Surface,
    endpoint_path: &str,
    service: &AciService,
    request_id: &str,
    err: ServiceError,
    e2ee: Option<E2eeRequestContext>,
) -> Response {
    let status = forward_error_status(&err);
    let e2ee = match &err {
        ServiceError::E2ee(_) => None,
        _ => e2ee,
    };
    let body = errors::envelope_bytes(
        surface,
        errors::error_type(surface, status),
        &err.to_string(),
        Some(request_id),
    );
    finalize_generated(surface, service, endpoint_path, status, body, &[], e2ee)
}

// Build a generated (no-receipt) response, E2EE-encrypting the body when a
// request context is present. If encryption fails it is fail-closed: a generic
// error is returned rather than the cleartext body.
fn finalize_generated(
    surface: Surface,
    service: &AciService,
    endpoint_path: &str,
    status: u16,
    body: Vec<u8>,
    extra_headers: &[(&'static str, String)],
    e2ee: Option<E2eeRequestContext>,
) -> Response {
    let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    for (name, value) in extra_headers {
        insert_header(&mut headers, name, value);
    }
    if e2ee.is_none() {
        return (status_code, headers, body).into_response();
    }
    match service.finalize_middleware_generated_response(
        endpoint_path,
        &body,
        Some("application/json"),
        e2ee,
    ) {
        Ok(finalized) => {
            apply_e2ee_headers(&mut headers, finalized.e2ee.as_ref(), false);
            (status_code, headers, finalized.wire_body).into_response()
        }
        // Fail-closed: never return the cleartext body when E2EE was requested.
        Err(err) => {
            tracing::error!(error = %err, "E2EE generated-response finalization failed");
            errors::error_response(
                surface,
                500,
                errors::error_type(surface, 500),
                "response finalization failed",
                None,
            )
        }
    }
}

// Build response headers from the upstream response, dropping gateway-owned and
// hop-by-hop headers, and forcing the content type. Provider auth/server headers
// are not forwarded.
fn response_headers(
    upstream_headers: &std::collections::HashMap<String, String>,
    content_type: &str,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in upstream_headers {
        // The body we emit is always identity-encoded (re-serialized JSON or a
        // transformed/passthrough SSE stream), so a relayed `content-encoding`
        // would mislabel it. `content-type` is set explicitly below.
        if is_gateway_owned(name)
            || is_hop_by_hop(name)
            || name.eq_ignore_ascii_case("content-type")
            || name.eq_ignore_ascii_case("content-encoding")
        {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(CONTENT_TYPE, value);
    }
    headers
}

fn is_gateway_owned(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "x-receipt-id"
        || lower.starts_with("x-e2ee-")
        || lower.starts_with("x-aci-")
        || lower.starts_with("x-private-ai-gateway-")
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    )
}

fn apply_e2ee_headers(
    headers: &mut HeaderMap,
    e2ee: Option<&E2eeResponseInfo>,
    include_plain_false: bool,
) {
    match e2ee {
        Some(info) => {
            headers.insert(
                HeaderName::from_static("x-e2ee-applied"),
                HeaderValue::from_static("true"),
            );
            insert_header(headers, "x-e2ee-version", &info.version);
            insert_header(headers, "x-e2ee-algo", &info.algo);
        }
        None if include_plain_false => {
            headers.insert(
                HeaderName::from_static("x-e2ee-applied"),
                HeaderValue::from_static("false"),
            );
        }
        None => {}
    }
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}
