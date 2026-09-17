//! Streaming response pipeline: SSE keep-alive and the metering/cost-injection
//! wrapper that sits between the provider stream and the receipt finalizer.
//!
//! Ordering preserves receipt integrity: the provider stream drafts
//! `response.received` as it is read; the cost injection and keep-alive here run
//! after that drafting and before the finalizer hashes `response.returned`, so
//! the receipt reflects exactly the client-visible bytes (heartbeats + cost
//! included). Metering wraps the provider stream directly, inside the keep-alive,
//! so it only ever parses real upstream SSE bytes — never an injected heartbeat.
//!
//! Stateful cross-format SSE transforms (Anthropic↔OpenAI) are a later step; this
//! module handles native passthrough plus metering, which covers same-format
//! streaming.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use futures_util::Stream;
use serde_json::Value;
use tokio::time::{sleep, Sleep};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{ServiceError, ServiceResponseStream};
use crate::sse_protocol::SseProtocol;

use super::control::ControlClient;
use super::pricing;
use super::response_transform;
use super::types::{ErrorClass, ErrorSource, PostReport, SpendMode, TenantIdentity};

/// Cap on the partial-line reassembly buffer. An upstream that streams bytes
/// without ever sending a `\n` would otherwise grow it without bound until the
/// request OOMs. A single SSE line above this is pathological (legitimate events
/// — even large tool-call arguments or embedded data — stay far below it), so we
/// treat the overflow as an upstream failure rather than keep buffering.
pub(super) const MAX_SSE_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Terminal classification of a stream: how the response body actually ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    ClientClosed,
}

/// Map a stream outcome onto the recorded status: client disconnect → 499, a
/// failure (broken/truncated/in-band error in a 200 stream) → the failure's
/// own status (502, or 504 when the gateway's read deadline cut the stream),
/// otherwise the raw upstream status.
fn metered_status(outcome: Outcome, upstream_status: u16, failure_status: u16) -> u16 {
    match outcome {
        Outcome::ClientClosed => 499,
        Outcome::Failed => failure_status,
        Outcome::Completed => upstream_status,
    }
}

/// How a stream failed, for the usage report: the status to record, the class
/// of the failure, and which component broke when it was not the upstream. A
/// stream that ends any other way carries the default.
#[derive(Debug, Clone, Copy)]
pub(super) struct StreamFailure {
    pub status: u16,
    pub class: Option<ErrorClass>,
    pub source: Option<ErrorSource>,
}

impl Default for StreamFailure {
    fn default() -> Self {
        Self {
            status: 502,
            class: None,
            source: None,
        }
    }
}

impl StreamFailure {
    /// Classify the error that ended the stream. A gateway-enforced read
    /// deadline is 504; any other upstream failure 502; an error raised by a
    /// gateway stage ahead of the meter (receipt drafting, a transform) is
    /// still 502 but attributed to the gateway so it does not count against
    /// the serving route.
    fn from_error(err: &ServiceError) -> Self {
        let (status, source) = match err {
            ServiceError::Upstream(UpstreamError::Timeout(_)) => (504, None),
            ServiceError::Upstream(_) => (502, None),
            _ => (502, Some(ErrorSource::Gateway)),
        };
        Self {
            status,
            class: Some(ErrorClass::from(err)),
            source,
        }
    }
}

/// Fixed fields for the post-stream usage report; `settle` fills in the rest.
pub struct StreamReport {
    pub control: ControlClient,
    pub request_id: String,
    pub endpoint: String,
    pub request_model: String,
    pub pricing: Option<Value>,
    pub spend_mode: Option<SpendMode>,
    pub tenant: TenantIdentity,
    pub virtual_key_id: Option<i64>,
    pub selected_route_id: Option<String>,
    pub attempt_index: u32,
    pub upstream_status: u16,
    /// Echoed on the settle report; see `PostReport::prefix_hash`.
    pub prefix_hash: Option<String>,
    pub started: Instant,
    /// Set by the response-body wrapper when the downstream finalizer
    /// (receipt drafting / E2EE) errors mid-consumption. The meter's drop then
    /// settles the stream as an internal failure instead of misattributing the
    /// teardown to the client.
    pub downstream_abort: Arc<AtomicBool>,
    /// Set once this report has settled. Lets the response-body wrapper tell
    /// whether a finalizer error arrived after a clean end-of-stream (the
    /// meter has already settled Completed and will not emit again) so it can
    /// record the failure itself.
    pub settled: Arc<AtomicBool>,
}

