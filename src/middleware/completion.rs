//! Completion forwarding.
//!
//! Consults control, shapes candidates, forwards, transforms and meters the
//! response, then finalizes its receipt/E2EE envelope.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::Value;

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{
    AciService, ChatCompletionRequest, E2eeRequestContext, E2eeResponseInfo, ForwardCandidate,
    GatewayRequestContext, MiddlewareForwardResult, MiddlewareReceiptJournal, ReceiptOwner,
    ServiceError, ServiceResponseStream, UpstreamVerificationError,
};

use super::control::ControlClient;
use super::errors::{self, Surface};
use super::reasoning;
use super::request_transform::{build_candidates, Endpoint};
use super::sse::{KeepAliveStream, MeterStream, StreamReport};
use super::stream_transform::{SseTransformStream, StreamTransform};
use super::types::{ErrorSource, PostReport, ProviderFormat, SpendMode};
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
    /// Request is restricted to ACI-verified attested upstreams.
    pub aci_required: bool,
    /// Optional hard allowlist of attested session ids.
    pub aci_session_ids: Vec<String>,
    pub request_id: String,
    pub user_model: Option<String>,
    pub stream: bool,
    /// Makes control reject non-TEE models before forwarding.
    pub tee_only: bool,
}

/// Cap provider-controlled error detail in outcome logs.
const MAX_DETAIL_CHARS: usize = 240;

/// Log every terminal failure except 429, which the usage pipeline records.
pub(super) fn should_log_failure(status: u16) -> bool {
    status != 429
}

/// Standard cross-provider finish reasons; other values are logged as anomalies.
pub(super) const STANDARD_FINISH_REASONS: &[&str] = &[
    "stop",
    "length",
    "tool_calls",
    "function_call",
    "content_filter",
    "end_turn",
    "max_tokens",
    "stop_sequence",
    "tool_use",
    "pause_turn",
    "refusal",
    "model_context_window_exceeded",
];

pub(super) fn finish_reasons_anomalous<'a, I: IntoIterator<Item = &'a str>>(reasons: I) -> bool {
    reasons
        .into_iter()
        .any(|r| !STANDARD_FINISH_REASONS.contains(&r))
}

/// Per-reason log length cap.
const MAX_REASON_CHARS: usize = 32;

/// Bound and flatten client-controlled identifiers before logging.
pub(super) fn sanitize_identifier(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(128)
        .collect()
}

/// Bound and flatten provider-controlled finish reasons before logging.
pub(super) fn sanitize_reason(reason: &str) -> String {
    reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_REASON_CHARS)
        .collect()
}

/// Bound emitted reasons without changing anomaly detection.
pub(super) fn sanitized_reasons<'a, I: IntoIterator<Item = &'a str>>(reasons: I) -> String {
    reasons
        .into_iter()
        .take(8)
        .map(sanitize_reason)
        .collect::<Vec<_>>()
        .join(",")
}

/// Reveal potentially sensitive upstream detail only at debug level.
pub(super) fn debug_gated_detail(detail: &str) -> &str {
    if tracing::enabled!(target: "request_outcome", tracing::Level::DEBUG) {
        detail
    } else {
        ""
    }
}

/// Build a bounded, single-line, UTF-8-lossy error snippet.
pub(super) fn detail_snippet(raw: &[u8]) -> String {
    let capped = &raw[..raw.len().min(4 * MAX_DETAIL_CHARS)];
    String::from_utf8_lossy(capped)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_DETAIL_CHARS)
        .collect()
}

/// Identity needed to supersede an outcome with a late finalization failure.
#[derive(Clone, Copy)]
struct OutcomeCtx<'a> {
    surface: Surface,
    service: &'a AciService,
    endpoint_path: &'a str,
    request_id: &'a str,
    model: &'a str,
    started: Instant,
}

#[allow(clippy::too_many_arguments)]
fn log_generated_outcome(
    request_id: &str,
    model: &str,
    phase: &'static str,
    status: u16,
    upstream_status: u16,
    route: &str,
    attempt: u32,
    started: Instant,
    detail: &str,
) {
    tracing::info!(
        target: "request_outcome",
        request_id = %request_id,
        model = %sanitize_identifier(model),
        route = %route,
        attempt,
        upstream_status,
        status,
        outcome = "Generated",
        phase = %phase,
        duration_ms = started.elapsed().as_millis() as u64,
        detail = %debug_gated_detail(detail),
        "generated response"
    );
}

