//! Typed accounting and gateway heartbeat scheduling. Only the response owner
//! holds the cancellation guard and settles the request.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_util::Stream;
use serde_json::Value;
use tokio::time::{sleep, Sleep};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{MiddlewareReceiptJournal, ServiceError};
use crate::sse::{EventStream, Record};

use super::control::ControlClient;
use super::pricing;
use super::request_features::SessionKind;
use super::response_transform;
use super::stream_transform::UpstreamUsage;
use super::types::{ErrorClass, ErrorSource, PostReport, SpendMode, TenantIdentity};

/// Terminal classification of a stream: how the response body actually ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
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
            class: Some(
                if matches!(err, ServiceError::Upstream(UpstreamError::Transport(message)) if message == "sse line overflow")
                {
                    ErrorClass::StreamLineOverflow
                } else if matches!(err, ServiceError::Upstream(UpstreamError::Transport(message)) if message == "provider ended mid-event")
                {
                    ErrorClass::StreamTruncated
                } else {
                    ErrorClass::from(err)
                },
            ),
            source,
        }
    }
}

/// Fixed fields for the post-stream usage report; `settle` fills in the rest.
pub(crate) struct StreamReport {
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
    pub session_kind: Option<SessionKind>,
    pub started: Instant,
}

impl StreamReport {
    fn settle(
        &self,
        outcome: Outcome,
        usage: Option<Value>,
        ttft_ms: Option<u64>,
        failure: StreamFailure,
    ) {
        let (error_source, error_class) = if outcome == Outcome::Failed
            || (outcome == Outcome::ClientClosed
                && failure.class == Some(ErrorClass::ClientDisconnected))
        {
            (failure.source, failure.class)
        } else {
            (None, None)
        };
        let report = PostReport {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
            status: metered_status(outcome, self.upstream_status, failure.status),
            duration_ms: response_transform::elapsed_ms(self.started),
            ttft_ms,
            is_streaming: Some(true),
            attempt_index: Some(self.attempt_index),
            selected_route_id: self.selected_route_id.clone(),
            request_model: self.request_model.clone(),
            usage,
            images: None,
            audio: None,
            pricing: self.pricing.clone(),
            spend_mode: self.spend_mode,
            tenant: self.tenant,
            virtual_key_id: self.virtual_key_id,
            error_source,
            error_message: error_class,
            prefix_hash: self.prefix_hash.clone(),
            session_kind: self.session_kind,
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

/// Shared observations; this handle never settles or reports on drop.
#[derive(Clone)]
pub(crate) struct AccountingHandle(Arc<Mutex<AccountingState>>);

struct AccountingState {
    report: StreamReport,
    last_usage: Option<Value>,
    start_usage: Option<Value>,
    upstream_usage: Option<UpstreamUsage>,
    ttft_ms: Option<u64>,
    saw_terminal: bool,
    ingress_terminal: bool,
    saw_error: bool,
    truncated: bool,
    failure: StreamFailure,
    settled: bool,
}

/// The response owner's sole reporting guard. It survives source EOF and all
/// transform stages; its destructor records abandonment, never receipt work.
pub(crate) struct Accounting {
    pub handle: AccountingHandle,
    pending: Option<MiddlewareReceiptJournal>,
}
impl Accounting {
    pub fn new(report: StreamReport, pending: Option<MiddlewareReceiptJournal>) -> Self {
        Self {
            handle: AccountingHandle(Arc::new(Mutex::new(AccountingState {
                report,
                last_usage: None,
                start_usage: None,
                upstream_usage: None,
                ttft_ms: None,
                saw_terminal: false,
                ingress_terminal: false,
                saw_error: false,
                truncated: false,
                failure: StreamFailure::default(),
                settled: false,
            }))),
            pending,
        }
    }
    pub fn settle(&mut self, clean_eof: bool, downstream_failure: bool) {
        let mut state = self.handle.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.settled {
            return;
        }
        let outcome = if downstream_failure || state.saw_error {
            Outcome::Failed
        } else if state.saw_terminal || (clean_eof && !state.truncated) {
            Outcome::Completed
        } else {
            Outcome::Failed
        };
        if downstream_failure {
            state.failure = StreamFailure {
                status: 502,
                class: Some(ErrorClass::DownstreamFinalizerFailed),
                source: Some(ErrorSource::Gateway),
            };
        } else if outcome == Outcome::Failed && state.failure.class.is_none() {
            state.failure.class = Some(if state.saw_error {
                ErrorClass::StreamInbandError
            } else {
                ErrorClass::StreamTruncated
            });
        }
        state.emit(outcome);
    }
}
impl Drop for Accounting {
    fn drop(&mut self) {
        let mut state = self.handle.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.settled {
            return;
        }
        if state.report.selected_route_id.is_none() {
            state.failure = StreamFailure {
                class: Some(ErrorClass::ClientDisconnected),
                ..StreamFailure::default()
            };
            if let Some(journal) = &self.pending {
                let abandoned = journal.abandoned_attempts();
                for (index, attempt) in abandoned.iter().enumerate().filter(|(_, a)| a.status != 0)
                {
                    state.report_attempt(
                        &attempt.route_id,
                        index as u32,
                        attempt.status,
                        attempt.duration_ms,
                    );
                }
                let in_flight = journal.in_flight();
                state.report.attempt_index = in_flight
                    .as_ref()
                    .map_or(abandoned.len() as u32, |a| a.attempt_index);
                state.report.selected_route_id = in_flight.map(|a| a.route_id);
            }
        }
        state.emit(Outcome::ClientClosed);
    }
}
impl AccountingState {
    fn emit(&mut self, outcome: Outcome) {
        self.settled = true;
        let usage = self.upstream_usage.as_ref().map_or_else(
            || self.last_usage.take(),
            |slot| slot.lock().unwrap_or_else(PoisonError::into_inner).take(),
        );
        self.report
            .settle(outcome, usage, self.ttft_ms, self.failure);
    }
    fn report_attempt(&self, route: &str, index: u32, status: u16, duration_ms: u64) {
        // Attempt rows have no usage and do not settle the response owner.
        let report = PostReport {
            request_id: self.report.request_id.clone(),
            endpoint: self.report.endpoint.clone(),
            status,
            duration_ms,
            ttft_ms: None,
            is_streaming: Some(true),
            attempt_index: Some(index),
            selected_route_id: Some(route.into()),
            request_model: self.report.request_model.clone(),
            usage: None,
            images: None,
            audio: None,
            pricing: self.report.pricing.clone(),
            spend_mode: self.report.spend_mode,
            tenant: self.report.tenant,
            virtual_key_id: self.report.virtual_key_id,
            error_source: None,
            error_message: None,
            prefix_hash: self.report.prefix_hash.clone(),
            session_kind: self.report.session_kind,
        };
        let control = self.report.control.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                control.consult_post(&report).await;
            });
        }
    }
}
impl AccountingHandle {
    pub(crate) fn first_byte(&self) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.ttft_ms.is_none() {
            state.ttft_ms = Some(response_transform::elapsed_ms(state.report.started));
        }
    }
    /// Capture protocol facts before any stage can filter or reshape a record.
    pub(crate) fn ingress(&self, record: &mut Record) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.truncated |= record.residual;
        if let Some(event) = &mut record.event {
            event.after_terminal = state.ingress_terminal;
            // Preserve classification for sanitation and client representation.
            // Only the owner decides whether it can still affect the outcome.
            if !event.after_terminal {
                state.saw_error |= event.meaning.failure.is_some();
                state.ingress_terminal |= event.meaning.terminal.is_some();
            }
        }
    }
    pub(crate) fn protocol_facts(&self) -> (bool, bool, bool) {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        (state.saw_terminal, state.saw_error, state.truncated)
    }
    pub fn selected(&self, route: String, attempt: u32, status: u16) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.report.selected_route_id = Some(route);
        state.report.attempt_index = attempt;
        state.report.upstream_status = status;
    }
    pub fn upstream_usage(&self, usage: UpstreamUsage) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upstream_usage = Some(usage);
    }
    pub fn error(&self, error: &ServiceError) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failure = StreamFailure::from_error(error);
    }
    pub fn disarm(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .settled = true;
    }
    pub(crate) fn observe(&self, record: &mut Record) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if record.residual {
            state.truncated = true;
            return;
        }
        if state.ttft_ms.is_none() {
            state.ttft_ms = Some(response_transform::elapsed_ms(state.report.started));
        }
        let Some(event) = &record.event else { return };
        state.saw_terminal |= event.meaning.terminal.is_some();
        state.saw_error |= !event.after_terminal && event.meaning.failure.is_some();
        let event_kind = event.meaning.kind;
        record.edit_json(|parsed| {
            if event_kind == crate::sse_protocol::EventKind::MessageStart {
                if let Some(usage) = parsed
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .filter(|u| u.is_object())
                {
                    state.start_usage = Some(usage.clone());
                }
            }
            let start_usage = (event_kind == crate::sse_protocol::EventKind::MessageDelta)
                .then(|| state.start_usage.clone())
                .flatten();
            let top_usage = parsed.get("usage").is_some_and(|u| !u.is_null());
            let usage = if top_usage {
                parsed.get_mut("usage")
            } else {
                parsed.get_mut("response").and_then(|r| r.get_mut("usage"))
            };
            let Some(usage) = usage.filter(|u| !u.is_null()) else {
                return;
            };
            let _ = response_transform::normalize_reasoning_usage_value(usage);
            let billed =
                start_usage.map_or_else(|| usage.clone(), |start| complete_usage(start, usage));
            state.last_usage = Some(billed.clone());
            if let Some(prices) = state.report.pricing.as_ref().filter(|p| !p.is_null()) {
                let upstream = state
                    .upstream_usage
                    .as_ref()
                    .and_then(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).clone());
                let cost = pricing::compute_cost(upstream.as_ref().unwrap_or(&billed), prices);
                if let Some(fields) = usage.as_object_mut() {
                    fields.insert("cost".into(), pricing::cost_to_json(cost));
                }
            }
        });
    }
}