impl StreamReport {
    fn settle(
        &self,
        outcome: Outcome,
        usage: Option<Value>,
        ttft_ms: Option<u64>,
        failure: StreamFailure,
    ) {
        self.settled.store(true, Ordering::Relaxed);
        // A Failed settle caused by the downstream finalizer is a gateway
        // failure, not the upstream's: attribute it so health scoring does not
        // count it against the serving route.
        let downstream =
            matches!(outcome, Outcome::Failed) && self.downstream_abort.load(Ordering::Relaxed);
        let (error_source, error_class) = if downstream {
            (
                Some(ErrorSource::Gateway),
                Some(ErrorClass::DownstreamFinalizerFailed),
            )
        } else if matches!(outcome, Outcome::Failed) {
            (failure.source, failure.class)
        } else {
            (None, None)
        };
        let report = PostReport {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
            status: metered_status(outcome, self.upstream_status, failure.status),
            duration_ms: self.started.elapsed().as_millis() as u64,
            ttft_ms,
            is_streaming: Some(true),
            attempt_index: Some(self.attempt_index),
            selected_route_id: self.selected_route_id.clone(),
            request_model: self.request_model.clone(),
            usage,
            pricing: self.pricing.clone(),
            spend_mode: self.spend_mode,
            tenant: self.tenant,
            virtual_key_id: self.virtual_key_id,
            error_source,
            error_message: error_class,
            prefix_hash: self.prefix_hash.clone(),
        };
        let control = self.control.clone();
        // Fire-and-forget. Guard against being called from a drop that runs
        // outside a Tokio runtime (e.g. during shutdown teardown), where
        // `tokio::spawn` would panic and abort the process.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                control.consult_post(&report).await;
            });
        }
    }
}

/// Result of feeding one upstream chunk to the meter: bytes ready for the client,
/// a partial line that needs more input, or a line past the cap (end as failure).
enum Fed {
    Emit(Bytes),
    NeedMore,
    Overflow,
}

/// Wraps a client-surface SSE stream: injects `usage.cost`, measures TTFT,
/// classifies the outcome, and fires the usage report exactly once (on clean
/// end, upstream error, or downstream cancel via `Drop`).
pub struct MeterStream {
    inner: ServiceResponseStream,
    report: StreamReport,
    inject: bool,
    started: bool,
    // Holds a partial trailing SSE line across upstream chunks: a `data:` line
    // split at a network boundary must be parsed whole, or its usage/terminal
    // markers are lost. Native same-format passthrough has no event reframer
    // ahead of us, so we buffer at the line level here. The keep-alive is layered
    // outside metering, so only real upstream bytes reach this buffer — a
    // heartbeat comment can never splice into a held line.
    buf: Vec<u8>,
    inner_done: bool,
    last_usage: Option<Value>,
    ttft_ms: Option<u64>,
    saw_error: bool,
    /// The client-facing outcome (Completed / Failed) is read from this shared
    /// observer, byte-accurate about SSE framing, so it agrees with what the
    /// finalizer does to the body. The line parse below stays for cost, TTFT and
    /// in-band error detection.
    framing: crate::sse_framing::SseFramingObserver,
    settled: bool,
    /// Recorded when the inner stream errors, so the settle reports the
    /// failure's own status and class rather than a generic 502.
    failure: StreamFailure,
}

impl MeterStream {
    pub fn new(inner: ServiceResponseStream, report: StreamReport, protocol: SseProtocol) -> Self {
        let inject = report.pricing.as_ref().is_some_and(|p| !p.is_null());
        Self {
            inner,
            report,
            inject,
            started: false,
            failure: StreamFailure::default(),
            buf: Vec::new(),
            inner_done: false,
            last_usage: None,
            ttft_ms: None,
            saw_error: false,
            framing: crate::sse_framing::SseFramingObserver::for_protocol(protocol),
            settled: false,
        }
    }

