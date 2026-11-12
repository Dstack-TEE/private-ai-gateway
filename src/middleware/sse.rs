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
use crate::sse_protocol::{sse_protocol, SseProtocol};

use super::control::ControlClient;
use super::pricing;
use super::request_features::SessionKind;
use super::response_transform;
use super::types::{ErrorClass, ErrorSource, PostReport, SpendMode, TenantIdentity};
use crate::aggregator::service::StreamControl;

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
    source: Option<SseProtocol>,
    ttft_ms: Option<u64>,
    settled: bool,
}

/// The response owner's sole reporting guard. It survives source EOF and all
/// transform stages; its destructor records abandonment, never receipt work.
pub(crate) struct Accounting {
    pub handle: AccountingHandle,
    pub control: StreamControl,
    pending: Option<MiddlewareReceiptJournal>,
}
impl Accounting {
    pub fn new(report: StreamReport, pending: Option<MiddlewareReceiptJournal>) -> Self {
        Self {
            handle: AccountingHandle(Arc::new(Mutex::new(AccountingState {
                report,
                last_usage: None,
                start_usage: None,
                source: None,
                ttft_ms: None,
                settled: false,
            }))),
            control: StreamControl::default(),
            pending,
        }
    }
    pub fn settle(&mut self, failure: Option<StreamFailure>) {
        let mut state = self.handle.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.settled {
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
        if let Some(failure) = self.control.aborted() {
            state.emit(failure.status, Some(failure));
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
        self.report
            .settle(status, self.last_usage.take(), self.ttft_ms, failure);
    }
    fn price(&self) -> Option<Value> {
        let prices = self.report.pricing.as_ref().filter(|p| !p.is_null())?;
        Some(pricing::cost_to_json(pricing::compute_cost(
            self.last_usage.as_ref().unwrap_or(&Value::Null),
            prices,
        )))
    }
    fn projected_usage(&self, destination: SseProtocol) -> Option<Value> {
        let usage = self.last_usage.as_ref()?;
        if self.source == Some(destination) {
            return Some(usage.clone());
        }
        let chat = if self.source == Some(SseProtocol::AnthropicMessages) {
            response_transform::chat_usage(
                Some(usage),
                response_transform::i64_field(usage, "output_tokens"),
            )
        } else {
            usage.clone()
        };
        Some(match destination {
            SseProtocol::OpenaiChat => chat,
            SseProtocol::AnthropicMessages => response_transform::messages_usage(Some(&chat)),
            SseProtocol::OpenaiResponses => response_transform::responses_usage(Some(&chat)),
        })
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
    /// Observe the upstream's cumulative usage before any content transform.
    /// Position and destination protocol never select what is billed.
    pub(crate) fn observe_source(&self, record: &Record) {
        let Some(event) = record.event() else { return };
        let Some(body) = event.payload.json() else {
            return;
        };
        let Some(protocol) = event.source_protocol else {
            return;
        };
        self.observe_payload(body, protocol);
    }
    pub(crate) fn observe_payload(&self, body: &Value, protocol: SseProtocol) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.source = Some(protocol);
        let Some(mut usage) = source_usage(body, protocol) else {
            return;
        };
        filter_observed_usage(Some(&mut usage), protocol, Some(&state.report.endpoint));
        response_transform::normalize_reasoning_usage_value(&mut usage);
        if protocol == SseProtocol::AnthropicMessages {
            if message_start_usage(body).is_some() {
                state.start_usage = Some(usage.clone());
            } else if body.get("type").and_then(Value::as_str) == Some("message_delta") {
                if let Some(start) = &state.start_usage {
                    usage = complete_usage(start.clone(), &usage);
                }
            }
        }
        state.last_usage = Some(usage);
    }
    pub fn selected(&self, route: String, attempt: u32, status: u16) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.report.selected_route_id = Some(route);
        state.report.attempt_index = attempt;
        state.report.upstream_status = status;
    }
    pub fn disarm(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .settled = true;
    }
    /// Client usage is a projection only; it never feeds accounting.
    pub(crate) fn decorate(&self, record: &mut Record, failure: bool) -> Option<Value> {
        record.event()?;
        self.first_byte();
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let destination = sse_protocol(&state.report.endpoint);
        let mut wire_usage = None;
        record.edit_json(|body| {
            let start = message_start_usage(body).is_some();
            let usage = if body.get("usage").is_some_and(|u| !u.is_null()) {
                body.get_mut("usage")
            } else if start {
                body.get_mut("message").and_then(|m| m.get_mut("usage"))
            } else {
                body.get_mut("response").and_then(|r| r.get_mut("usage"))
            };
            let Some(usage) = usage.filter(|u| !u.is_null()) else {
                return;
            };
            if failure && state.source != Some(destination) {
                if let Some(projected) = state.projected_usage(destination) {
                    *usage = projected;
                }
            }
            filter_observed_usage(Some(usage), destination, Some(&state.report.endpoint));
            response_transform::normalize_reasoning_usage_value(usage);
            if start && !failure {
                return;
            }
            if let Some(cost) = state.price() {
                usage["cost"] = cost;
            }
            if failure {
                wire_usage = Some(usage.clone());
            }
        });
        wire_usage
    }
    pub(crate) fn responses_usage(&self) -> Value {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut usage = state
            .projected_usage(SseProtocol::OpenaiResponses)
            .unwrap_or_else(|| response_transform::responses_usage(None));
        if let Some(cost) = state.price() {
            usage["cost"] = cost;
        }
        usage
    }
}

/// Extract reported usage from the source payload, never a converted envelope.
/// Control classification does not change the payload's usage schema.
pub(crate) fn source_usage(body: &Value, protocol: SseProtocol) -> Option<Value> {
    let usage = match protocol {
        SseProtocol::AnthropicMessages => message_start_usage(body).or_else(|| body.get("usage")),
        SseProtocol::OpenaiChat => body.get("usage"),
        SseProtocol::OpenaiResponses => body.get("response").unwrap_or(body).get("usage"),
    };
    usage.filter(|usage| usage.is_object()).cloned()
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
