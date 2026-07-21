//! Completion forwarding.
//!
//! Runs the completion flow: consult the control plane, shape one
//! body per candidate, call `AciService::forward_chat_completion_for_middleware`
//! directly, consume the typed result, transform the buffered or streaming
//! response, inject cost, post the usage report, and finalize through the
//! existing receipt/E2EE finalizers.

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
use super::request_transform::{build_candidates, Endpoint};
use super::sse::{KeepAliveStream, MeterStream, StreamReport};
use super::stream_transform::SseTransformStream;
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
    pub upstream_required: bool,
    /// Request is restricted to ACI-verified attested upstreams.
    pub aci_required: bool,
    /// Optional hard allowlist of attested session ids.
    pub aci_session_ids: Vec<String>,
    pub request_id: String,
    pub user_model: Option<String>,
    pub stream: bool,
}

/// Cap on the error-detail snippet in `request_outcome` lines. Long enough to
/// carry a provider's error envelope, short enough to bound log growth and to
/// avoid replaying large bodies into the log.
const MAX_DETAIL_CHARS: usize = 240;

/// Whether a terminal failure with this client-facing status gets a
/// `request_outcome` line. Always-on for every model; the only exclusion is
/// final 429s — the highest-volume, lowest-information class, already recorded
/// per-attempt in the usage pipeline. Every other failure (4xx/5xx, client
/// disconnects, stream failures) is logged with content-free structured
/// fields (statuses, route, phase, finish reasons, timings); the raw error
/// detail appears only with `request_outcome=debug`. Silence the target via
/// `RUST_LOG` if ever needed — there is deliberately no config knob.
pub(super) fn should_log_failure(status: u16) -> bool {
    status != 429
}

/// Finish/stop reasons that mark a genuinely clean completion on the OpenAI
/// and Anthropic surfaces. A completed stream whose collected reasons include
/// anything outside this set is logged as an anomaly: an upstream signalling
/// an error through a nonstandard finish reason would otherwise be recorded
/// as a plain success.
///
/// This is a heuristic: a miss on a newly introduced legitimate value costs
/// only an info-level false positive and a one-line addition here. Keep it a
/// flat list — no provider-specific registries or runtime configuration.
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

/// Per-reason length cap for logged finish reasons. Long enough for every
/// standard value and any plausible provider-specific token, short enough
/// that a provider-controlled string cannot become a content channel in the
/// always-on log.
const MAX_REASON_CHARS: usize = 32;

/// Log-safe form of a client-controlled identifier (the requested model
/// name): single-line, control characters replaced, length-capped. The
/// request body allows megabytes, so an unbounded identifier in an always-on
/// info log would be a log-injection and disk-amplification vector.
pub(super) fn sanitize_identifier(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(128)
        .collect()
}

/// Log-safe form of a single provider-controlled finish reason:
/// length-capped, with every control character (newlines, ANSI escapes)
/// replaced — a JSON string can embed them after parsing, enabling forged
/// log records or terminal-escape injection.
pub(super) fn sanitize_reason(reason: &str) -> String {
    reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_REASON_CHARS)
        .collect()
}

/// Emission form of collected finish reasons: count-capped, each value
/// sanitized. Anomaly *detection* runs on the raw values; only what gets
/// stored or logged is bounded.
pub(super) fn sanitized_reasons<'a, I: IntoIterator<Item = &'a str>>(reasons: I) -> String {
    reasons
        .into_iter()
        .take(8)
        .map(sanitize_reason)
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether raw error detail may be included in `request_outcome` lines.
/// Upstream error bodies can echo request content (validation errors quoting
/// input, signed URLs), and this gateway's confidentiality model treats logs
/// as operator-visible — so raw detail is opt-in via the tracing filter
/// (`RUST_LOG=request_outcome=debug`); at the default level the structured
/// fields (statuses, route, phase, finish reasons, timings) still identify
/// the failure class.
pub(super) fn debug_gated_detail(detail: &str) -> &str {
    if tracing::enabled!(target: "request_outcome", tracing::Level::DEBUG) {
        detail
    } else {
        ""
    }
}

/// Single-line, length-capped snippet of an error body/message for the
/// `request_outcome` `detail` field (char-boundary safe). The input is
/// byte-capped before the lossy conversion so a large non-UTF-8 body is never
/// copied whole; a char split at the cap degrades to a replacement character,
/// which is fine for a log snippet.
pub(super) fn detail_snippet(raw: &[u8]) -> String {
    let capped = &raw[..raw.len().min(4 * MAX_DETAIL_CHARS)];
    String::from_utf8_lossy(capped)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_DETAIL_CHARS)
        .collect()
}