    /// What the stream amounts to, read from the shared framing observer so it
    /// agrees with what the finalizer does to the body.
    ///
    /// A framing terminal settles it as completed regardless of how the body
    /// ended — a transport error after the terminal does not override a
    /// delivered response. Without one, a clean end of stream at an event
    /// boundary is also completed on any surface: a provider that closed
    /// normally has finished, whether or not it sent the terminator. A transport
    /// error, or a partial or pending tail, is a failure. An in-band error is
    /// always a failure.
    fn protocol_outcome(&self, clean_eof: bool) -> Outcome {
        let f = &self.framing;
        // Two complementary error detectors: the meter's own parse catches
        // error-shaped finish reasons; the framing observer catches framed
        // errors (in-band `error`, `response.failed`, nested `response.error`).
        // Either one fails the stream.
        if self.saw_error || f.saw_error() {
            Outcome::Failed
        } else if f.saw_terminal() || (clean_eof && f.at_event_boundary() && f.observation_usable())
        {
            Outcome::Completed
        } else {
            Outcome::Failed
        }
    }

    fn settle(&mut self, outcome: Outcome) {
        if self.settled {
            return;
        }
        self.settled = true;
        let mut failure = self.failure;
        if outcome == Outcome::Failed && failure.class.is_none() {
            failure.class = Some(if self.saw_error || self.framing.saw_error() {
                ErrorClass::StreamInbandError
            } else {
                ErrorClass::StreamTruncated
            });
        }
        self.report
            .settle(outcome, self.last_usage.take(), self.ttft_ms, failure);
    }

