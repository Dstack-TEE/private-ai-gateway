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

use axum::body::Bytes;
use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use std::future::Future;
use std::pin::Pin;

use futures_util::StreamExt;
use serde_json::Value;

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{
    is_sse_content_type, AciService, ChatCompletionRequest, E2eeError, E2eeRequestContext,
    E2eeResponseInfo, FailedAttempt, ForwardCandidate, GatewayRequestContext,
    MiddlewareForwardResult, MiddlewareReceiptJournal, ReceiptOwner, ServiceError,
    ServiceResponseStream, UpstreamVerificationError,
};

use super::control::ControlClient;
use super::errors::{self, Surface};
use super::reasoning;
use super::request_features;
use super::request_transform::{build_candidates, Endpoint};
use super::sse::{KeepAliveStream, MeterStream, StreamReport};
use super::stream_transform::{SseTransformStream, StreamTransform};
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
    /// Request is restricted to ACI-verified attested upstreams.
    pub aci_required: bool,
    /// Optional hard allowlist of attested session ids.
    pub aci_session_ids: Vec<String>,
    pub request_id: String,
    pub user_model: Option<String>,
    pub stream: bool,
    /// Request arrived on a TEE-only host: the pre-consult carries `tee: true`
    /// so the control plane 404s a non-TEE model before any forward. Attested
    /// serving itself is enforced via `aci_required` (forced true alongside this).
    pub tee_only: bool,
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
    surface: Surface,
    service: &'a AciService,
    endpoint_path: &'a str,
    request_id: &'a str,
    model: &'a str,
    started: Instant,
    /// The exact bytes the workload received — a §7.5 refusal receipt
    /// commits to them.
    received_body: &'a [u8],
    requester: &'a Option<ReceiptOwner>,
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
    service: &Arc<AciService>,
    sse_keepalive_ms: Option<u64>,
    send_request_features: bool,
    prefix_hash_secret: Option<&str>,
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

    // What the client is told about who served the request. Built before
    // `user_model` moves into the request context, and shared with the streaming
    // transform, which needs it for every chunk.
    let received_body = Arc::new(received_body);
    let identity = Arc::new(response_transform::ResponseIdentity {
        request_id: request_id.clone(),
        user_model: user_model.clone(),
    });

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
                        received_body: received_body.as_slice(),
                        requester: &requester,
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
        received_body: received_body.as_slice(),
        requester: &requester,
    };
    // Forward the routing block verbatim; the control plane validates it. Parsing
    // it here would silently drop a caller's restrictions on a malformed field.
    let provider = params.get("provider");

    // Content-derived scalars for request-aware routing; the content itself
    // never leaves this process. Computed before the consult (its whole point
    // is to inform it) and echoed on the post report via the meters below.
    let request_features = if send_request_features {
        request_features::extract(
            endpoint,
            &params,
            reasoning_requirements.as_ref(),
            prefix_hash_secret.map(str::as_bytes),
        )
    } else {
        None
    };

    let consult = control
        .consult_pre(
            model,
            api_key_hash.as_deref(),
            provider,
            tee_only,
            request_features.as_ref(),
        )
        .await;

    let mut meter = Meter {
        control: control.clone(),
        request_id: request_id.clone(),
        endpoint_path,
        request_model: model.unwrap_or("").to_string(),
        pricing: consult.pricing.clone(),
        spend_mode: consult.spend_mode,
        user_id: consult.user_id,
        virtual_key_id: consult.virtual_key_id,
        prefix_hash: request_features
            .as_ref()
            .and_then(|features| features.prefix_hash.clone()),
        started,
        is_streaming: stream,
        armed: None,
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
        // Report every denial the control plane could attribute to a key
        // (identity present), plus 429/5xx regardless. Unauthenticated
        // denials (401/402/403, malformed-body 400) stay unreported: a report
        // without identity is unattributable and a scanner would otherwise
        // flood the usage pipeline. ErrorSource::Control keeps these out of
        // upstream-health signals.
        if consult.user_id.is_some() || status == 429 || status >= 500 {
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

    let candidates = drop_conflicting_route_twins(consult.candidates.clone().unwrap_or_default());
    if candidates.is_empty() {
        // Not found, not malformed — 404 is what the `model_not_found` body has
        // always said, and what an OpenAI-compatible client expects for a model
        // it cannot reach.
        let message = format!("no route available for model {}", model.unwrap_or("(none)"));
        if should_log_failure(404) {
            log_generated_outcome(
                &request_id,
                model.unwrap_or(""),
                "no_route",
                404,
                0,
                "",
                0,
                started,
                "",
            );
        }
        // The control plane answered with nothing to route to; report it as
        // its failure so the request is accounted for, like a denial.
        meter.gateway_failure(404, ErrorSource::Control, &message, stream);
        let body = errors::envelope_bytes(surface, "model_not_found", &message, Some(&request_id));
        return finalize_generated(404, body, &[], e2ee, outcome_ctx);
    }

    // Shape one body per candidate (typed per-route contract).
    let shaped = match build_candidates(
        &params,
        endpoint,
        &candidates,
        reasoning_requirements.as_ref(),
    ) {
        Ok(shaped) => shaped,
        Err(err) => {
            // A body we cannot shape for the chosen route is a 400, not a 500:
            // reported as 500 it became `type: "upstream_error"` on the OpenAI
            // surface, blaming the provider for a request the gateway itself
            // declined to send.
            let status = err.client_status();
            let message = format!("cannot shape this request: {err}");
            if should_log_failure(status) {
                // The route id lives only here — see `TransformError::detail`.
                let logged = match err.detail() {
                    Some(detail) => format!("{message} ({detail})"),
                    None => message.clone(),
                };
                log_generated_outcome(
                    &request_id,
                    model.unwrap_or(""),
                    "shape_error",
                    status,
                    0,
                    "",
                    0,
                    started,
                    &detail_snippet(logged.as_bytes()),
                );
            }
            meter.gateway_failure(status, ErrorSource::Gateway, &message, stream);
            let body = errors::envelope_bytes(
                surface,
                errors::error_type(surface, status),
                &message,
                Some(&request_id),
            );
            return finalize_generated(status, body, &[], e2ee, outcome_ctx);
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
        // §7.3: the receipt `model` is the model the client asked for — under
        // E2EE the envelope model, which `user_model` already carries.
        user_model,
        target_route_id: None,
        user_tier: consult.user_tier.clone(),
    };

    // The receipt-draft journal is only consumed by the streaming finalizer; the
    // buffered result carries its draft inline.
    let journal = MiddlewareReceiptJournal::default();
    // The consult above is not covered: a client that leaves during it has no
    // identity to report under yet.
    meter.arm(journal.clone());

    // 0 (or unset -> default) disables the heartbeat, and with it the pre-first-
    // byte early commit below.
    let keepalive = match sse_keepalive_ms.unwrap_or(10_000) {
        0 => None,
        ms => Some(Duration::from_millis(ms)),
    };

    // The forward is driven as an owned future so that, when the upstream is
    // slow, it can be moved into the response body and kept running while the
    // client is held with heartbeats.
    let forward_service = service.clone();
    let forward_body = received_body.clone();
    let forward_requester = requester.clone();
    let forward_e2ee = e2ee.clone();
    let forward_journal = journal.clone();
    let mut forward: Pin<
        Box<dyn Future<Output = Result<MiddlewareForwardResult, ServiceError>> + Send>,
    > = Box::pin(async move {
        forward_service
            .forward_chat_completion_for_middleware(
                ChatCompletionRequest {
                    context,
                    endpoint_path,
                    received_body: forward_body.as_slice(),
                    forwarded_body: None,
                    aci_required,
                    aci_session_ids,
                    upstream_verification_event: None,
                    requester: forward_requester,
                    e2ee: forward_e2ee,
                },
                forward_candidates,
                stream,
                forward_journal,
            )
            .await
    });

    // Pre-first-byte early commit: a streaming request whose upstream has not
    // answered within the keep-alive interval is answered now with a 200 SSE
    // body that heartbeats until the upstream does. E2EE responses are excluded
    // — they can only be re-encrypted once the stream is known to be SSE, which
    // is not known before the upstream answers. A fast upstream (answering
    // inside the interval) keeps today's real HTTP status semantics.
    let result = match (keepalive, stream, e2ee.is_none()) {
        (Some(interval), true, true) => match tokio::time::timeout(interval, &mut forward).await {
            Ok(result) => result,
            Err(_) => {
                let pipeline_inputs = StreamPipelineInputs {
                    endpoint,
                    endpoint_path,
                    identity: identity.clone(),
                    exclude_reasoning,
                    candidates: candidates.clone(),
                };
                return build_early_streaming_response(
                    service.clone(),
                    control.clone(),
                    meter,
                    forward,
                    journal,
                    pipeline_inputs,
                    keepalive,
                    surface,
                    received_body.clone(),
                    requester.clone(),
                    request_id.clone(),
                    model.unwrap_or("").to_string(),
                    started,
                );
            }
        },
        _ => forward.await,
    };

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
            // Looked up in the ORIGINAL list even though shaping may have
            // skipped candidates: a route id names one deployment and a
            // deployment has one format, so a repeated id (an ordered list
            // may name a route twice) cannot disagree on format — and
            // same-id copies shape identically, so a skip can never split
            // them either.
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
                response_transform::rewrite_identity(&mut transformed, &identity);

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
                    None,
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
                // Last, after identity rewrite and cost injection: reduce the body
                // to the gateway's documented output schema, so the client sees one
                // shape and no upstream-specific field — including any we have never
                // seen — can leak.
                response_transform::canonicalize(
                    &mut transformed,
                    endpoint,
                    Some(request_id.as_str()),
                );
                (
                    upstream_status,
                    serde_json::to_vec(&transformed).unwrap_or_default(),
                )
            } else {
                let (mapped, body) = errors::normalize_upstream_error_parts(
                    surface,
                    upstream_status,
                    &forward.upstream_body,
                    received_body.as_slice(),
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
                    reported_status(mapped, upstream_status, &forward.upstream_body),
                    attempt_index,
                    Some(&forward.selected_route),
                    None,
                    errors::client_safe_error_message(&forward.upstream_body),
                );
                meter.failed_attempts(&forward.failed_attempts, false);
                (mapped, body)
            };

            match service.finalize_middleware_receipt(
                forward.receipt,
                &final_body,
                Some("application/json"),
                requester.clone(),
                e2ee,
            ) {
                Ok(finalized) => {
                    let status =
                        StatusCode::from_u16(client_status).unwrap_or(StatusCode::BAD_GATEWAY);
                    let mut headers = gateway_owned_headers("application/json");
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
                    service_error_response(outcome_ctx, err, None)
                }
            }
        }
        Ok(MiddlewareForwardResult::Stream(forward)) => {
            // Normalized, not relayed: providers spell the content type
            // differently (`text/event-stream` vs `text/event-stream; charset=utf-8`),
            // and that difference alone distinguishes backends. Emit only the base
            // media type — everything before any `;` parameter — so the value is
            // identical no matter which upstream served the stream, while a genuinely
            // non-SSE type keeps its own base type rather than being mislabeled.
            let content_type = forward
                .upstream_headers
                .get("content-type")
                .map(|value| value.split(';').next().unwrap_or("").trim())
                .filter(|base| !base.is_empty())
                .unwrap_or("text/event-stream")
                .to_string();
            let upstream_status = forward.upstream_status;
            let attempt_index = forward.failed_attempts.len() as u32;
            meter.failed_attempts(&forward.failed_attempts, true);

            // An E2EE response can only be re-encrypted as SSE. Decided here,
            // before the pipeline exists, so the caller's request shape is
            // answered as such — a 400 with no usage report, like the other
            // client-attributable failures — rather than reaching the
            // finalizer, which would refuse a stream already built.
            if e2ee.is_some() && !is_sse_content_type(Some(&content_type)) {
                let err = ServiceError::E2ee(E2eeError::EncryptionFailed);
                let status = forward_error_status(&err);
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
                meter.disarm();
                return service_error_response(outcome_ctx, err, None);
            }

            // Set when the downstream finalizer (receipt drafting / E2EE)
            // errors while the body is being consumed: the meter's drop must
            // then record an internal failure, not a client disconnect.
            let downstream_abort = Arc::new(AtomicBool::new(false));
            let meter_settled = Arc::new(AtomicBool::new(false));
            // A finalizer failure after the meter has settled Completed
            // (receipt store / E2EE finish at end of stream) is reported from
            // the body wrapper below with this template.
            let late_failure = PostReport {
                status: 502,
                is_streaming: Some(true),
                attempt_index: Some(attempt_index),
                selected_route_id: Some(forward.selected_route.clone()),
                error_source: Some(ErrorSource::Gateway),
                error_message: Some("downstream finalizer failed after end of stream".to_string()),
                ..meter.base()
            };
            let late_failure_control = control.clone();
            let report = meter.into_stream_report(
                forward.selected_route.clone(),
                attempt_index,
                upstream_status,
                downstream_abort.clone(),
                meter_settled.clone(),
            );
            // Order: provider stream (drafts response.received) -> format
            // transform (if cross-format) -> response visibility -> sanitize
            // -> meter/cost -> keep-alive -> finalizer (hashes response.returned).
            // Same-format streaming skips only the format transform. Metering sits inside the
            // keep-alive so it only ever buffers real upstream SSE bytes; heartbeat
            // comments are injected downstream and never enter its line reassembly.
            let response_header_map = gateway_owned_headers(&content_type);
            let pipeline_inputs = StreamPipelineInputs {
                endpoint,
                endpoint_path,
                identity: identity.clone(),
                exclude_reasoning,
                candidates: candidates.clone(),
            };
            let metered = build_metered_pipeline(
                forward.body,
                &forward.selected_route,
                report,
                &pipeline_inputs,
            );
            // A failure anywhere below reaches the finalizer, which holds the
            // protocol state needed to decide whether the client can be told.
            let kept: ServiceResponseStream = Box::pin(KeepAliveStream::new(metered, keepalive));

            let receipt_id = journal.peek_receipt_id();
            match service.finalize_middleware_response_stream(
                journal,
                kept,
                endpoint_path,
                Some(&content_type),
                requester.clone(),
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
                                    let mut report = late_failure.clone();
                                    report.duration_ms = started.elapsed().as_millis() as u64;
                                    spawn_report(&late_failure_control, report);
                                }
                                None
                            }
                        })
                    }));
                    (status, headers, body).into_response()
                }
                Err(err) => {
                    // Synchronous finalizer failure. The pipeline it refused is
                    // dropped unpolled inside the finalizer, and the meter in
                    // it reports that as a gateway failure against the route;
                    // this line records what the client was told.
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
            // Streaming non-2xx: no receipt (no completed stream to bind), but the
            // attempt did reach an upstream, so it reports the serving route and
            // every failed-over candidate exactly like the Stream arm.
            let (status, body) = errors::normalize_upstream_error_parts(
                surface,
                forward.error.upstream_status,
                &forward.error.upstream_body,
                received_body.as_slice(),
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
                reported_status(
                    status,
                    forward.error.upstream_status,
                    &forward.error.upstream_body,
                ),
                attempt_index,
                &forward.selected_route,
                errors::client_safe_error_message(&forward.error.upstream_body),
            );
            finalize_generated(status, body, &[], e2ee, outcome_ctx)
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
                    None,
                    status,
                    forward_error_source(&forward.error),
                    &forward.error.to_string(),
                    stream,
                );
            } else {
                meter.disarm();
            }
            service_error_response(outcome_ctx, forward.error, e2ee)
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
            } else {
                meter.disarm();
            }
            service_error_response(outcome_ctx, err, e2ee)
        }
    }
}