/// `request_outcome` line for a terminal path that never settles a stream or a
/// buffered 2xx: consult denials, routing/shaping failures, upstream error
/// responses, and forward errors. Together with the stream-settle and
/// buffered-2xx lines this makes observation exhaustive. Contract: a request
/// emits at most one primary line, and a late finalization failure may append
/// one supplemental `phase=finalize_error` line for the same request_id —
/// aggregate by unique request_id, with `finalize_error` superseding the
/// earlier record.
/// Identity fields threaded into response finalization so a late failure
/// there (E2EE encryption of a generated body) can record the actual
/// client-facing terminal as `phase=finalize_error`.
#[derive(Clone, Copy)]
struct OutcomeCtx<'a> {
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
        upstream_required,
        aci_required,
        aci_session_ids,
        request_id,
        user_model,
        stream,
    } = input;

    let started = Instant::now();
    let model = params.get("model").and_then(Value::as_str);
    let outcome_ctx = OutcomeCtx {
        request_id: &request_id,
        model: model.unwrap_or(""),
        started,
    };
    // Forward the routing block verbatim; the control plane validates it. Parsing
    // it here would silently drop a caller's restrictions on a malformed field.
    let provider = params.get("provider");

    let consult = control
        .consult_pre(model, api_key_hash.as_deref(), provider)
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
        // Record 5xx and 429 denials as gateway failures; other user denials
        // (401/402/404) are caller-attributable and left unrecorded. Tagging
        // these ErrorSource::Control keeps them out of upstream-health signals.
        if status == 429 || status >= 500 {
            meter.gateway_failure(status, ErrorSource::Control, message, stream);
        }
        if status == 429 {
            if let Some(rate_limit) = &consult.rate_limit {
                let body = errors::rate_limit_envelope_bytes(surface, message, Some(&request_id));
                let extra = errors::rate_limit_headers(rate_limit.limit, rate_limit.reset_at);
                return finalize_generated(
                    surface,
                    service,
                    endpoint_path,
                    429,
                    body,
                    &extra,
                    e2ee,
                    outcome_ctx,
                );
            }
        }
        let body = errors::envelope_bytes(
            surface,
            errors::error_type(surface, status),
            message,
            Some(&request_id),
        );
        return finalize_generated(
            surface,
            service,
            endpoint_path,
            status,
            body,
            &[],
            e2ee,
            outcome_ctx,
        );
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
        return finalize_generated(
            surface,
            service,
            endpoint_path,
            400,
            body,
            &[],
            e2ee,
            outcome_ctx,
        );
    }

    // Shape one body per candidate (typed per-route contract).
    let shaped = match build_candidates(&params, endpoint, &candidates) {
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
            return finalize_generated(
                surface,
                service,
                endpoint_path,
                500,
                body,
                &[],
                e2ee,
                outcome_ctx,
            );
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
                upstream_required: Some(upstream_required),
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
            // The forwarder tries candidates in order and pushes exactly one
            // `failed_attempts` entry per candidate it abandons, so the serving
            // candidate's index is the number of attempts before it (all three
            // arms derive it this way). Derived, not looked up by route id: the
            // candidate list is not deduped here, and a repeated route id would
            // resolve to the earlier copy — colliding with that attempt's report
            // under control's (request_id, attempt, status) idempotency gate and
            // mislabeling a failed-over serve as a first-choice one.
            let attempt_index = forward.failed_attempts.len() as u32;
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
                        return finalize_generated(
                            surface,
                            service,
                            endpoint_path,
                            502,
                            body,
                            &[],
                            e2ee,
                            outcome_ctx,
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
                // A buffered 2xx is only observable when its finish reasons are
                // nonstandard — an upstream error smuggled through a "success".
                // Covers both response shapes: OpenAI `choices[].finish_reason`
                // and Anthropic top-level `stop_reason`.
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
                    // Any earlier outcome line for this request described the
                    // upstream outcome; the client actually receives this
                    // finalization error, so record the real terminal too.
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
                    service_error_response(surface, endpoint_path, service, outcome_ctx, err, None)
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

            // Set when the downstream finalizer (receipt drafting / E2EE)
            // errors while the body is being consumed: the meter's drop must
            // then record an internal failure, not a client disconnect.
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
            // Order: provider stream (drafts response.received) -> format
            // transform (if cross-format) -> meter/cost -> keep-alive -> finalizer
            // (hashes response.returned). Same-format streaming is native
            // passthrough (no transform). Metering sits inside the keep-alive so
            // it only ever buffers real upstream SSE bytes; the heartbeat comments
            // are injected downstream and never enter its line reassembly.
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
            let metered: ServiceResponseStream = Box::pin(MeterStream::new(
                transformed,
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
                    // A response-stream error must not become a body Err: hyper
                    // aborts the connection (TCP RST toward the proxy), which
                    // clients experience as a silently killed stream and a
                    // poisoned keep-alive pool, invisible to application logs.
                    // Log the error (this is its only surface) and end the body
                    // instead — a clean HTTP body termination (h1 terminal
                    // chunk, h2 END_STREAM), leaving the connection reusable.
                    let stream_request_id = request_id.clone();
                    let stream_model = model.unwrap_or("").to_string();
                    let stream_route = forward.selected_route.clone();
                    let body = Body::from_stream(finalized.body.scan((), move |_, chunk| {
                        std::future::ready(match chunk {
                            Ok(bytes) => Some(Ok::<_, std::io::Error>(bytes)),
                            Err(err) => {
                                // Mark before the chain drops so the meter's
                                // drop settles this as an internal failure
                                // rather than misreading it as a client
                                // disconnect.
                                downstream_abort.store(true, Ordering::Relaxed);
                                tracing::warn!(
                                    target: "stream_abort",
                                    request_id = %stream_request_id,
                                    error = %err,
                                    "response stream error; ending body gracefully instead of aborting the connection"
                                );
                                // A finalizer error after a clean end-of-stream
                                // (receipt store / E2EE finish): the meter has
                                // already settled Completed and will not emit,
                                // so record the client-visible failure here.
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
                    // Synchronous finalizer failure: the stream never started,
                    // so the meter never settles — this is the request's only
                    // outcome line.
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
                    service_error_response(surface, endpoint_path, service, outcome_ctx, err, None)
                }
            }
        }
        Ok(MiddlewareForwardResult::UpstreamError(forward)) => {
            // Streaming non-2xx: no receipt (no completed stream to bind), but the
            // attempt did reach an upstream, so it reports the serving route and
            // every failed-over candidate exactly like the Stream arm.
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
            finalize_generated(
                surface,
                service,
                endpoint_path,
                status,
                body,
                &[],
                e2ee,
                outcome_ctx,
            )
        }
        // Every candidate was attempted and failed without an HTTP response to
        // relay (a chain that ends in an upstream HTTP status — including an
        // all-429 chain — exits via the UpstreamError arm above, which relays
        // that status). Report each attempt so deployment health and triage
        // see the full chain, then a summary row placed after them carrying
        // the aggregated error message.
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
            service_error_response(
                surface,
                endpoint_path,
                service,
                outcome_ctx,
                forward.error,
                e2ee,
            )
        }
        // Failures where no attempt chain is available (pre-forward errors,
        // plus the forwarder's rare mid-walk internal-error abort): record the
        // failure so the request is visible to billing/health, attributed by
        // `forward_error_source` (upstream, except the gateway's own TEE-policy
        // rejection). Client-attributable errors (E2EE/4xx) are not recorded.
        // The E2EE context is still available to encrypt the body.
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
            service_error_response(surface, endpoint_path, service, outcome_ctx, err, e2ee)
        }
    }
}

// Posts usage reports to the control plane (fire-and-forget). Buffered reports
// have no TTFT and `is_streaming = false`; the status recorded is the raw upstream
// status, distinct from the client-facing mapped status.
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

    // Like `gateway_failure`, but placed at an explicit attempt index. Control
    // dedupes reports by (request_id, attempt, status), so a summary row that
    // follows per-attempt rows must sit after them or it collides with (and
    // silently drops) the first attempt's row.
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

// Which component a terminal forward failure is attributable to.
//
// Upstream by default: a forward chain that ends in a 5xx is a provider
// failure. The exceptions are ACI constraints that leave no eligible route or
// current session — no prompt was forwarded, so attributing them to a provider
// would report the gateway's policy decision as someone else's failure.
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

// Map a forward/finalize `ServiceError` to a client-facing generated response.
// E2EE clients still get an encrypted error body, except for `E2ee` errors
// themselves (the E2EE setup failed, so the response cannot be encrypted).
fn service_error_response(
    surface: Surface,
    endpoint_path: &str,
    service: &AciService,
    outcome: OutcomeCtx<'_>,
    err: ServiceError,
    e2ee: Option<E2eeRequestContext>,
) -> Response {
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
    finalize_generated(
        surface,
        service,
        endpoint_path,
        status,
        body,
        &[],
        e2ee,
        outcome,
    )
}

// Build a generated (no-receipt) response, E2EE-encrypting the body when a
// request context is present. If encryption fails it is fail-closed: a generic
// error is returned rather than the cleartext body.
#[allow(clippy::too_many_arguments)]
fn finalize_generated(
    surface: Surface,
    service: &AciService,
    endpoint_path: &str,
    status: u16,
    body: Vec<u8>,
    extra_headers: &[(&'static str, String)],
    e2ee: Option<E2eeRequestContext>,
    outcome: OutcomeCtx<'_>,
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
            // Any earlier outcome line recorded the pre-finalization status;
            // the client actually receives this 500.
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