pub(crate) struct MeterEvents {
    inner: EventStream,
    accounting: AccountingHandle,
}
impl MeterEvents {
    pub fn new(inner: EventStream, accounting: AccountingHandle) -> Self {
        Self { inner, accounting }
    }
}
impl Stream for MeterEvents {
    type Item = Result<Record, ServiceError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(mut record))) => {
                this.accounting.observe(&mut record);
                Poll::Ready(Some(Ok(record)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.accounting.error(&error);
                Poll::Ready(Some(Err(error)))
            }
            other => other,
        }
    }
}

fn complete_usage(mut start: Value, delta: &Value) -> Value {
    if let (Some(merged), Some(delta)) = (start.as_object_mut(), delta.as_object()) {
        for (key, value) in delta {
            let keeps_start = match (merged.get(key).and_then(Value::as_f64), value.as_f64()) {
                (Some(started), Some(latest)) => started > latest,
                _ => value.is_null(),
            };
            if !keeps_start {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    start
}

/// Wraps a stream with an idle keep-alive heartbeat: a `: PROCESSING` SSE comment
/// is emitted when no bytes have flowed for `interval`. `None` disables it.
pub(crate) struct KeepAliveStream {
    inner: EventStream,
    interval: Option<Duration>,
    sleep: Option<Pin<Box<Sleep>>>,
    done: bool,
}

impl KeepAliveStream {
    pub fn new(inner: EventStream, interval: Option<Duration>) -> Self {
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

impl Stream for KeepAliveStream {
    type Item = Result<Record, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                if item.as_ref().is_ok_and(|record| record.non_sse.is_some()) {
                    this.interval = None;
                    this.sleep = None;
                }
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
                        return Poll::Ready(Some(Ok(Record::heartbeat())));
                    }
                }
                Poll::Pending
            }
        }
    }
}