/// Enforce at the consult boundary what the selected-format lookups below
/// assume: one route id, one description. A repeated id is legitimate (an
/// ordered candidate list may name a route twice), but only as an IDENTICAL copy —
/// a later copy that disagrees on format/engine/anything would make the
/// route-id -> format lookup ambiguous, so it is dropped (keeping the first,
/// which is what the lookups resolve to anyway) and logged as control's bug.
/// This turns the invariant from an assumption about our control plane into a
/// property of whatever arrives on the wire.
fn drop_conflicting_route_twins(candidates: Vec<RouteCandidate>) -> Vec<RouteCandidate> {
    let mut kept: Vec<RouteCandidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match kept.iter().find(|k| k.route_id == candidate.route_id) {
            Some(first) if *first != candidate => {
                tracing::error!(
                    route_id = %candidate.route_id,
                    "control returned conflicting candidates under one route id; dropping the later copy"
                );
            }
            _ => kept.push(candidate),
        }
    }
    kept
}

// Posts usage reports to the control plane (fire-and-forget). Buffered reports
// have no TTFT and `is_streaming = false`; the status recorded is the raw upstream
// status, distinct from the client-facing mapped status.
//
// Every admitted request must produce exactly one terminal report, whichever
// way the handler exits. The methods below cover the exits the code reaches;
// `Drop` covers the one it does not — hyper dropping the handler future
// because the client connection closed while the upstream was still being
// waited for. While `armed` holds the forward's journal, that drop reports a
// client disconnect against the candidate that was in flight.
struct Meter {
    control: ControlClient,
    request_id: String,
    endpoint_path: &'static str,
    request_model: String,
    pricing: Option<Value>,
    spend_mode: Option<SpendMode>,
    user_id: Option<i64>,
    virtual_key_id: Option<i64>,
    /// Echoed on every report so billing can key cache affinity; see
    /// `PostReport::prefix_hash`.
    prefix_hash: Option<String>,
    started: Instant,
    is_streaming: bool,
    /// Present only between arming the forward and reporting its outcome.
    armed: Option<MiddlewareReceiptJournal>,
}

