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
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use futures_util::Stream;
use serde_json::Value;
use tokio::time::{sleep, Sleep};

use crate::aggregator::service::{ServiceError, ServiceResponseStream};

use super::control::ControlClient;
use super::pricing;
use super::types::{PostReport, SpendMode};

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

/// Map a stream outcome onto the recorded status: client disconnect → 499, any
/// failure (broken/truncated/in-band error in a 200 stream) → 502, otherwise the
/// raw upstream status.
fn metered_status(outcome: Outcome, upstream_status: u16) -> u16 {
    match outcome {
        Outcome::ClientClosed => 499,
        Outcome::Failed => 502,
        Outcome::Completed => upstream_status,
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
    pub user_id: Option<i64>,
    pub virtual_key_id: Option<i64>,
    pub selected_route_id: Option<String>,
    pub attempt_index: u32,
    pub upstream_status: u16,
    pub started: Instant,
}

impl StreamReport {
    fn settle(&self, outcome: Outcome, usage: Option<Value>, ttft_ms: Option<u64>) {
        let report = PostReport {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
            status: metered_status(outcome, self.upstream_status),
            duration_ms: self.started.elapsed().as_millis() as u64,
            ttft_ms,
            is_streaming: Some(true),
            attempt_index: Some(self.attempt_index),
            selected_route_id: self.selected_route_id.clone(),
            request_model: self.request_model.clone(),
            usage,
            pricing: self.pricing.clone(),
            spend_mode: self.spend_mode,
            user_id: self.user_id,
            virtual_key_id: self.virtual_key_id,
            error_source: None,
            error_message: None,
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
    saw_terminal: bool,
    saw_error: bool,
    settled: bool,
}

impl MeterStream {
    pub fn new(inner: ServiceResponseStream, report: StreamReport) -> Self {
        let inject = report.pricing.as_ref().is_some_and(|p| !p.is_null());
        Self {
            inner,
            report,
            inject,
            started: false,
            buf: Vec::new(),
            inner_done: false,
            last_usage: None,
            ttft_ms: None,
            saw_terminal: false,
            saw_error: false,
            settled: false,
        }
    }

    fn settle(&mut self, outcome: Outcome) {
        if self.settled {
            return;
        }
        self.settled = true;
        self.report
            .settle(outcome, self.last_usage.take(), self.ttft_ms);
    }

    // Detect in-band terminal/error signals, surface-agnostic (works on either
    // the OpenAI or Anthropic shape).
    fn detect_outcome(&mut self, parsed: &Value) {
        if parsed.get("error").is_some_and(|e| !e.is_null())
            || parsed.get("type").and_then(Value::as_str) == Some("error")
            || parsed
                .get("response")
                .and_then(|r| r.get("error"))
                .is_some_and(|e| !e.is_null())
        {
            self.saw_error = true;
        }
        let response_status = parsed
            .get("response")
            .and_then(|r| r.get("status"))
            .and_then(Value::as_str);
        if parsed.get("type").and_then(Value::as_str) == Some("message_stop")
            || matches!(response_status, Some("completed") | Some("incomplete"))
        {
            self.saw_terminal = true;
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
        for reason in reasons {
            if !reason.is_empty() {
                self.saw_terminal = true;
                if reason == "error" || reason.ends_with("_error") {
                    self.saw_error = true;
                }
            }
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
        let rewritten: Option<String> = {
            let text = String::from_utf8_lossy(&raw);
            let mut changed = false;
            let out_lines: Vec<String> = text
                .split('\n')
                .map(|line| {
                    // TTFT: the first non-empty, non-comment line marks the first
                    // content delivered. Evaluated on complete lines only, so a
                    // comment (`:`-prefixed) split across chunks can't have its
                    // tail fragment counted as content.
                    if self.ttft_ms.is_none() && !line.trim().is_empty() && !line.starts_with(':') {
                        self.ttft_ms = Some(self.report.started.elapsed().as_millis() as u64);
                    }
                    // SSE allows `data:{...}` as well as `data: {...}` (at most
                    // one space after the colon is stripped). Match the field, not
                    // the space, or a space-less line is missed — dropping its
                    // usage/terminal and misrecording a clean end as a 502. `trim`
                    // then covers that space and a trailing CR from CRLF endings.
                    let Some(data) = line.strip_prefix("data:") else {
                        return line.to_string();
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        self.saw_terminal = true;
                        return line.to_string();
                    }
                    let Ok(parsed) = serde_json::from_str::<Value>(data) else {
                        return line.to_string();
                    };
                    self.detect_outcome(&parsed);

                    let top_usage = parsed.get("usage").filter(|u| !u.is_null());
                    let nested = parsed
                        .get("response")
                        .and_then(|r| r.get("usage"))
                        .filter(|u| !u.is_null());
                    let Some(usage_obj) = top_usage.or(nested) else {
                        return line.to_string();
                    };
                    self.last_usage = Some(usage_obj.clone());
                    if !self.inject {
                        return line.to_string();
                    }
                    let pricing = self
                        .report
                        .pricing
                        .as_ref()
                        .expect("inject implies pricing");
                    let cost = pricing::compute_cost(usage_obj, pricing);
                    changed = true;

                    let mut updated = parsed.clone();
                    let target = if top_usage.is_some() {
                        updated.get_mut("usage")
                    } else {
                        updated.get_mut("response").and_then(|r| r.get_mut("usage"))
                    };
                    if let Some(usage_map) = target.and_then(Value::as_object_mut) {
                        usage_map.insert("cost".to_string(), pricing::cost_to_json(cost));
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
                Poll::Ready(Some(Ok(bytes))) => match this.process(&bytes) {
                    Fed::Emit(out) => return Poll::Ready(Some(Ok(out))),
                    // Only a partial line so far: poll again rather than emit a
                    // fragment.
                    Fed::NeedMore => {}
                    // A single line blew past the cap: drop it and end as an
                    // upstream failure rather than buffer or parse it.
                    Fed::Overflow => {
                        this.inner_done = true;
                        this.buf.clear();
                        this.settle(Outcome::Failed);
                        return Poll::Ready(None);
                    }
                },
                // An upstream break ends the client stream cleanly and records a
                // failure rather than propagating the error downstream. The
                // truncated residual is dropped, never parsed (a partial line
                // must not synthesize a spurious terminal marker).
                Poll::Ready(Some(Err(_))) => {
                    this.inner_done = true;
                    this.buf.clear();
                    this.settle(Outcome::Failed);
                    return Poll::Ready(None);
                }
                Poll::Ready(None) => {
                    this.inner_done = true;
                    // Parse the final unterminated line (if any) before settling so
                    // a usage or terminal marker in it is still counted, then emit
                    // it so no client-visible bytes are lost.
                    let residual = this.flush();
                    let outcome = if this.saw_terminal && !this.saw_error {
                        Outcome::Completed
                    } else {
                        Outcome::Failed
                    };
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
        // downstream consumer went away. A drop before the first poll (the stream
        // was never consumed, e.g. the finalizer errored) is not a client cancel.
        if self.started {
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
        assert_eq!(metered_status(Outcome::Completed, 200), 200);
        assert_eq!(metered_status(Outcome::Failed, 200), 502);
        assert_eq!(metered_status(Outcome::ClientClosed, 200), 499);
    }

    fn test_meter() -> MeterStream {
        let control = ControlClient::new(&MiddlewareConfig {
            control_url: "http://control.invalid".to_string(),
            control_token: None,
            control_timeout_ms: Some(200),
            control_post_timeout_ms: Some(200),
            sse_keepalive_ms: None,
        })
        .unwrap();
        let report = StreamReport {
            control,
            request_id: "req".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            request_model: "m".to_string(),
            pricing: None,
            spend_mode: None,
            user_id: None,
            virtual_key_id: None,
            selected_route_id: None,
            attempt_index: 0,
            upstream_status: 200,
            started: Instant::now(),
        };
        let inner: ServiceResponseStream = Box::pin(futures_util::stream::empty());
        MeterStream::new(inner, report)
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
        assert!(meter.saw_terminal, "[DONE] recorded as a clean terminal");

        // Byte fidelity: the buffered chunk is emitted intact, nothing dropped.
        assert_eq!(out.as_ref(), [a.as_ref(), b.as_ref()].concat().as_slice());
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
        assert!(meter.saw_terminal, "space-less [DONE] is a clean terminal");
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
