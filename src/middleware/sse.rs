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
use crate::sse_protocol::{sse_protocol, EventKind, SseProtocol};

use super::control::ControlClient;
use super::pricing;
use super::request_features::SessionKind;
use super::response_transform;
use super::stream_transform::UpstreamUsage;
use super::types::{ErrorClass, ErrorSource, PostReport, SpendMode, TenantIdentity};

/// How a stream failed, for the usage report: the status to record, the class
/// of the failure, and which component broke when it was not the upstream. A
/// stream that ends any other way carries the default.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamFailure {
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
    pub(crate) fn from_error(err: &ServiceError) -> Self {
        let (status, source) = match err {
            ServiceError::Upstream(UpstreamError::Timeout(_)) => (504, None),
            ServiceError::Upstream(_) => (502, None),
            _ => (502, Some(ErrorSource::Gateway)),
        };
        Self {
            status,
            class: Some(match err {
                ServiceError::Upstream(UpstreamError::SseLineOverflow) => {
                    ErrorClass::StreamLineOverflow
                }
                ServiceError::Upstream(UpstreamError::SseTruncated) => ErrorClass::StreamTruncated,
                _ => ErrorClass::from(err),
            }),
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
        status: u16,
        usage: Option<Value>,
        ttft_ms: Option<u64>,
        failure: Option<StreamFailure>,
    ) {
        let report = PostReport {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
            status,
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
            error_source: failure.and_then(|failure| failure.source),
            error_message: failure.and_then(|failure| failure.class),
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

/// A bridge can observe usage without emitting a destination event for it.
pub(super) fn observe_bridge_usage(slot: &UpstreamUsage, mut usage: Value, protocol: SseProtocol) {
    filter_observed_usage(Some(&mut usage), protocol, None);
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(usage);
}

fn filter_observed_usage(usage: Option<&mut Value>, protocol: SseProtocol, endpoint: Option<&str>) {
    if endpoint == Some(crate::aggregator::service::SYSTEMONE_PATH) {
        response_transform::filter_systemone_usage(usage);
    } else {
        response_transform::filter_usage(usage, protocol);
    }
}

struct AccountingState {
    report: StreamReport,
    last_usage: Option<Value>,
    start_usage: Option<Value>,
    chat_start_usage: Option<Value>,
    upstream_usage: Option<UpstreamUsage>,
    bridge: Option<super::stream_transform::BridgeHandle>,
    ttft_ms: Option<u64>,
    ingress_terminal: bool,
    failure_terminal: bool,
    continuation_failure: Option<StreamFailure>,
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
                chat_start_usage: None,
                upstream_usage: None,
                ttft_ms: None,
                ingress_terminal: false,
                failure_terminal: false,
                continuation_failure: None,
                bridge: None,
                settled: false,
            }))),
            pending,
        }
    }
    pub fn settle(&mut self, failure: Option<StreamFailure>) {
        let mut state = self.handle.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.settled {
            // Reading to EOF must not replace the first failure that ended the
            // old bridge's accounting. Gateway finalization can still fail.
            let failure = if failure.is_some_and(|f| f.source == Some(ErrorSource::Gateway)) {
                failure
            } else {
                state.continuation_failure.or(failure)
            };
            let status = failure.map_or(state.report.upstream_status, |failure| failure.status);
            state.emit(status, failure);
        }
    }
}
impl Drop for Accounting {
    fn drop(&mut self) {
        let mut state = self.handle.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.settled {
            return;
        }
        let failure = state
            .report
            .selected_route_id
            .is_none()
            .then_some(StreamFailure {
                class: Some(ErrorClass::ClientDisconnected),
                ..StreamFailure::default()
            });
        if failure.is_some() {
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
        state.emit(499, failure);
    }
}
impl AccountingState {
    fn emit(&mut self, status: u16, failure: Option<StreamFailure>) {
        self.settled = true;
        let usage = self.upstream_usage.as_ref().map_or_else(
            || self.last_usage.take(),
            |slot| slot.lock().unwrap_or_else(PoisonError::into_inner).take(),
        );
        self.report.settle(status, usage, self.ttft_ms, failure);
    }
    fn price(&self, usage: &Value, billed: &Value) -> Option<Value> {
        let prices = self.report.pricing.as_ref().filter(|p| !p.is_null())?;
        let upstream = self
            .upstream_usage
            .as_ref()
            .map(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner));
        let cost = pricing::compute_cost(
            upstream
                .as_deref()
                .and_then(Option::as_ref)
                .unwrap_or(billed),
            prices,
        );
        usage.is_object().then(|| pricing::cost_to_json(cost))
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
    /// Retain source phase and usage through conversion. After a failure
    /// terminal, retain the old bridge's billing boundaries without forwarding
    /// records to converters. Source reading can still continue to EOF.
    pub(crate) fn ingress(&self, record: &mut Record) -> bool {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(event) = record.event_mut() else {
            return !state.failure_terminal;
        };
        event.after_terminal = state.ingress_terminal;
        state.ingress_terminal |= event.meaning.terminal.is_some();
        if let Some(usage) = event.payload.json().and_then(message_start_usage) {
            state.start_usage = Some(usage.clone());
            // Match the Messages-to-Chat bridge's usage observations without
            // converting records that arrive after the destination terminal.
            if event.meaning.failure.is_none()
                && event.meaning.terminal.is_none()
                && !matches!(
                    event.meaning.kind,
                    EventKind::Ping | EventKind::ContentBlockStop
                )
            {
                state.chat_start_usage = Some(usage.clone());
            }
        }
        if !state.failure_terminal {
            return true;
        }
        if state.bridge.is_some() {
            if state.continuation_failure.is_some()
                || (event.source_protocol == Some(SseProtocol::OpenaiChat)
                    && state.ingress_terminal)
            {
                return false;
            }
            let ignored_control = event.source_protocol == Some(SseProtocol::AnthropicMessages)
                && matches!(
                    event.meaning.kind,
                    EventKind::Ping | EventKind::ContentBlockStop | EventKind::MessageStop
                );
            if !ignored_control && !event.data.trim().is_empty() && event.payload.json().is_none() {
                state.continuation_failure = Some(StreamFailure::from_error(
                    &ServiceError::Upstream(UpstreamError::Transport(
                        "provider sent an event this surface cannot represent".into(),
                    )),
                ));
                return false;
            }
        }
        // Preserve the old bridges' usage selection in accounting only.
        let failed = event.meaning.failure.is_some();
        let source = event.source_protocol;
        if state.bridge.is_none()
            || failed
            || (source == Some(SseProtocol::OpenaiChat)
                && state.report.endpoint != crate::aggregator::service::COMPLETIONS_PATH)
        {
            if state.bridge.is_none() && !failed {
                record.edit_json(|body| {
                    response_transform::canonicalize_protocol_stream(
                        body,
                        sse_protocol(&state.report.endpoint),
                        Some(&state.report.request_id),
                    )
                });
            }
            drop(state);
            self.observe(record, failed);
        } else if source == Some(SseProtocol::AnthropicMessages)
            && sse_protocol(&state.report.endpoint) == SseProtocol::OpenaiChat
            && event.meaning.kind == EventKind::MessageDelta
            && event.meaning.terminal.is_none()
        {
            if let Some(usage) = event.payload.json().and_then(|body| body.get("usage")) {
                let mut usage = response_transform::chat_usage(
                    state.chat_start_usage.as_ref(),
                    response_transform::i64_field(usage, "output_tokens"),
                );
                filter_observed_usage(Some(&mut usage), SseProtocol::OpenaiChat, None);
                state.last_usage = Some(usage);
            }
        }
        false
    }
    pub(crate) fn failure_terminal(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failure_terminal = true;
    }
    pub fn selected(&self, route: String, attempt: u32, status: u16) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.report.selected_route_id = Some(route);
        state.report.attempt_index = attempt;
        state.report.upstream_status = status;
    }
    pub(crate) fn bridge(&self, bridge: super::stream_transform::BridgeHandle) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).bridge = Some(bridge);
    }
    pub(crate) fn has_bridge(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bridge
            .is_some()
    }
    pub(crate) fn close_bridge(&self) -> super::stream_transform::BridgeClosure {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bridge
            .as_ref()
            .map_or_else(
                Default::default,
                super::stream_transform::BridgeHandle::close,
            )
    }
    pub fn upstream_usage(&self, usage: UpstreamUsage) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upstream_usage = Some(usage);
    }
    pub fn disarm(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .settled = true;
    }
    /// Capture usage and inject cost. The owner decides which records to emit.
    pub(crate) fn observe(&self, record: &mut Record, include_start: bool) -> Option<Value> {
        if matches!(
            record,
            Record::Residual | Record::Heartbeat | Record::NonSse(_)
        ) {
            return None;
        }
        self.first_byte();
        let event = record.event()?;
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let destination = sse_protocol(&state.report.endpoint);
        let source = event.source_protocol;
        let bridged = source.is_some_and(|source| source != destination);
        let mut wire_usage = None;
        record.edit_json(|parsed| {
            // Filter each usage object once, including when it is emitted but
            // another usage object on the same event is selected for billing.
            let filter = |usage: Option<&mut Value>| {
                filter_observed_usage(
                    usage,
                    source.unwrap_or(destination),
                    Some(&state.report.endpoint),
                );
            };
            filter(parsed.get_mut("usage"));
            filter(parsed.get_mut("response").and_then(|r| r.get_mut("usage")));
            if message_start_usage(parsed).is_some() {
                filter(parsed.get_mut("message").and_then(|m| m.get_mut("usage")));
                state.start_usage = message_start_usage(parsed).cloned();
            }
            let message_delta = parsed.get("type").and_then(Value::as_str) == Some("message_delta");
            let top_usage = parsed.get("usage").is_some_and(|u| !u.is_null());
            let usage = if top_usage {
                parsed.get_mut("usage")
            } else if include_start && message_start_usage(parsed).is_some() {
                parsed.get_mut("message").and_then(|m| m.get_mut("usage"))
            } else {
                parsed.get_mut("response").and_then(|r| r.get_mut("usage"))
            };
            let Some(usage) = usage.filter(|u| !u.is_null()) else {
                return;
            };
            response_transform::normalize_reasoning_usage_value(usage);
            let mut billed =
                if message_delta || (bridged && source == Some(SseProtocol::AnthropicMessages)) {
                    state
                        .start_usage
                        .clone()
                        .map_or_else(|| usage.clone(), |start| complete_usage(start, usage))
                } else {
                    usage.clone()
                };
            if bridged {
                billed = match (source, destination) {
                    (Some(SseProtocol::AnthropicMessages), _) => response_transform::chat_usage(
                        state.start_usage.as_ref(),
                        response_transform::i64_field(&billed, "output_tokens"),
                    ),
                    (_, SseProtocol::AnthropicMessages) => {
                        response_transform::messages_usage(Some(&billed))
                    }
                    _ => billed,
                };
            }
            state.last_usage = Some(billed.clone());
            if bridged {
                if let Some(slot) = &state.upstream_usage {
                    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(billed.clone());
                }
                *usage = if destination == SseProtocol::OpenaiResponses {
                    response_transform::responses_usage(Some(&billed))
                } else {
                    billed.clone()
                };
            }
            if let Some(cost) = state.price(usage, &billed) {
                usage["cost"] = cost;
            }
            if include_start {
                wire_usage = Some(usage.clone());
            }
        });
        wire_usage
    }
    /// A Responses terminal repeats the last converted usage, or zero if the
    /// bridge never saw usage. This client projection does not bill zero usage.
    pub(crate) fn responses_usage(&self) -> Value {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let billed = state
            .upstream_usage
            .as_ref()
            .and_then(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).clone());
        let mut usage = response_transform::responses_usage(billed.as_ref());
        if let Some(cost) = state.price(&usage, billed.as_ref().unwrap_or(&usage)) {
            usage["cost"] = cost;
        }
        usage
    }
}

pub(super) fn message_start_usage(value: &Value) -> Option<&Value> {
    // Usage follows the payload schema, independently of the event's classified
    // control meaning. This is the same extraction used before typed events.
    if value.get("type").and_then(Value::as_str) != Some("message_start") {
        return None;
    }
    value
        .get("message")?
        .get("usage")
        .filter(|usage| usage.is_object())
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
                if item
                    .as_ref()
                    .is_ok_and(|record| matches!(record, Record::NonSse(_)))
                {
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
                        return Poll::Ready(Some(Ok(Record::Heartbeat)));
                    }
                }
                Poll::Pending
            }
        }
    }
}