impl Meter {
    /// Account one non-Stream forward result to the usage pipeline and return
    /// the client-facing status for it. Mirrors the terminal reporting the
    /// immediate match arms do; used by the early-committed streaming path,
    /// which cannot take those arms because it has already sent 200 headers
    /// and delivers the failure in-band.
    fn account_forward_failure(
        &mut self,
        surface: Surface,
        received_body: &[u8],
        result: Result<MiddlewareForwardResult, ServiceError>,
    ) -> u16 {
        let meter = self;
        let request_id = &meter.request_id.clone();
        let model = &meter.request_model.clone();
        let started = meter.started;
        let is_streaming = meter.is_streaming;
        match result {
            Ok(MiddlewareForwardResult::UpstreamError(forward)) => {
                let (status, _) = errors::normalize_upstream_error_parts(
                    surface,
                    forward.error.upstream_status,
                    &forward.error.upstream_body,
                    received_body,
                    Some(request_id),
                );
                let attempt_index = forward.failed_attempts.len() as u32;
                if should_log_failure(status) {
                    log_generated_outcome(
                        request_id,
                        model,
                        "upstream_error_stream",
                        status,
                        forward.error.upstream_status,
                        &forward.selected_route,
                        attempt_index,
                        started,
                        &detail_snippet(&forward.error.upstream_body),
                    );
                }
                meter.failed_attempts(&forward.failed_attempts, is_streaming);
                meter.upstream_error(
                    reported_status(
                        status,
                        forward.error.upstream_status,
                        &forward.error.upstream_body,
                    ),
                    attempt_index,
                    &forward.selected_route,
                    errors::client_safe_error_message(&forward.error.upstream_body),
                );
                status
            }
            Ok(MiddlewareForwardResult::AllFailed(forward)) => {
                let status = forward_error_status(&forward.error);
                meter.failed_attempts(&forward.failed_attempts, is_streaming);
                if should_log_failure(status) {
                    log_generated_outcome(
                        request_id,
                        model,
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
                        None,
                        status,
                        forward_error_source(&forward.error),
                        &forward.error.to_string(),
                        is_streaming,
                    );
                } else {
                    meter.disarm();
                }
                status
            }
            Err(err) => {
                let status = forward_error_status(&err);
                if should_log_failure(status) {
                    log_generated_outcome(
                        request_id,
                        model,
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
                    meter.gateway_failure(
                        status,
                        forward_error_source(&err),
                        &err.to_string(),
                        is_streaming,
                    );
                } else {
                    meter.disarm();
                }
                status
            }
            // A streaming request never resolves to a buffered forward, and the
            // Stream case is handled by the caller; an unexpected value is recorded
            // as a gateway failure rather than dropped.
            Ok(MiddlewareForwardResult::Forwarded(_)) | Ok(MiddlewareForwardResult::Stream(_)) => {
                meter.gateway_failure(
                    502,
                    ErrorSource::Gateway,
                    "unexpected forward result on the streaming path",
                    is_streaming,
                );
                502
            }
        }
    }
}

/// Answer a slow streaming request that has not produced upstream headers within
/// the keep-alive interval: commit `200 text/event-stream` now and move the
/// still-running forward into the response body. The body heartbeats until the
/// forward resolves, then splices in the metered client pipeline (2xx) or
/// delivers the failure as the surface's in-band error event. A drop before the
/// forward resolves is the client giving up: the meter is still armed, so its
/// drop records the 499.
#[allow(clippy::too_many_arguments)]
fn build_early_streaming_response(
    service: Arc<AciService>,
    control: ControlClient,
    mut meter: Meter,
    forward: Pin<Box<dyn Future<Output = Result<MiddlewareForwardResult, ServiceError>> + Send>>,
    journal: MiddlewareReceiptJournal,
    inputs: StreamPipelineInputs,
    keepalive: Option<Duration>,
    surface: Surface,
    received_body: Arc<Vec<u8>>,
    requester: Option<ReceiptOwner>,
    request_id: String,
    model: String,
    started: Instant,
) -> Response {
    let endpoint_path = inputs.endpoint_path;
    let protocol = errors::sse_protocol(endpoint_path);
    let content_type = "text/event-stream";
    let downstream_abort = Arc::new(AtomicBool::new(false));
    let meter_settled = Arc::new(AtomicBool::new(false));
    // Template for a finalizer error that lands after the meter has already
    // settled at end of stream; the exact route/attempt are filled in once the
    // Stream result is known, so this carries what is knowable now.
    let late_failure_control = control.clone();

    let stream_request_id = request_id.clone();
    let stream_abort = downstream_abort.clone();
    let stream_settled = meter_settled.clone();
    let body_stream = async_stream::stream! {
        // Committed after the interval already elapsed: give the client a byte
        // immediately so the 200 does not read as an empty hang.
        yield Ok::<Bytes, ServiceError>(Bytes::from_static(b": PROCESSING\n\n"));
        match forward.await {
            Ok(MiddlewareForwardResult::Stream(f)) => {
                let attempt_index = f.failed_attempts.len() as u32;
                meter.failed_attempts(&f.failed_attempts, true);
                let upstream_status = f.upstream_status;
                let selected_route = f.selected_route.clone();
                let report = meter.into_stream_report(
                    selected_route.clone(),
                    attempt_index,
                    upstream_status,
                    stream_abort.clone(),
                    stream_settled.clone(),
                );
                let mut pipeline = build_metered_pipeline(f.body, &selected_route, report, &inputs);
                while let Some(item) = pipeline.next().await {
                    yield item;
                }
            }
            other => {
                let status =
                    meter.account_forward_failure(surface, received_body.as_slice(), other);
                yield Ok(Bytes::from(errors::stream_error_event(
                    protocol,
                    status,
                    Some(&stream_request_id),
                    None,
                )));
            }
        }
    };
    let kept: ServiceResponseStream =
        Box::pin(KeepAliveStream::new(Box::pin(body_stream), keepalive));

    let finalized = match service.finalize_middleware_response_stream(
        journal,
        kept,
        endpoint_path,
        Some(content_type),
        requester,
        None,
        Some(request_id.clone()),
    ) {
        Ok(finalized) => finalized,
        Err(_) => {
            // The finalizer only refuses an E2EE-on-non-SSE stream, which the
            // early path never builds; treat any refusal as an internal error.
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                gateway_owned_headers("application/json"),
                Vec::new(),
            )
                .into_response();
        }
    };