/// Run the completion flow and produce the client response.
pub async fn run(
    control: &ControlClient,
    service: &AciService,
    sse_keepalive_ms: Option<u64>,
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
        aci_required,
        aci_session_ids,
        request_id,
        user_model,
        stream,
        tee_only,
    } = input;

    let started = Instant::now();
    let (params, reasoning_requirements, exclude_reasoning) = if endpoint == Endpoint::ChatComplete
    {
        match reasoning::normalize_chat_request(&params) {
            Ok(normalized) => normalized,
            Err(err) => {
                let model = params.get("model").and_then(Value::as_str).unwrap_or("");
                let message = err.to_string();
                log_generated_outcome(
                    &request_id,
                    model,
                    "reasoning_validation",
                    400,
                    0,
                    "",
                    0,
                    started,
                    &detail_snippet(message.as_bytes()),
                );
                let body = errors::envelope_bytes(
                    surface,
                    errors::error_type(surface, 400),
                    &message,
                    Some(&request_id),
                );
                return finalize_generated(
                    400,
                    body,
                    &[],
                    e2ee,
                    OutcomeCtx {
                        surface,
                        service,
                        endpoint_path,
                        request_id: &request_id,
                        model,
                        started,
                    },
                );
            }
        }
    } else {
        (params, None, false)
    };
    let model = params.get("model").and_then(Value::as_str);
    let outcome_ctx = OutcomeCtx {
        surface,
        service,
        endpoint_path,
        request_id: &request_id,
        model: model.unwrap_or(""),
        started,
    };
    // Forward the routing block verbatim; the control plane validates it. Parsing
    // it here would silently drop a caller's restrictions on a malformed field.
    let provider = params.get("provider");

    let consult = control
        .consult_pre(
            model,
            api_key_hash.as_deref(),
            provider,
            reasoning_requirements.as_ref(),
            tee_only,
        )
        .await;

    let meter = Meter {
        control,
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
        if should_log_failure(status) {
            log_generated_outcome(
                &request_id,
                model.unwrap_or(""),
                "consult_deny",
                status,
                0,
                "",
                0,
                started,
                &detail_snippet(message.as_bytes()),
            );
        }
        // Record control failures without affecting upstream health.
        if status == 429 || status >= 500 {
            meter.gateway_failure(status, ErrorSource::Control, message, stream);
        }
        if status == 429 {
            if let Some(rate_limit) = &consult.rate_limit {
                let body = errors::rate_limit_envelope_bytes(surface, message, Some(&request_id));
                let extra = errors::rate_limit_headers(rate_limit.limit, rate_limit.reset_at);
                return finalize_generated(429, body, &extra, e2ee, outcome_ctx);
            }
        }
        let body = errors::envelope_bytes(
            surface,
            errors::error_type(surface, status),
            message,
            Some(&request_id),
        );
        return finalize_generated(status, body, &[], e2ee, outcome_ctx);
    }

    let candidates = consult.candidates.clone().unwrap_or_default();
    if candidates.is_empty() {
        let message = format!("no route available for model {}", model.unwrap_or("(none)"));
        if should_log_failure(400) {
            log_generated_outcome(
                &request_id,
                model.unwrap_or(""),
                "no_route",
                400,
                0,
                "",
                0,
                started,
                "",
            );
        }
        let body = errors::envelope_bytes(surface, "model_not_found", &message, Some(&request_id));
        return finalize_generated(400, body, &[], e2ee, outcome_ctx);
    }

    // Shape one body per candidate (typed per-route contract).
    let shaped = match build_candidates(
        &params,
        endpoint,
        &candidates,
        reasoning_requirements.is_some(),
    ) {
        Ok(shaped) => shaped,
        Err(err) => {
            let message = format!("failed to shape provider request: {err}");
            if should_log_failure(500) {
                log_generated_outcome(
                    &request_id,
                    model.unwrap_or(""),
                    "shape_error",
                    500,
                    0,
                    "",
                    0,
                    started,
                    &detail_snippet(message.as_bytes()),
                );
            }
            meter.gateway_failure(500, ErrorSource::Gateway, &message, stream);
            let body = errors::envelope_bytes(
                surface,
                errors::error_type(surface, 500),
                &message,
                Some(&request_id),
            );
            return finalize_generated(500, body, &[], e2ee, outcome_ctx);
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
        request_id: request_id.clone(),
        user_model,
        target_route_id: None,
        user_tier: consult.user_tier.clone(),
    };

    // The receipt-draft journal is only consumed by the streaming finalizer; the
    // buffered result carries its draft inline.
    let journal = MiddlewareReceiptJournal::default();
    let result = service
        .forward_chat_completion_for_middleware(
            ChatCompletionRequest {
                context,
                endpoint_path,
                received_body: &received_body,
                forwarded_body: None,
                aci_required,
                aci_session_ids,
                upstream_verification_event: None,
                requester: requester.clone(),
                e2ee: e2ee.clone(),
            },
            forward_candidates,
            stream,
            journal.clone(),
        )
        .await;

    match result {
        Ok(MiddlewareForwardResult::Forwarded(forward)) => {
            let upstream_status = forward.upstream_status;
            // Failed-attempt count is the serving candidate's stable index.
            let attempt_index = forward.failed_attempts.len() as u32;
            let selected_format = candidates
                .iter()
                .find(|c| c.route_id == forward.selected_route)
                .or_else(|| candidates.first())
                .map(|c| c.format)
                .unwrap_or(ProviderFormat::Openai);

            // Normalize non-2xx bodies; transform successful bodies.
            let (client_status, final_body) = if (200..300).contains(&upstream_status) {
                let upstream_json: Value = match serde_json::from_slice(&forward.upstream_body) {
                    Ok(value) => value,
                    Err(_) => {
                        // Never fabricate success from malformed upstream JSON.
                        let message = "upstream returned a malformed success body";
                        if should_log_failure(502) {
                            log_generated_outcome(
                                &request_id,
                                model.unwrap_or(""),
                                "malformed_body",
                                502,
                                upstream_status,
                                &forward.selected_route,
                                attempt_index,
                                started,
                                message,
                            );
                        }
                        meter.gateway_failure(502, ErrorSource::Upstream, message, false);
                        let body = errors::envelope_bytes(
                            surface,
                            errors::error_type(surface, 502),
                            message,
                            Some(&request_id),
                        );
                        return finalize_generated(502, body, &[], e2ee, outcome_ctx);
                    }
                };
                let mut transformed = response_transform::transform_response(
                    selected_format,
                    endpoint,
                    upstream_json,
                );
                if exclude_reasoning {
                    response_transform::exclude_reasoning(&mut transformed);
                }

                // Raw usage (pre-cost) goes to the report; cost is injected only
                // into the client body's top-level usage.
                let raw_usage = transformed.get("usage").cloned();
                // Surface provider errors smuggled through nonstandard reasons.
                let mut finish_reasons: Vec<&str> = transformed
                    .get("choices")
                    .and_then(Value::as_array)
                    .map(|choices| {
                        choices
                            .iter()
                            .filter_map(|c| c.get("finish_reason").and_then(Value::as_str))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(stop_reason) = transformed.get("stop_reason").and_then(Value::as_str) {
                    finish_reasons.push(stop_reason);
                }
                if finish_reasons_anomalous(finish_reasons.iter().copied()) {
                    let out_tokens = raw_usage.as_ref().and_then(|u| {
                        u.get("completion_tokens")
                            .or_else(|| u.get("output_tokens"))
                            .and_then(Value::as_u64)
                    });
                    tracing::info!(
                        target: "request_outcome",
                        request_id = %request_id,
                        model = %sanitize_identifier(model.unwrap_or("")),
                        route = %forward.selected_route,
                        attempt = attempt_index,
                        upstream_status,
                        status = upstream_status,
                        outcome = "Buffered",
                        anomalous_finish = true,
                        duration_ms = started.elapsed().as_millis() as u64,
                        out_tokens,
                        finish_reasons = %sanitized_reasons(finish_reasons.iter().copied()),
                        "buffered response with nonstandard finish reason"
                    );
                }
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
                if should_log_failure(mapped) {
                    log_generated_outcome(
                        &request_id,
                        model.unwrap_or(""),
                        "upstream_error_buffered",
                        mapped,
                        upstream_status,
                        &forward.selected_route,
                        attempt_index,
                        started,
                        &detail_snippet(&forward.upstream_body),
                    );
                }
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
                    // Record the final client-visible terminal.
                    let status = forward_error_status(&err);
                    if should_log_failure(status) {
                        log_generated_outcome(
                            &request_id,
                            model.unwrap_or(""),
                            "finalize_error",
                            status,
                            upstream_status,
                            &forward.selected_route,
                            attempt_index,
                            started,
                            &detail_snippet(err.to_string().as_bytes()),
                        );
                    }
                    service_error_response(outcome_ctx, err, None)
                }
            }
        }
        Ok(MiddlewareForwardResult::Stream(forward)) => {
            let content_type = forward
                .upstream_headers
                .get("content-type")
                .cloned()
                .unwrap_or_else(|| "text/event-stream".to_string());
            let upstream_status = forward.upstream_status;
            let attempt_index = forward.failed_attempts.len() as u32;
            meter.failed_attempts(&forward.failed_attempts, true);

            // Distinguish downstream finalization failure from disconnect.
            let downstream_abort = Arc::new(AtomicBool::new(false));
            let meter_settled = Arc::new(AtomicBool::new(false));
            let report = StreamReport {
                control: control.clone(),
                request_id: request_id.clone(),
                endpoint: endpoint_path.to_string(),
                request_model: model.unwrap_or("").to_string(),
                pricing: consult.pricing.clone(),
                spend_mode: consult.spend_mode,
                user_id: consult.user_id,
                virtual_key_id: consult.virtual_key_id,
                selected_route_id: Some(forward.selected_route.clone()),
                attempt_index,
                upstream_status,
                started,
                downstream_abort: downstream_abort.clone(),
                settled: meter_settled.clone(),
            };
            // 0 (or unset → default) disables the heartbeat.
            let keepalive = match sse_keepalive_ms.unwrap_or(10_000) {
                0 => None,
                ms => Some(Duration::from_millis(ms)),
            };
            // Provider -> format -> visibility -> meter -> keepalive -> finalizer.
            // Heartbeats stay outside metering and receipt input.
            let response_header_map = response_headers(&forward.upstream_headers, &content_type);
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
            let visible: ServiceResponseStream = if exclude_reasoning {
                Box::pin(SseTransformStream::new(
                    transformed,
                    StreamTransform::ExcludeReasoning,
                ))
            } else {
                transformed
            };
            let metered: ServiceResponseStream = Box::pin(MeterStream::new(
                visible,
                report,
                errors::sse_protocol(endpoint_path),
            ));
            // A failure anywhere below reaches the finalizer, which holds the
            // protocol state needed to decide whether the client can be told.
            let kept: ServiceResponseStream = Box::pin(KeepAliveStream::new(metered, keepalive));

            let receipt_id = journal.peek_receipt_id();
            match service.finalize_middleware_response_stream(
                journal,
                kept,
                endpoint_path,
                Some(&content_type),
                requester,
                e2ee,
                Some(request_id.clone()),
            ) {
                Ok(finalized) => {
                    let status =
                        StatusCode::from_u16(upstream_status).unwrap_or(StatusCode::BAD_GATEWAY);
                    let mut headers = response_header_map;
                    match &receipt_id {
                        Some(receipt_id) => {
                            insert_header(&mut headers, "x-receipt-id", receipt_id);
                            apply_e2ee_headers(&mut headers, finalized.e2ee.as_ref(), true);
                        }
                        None => apply_e2ee_headers(&mut headers, finalized.e2ee.as_ref(), false),
                    }
                    headers.insert(
                        HeaderName::from_static("x-accel-buffering"),
                        HeaderValue::from_static("no"),
                    );
                    headers.insert(
                        HeaderName::from_static("cache-control"),
                        HeaderValue::from_static("no-cache"),
                    );
                    // End cleanly on body errors so hyper does not reset the socket.
                    let stream_request_id = request_id.clone();
                    let stream_model = model.unwrap_or("").to_string();
                    let stream_route = forward.selected_route.clone();
                    let body = Body::from_stream(finalized.body.scan((), move |_, chunk| {
                        std::future::ready(match chunk {
                            Ok(bytes) => Some(Ok::<_, std::io::Error>(bytes)),
                            Err(err) => {
                                // Mark before meter drop classifies the terminal.
                                downstream_abort.store(true, Ordering::Relaxed);
                                tracing::warn!(
                                    target: "stream_abort",
                                    request_id = %stream_request_id,
                                    error = %err,
                                    "response stream error; ending body gracefully instead of aborting the connection"
                                );
                                // Meter already settled; record this late failure here.
                                if meter_settled.load(Ordering::Relaxed) {
                                    log_generated_outcome(
                                        &stream_request_id,
                                        &stream_model,
                                        "finalize_error",
                                        502,
                                        upstream_status,
                                        &stream_route,
                                        attempt_index,
                                        started,
                                        &detail_snippet(err.to_string().as_bytes()),
                                    );
                                }
                                None
                            }
                        })
                    }));
                    (status, headers, body).into_response()
                }
                Err(err) => {
                    // The stream never started, so emit its only outcome here.
                    let status = forward_error_status(&err);
                    if should_log_failure(status) {
                        log_generated_outcome(
                            &request_id,
                            model.unwrap_or(""),
                            "finalize_error",
                            status,
                            upstream_status,
                            &forward.selected_route,
                            attempt_index,
                            started,
                            &detail_snippet(err.to_string().as_bytes()),
                        );
                    }
                    service_error_response(outcome_ctx, err, None)
                }
            }
        }
        Ok(MiddlewareForwardResult::UpstreamError(forward)) => {
            // A streaming non-2xx has no receipt but still reports all attempts.
            let (status, body) = errors::normalize_upstream_error_parts(
                surface,
                forward.error.upstream_status,
                &forward.error.upstream_body,
                &received_body,
                Some(&request_id),
            );
            let attempt_index = forward.failed_attempts.len() as u32;
            if should_log_failure(status) {
                log_generated_outcome(
                    &request_id,
                    model.unwrap_or(""),
                    "upstream_error_stream",
                    status,
                    forward.error.upstream_status,
                    &forward.selected_route,
                    attempt_index,
                    started,
                    &detail_snippet(&forward.error.upstream_body),
                );
            }
            meter.failed_attempts(&forward.failed_attempts, true);
            meter.upstream_error(
                reported_status(status, forward.error.upstream_status),
                attempt_index,
                &forward.selected_route,
            );
            finalize_generated(status, body, &[], e2ee, outcome_ctx)
        }
        // Report transport failures per attempt, followed by their summary.
        Ok(MiddlewareForwardResult::AllFailed(forward)) => {
            let status = forward_error_status(&forward.error);
            meter.failed_attempts(&forward.failed_attempts, stream);
            if should_log_failure(status) {
                log_generated_outcome(
                    &request_id,
                    model.unwrap_or(""),
                    "all_candidates_failed",
                    status,
                    0,
                    "",
                    forward.failed_attempts.len() as u32,
                    started,
                    &detail_snippet(forward.error.to_string().as_bytes()),
                );
            }
            if status >= 500 {
                meter.gateway_failure_at(
                    forward.failed_attempts.len() as u32,
                    status,
                    forward_error_source(&forward.error),
                    &forward.error.to_string(),
                    stream,
                );
            }
            service_error_response(outcome_ctx, forward.error, e2ee)
        }
        // Report unattributed forwarding failures; preserve E2EE for the response.
        Err(err) => {
            let status = forward_error_status(&err);
            if should_log_failure(status) {
                log_generated_outcome(
                    &request_id,
                    model.unwrap_or(""),
                    "forward_failed",
                    status,
                    0,
                    "",
                    0,
                    started,
                    &detail_snippet(err.to_string().as_bytes()),
                );
            }
            if status >= 500 {
                meter.gateway_failure(status, forward_error_source(&err), &err.to_string(), stream);
            }
            service_error_response(outcome_ctx, err, e2ee)
        }
    }
}

// Posts usage reports to control without blocking the response.
struct Meter<'a> {
    control: &'a ControlClient,
    request_id: String,
    endpoint_path: &'static str,
    request_model: String,
    pricing: Option<Value>,
    spend_mode: Option<SpendMode>,
    user_id: Option<i64>,
    virtual_key_id: Option<i64>,
    started: Instant,
}