    // Detect in-band error signals, surface-agnostic (works on either the OpenAI
    // or Anthropic shape).
    fn detect_outcome(&mut self, parsed: &Value) {
        let error_value = parsed
            .get("error")
            .filter(|e| !e.is_null())
            .or_else(|| {
                (parsed.get("type").and_then(Value::as_str) == Some("error")).then_some(parsed)
            })
            .or_else(|| {
                parsed
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .filter(|e| !e.is_null())
            });
        // Only the event's presence is noted; its contents are not read.
        if error_value.is_some() {
            self.saw_error = true;
            self.failure
                .class
                .get_or_insert(ErrorClass::StreamInbandError);
        }
        let mut reasons: Vec<&str> = parsed
            .get("choices")
            .and_then(Value::as_array)
            .map(|choices| {
                choices
                    .iter()
                    .filter_map(|c| c.get("finish_reason").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(reason) = parsed
            .get("delta")
            .and_then(|d| d.get("stop_reason"))
            .and_then(Value::as_str)
        {
            reasons.push(reason);
        }
        if reasons
            .iter()
            .any(|reason| *reason == "error" || reason.ends_with("_error"))
        {
            self.saw_error = true;
        }
    }

    // Feed one raw upstream chunk: buffer it and process every now-complete line
    // (a line is complete once its `\n` terminator arrives). Returns the
    // client-facing bytes for the complete portion, `NeedMore` when only a
    // partial line has accumulated (nothing to emit yet, held in `self.buf` until
    // its terminator arrives), or `Overflow` when a line runs past the cap. TTFT
    // is measured per complete line in `transform_lines`, not on the raw chunk, so
    // a comment split across chunks can't be mistaken for the first content.
    fn process(&mut self, bytes: &Bytes) -> Fed {
        self.buf.extend_from_slice(bytes);
        // Enforce the cap after appending and before parsing/holding: reject an
        // oversized complete line (a huge allocate-and-parse) or an oversized
        // still-open trailing run (unbounded growth toward OOM). A backlog of many
        // small complete lines in one chunk is fine — they all drain this pass.
        if self.oversized_line() {
            return Fed::Overflow;
        }
        let Some(nl) = self.buf.iter().rposition(|&b| b == b'\n') else {
            return Fed::NeedMore;
        };
        let complete: Vec<u8> = self.buf.drain(..=nl).collect();
        Fed::Emit(self.transform_lines(complete))
    }

    // True if any single line in the buffer — a complete one or the still-open
    // trailing run — exceeds the cap. Only the per-line length is bounded, never
    // the total: a large chunk of many small complete events is legitimate.
    fn oversized_line(&self) -> bool {
        // Fast path: if the whole buffer fits, no single line can exceed the cap.
        if self.buf.len() <= MAX_SSE_LINE_BYTES {
            return false;
        }
        let mut start = 0;
        for (i, &b) in self.buf.iter().enumerate() {
            if b == b'\n' {
                if i - start > MAX_SSE_LINE_BYTES {
                    return true;
                }
                start = i + 1;
            }
        }
        self.buf.len() - start > MAX_SSE_LINE_BYTES
    }

    // Flush the residual partial line at a clean end of stream: an upstream that
    // ends right after the usage line (no trailing `\n`) still gets it counted.
    fn flush(&mut self) -> Option<Bytes> {
        if self.buf.is_empty() {
            return None;
        }
        let residual = std::mem::take(&mut self.buf);
        Some(self.transform_lines(residual))
    }

    // Parse each `data:` line in a complete byte run: update TTFT/outcome/usage
    // state for the report and inject cost. Returns `raw` verbatim unless a line
    // was rewritten (only rewritten runs are re-encoded).
    fn transform_lines(&mut self, raw: Vec<u8>) -> Bytes {
        let text = String::from_utf8_lossy(&raw);
        let lines: Vec<&str> = text.split('\n').collect();
        let rewritten: Option<String> = {
            let mut changed = false;
            let out_lines: Vec<String> = lines
                .iter()
                .map(|&line| {
                    // TTFT: the first non-empty, non-comment line marks the first
                    // content delivered. Evaluated on complete lines only, so a
                    // comment (`:`-prefixed) split across chunks can't have its
                    // tail fragment counted as content.
                    if self.ttft_ms.is_none() && !line.trim().is_empty() && !line.starts_with(':') {
                        self.ttft_ms = Some(self.report.started.elapsed().as_millis() as u64);
                    }
                    // SSE allows `data:{...}` as well as `data: {...}` (at most
                    // one space after the colon is stripped). Match the field, not
                    // the space, or a space-less line is missed and its usage
                    // dropped. `trim` then covers that space and a trailing CR
                    // from CRLF endings.
                    let Some(data) = line.strip_prefix("data:") else {
                        return line.to_string();
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        return line.to_string();
                    }
                    let Ok(parsed) = serde_json::from_str::<Value>(data) else {
                        return line.to_string();
                    };
                    self.detect_outcome(&parsed);

                    let top_usage = parsed.get("usage").is_some_and(|u| !u.is_null());
                    let nested_usage = parsed
                        .get("response")
                        .and_then(|r| r.get("usage"))
                        .is_some_and(|u| !u.is_null());
                    if !top_usage && !nested_usage {
                        return line.to_string();
                    }

                    let mut updated = parsed;
                    let usage_obj = if top_usage {
                        updated.get_mut("usage")
                    } else {
                        updated.get_mut("response").and_then(|r| r.get_mut("usage"))
                    }
                    .expect("usage presence checked above");
                    let normalized = response_transform::normalize_reasoning_usage_value(usage_obj)
                        .unwrap_or(false);
                    self.last_usage = Some(usage_obj.clone());
                    if !self.inject && !normalized {
                        return line.to_string();
                    }
                    changed = true;
                    if self.inject {
                        let pricing = self
                            .report
                            .pricing
                            .as_ref()
                            .expect("inject implies pricing");
                        let cost = pricing::compute_cost(usage_obj, pricing);
                        if let Some(usage_map) = usage_obj.as_object_mut() {
                            usage_map.insert("cost".to_string(), pricing::cost_to_json(cost));
                        }
                    }
                    format!(
                        "data: {}",
                        serde_json::to_string(&updated).unwrap_or_default()
                    )
                })
                .collect();
            changed.then(|| out_lines.join("\n"))
        };

        match rewritten {
            Some(joined) => Bytes::from(joined),
            None => Bytes::from(raw),
        }
    }
}

impl Stream for MeterStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // Mark the stream as started so a drop before any poll (e.g. the finalizer
        // erroring before it consumes the body) does not report a spurious cancel.
        this.started = true;
        loop {
            if this.inner_done {
                return Poll::Ready(None);
            }
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    this.framing.observe(&bytes);
                    match this.process(&bytes) {
                        Fed::Emit(out) => return Poll::Ready(Some(Ok(out))),
                        // Only a partial line so far: poll again rather than emit a
                        // fragment.
                        Fed::NeedMore => {}
                        // A single line blew past the cap: drop it and end as an
                        // upstream failure rather than buffer or parse it.
                        Fed::Overflow => {
                            this.inner_done = true;
                            this.buf.clear();
                            this.failure
                                .class
                                .get_or_insert(ErrorClass::StreamLineOverflow);
                            let outcome = this.protocol_outcome(false);
                            this.settle(outcome);
                            return Poll::Ready(Some(Err(ServiceError::Upstream(
                                UpstreamError::Transport("sse line overflow".to_string()),
                            ))));
                        }
                    }
                }
                // The truncated residual is dropped, never parsed (a partial
                // line must not synthesize a spurious terminal marker). The
                // error is passed on for the wrapper above to turn into a
                // client-visible one; it never reaches hyper.
                Poll::Ready(Some(Err(err))) => {
                    this.inner_done = true;
                    this.buf.clear();
                    this.failure = StreamFailure::from_error(&err);
                    let outcome = this.protocol_outcome(false);
                    this.settle(outcome);
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(None) => {
                    this.inner_done = true;
                    // Parse the final unterminated line (if any) so a usage
                    // marker in it is still counted, then emit it so no
                    // client-visible bytes are lost. A clean end completes only
                    // where the framing observer says the finalizer can — that
                    // check lives in `protocol_outcome`.
                    let residual = this.flush();
                    let outcome = this.protocol_outcome(true);
                    this.settle(outcome);
                    return match residual {
                        Some(out) => Poll::Ready(Some(Ok(out))),
                        None => Poll::Ready(None),
                    };
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for MeterStream {
    fn drop(&mut self) {
        // A drop after streaming started but before a terminal poll means the
        // downstream consumer went away. A drop before the first poll is the
        // same client disconnect (hyper never got to read the body) unless the
        // pipeline explicitly marked its own teardown via `downstream_abort` —
        // the handler sets that flag around the finalizer hand-off, the one
        // point that can drop the pipeline unpolled for an internal reason.
        // Inferring "internal" from "unpolled" alone would misreport a client
        // that vanishes right after the headers as a gateway failure.
        if !self.started && self.report.downstream_abort.load(Ordering::Relaxed) {
            self.failure = StreamFailure {
                status: 502,
                class: Some(ErrorClass::DownstreamFinalizerFailed),
                source: Some(ErrorSource::Gateway),
            };
            self.settle(Outcome::Failed);
        } else if self.report.downstream_abort.load(Ordering::Relaxed) {
            // The finalizer aborted mid-consumption: an internal failure,
            // not a client disconnect (the settle line carries the
            // `downstream_abort` flag).
            self.settle(Outcome::Failed);
        } else {
            self.settle(Outcome::ClientClosed);
        }
    }
}

/// Wraps a stream with an idle keep-alive heartbeat: a `: PROCESSING` SSE comment
/// is emitted when no bytes have flowed for `interval`. `None` disables it.
pub struct KeepAliveStream {
    inner: ServiceResponseStream,
    interval: Option<Duration>,
    sleep: Option<Pin<Box<Sleep>>>,
    done: bool,
}

impl KeepAliveStream {
    pub fn new(inner: ServiceResponseStream, interval: Option<Duration>) -> Self {
        let sleep = interval.map(|d| Box::pin(sleep(d)));
        Self {
            inner,
            interval,
            sleep,
            done: false,
        }
    }

    fn arm(&mut self) {
        if let (Some(interval), Some(sleep)) = (self.interval, self.sleep.as_mut()) {
            sleep.as_mut().reset(tokio::time::Instant::now() + interval);
        }
    }
}

const KEEP_ALIVE_COMMENT: &[u8] = b": PROCESSING\n\n";

impl Stream for KeepAliveStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                this.arm();
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => {
                if let Some(sleep) = this.sleep.as_mut() {
                    if sleep.as_mut().poll(cx).is_ready() {
                        this.arm();
                        return Poll::Ready(Some(Ok(Bytes::from_static(KEEP_ALIVE_COMMENT))));
                    }
                }
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::config::MiddlewareConfig;

    #[test]
    fn metered_status_mapping() {
        assert_eq!(metered_status(Outcome::Completed, 200, 502), 200);
        assert_eq!(metered_status(Outcome::Failed, 200, 502), 502);
        // The gateway's own read deadline cutting a stream is a 504.
        assert_eq!(metered_status(Outcome::Failed, 200, 504), 504);
        assert_eq!(metered_status(Outcome::ClientClosed, 200, 502), 499);
    }

    // A clean end of stream and a transport error are the dividing line, not a
    // per-choice finish reason. Chat completes on a clean end even without
    // `[DONE]` (the gateway supplies it); a transport error without `[DONE]` is
    // a failure whatever content preceded it.
    #[tokio::test]
    async fn the_outcome_turns_on_clean_end_versus_transport_error() {
        // Feed one chunk, then either end cleanly or break; return the outcome.
        async fn outcome(first: &'static [u8], broken: bool) -> Outcome {
            let mut meter = test_meter();
            let mut chunks: Vec<Result<Bytes, ServiceError>> = vec![Ok(Bytes::from_static(first))];
            if broken {
                chunks.push(Err(ServiceError::Metrics("dropped".to_string())));
            }
            meter.inner = Box::pin(futures_util::stream::iter(chunks));
            let mut errored = false;
            while let Some(chunk) = futures_util::StreamExt::next(&mut meter).await {
                if chunk.is_err() {
                    errored = true;
                    break;
                }
            }
            meter.protocol_outcome(!errored)
        }

        let finish = b"data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n";
        let content = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";

        // Clean end: chat completes with or without a finish reason.
        assert_eq!(
            outcome(b"data: [DONE]\n\n", false).await,
            Outcome::Completed
        );
        assert_eq!(outcome(finish, false).await, Outcome::Completed);
        assert_eq!(
            outcome(content, false).await,
            Outcome::Completed,
            "a chat provider that closed normally has finished"
        );

        // Transport error: only a delivered `[DONE]` keeps it completed.
        assert_eq!(outcome(b"data: [DONE]\n\n", true).await, Outcome::Completed);
        assert_eq!(
            outcome(finish, true).await,
            Outcome::Failed,
            "a finish reason does not rescue a broken connection"
        );
        assert_eq!(outcome(content, true).await, Outcome::Failed);
    }

    // A clean end settles the chat surface as complete only at an event
    // boundary — the same point the finalizer supplies the missing `[DONE]`. A
    // partial line or a pending event (a `data:` line not closed by a blank
    // line) is a truncation and stays Failed.
    // An error the stream reported is a failure however it is framed — even
    // alongside the protocol's terminal. A `response.failed` event, or an
    // error-shaped finish reason followed by `[DONE]`, must not settle Completed.
    #[tokio::test]
    async fn a_reported_error_fails_the_outcome_even_with_a_terminal() {
        async fn drain(meter: &mut MeterStream) -> Outcome {
            while futures_util::StreamExt::next(meter).await.is_some() {}
            meter.protocol_outcome(true)
        }

        let mut responses = test_meter_for(crate::sse_protocol::SseProtocol::OpenaiResponses);
        responses.inner = Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from_static(
            b"data: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"error\":{\"message\":\"x\"}}}\n\n",
        ))]));
        assert_eq!(
            drain(&mut responses).await,
            Outcome::Failed,
            "response.failed is not a completed response"
        );

        // Anthropic→OpenAI transforms an upstream error into an error finish
        // reason plus `[DONE]`; the terminal must not mask it.
        let mut chat = test_meter();
        chat.inner = Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"finish_reason\":\"upstream_error\"}]}\n\ndata: [DONE]\n\n",
        ))]));
        assert_eq!(
            drain(&mut chat).await,
            Outcome::Failed,
            "an error finish reason fails even with [DONE]"
        );
    }

    // A clean end of stream at an event boundary completes on every surface —
    // a provider that closed normally has finished, terminator sent or not.
    #[tokio::test]
    async fn a_clean_end_completes_on_every_surface() {
        use crate::sse_protocol::SseProtocol;
        for (protocol, chunk) in [
            (
                SseProtocol::OpenaiChat,
                &b"data: {\"choices\":[{\"delta\":{}}]}\n\n"[..],
            ),
            (
                SseProtocol::AnthropicMessages,
                &b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n"[..],
            ),
            (
                SseProtocol::OpenaiResponses,
                &b"data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1}\n\n"[..],
            ),
        ] {
            let mut meter = test_meter_for(protocol);
            meter.inner = Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from(chunk))]));
            while futures_util::StreamExt::next(&mut meter).await.is_some() {}
            assert_eq!(
                meter.protocol_outcome(true),
                Outcome::Completed,
                "{protocol:?} clean end at a boundary"
            );
        }
    }

    #[tokio::test]
    async fn chat_clean_eof_completes_only_at_an_event_boundary() {
        async fn eof_outcome(chunk: &'static [u8]) -> Outcome {
            let mut meter = test_meter();
            meter.inner = Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from_static(
                chunk,
            ))]));
            while futures_util::StreamExt::next(&mut meter).await.is_some() {}
            // A clean end: whether it completes is decided inside
            // `protocol_outcome` from the framing observer's boundary state.
            meter.protocol_outcome(true)
        }

        assert_eq!(
            eof_outcome(b"data: {\"choices\":[{\"delta\":{}}]}\n\n").await,
            Outcome::Completed,
            "a complete event, cleanly ended, is completable"
        );
        assert_eq!(
            eof_outcome(b"data: {\"choices\":[{\"delta\":{}}]}\n").await,
            Outcome::Failed,
            "an event never closed by a blank line is pending, not complete"
        );
        assert_eq!(
            eof_outcome(b"data: {\"choices\":[").await,
            Outcome::Failed,
            "a partial line is a truncation"
        );
    }

    fn test_meter() -> MeterStream {
        test_meter_for(crate::sse_protocol::SseProtocol::OpenaiChat)
    }

    fn test_meter_for(protocol: crate::sse_protocol::SseProtocol) -> MeterStream {
        let control = ControlClient::new(&MiddlewareConfig {
            control_url: "http://control.invalid".to_string(),
            control_token: None,
            control_timeout_ms: Some(200),
            control_post_timeout_ms: Some(200),
            sse_keepalive_ms: None,
            send_request_features: None,
            prefix_hash_secret: None,
            tee_only_domains: Vec::new(),
        })
        .unwrap();
        let report = StreamReport {
            control,
            request_id: "req".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            request_model: "m".to_string(),
            pricing: None,
            spend_mode: None,
            tenant: TenantIdentity::default(),
            virtual_key_id: None,
            selected_route_id: None,
            attempt_index: 0,
            upstream_status: 200,
            prefix_hash: None,
            started: Instant::now(),
            downstream_abort: Arc::new(AtomicBool::new(false)),
            settled: Arc::new(AtomicBool::new(false)),
        };
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::empty());
        MeterStream::new(inner, report, protocol)
    }

    // An in-band error event sets the failure class and nothing else: no part
    // of the event is copied into the meter's state.
    #[test]
    fn in_band_error_records_a_class_and_none_of_its_text() {
        let mut meter = test_meter();
        let chunk =
            Bytes::from("data: {\"error\":{\"message\":\"CANARY-in-band\",\"code\":500}}\n");
        assert!(matches!(meter.process(&chunk), Fed::Emit(_)));
        assert!(meter.saw_error);
        assert_eq!(meter.failure.class, Some(ErrorClass::StreamInbandError));
        assert!(
            !format!("{:?}", meter.failure).contains("CANARY"),
            "failure state holds no upstream text"
        );
    }

    // An error-ish finish_reason (no error object) still marks the stream.
    #[test]
    fn error_finish_reason_marks_the_stream_as_errored() {
        let mut meter = test_meter();
        let chunk = Bytes::from(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"upstream_error\"}]}\n",
        );
        assert!(matches!(meter.process(&chunk), Fed::Emit(_)));
        assert!(meter.saw_error);
    }

    // A usage-bearing SSE event split across two upstream chunks must still be
    // parsed whole: the first chunk buffers with nothing to emit, and the usage
    // is captured only once the terminator arrives in the second. Regression for
    // 200 responses logged with 0/0 tokens (report went out with null usage).
    #[test]
    fn split_usage_line_is_reassembled_and_captured() {
        let mut meter = test_meter();
        let a = Bytes::from("data: {\"choices\":[],\"usage\":{\"prompt_toke");
        let b = Bytes::from("ns\":11,\"completion_tokens\":7}}\n\ndata: [DONE]\n\n");

        assert!(
            matches!(meter.process(&a), Fed::NeedMore),
            "a partial line emits nothing until its terminator arrives"
        );
        let Fed::Emit(out) = meter.process(&b) else {
            panic!("completing the line emits bytes");
        };

        let usage = meter
            .last_usage
            .as_ref()
            .expect("usage captured across split");
        assert_eq!(usage["prompt_tokens"], 11);
        assert_eq!(usage["completion_tokens"], 7);

        // Byte fidelity: the buffered chunk is emitted intact, nothing dropped.
        assert_eq!(out.as_ref(), [a.as_ref(), b.as_ref()].concat().as_slice());
    }

    #[test]
    fn legacy_reasoning_usage_is_normalized_without_rejecting_non_json_data() {
        let mut meter = test_meter();
        let chunk = Bytes::from(concat!(
            "data: {\"choices\":[],\"usage\":{\"reasoning_tokens\":5,\"completion_tokens_details\":null}}\n",
            "data: ping\n",
        ));
        let Fed::Emit(out) = meter.process(&chunk) else {
            panic!("complete lines emit bytes");
        };
        let text = String::from_utf8(out.to_vec()).unwrap();

        assert!(
            text.contains("\"completion_tokens_details\":{\"reasoning_tokens\":5}"),
            "{text}"
        );
        assert!(text.contains("data: ping\n"), "{text}");
        assert_eq!(
            meter.last_usage.as_ref().unwrap()["completion_tokens_details"]["reasoning_tokens"],
            5
        );
    }

    // `data:{...}` without the optional space is valid SSE; missing it would drop
    // the usage and misrecord the clean `[DONE]` end as a 502.
    #[test]
    fn data_line_without_space_is_parsed() {
        let mut meter = test_meter();
        let chunk = Bytes::from(
            "data:{\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":9}}\n\ndata:[DONE]\n\n",
        );
        assert!(matches!(meter.process(&chunk), Fed::Emit(_)));

        let usage = meter.last_usage.as_ref().expect("usage captured");
        assert_eq!(usage["prompt_tokens"], 3);
        assert_eq!(usage["completion_tokens"], 9);
    }

    // A complete short line followed by an oversized still-open trailing run must
    // overflow: the cap is checked per line after appending, so the huge tail is
    // rejected before it is parsed or held across polls — not silently retained
    // because an earlier line in the same chunk was complete.
    #[test]
    fn oversized_trailing_line_overflows_despite_a_complete_line() {
        let mut meter = test_meter();
        let mut chunk = b"data: {}\n".to_vec();
        chunk.extend(std::iter::repeat_n(b'x', MAX_SSE_LINE_BYTES + 1));
        assert!(matches!(meter.process(&Bytes::from(chunk)), Fed::Overflow));
    }

    // A comment line split across chunks must not set TTFT from its tail fragment:
    // TTFT is evaluated per complete line, so only real content triggers it.
    #[test]
    fn split_comment_does_not_trigger_ttft() {
        let mut meter = test_meter();
        // A `: keep-alive` comment cut mid-word across two chunks.
        assert!(matches!(
            meter.process(&Bytes::from(": keep-al")),
            Fed::NeedMore
        ));
        assert!(matches!(meter.process(&Bytes::from("ive\n")), Fed::Emit(_)));
        assert!(meter.ttft_ms.is_none(), "a comment is not first content");

        // The first real event now sets it.
        assert!(matches!(
            meter.process(&Bytes::from("data: {\"choices\":[]}\n")),
            Fed::Emit(_)
        ));
        assert!(meter.ttft_ms.is_some(), "first content sets TTFT");
    }
}