    // No `x-receipt-id`: at commit time no candidate has been chosen, so none is
    // reserved yet. The receipt is still issued and retrievable by the response
    // `id`.
    let mut headers = gateway_owned_headers(content_type);
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    headers.insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-cache"),
    );
    let scan_request_id = request_id.clone();
    let scan_model = model.clone();
    let body = Body::from_stream(finalized.body.scan((), move |_, chunk| {
        std::future::ready(match chunk {
            Ok(bytes) => Some(Ok::<_, std::io::Error>(bytes)),
            Err(err) => {
                downstream_abort.store(true, Ordering::Relaxed);
                tracing::warn!(
                    target: "stream_abort",
                    request_id = %scan_request_id,
                    error = %err,
                    "response stream error; ending body gracefully instead of aborting the connection"
                );
                if meter_settled.load(Ordering::Relaxed) {
                    log_generated_outcome(
                        &scan_request_id,
                        &scan_model,
                        "finalize_error",
                        502,
                        0,
                        "",
                        0,
                        started,
                        &detail_snippet(err.to_string().as_bytes()),
                    );
                    spawn_report(
                        &late_failure_control,
                        PostReport {
                            status: 502,
                            duration_ms: started.elapsed().as_millis() as u64,
                            is_streaming: Some(true),
                            attempt_index: Some(0),
                            selected_route_id: None,
                            request_model: scan_model.clone(),
                            error_source: Some(ErrorSource::Gateway),
                            error_message: Some(
                                "downstream finalizer failed after end of stream".to_string(),
                            ),
                            ..empty_report(&scan_request_id, endpoint_path)
                        },
                    );
                }
                None
            }
        })
    }));
    (StatusCode::OK, headers, body).into_response()
}