impl Meter<'_> {
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

    fn upstream_error(&self, status: u16, attempt_index: u32, selected_route_id: &str) {
        self.spawn(PostReport {
            status,
            is_streaming: Some(true),
            attempt_index: Some(attempt_index),
            selected_route_id: Some(selected_route_id.to_string()),
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
        self.gateway_failure_at(0, status, source, message, is_streaming);
    }

    // Put summaries after attempt rows to avoid control's idempotency key.
    fn gateway_failure_at(
        &self,
        attempt_index: u32,
        status: u16,
        source: ErrorSource,
        message: &str,
        is_streaming: bool,
    ) {
        self.spawn(PostReport {
            status,
            is_streaming: Some(is_streaming),
            attempt_index: Some(attempt_index),
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

// Report client-attributable 4xx mappings; otherwise preserve upstream status.
fn reported_status(mapped: u16, upstream_status: u16) -> u16 {
    if (400..500).contains(&mapped) {
        mapped
    } else {
        upstream_status
    }
}

// ACI eligibility is gateway policy; other forwarding failures are upstream.
fn forward_error_source(err: &ServiceError) -> ErrorSource {
    match err {
        ServiceError::UpstreamVerification(
            UpstreamVerificationError::NoEligibleAttestedRoute(_)
            | UpstreamVerificationError::NoEligibleAttestedSession(_),
        ) => ErrorSource::Gateway,
        _ => ErrorSource::Upstream,
    }
}

// Client-facing status for a forward/finalize `ServiceError`.
fn forward_error_status(err: &ServiceError) -> u16 {
    match err {
        ServiceError::E2ee(_) => 400,
        ServiceError::UpstreamVerification(_) => 503,
        ServiceError::Upstream(UpstreamError::Routing(_)) => 404,
        _ => 502,
    }
}

// Encrypt generated errors unless E2EE setup itself failed.
fn service_error_response(
    outcome: OutcomeCtx<'_>,
    err: ServiceError,
    e2ee: Option<E2eeRequestContext>,
) -> Response {
    let surface = outcome.surface;
    let request_id = outcome.request_id;
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
    finalize_generated(status, body, &[], e2ee, outcome)
}

// Build a generated response; E2EE finalization fails closed.
#[allow(clippy::too_many_arguments)]
fn finalize_generated(
    status: u16,
    body: Vec<u8>,
    extra_headers: &[(&'static str, String)],
    e2ee: Option<E2eeRequestContext>,
    outcome: OutcomeCtx<'_>,
) -> Response {
    let OutcomeCtx {
        surface,
        service,
        endpoint_path,
        ..
    } = outcome;
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
            // Supersede the earlier outcome with the client-visible 500.
            log_generated_outcome(
                outcome.request_id,
                outcome.model,
                "finalize_error",
                500,
                0,
                "",
                0,
                outcome.started,
                &detail_snippet(err.to_string().as_bytes()),
            );
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

// Drop gateway-owned/hop-by-hop headers and force content type.
fn response_headers(
    upstream_headers: &std::collections::HashMap<String, String>,
    content_type: &str,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in upstream_headers {
        // The reserialized body is identity-encoded.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_failure_policy() {
        // Every client-visible failure class is logged...
        for status in [400u16, 401, 402, 404, 499, 500, 502, 503, 504] {
            assert!(should_log_failure(status), "{status} must be observable");
        }
        // ...except final 429s, which are recorded in the usage pipeline.
        assert!(!should_log_failure(429));
    }

    #[test]
    fn client_controlled_identifier_is_bounded_and_single_line() {
        let hostile = format!("bad\u{1b}[2Jmodel\n{}", "m".repeat(4096));
        let cleaned = sanitize_identifier(&hostile);
        assert!(cleaned.chars().count() <= 128);
        assert!(!cleaned.contains('\n') && !cleaned.contains('\u{1b}'));
        assert_eq!(sanitize_identifier("z-ai/glm-5.2"), "z-ai/glm-5.2");
    }

    #[test]
    fn finish_reason_anomaly_detection() {
        assert!(!finish_reasons_anomalous(["stop"]));
        assert!(!finish_reasons_anomalous(["length", "tool_calls"]));
        assert!(!finish_reasons_anomalous(["end_turn", "max_tokens"]));
        // A legitimate context-window truncation is a successful response.
        assert!(!finish_reasons_anomalous(["model_context_window_exceeded"]));
        // Empty is not anomalous: truncation without a terminal is already a
        // Failed outcome, and some surfaces terminate without finish reasons.
        assert!(!finish_reasons_anomalous([]));
        // Nonstandard values — the "error smuggled through a success"
        // class — must trip the anomaly.
        assert!(finish_reasons_anomalous(["upstream_error"]));
        assert!(finish_reasons_anomalous(["stop", "weird_provider_reason"]));
    }
}
