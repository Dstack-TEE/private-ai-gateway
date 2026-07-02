//! Streaming response pipeline: SSE keep-alive and the metering/cost-injection
//! wrapper that sits between the provider stream and the receipt finalizer.
//!
//! Ordering preserves receipt integrity: the provider stream drafts
//! `response.received` as it is read; the keep-alive and cost injection here run
//! after that drafting and before the finalizer hashes `response.returned`, so
//! the receipt reflects exactly the client-visible bytes (heartbeats + cost
//! included).
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

/// Wraps a client-surface SSE stream: injects `usage.cost`, measures TTFT,
/// classifies the outcome, and fires the usage report exactly once (on clean
/// end, upstream error, or downstream cancel via `Drop`).
pub struct MeterStream {
    inner: ServiceResponseStream,
    report: StreamReport,
    inject: bool,
    started: bool,
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

    // Process one chunk: update TTFT/outcome state and inject cost. Returns the
    // original bytes unless a `data:` line was rewritten: only modified chunks
    // are re-encoded, the rest pass through verbatim.
    fn process(&mut self, bytes: &Bytes) -> Bytes {
        let text = String::from_utf8_lossy(bytes);
        let lines: Vec<&str> = text.split('\n').collect();

        if self.ttft_ms.is_none()
            && lines
                .iter()
                .any(|line| !line.trim().is_empty() && !line.starts_with(':'))
        {
            self.ttft_ms = Some(self.report.started.elapsed().as_millis() as u64);
        }

        let mut rewritten = false;
        let out_lines: Vec<String> = lines
            .iter()
            .map(|line| {
                let Some(data) = line.strip_prefix("data: ") else {
                    return (*line).to_string();
                };
                let data = data.trim();
                if data == "[DONE]" {
                    self.saw_terminal = true;
                    return (*line).to_string();
                }
                let Ok(parsed) = serde_json::from_str::<Value>(data) else {
                    return (*line).to_string();
                };
                self.detect_outcome(&parsed);

                let top_usage = parsed.get("usage").filter(|u| !u.is_null());
                let nested = parsed
                    .get("response")
                    .and_then(|r| r.get("usage"))
                    .filter(|u| !u.is_null());
                let Some(usage_obj) = top_usage.or(nested) else {
                    return (*line).to_string();
                };
                self.last_usage = Some(usage_obj.clone());
                if !self.inject {
                    return (*line).to_string();
                }
                let pricing = self
                    .report
                    .pricing
                    .as_ref()
                    .expect("inject implies pricing");
                let cost = pricing::compute_cost(usage_obj, pricing);
                rewritten = true;

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

        if rewritten {
            Bytes::from(out_lines.join("\n"))
        } else {
            bytes.clone()
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
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                let out = this.process(&bytes);
                Poll::Ready(Some(Ok(out)))
            }
            // An upstream break ends the client stream cleanly and records a
            // failure rather than propagating the error downstream.
            Poll::Ready(Some(Err(_))) => {
                this.settle(Outcome::Failed);
                Poll::Ready(None)
            }
            Poll::Ready(None) => {
                let outcome = if this.saw_terminal && !this.saw_error {
                    Outcome::Completed
                } else {
                    Outcome::Failed
                };
                this.settle(outcome);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
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

/// The still-running forward behind an early-committed response: resolves to
/// the client-surface stream, or to the prepared in-band error event bytes
/// when no candidate served.
pub type EarlyCommitFuture =
    Pin<Box<dyn Future<Output = Result<ServiceResponseStream, Bytes>> + Send>>;

enum EarlyCommitState {
    Waiting(EarlyCommitFuture),
    Streaming(ServiceResponseStream),
    Done,
}

/// The body of a streaming response that was committed before the upstream
/// resolved: emits an immediate `: PROCESSING` comment (the client's first
/// byte), then hands over to the resolved stream — or, if every candidate
/// failed, emits the prepared in-band error event and ends.
///
/// Idle-wait heartbeats are NOT produced here: the caller wraps this stream in
/// a [`KeepAliveStream`], which covers the waiting period the same way it
/// covers mid-stream stalls. Sits inside the receipt finalizer like every
/// other client-visible byte source, so the comment and the in-band error are
/// hashed into `response.returned`.
pub struct EarlyCommitStream {
    state: EarlyCommitState,
    sent_initial: bool,
}

impl EarlyCommitStream {
    pub fn new(forward: EarlyCommitFuture) -> Self {
        Self {
            state: EarlyCommitState::Waiting(forward),
            sent_initial: false,
        }
    }
}

impl Stream for EarlyCommitStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                EarlyCommitState::Waiting(forward) => {
                    // The commit itself already spent the grace window in
                    // silence; give the client its first byte right away.
                    if !this.sent_initial {
                        this.sent_initial = true;
                        return Poll::Ready(Some(Ok(Bytes::from_static(KEEP_ALIVE_COMMENT))));
                    }
                    match forward.as_mut().poll(cx) {
                        Poll::Ready(Ok(stream)) => {
                            this.state = EarlyCommitState::Streaming(stream);
                        }
                        Poll::Ready(Err(event)) => {
                            this.state = EarlyCommitState::Done;
                            return Poll::Ready(Some(Ok(event)));
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }
                EarlyCommitState::Streaming(stream) => return stream.as_mut().poll_next(cx),
                EarlyCommitState::Done => return Poll::Ready(None),
            }
        }
    }
}

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
    use futures_util::StreamExt;

    #[test]
    fn metered_status_mapping() {
        assert_eq!(metered_status(Outcome::Completed, 200), 200);
        assert_eq!(metered_status(Outcome::Failed, 200), 502);
        assert_eq!(metered_status(Outcome::ClientClosed, 200), 499);
    }

    #[tokio::test(start_paused = true)]
    async fn early_commit_stream_heartbeats_delegates_and_surfaces_errors() {
        // Pending forward, wrapped in KeepAliveStream as the caller composes
        // it: the commit emits an immediate comment, the keep-alive layer
        // heartbeats the idle wait, and the resolved stream takes over.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let forward: EarlyCommitFuture = Box::pin(async move {
            let _ = rx.await;
            let inner: ServiceResponseStream = Box::pin(futures_util::stream::iter(vec![Ok(
                Bytes::from_static(b"data: {}\n\n"),
            )]));
            Ok(inner)
        });
        let mut stream = KeepAliveStream::new(
            Box::pin(EarlyCommitStream::new(forward)),
            Some(Duration::from_secs(10)),
        );
        assert_eq!(
            stream.next().await.unwrap().unwrap().as_ref(),
            KEEP_ALIVE_COMMENT,
            "first byte must be an immediate comment"
        );
        assert_eq!(
            stream.next().await.unwrap().unwrap().as_ref(),
            KEEP_ALIVE_COMMENT,
            "idle wait emits keep-alive heartbeats"
        );
        tx.send(()).unwrap();
        assert_eq!(
            stream.next().await.unwrap().unwrap().as_ref(),
            b"data: {}\n\n"
        );
        assert!(stream.next().await.is_none());

        // Failed forward: the prepared in-band error event terminates the stream.
        let forward: EarlyCommitFuture =
            Box::pin(async move { Err(Bytes::from_static(b"data: {\"error\":{}}\n\n")) });
        let mut stream = EarlyCommitStream::new(forward);
        assert_eq!(
            stream.next().await.unwrap().unwrap().as_ref(),
            KEEP_ALIVE_COMMENT
        );
        assert_eq!(
            stream.next().await.unwrap().unwrap().as_ref(),
            b"data: {\"error\":{}}\n\n"
        );
        assert!(stream.next().await.is_none());
    }
}