/// A `PostReport` with every optional field cleared, for callers that fill in
/// only what they know. Used where no `Meter` is in scope to supply `base()`.
fn empty_report(request_id: &str, endpoint_path: &str) -> PostReport {
    PostReport {
        request_id: request_id.to_string(),
        endpoint: endpoint_path.to_string(),
        status: 0,
        duration_ms: 0,
        ttft_ms: None,
        is_streaming: None,
        attempt_index: None,
        selected_route_id: None,
        request_model: String::new(),
        usage: None,
        pricing: None,
        spend_mode: None,
        user_id: None,
        virtual_key_id: None,
        error_source: None,
        error_message: None,
        prefix_hash: None,
    }
}

/// The per-request context a committed upstream stream needs to become the
/// client pipeline: which surface it is, the identity to stamp on it, whether
/// reasoning is excluded, and the candidate list the serving route's format is
/// read from. Cloned once so it can be shared between the immediate streaming
/// arm and the pre-first-byte deferred path.
#[derive(Clone)]
pub(super) struct StreamPipelineInputs {
    pub endpoint: Endpoint,
    pub endpoint_path: &'static str,
    pub identity: Arc<response_transform::ResponseIdentity>,
    pub exclude_reasoning: bool,
    pub candidates: Vec<RouteCandidate>,
}

/// Assemble the client-facing streaming pipeline for one committed upstream
/// stream: the format/visibility/sanitize transforms, then the meter. The meter
/// sits innermost so it only ever parses real upstream SSE bytes; the caller
/// layers the keep-alive and finalizer outside it.
pub(super) fn build_metered_pipeline(
    body: ServiceResponseStream,
    selected_route: &str,
    report: StreamReport,
    inputs: &StreamPipelineInputs,
) -> ServiceResponseStream {
    // Looked up in the ORIGINAL list even though shaping may have skipped
    // candidates: a route id names one deployment and a deployment has one
    // format, so a repeated id cannot disagree on format, and same-id copies
    // shape identically.
    let selected_format = inputs
        .candidates
        .iter()
        .find(|c| c.route_id == selected_route)
        .or_else(|| inputs.candidates.first())
        .map(|c| c.format)
        .unwrap_or(ProviderFormat::Openai);
    let transformed: ServiceResponseStream =
        match stream_transform::select_stream_transform(selected_format, inputs.endpoint) {
            Some(transform) => Box::pin(SseTransformStream::new(body, transform)),
            None => body,
        };
    let visible: ServiceResponseStream = if inputs.exclude_reasoning {
        Box::pin(SseTransformStream::new(
            transformed,
            StreamTransform::ExcludeReasoning,
        ))
    } else {
        transformed
    };
    // Unconditional, unlike the two above: same-format streaming skips every
    // other transform, and that is exactly the path that used to hand the
    // provider's bytes to the client verbatim.
    let sanitized: ServiceResponseStream = Box::pin(SseTransformStream::new(
        visible,
        StreamTransform::SanitizeResponse(inputs.identity.clone(), inputs.endpoint),
    ));
    Box::pin(MeterStream::new(
        sanitized,
        report,
        errors::sse_protocol(inputs.endpoint_path),
    ))
}

impl Meter {
    /// Cover the forward's await: a drop while armed reports the request as
    /// abandoned by the client. Every method that emits the terminal report
    /// disarms; `failed_attempts` alone does not, since attempt rows are not
    /// the request's outcome.
    fn arm(&mut self, journal: MiddlewareReceiptJournal) {
        self.armed = Some(journal);
    }

    /// The request's outcome is settled without a report — a client-
    /// attributable failure that the usage pipeline deliberately does not
    /// record. A later drop must then stay silent.
    fn disarm(&mut self) {
        self.armed = None;
    }

    /// Hand the request's outcome over to the stream meter. From here on the
    /// `MeterStream` settles exactly once, so this guard stands down.
    fn into_stream_report(
        mut self,
        selected_route_id: String,
        attempt_index: u32,
        upstream_status: u16,
        downstream_abort: Arc<AtomicBool>,
        settled: Arc<AtomicBool>,
    ) -> StreamReport {
        self.armed = None;
        StreamReport {
            control: self.control.clone(),
            request_id: self.request_id.clone(),
            endpoint: self.endpoint_path.to_string(),
            request_model: self.request_model.clone(),
            pricing: self.pricing.clone(),
            spend_mode: self.spend_mode,
            user_id: self.user_id,
            virtual_key_id: self.virtual_key_id,
            selected_route_id: Some(selected_route_id),
            attempt_index,
            upstream_status,
            prefix_hash: self.prefix_hash.clone(),
            started: self.started,
            downstream_abort,
            settled,
        }
    }

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
            prefix_hash: self.prefix_hash.clone(),
        }
    }

    // `error_message` carries the upstream's own words when the attempt failed,
    // already scrubbed to what a client may see. `error_source` stays unset:
    // the control plane reads any value there as "one of our components broke",
    // which would misattribute a provider's failure and take it out of that
    // provider's health signal entirely.
    fn success(
        &mut self,
        status: u16,
        attempt_index: u32,
        selected_route_id: Option<&str>,
        usage: Option<Value>,
        error_message: Option<String>,
    ) {
        self.armed = None;
        self.spawn(PostReport {
            status,
            attempt_index: Some(attempt_index),
            selected_route_id: selected_route_id.map(str::to_string),
            usage,
            error_message,
            ..self.base()
        });
    }

    fn upstream_error(
        &mut self,
        status: u16,
        attempt_index: u32,
        selected_route_id: &str,
        error_message: Option<String>,
    ) {
        self.armed = None;
        self.spawn(PostReport {
            status,
            is_streaming: Some(true),
            attempt_index: Some(attempt_index),
            selected_route_id: Some(selected_route_id.to_string()),
            error_message,
            ..self.base()
        });
    }

    fn failed_attempts(&self, attempts: &[FailedAttempt], is_streaming: bool) {
        for (index, attempt) in attempts.iter().enumerate() {
            if attempt.status == 0 {
                continue;
            }
            self.spawn(PostReport {
                status: attempt.status,
                duration_ms: attempt.duration_ms,
                is_streaming: Some(is_streaming),
                attempt_index: Some(index as u32),
                selected_route_id: Some(attempt.route_id.clone()),
                ..self.base()
            });
        }
    }

    fn gateway_failure(
        &mut self,
        status: u16,
        source: ErrorSource,
        message: &str,
        is_streaming: bool,
    ) {
        self.gateway_failure_at(0, None, status, source, message, is_streaming);
    }

    // Like `gateway_failure`, but placed at an explicit attempt index. Control
    // dedupes reports by (request_id, attempt, status), so a summary row that
    // follows per-attempt rows must sit after them or it collides with (and
    // silently drops) the first attempt's row.
    fn gateway_failure_at(
        &mut self,
        attempt_index: u32,
        selected_route_id: Option<&str>,
        status: u16,
        source: ErrorSource,
        message: &str,
        is_streaming: bool,
    ) {
        self.armed = None;
        self.spawn(PostReport {
            status,
            is_streaming: Some(is_streaming),
            attempt_index: Some(attempt_index),
            selected_route_id: selected_route_id.map(str::to_string),
            error_source: Some(source),
            error_message: Some(truncate(message, 500)),
            ..self.base()
        });
    }

    fn spawn(&self, report: PostReport) {
        spawn_report(&self.control, report);
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        // Reached only when the handler future was dropped between arming and
        // the forward's return: hyper cancels the handler when the client
        // connection closes, so this is the client giving up before the
        // upstream answered — the same client-attributed 499 that
        // `MeterStream` records for a disconnect mid-stream, with no TTFT
        // because no byte ever arrived. The route is the candidate the
        // forwarder was waiting on, when it had reached one.
        let Some(journal) = self.armed.take() else {
            return;
        };
        let in_flight = journal.in_flight();
        self.spawn(PostReport {
            status: 499,
            is_streaming: Some(self.is_streaming),
            attempt_index: Some(in_flight.as_ref().map_or(0, |a| a.attempt_index)),
            selected_route_id: in_flight.map(|a| a.route_id),
            error_message: Some("client disconnected before upstream response".to_string()),
            ..self.base()
        });
    }
}

/// Fire-and-forget delivery of one usage report. Safe to call from a `Drop`
/// that may run outside a Tokio runtime (shutdown teardown), where a bare
/// `tokio::spawn` would panic and abort the process.
fn spawn_report(control: &ControlClient, report: PostReport) {
    let control = control.clone();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            control.consult_post(&report).await;
        });
    }
}

pub(super) fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

// The status reported to the control plane for a normalized upstream error: the
// client-facing status when it is client-attributable (4xx) — a remapped
// image-fetch failure must not count against the provider's health — otherwise
// the raw upstream status, preserving the provider's real code in the logs.
fn reported_status(mapped: u16, upstream_status: u16, upstream_body: &[u8]) -> u16 {
    // A provider refusing because our account with it is unpaid records as
    // 402, whatever status it chose to say that under. The record then names
    // the condition rather than the wording: a provider that reports this as a
    // 429 and one that reports it as a literal 402 are the same row, and the
    // 429 it may have arrived as does not stand for load it is not carrying.
    let recorded = errors::recorded_attempt_status(upstream_status, upstream_body);
    if recorded != upstream_status {
        return recorded;
    }
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
        // §5.3: nothing failed verification — none of the pinned sessions is
        // current, so the client re-fetches the list and re-pins.
        ServiceError::UpstreamVerification(
            UpstreamVerificationError::NoEligibleAttestedSession(_),
        ) => 412,
        ServiceError::UpstreamVerification(_) => 503,
        ServiceError::Upstream(UpstreamError::Routing(_)) => 404,
        // The gateway's own connect/read deadline expired: the upstream was
        // reachable but never answered in time.
        ServiceError::Upstream(UpstreamError::Timeout(_)) => 504,
        // The caller's own input: a malformed nonce, or a Host the gateway
        // has no TLS domain for. Nothing upstream was involved.
        ServiceError::InvalidNonce(_)
        | ServiceError::DownstreamTlsDomainMissing
        | ServiceError::DownstreamTlsDomainUnknown(_) => 400,
        _ => 502,
    }
}

// Map a forward/finalize `ServiceError` to a client-facing generated response.
// E2EE clients still get an encrypted error body, except for `E2ee` errors
// themselves (the E2EE setup failed, so the response cannot be encrypted).
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
    // §7.5: a fail-closed refusal names its §10 type and is receipt-committed,
    // exactly like the direct path (`http/app/backend.rs::refusal_response`).
    if let ServiceError::UpstreamVerification(uv) = &err {
        let error_type = match uv {
            UpstreamVerificationError::NoEligibleAttestedSession(_) => "session_not_accepted",
            _ => "upstream_verification_failed",
        };
        let reason = uv.to_string();
        let body = errors::envelope_bytes(surface, error_type, &reason, Some(request_id));
        return match outcome.service.issue_upstream_refusal_receipt(
            outcome.endpoint_path,
            (!outcome.model.is_empty()).then(|| outcome.model.to_string()),
            outcome.received_body,
            &reason,
            &body,
            outcome.requester.clone(),
        ) {
            Ok(receipt) => {
                let headers = [("x-receipt-id", receipt.receipt_id.clone())];
                finalize_generated(status, body, &headers, e2ee, outcome)
            }
            // The receipt is part of the refusal contract (§5.2); a refusal
            // that cannot be committed is a server error, not a bare 503.
            Err(receipt_err) => {
                let body = errors::envelope_bytes(
                    surface,
                    errors::error_type(surface, 500),
                    &format!("refusal receipt could not be issued: {receipt_err}"),
                    Some(request_id),
                );
                finalize_generated(500, body, &[], e2ee, outcome)
            }
        };
    }
    let body = errors::envelope_bytes(
        surface,
        errors::error_type(surface, status),
        &err.to_string(),
        Some(request_id),
    );
    finalize_generated(status, body, &[], e2ee, outcome)
}

// Build a generated (no-receipt) response, E2EE-encrypting the body when a
// request context is present. If encryption fails it is fail-closed: a generic
// error is returned rather than the cleartext body.
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

// Client response headers, built from nothing. No upstream header reaches the
// client: which provider served a request is ours to know, and a denylist cannot
// keep that promise because a provider can name itself in a header we have never
// seen — for example a header that names the serving provider, or a `set-cookie`
// that would otherwise land on our own domain.
//
// The caller adds what it owns on top: `x-receipt-id`, `x-e2ee-*`, and for
// streaming `x-accel-buffering`/`cache-control`. `x-aci-*` is stamped by an outer
// layer (`http::app::aci_headers_middleware`).
fn gateway_owned_headers(content_type: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    // Ours, not the upstream's: the body we emit is always identity-encoded
    // (re-serialized JSON or a transformed SSE stream), so relaying the
    // upstream's `content-type`/`content-encoding` could mislabel it.
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(CONTENT_TYPE, value);
    }
    headers
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

    fn twin(route_id: &str, format: ProviderFormat) -> RouteCandidate {
        RouteCandidate {
            route_id: route_id.into(),
            format,
            engine: None,
            reasoning_format: None,
            reasoning_policy: None,
        }
    }

    #[test]
    fn client_attributable_forward_errors_map_to_400_and_a_timeout_to_504() {
        // A gateway-enforced upstream deadline is the upstream not answering:
        // 504, so the record lines up with what a downstream proxy saw.
        assert_eq!(
            forward_error_status(&ServiceError::Upstream(UpstreamError::Timeout(
                "read timeout".to_string()
            ))),
            504
        );
        // The caller's own input broke the request; nothing upstream was
        // involved, so it is a 400 (and, being <500, reported to no one).
        assert_eq!(
            forward_error_status(&ServiceError::DownstreamTlsDomainMissing),
            400
        );
        assert_eq!(
            forward_error_status(&ServiceError::DownstreamTlsDomainUnknown("h".to_string())),
            400
        );
    }

    #[test]
    fn conflicting_route_twins_are_dropped_identical_ones_kept() {
        let kept = drop_conflicting_route_twins(vec![
            twin("a:m", ProviderFormat::Anthropic),
            // Identical repeat: an ordered list may name a route twice.
            twin("a:m", ProviderFormat::Anthropic),
            // Conflicting repeat: would make the format lookup ambiguous.
            twin("a:m", ProviderFormat::Openai),
            twin("b:m", ProviderFormat::Openai),
        ]);
        assert_eq!(kept.len(), 3);
        assert!(kept[..2]
            .iter()
            .all(|c| c.format == ProviderFormat::Anthropic));
        assert_eq!(kept[2].route_id, "b:m");
    }

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
