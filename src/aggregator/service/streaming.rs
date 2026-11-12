//! One response driver owns receipt publication, wire hashing and accounting.
//! Sources hash accepted provider bytes; typed stages never serialize/reparse.
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::e2ee_crypto::encrypt_e2ee_record;
use super::{
    AciService, Clock, E2eeRequestContext, MiddlewareReceiptDraft, MiddlewareReceiptJournal,
    ReceiptOwner, ReceiptStore, ServiceError, ServiceResponseStream,
};
use crate::aci::keys::KeyProvider;
use crate::aci::receipt::{ReceiptBuilder, ReceiptError};
use crate::aci::upstream::UpstreamBodyStream;
use crate::aggregator::metrics::{RequestMode, ServiceMetrics, StreamErrorKind};
use crate::aggregator::session_store::{SessionLease, SessionStore};
use crate::middleware::response_transform::{self, ResponseIdentity};
use crate::middleware::sse::{Accounting, AccountingHandle, KeepAliveStream, StreamFailure};
use crate::middleware::stream_transform::{BridgeError, BridgeHandle};
use crate::middleware::types::{ErrorClass, ErrorSource};
use crate::sse::{serialize, DecodedStream, EventStream, Payload, Record};
use crate::sse_protocol::{
    chat_gateway_error, responses_error_event, responses_gateway_code, EventKind, EventMeaning,
    FailureEvidence, SseProtocol, TerminalKind,
};

#[derive(Clone, Default)]
pub(crate) struct Identifiers {
    pub chat_id: Option<String>,
    pub model_id: Option<String>,
}
impl Identifiers {
    pub fn observe(&mut self, record: &Record) -> bool {
        let Some(value) = record.json() else {
            return false;
        };
        let before = (self.chat_id.is_some(), self.model_id.is_some());
        if self.chat_id.is_none() {
            self.chat_id = value.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if self.model_id.is_none() {
            self.model_id = value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        before != (self.chat_id.is_some(), self.model_id.is_some())
    }
}

/// Raw source hashing has no lease, accounting guard or reporting destructor.
/// The draft and its lease belong to the response owner's journal from selection.
pub(super) struct MiddlewareProviderResponseDraftingStream {
    inner: UpstreamBodyStream,
    journal: MiddlewareReceiptJournal,
    finished: bool,
    metrics: Arc<ServiceMetrics>,
    endpoint: String,
    source_status: u16,
    raw_source_hash: Option<crate::sse::SourceHash>,
}
impl MiddlewareProviderResponseDraftingStream {
    pub(super) fn new(
        inner: UpstreamBodyStream,
        draft: MiddlewareReceiptDraft,
        journal: MiddlewareReceiptJournal,
        metrics: Arc<ServiceMetrics>,
        source_status: u16,
        hash_raw_bytes: bool,
    ) -> Self {
        let endpoint = draft.endpoint_path.clone();
        let raw_source_hash = hash_raw_bytes.then(|| journal.source_hash.clone());
        journal.set(draft);
        Self {
            inner,
            journal,
            finished: false,
            metrics,
            endpoint,
            source_status,
            raw_source_hash,
        }
    }
}
impl Stream for MiddlewareProviderResponseDraftingStream {
    type Item = Result<Bytes, ServiceError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(bytes))) => {
                if let Some(hash) = &this.raw_source_hash {
                    hash.update(&bytes);
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.finished = true;
                Poll::Ready(Some(Err(ServiceError::Upstream(error))))
            }
            Poll::Ready(None) => {
                this.finished = true;
                let result = this.journal.finish_source();
                this.metrics.record_upstream_response(
                    &this.endpoint,
                    RequestMode::Streaming,
                    this.source_status,
                    this.journal.response_model().as_deref(),
                );
                Poll::Ready(result.err().map(Err))
            }
        }
    }
}

/// Response phase and output control shared with ingress. Billing observes every
/// source record independently; only the owner decides when output has ended.
#[derive(Clone, Default)]
pub(crate) struct StreamControl(Arc<Mutex<StreamControlState>>);
#[derive(Default)]
struct StreamControlState {
    source_terminal: bool,
    published_terminal: TerminalPublication,
    output_ended: bool,
    bridges: Vec<BridgeHandle>,
    continuation_failure: Option<StreamFailure>,
    aborted: Option<StreamFailure>,
}
#[derive(Clone, Copy, Default)]
pub(crate) enum TerminalPublication {
    #[default]
    Unpublished,
    Published {
        failure: Option<StreamFailure>,
    },
}
impl StreamControl {
    fn source_terminal(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .source_terminal
    }
    fn terminal_returned(&self, failure: Option<StreamFailure>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .published_terminal = TerminalPublication::Published { failure };
    }
    pub(crate) fn published_terminal(&self) -> TerminalPublication {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .published_terminal
    }
    fn ingress(&self, mut record: Record) -> Result<Option<Record>, ServiceError> {
        validate_source_record(&record)?;
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(event) = record.event_mut() else {
            return Ok((!state.output_ended).then_some(record));
        };
        event.after_terminal = state.source_terminal;
        state.source_terminal |= event.meaning.terminal.is_some();
        if !state.output_ended {
            return Ok(Some(record));
        }
        if state.continuation_failure.is_none() {
            // Run the actual bridge chain, preserving its validation and source
            // state, but discard output after the destination failure terminal.
            let result = state
                .bridges
                .iter()
                .try_fold(vec![record], |records, bridge| {
                    records
                        .into_iter()
                        .try_fold(Vec::new(), |mut output, record| {
                            output.extend(bridge.continuation(record)?);
                            Ok(output)
                        })
                });
            // An already-closed destination needs no further completion. Only
            // malformed source content changes the continuation report label.
            if let Err(error @ BridgeError::InvalidPayload) = result {
                state.continuation_failure = Some(StreamFailure::from_error(&error.into()));
            }
        }
        Ok(None)
    }
    pub(crate) fn bridge(&self, bridge: BridgeHandle) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bridges
            .push(bridge);
    }
    fn has_bridge(&self) -> bool {
        !self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bridges
            .is_empty()
    }
    fn close_bridge(
        &self,
        request_id: Option<&str>,
    ) -> crate::middleware::stream_transform::BridgeClosure {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bridges
            .last()
            .map_or_else(Default::default, |bridge| bridge.close(request_id))
    }
    fn failure_terminal(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .output_ended = true;
    }
    fn abort(&self, failure: StreamFailure) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.output_ended = true;
        if failure.source == Some(ErrorSource::Gateway) {
            state.aborted = Some(failure);
        } else {
            state.aborted.get_or_insert(failure);
        }
    }
    pub(crate) fn aborted(&self) -> Option<StreamFailure> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .aborted
    }
    pub(crate) fn report_failure(&self, failure: Option<StreamFailure>) -> Option<StreamFailure> {
        if failure.is_some_and(|f| f.source == Some(ErrorSource::Gateway)) {
            return failure;
        }
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .continuation_failure
            .or(failure)
    }
}

fn validate_source_record(record: &Record) -> Result<(), ServiceError> {
    if record
        .event()
        .is_some_and(|event| event.meaning.failure == Some(FailureEvidence::ConflictingType))
    {
        Err(ServiceError::Upstream(
            crate::aci::upstream::UpstreamError::Transport(
                "provider SSE event has conflicting control types".into(),
            ),
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn decode_source(
    inner: ServiceResponseStream,
    protocol: SseProtocol,
    journal: MiddlewareReceiptJournal,
    accounting: AccountingHandle,
    control: StreamControl,
) -> EventStream {
    let mut ids = Identifiers::default();
    let hash = journal.source_hash.clone();
    Box::pin(
        DecodedStream::with_source_hash(inner, protocol, hash).filter_map(move |record| {
            let result = match record {
                Ok(record) => {
                    accounting.observe_source(&record);
                    if ids.observe(&record) {
                        journal.observe_source(&ids);
                    }
                    match control.ingress(record) {
                        Ok(record) => record.map(Ok),
                        Err(error) => Some(Err(error)),
                    }
                }
                Err(error) => Some(Err(error)),
            };
            std::future::ready(result)
        }),
    )
}

pub(crate) struct ResponseContext {
    pub requester: Option<ReceiptOwner>,
    pub endpoint: String,
    pub e2ee: Option<E2eeRequestContext>,
    pub request_id: Option<String>,
    pub identity: Option<Arc<ResponseIdentity>>,
}

pub(crate) enum ResponseInput {
    Events(EventStream),
    Bytes(ServiceResponseStream),
}
struct ReceiptContext {
    published: bool,
    keys: Arc<dyn KeyProvider>,
    receipt_store: Arc<dyn ReceiptStore>,
    session_store: Arc<dyn SessionStore>,
    key_id: String,
    requester: Option<ReceiptOwner>,
    ttl: u64,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    draft: ReceiptDraft,
    suppressed: bool,
}
enum ReceiptDraft {
    Middleware(MiddlewareReceiptJournal),
    Direct {
        builder: Option<Box<ReceiptBuilder>>,
        lease: Option<SessionLease>,
    },
}
impl ReceiptContext {
    fn new(service: &AciService, requester: Option<ReceiptOwner>, draft: ReceiptDraft) -> Self {
        Self {
            published: false,
            keys: service.keys.clone(),
            receipt_store: service.receipt_store.clone(),
            session_store: service.session_store.clone(),
            key_id: service.default_receipt_key_id.clone(),
            requester,
            ttl: service.config.receipt_ttl_seconds,
            clock: service.clock.clone(),
            metrics: service.metrics.clone(),
            draft,
            suppressed: false,
        }
    }
    fn publish(
        &mut self,
        ids: &Identifiers,
        hash: String,
        endpoint: &str,
        at_terminal: bool,
    ) -> Result<(), ServiceError> {
        if self.suppressed || self.published {
            return Ok(());
        }
        let (mut builder, lease, response_model) = match &mut self.draft {
            ReceiptDraft::Middleware(journal) => {
                if at_terminal {
                    journal.finish_source_hash()?;
                }
                let Some(draft) = journal.take() else {
                    return Ok(());
                };
                if !at_terminal && !journal.source_finished() {
                    return Ok(());
                }
                (
                    draft.builder,
                    draft.session_lease,
                    draft.response_model.get().cloned(),
                )
            }
            ReceiptDraft::Direct { builder, lease } => (
                *builder.take().ok_or(ReceiptError::EmptyReceipt)?,
                lease.take(),
                ids.model_id.clone(),
            ),
        };
        if ids.chat_id.is_some() {
            builder.set_chat_id(ids.chat_id.clone());
        }
        if matches!(self.draft, ReceiptDraft::Direct { .. }) {
            builder.set_upstream_verified_model_id(ids.model_id.clone());
        }
        builder.add_response_returned_hash(hash)?;
        let receipt = builder.finalize(self.keys.as_ref(), &self.key_id)?;
        super::receipts::store_receipt(
            self.receipt_store.as_ref(),
            self.session_store.as_ref(),
            receipt,
            self.requester.clone(),
            self.clock.now_secs(),
            self.ttl,
            lease.as_ref(),
        )?;
        self.metrics.record_receipt_issued(
            endpoint,
            RequestMode::Streaming,
            response_model.as_deref(),
        );
        self.published = true;
        Ok(())
    }
}

enum ResponseMode {
    Direct,
    Middleware(Accounting),
}
impl ResponseMode {
    fn accounting(&self) -> Option<&Accounting> {
        match self {
            Self::Direct => None,
            Self::Middleware(accounting) => Some(accounting),
        }
    }
    fn accounting_mut(&mut self) -> Option<&mut Accounting> {
        match self {
            Self::Direct => None,
            Self::Middleware(accounting) => Some(accounting),
        }
    }
}

/// The sole response lifetime owner. An accepted source terminal permits
/// publication at the destination terminal while source accounting continues.
pub(crate) struct ResponseOwner {
    inner: ResponseInput,
    receipt: Option<ReceiptContext>,
    mode: ResponseMode,
    e2ee: Option<E2eeRequestContext>,
    endpoint: String,
    protocol: Option<SseProtocol>,
    non_sse: bool,
    request_id: Option<String>,
    ids: Identifiers,
    identity: Option<Arc<ResponseIdentity>>,
    wire_hasher: Sha256,
    saw_terminal: bool,
    failure: Option<StreamFailure>,
    truncated: bool,
    sequence: Option<u64>,
    ended: bool,
    finished: bool,
    tail: VecDeque<Record>,
    output_error: Option<ServiceError>,
}
impl ResponseOwner {
    fn new(
        inner: ResponseInput,
        mode: ResponseMode,
        e2ee: Option<E2eeRequestContext>,
        endpoint: String,
        protocol: Option<SseProtocol>,
        request_id: Option<String>,
    ) -> Self {
        Self {
            inner,
            receipt: None,
            mode,
            e2ee,
            endpoint,
            protocol,
            non_sse: false,
            request_id,
            ids: Identifiers::default(),
            identity: None,
            wire_hasher: Sha256::new(),
            saw_terminal: false,
            failure: None,
            truncated: false,
            sequence: None,
            ended: false,
            finished: false,
            tail: VecDeque::new(),
            output_error: None,
        }
    }
    pub(crate) fn middleware(
        service: &AciService,
        inner: ResponseInput,
        journal: MiddlewareReceiptJournal,
        context: ResponseContext,
        accounting: Accounting,
    ) -> Self {
        let ResponseContext {
            requester,
            endpoint,
            e2ee,
            request_id,
            identity,
        } = context;
        let protocol = matches!(&inner, ResponseInput::Events(_)).then(|| {
            SseProtocol::from(crate::middleware::request_transform::Endpoint::from_path(
                &endpoint,
            ))
        });
        let mut owner = Self::new(
            inner,
            ResponseMode::Middleware(accounting),
            e2ee,
            endpoint,
            protocol,
            request_id,
        );
        owner.identity = identity;
        owner.receipt = Some(ReceiptContext::new(
            service,
            requester,
            ReceiptDraft::Middleware(journal),
        ));
        owner
    }
    fn end(&mut self) {
        self.ended = true;
        if self.saw_terminal || self.failure.is_some() {
            return;
        }
        if self.truncated {
            self.failure = Some(StreamFailure {
                class: Some(ErrorClass::StreamTruncated),
                ..StreamFailure::default()
            });
            if let Some(protocol) = self.protocol {
                self.tail.extend(error_records(
                    protocol,
                    502,
                    self.request_id.as_deref(),
                    self.sequence,
                ));
            }
        } else if !self.non_sse && self.protocol == Some(SseProtocol::OpenaiChat) {
            self.tail.push_back(done_record());
        }
    }

    fn accept(&mut self, mut record: Record) -> Result<Bytes, ServiceError> {
        if self.failure.is_some() && self.saw_terminal {
            return Ok(Bytes::new());
        }
        self.truncated |= matches!(record, Record::Residual);
        let failure = record
            .event()
            .is_some_and(|event| event.meaning.failure.is_some());
        let usage = self
            .mode
            .accounting()
            .and_then(|accounting| accounting.handle.decorate(&mut record, failure));
        let Some(event) = record.event().filter(|_| failure) else {
            return self.encode(record);
        };
        if matches!(self.mode, ResponseMode::Direct) {
            // Direct forwarding retains the provider's application payloads.
            if !self.saw_terminal {
                self.failure = Some(inband_failure());
            }
            return self.encode(record);
        }
        let Some(protocol) = self.protocol else {
            return Ok(Bytes::new());
        };
        let native = self.native_source(event.source_protocol);
        if !event.after_terminal {
            self.failure.get_or_insert_with(inband_failure);
        }
        if native {
            self.sanitize_native_failure(&mut record, protocol);
            return self.encode(record);
        }
        // A bridge generates one destination failure, never a second terminal.
        if event.after_terminal {
            return Ok(Bytes::new());
        }
        let failure = self.bridge_failure(record, protocol, usage);
        self.tail.extend(failure);
        Ok(Bytes::new())
    }

    fn native_source(&self, source: Option<SseProtocol>) -> bool {
        source.is_none()
            || (source == self.protocol
                && !self
                    .mode
                    .accounting()
                    .is_some_and(|accounting| accounting.control.has_bridge()))
    }

    fn sanitize_native_failure(&self, record: &mut Record, protocol: SseProtocol) {
        let Some(event) = record.event() else { return };
        let meaning = event.meaning;
        record.edit_json(|body| {
            let changed = self
                .identity
                .as_ref()
                .is_some_and(|identity| response_transform::rewrite_identity(body, identity));
            let sanitized = match (protocol, meaning.terminal) {
                (SseProtocol::AnthropicMessages, Some(TerminalKind::Error)) => {
                    if let Some(object) = body.as_object_mut() {
                        response_transform::sanitize_messages_error_event(
                            object,
                            self.request_id.as_deref(),
                        )
                    } else {
                        false
                    }
                }
                (SseProtocol::OpenaiResponses, Some(TerminalKind::Error)) => {
                    if let Some(object) = body.as_object_mut() {
                        response_transform::sanitize_responses_error_event(object)
                    } else {
                        false
                    }
                }
                _ => response_transform::canonicalize_stream(
                    body,
                    protocol.into(),
                    self.request_id.as_deref(),
                ),
            };
            changed | sanitized
        });
        match (protocol, meaning.terminal) {
            (
                SseProtocol::AnthropicMessages | SseProtocol::OpenaiResponses,
                Some(TerminalKind::Error),
            ) => {
                record.set_event_name("error");
            }
            (SseProtocol::OpenaiResponses, Some(TerminalKind::Failed)) => {
                record.set_event_name("response.failed");
            }
            _ => {}
        }
    }

    fn bridge_failure(
        &mut self,
        record: Record,
        protocol: SseProtocol,
        usage: Option<Value>,
    ) -> Vec<Record> {
        if record.event().is_none() {
            return Vec::new();
        }
        let error = record
            .json()
            .and_then(|body| body.get("error"))
            .filter(|error| !error.is_null())
            .cloned();
        let request_id = self.request_id.as_deref();
        let closed = self
            .mode
            .accounting()
            .map(|accounting| accounting.control.close_bridge(request_id))
            .unwrap_or_default();
        let mut output = closed.records;
        if self.ids.chat_id.is_none() {
            self.ids.chat_id = closed.chat_id;
        }
        let failure = match protocol {
            SseProtocol::OpenaiChat | SseProtocol::AnthropicMessages => {
                let mut records = error_records(protocol, 502, request_id, None);
                if let Some(first) = records.first_mut() {
                    first.edit_json(|body| {
                        if let Some(error) = error {
                            body["error"] = error;
                            if protocol == SseProtocol::OpenaiChat {
                                response_transform::sanitize_error(body.get_mut("error"));
                            } else if let Some(object) = body.as_object_mut() {
                                response_transform::sanitize_messages_error_event(
                                    object, request_id,
                                );
                            }
                        }
                        if let Some(usage) = usage.filter(|_| protocol == SseProtocol::OpenaiChat) {
                            body["usage"] = usage;
                        }
                        true
                    });
                }
                records
            }
            SseProtocol::OpenaiResponses => {
                let usage = usage.or_else(|| {
                    self.mode
                        .accounting()
                        .map(|accounting| accounting.handle.responses_usage())
                });
                let mut error = error.unwrap_or_else(responses_failure_error);
                response_transform::sanitize_responses_error_field(Some(&mut error));
                let Some(sequence) = next_sequence(self.sequence) else {
                    return output;
                };
                let body = json!({
                    "response": {
                        "store": false,
                        "id": request_id,
                        "object": "response",
                        "created_at": response_transform::now_secs(),
                        "model": self.identity.as_ref().and_then(|i| i.user_model.as_deref()),
                        "status": "failed",
                        "incomplete_details": null,
                        "error": error,
                        "output": [],
                        "usage": usage,
                    },
                    "type": "response.failed",
                    "sequence_number": sequence
                });
                let mut terminal = closed.response.unwrap_or_else(|| {
                    Record::generated(
                        Some("response.failed"),
                        Payload::Json(body.clone()),
                        EventMeaning::generated(protocol, EventKind::ResponseFailed),
                    )
                });
                terminal.edit_json(|existing| {
                    if let Some(identity) = &self.identity {
                        response_transform::rewrite_identity(existing, identity);
                    }
                    existing["response"]["error"] = body["response"]["error"].clone();
                    existing["response"]["usage"] = body["response"]["usage"].clone();
                    true
                });
                vec![terminal]
            }
        };
        output.extend(failure);
        output
    }

    fn encode(&mut self, mut record: Record) -> Result<Bytes, ServiceError> {
        self.non_sse |= matches!(record, Record::NonSse(_) | Record::BeginNonSse);
        self.ids.observe(&record);
        if let Some(ctx) = &self.e2ee {
            encrypt_e2ee_record(&mut record, ctx, &self.endpoint)?;
        }
        let bytes = serialize(&record)?;
        if self.saw_terminal {
            return Ok(Bytes::new());
        }
        let mut publish_at_terminal = false;
        if let Some(event) = record.event() {
            let terminal = event.meaning.terminal.is_some();
            // The source must reach its own terminal or clean adapter EOF,
            // after any authentication barrier. Unknown evidence requires EOF.
            let source_terminal = self
                .mode
                .accounting()
                .is_none_or(|accounting| accounting.control.source_terminal())
                && self
                    .receipt
                    .as_ref()
                    .is_none_or(|receipt| match &receipt.draft {
                        ReceiptDraft::Middleware(journal) => journal.early_publication(),
                        ReceiptDraft::Direct { .. } => true,
                    });
            let source_finished = self.receipt.as_ref().is_some_and(|receipt| {
                matches!(&receipt.draft, ReceiptDraft::Middleware(journal) if journal.source_finished())
            });
            publish_at_terminal = terminal && (self.ended || source_terminal || source_finished);
            self.saw_terminal |= terminal;
            if let Some(sequence) = event
                .payload
                .json()
                .and_then(|v| v.get("sequence_number"))
                .and_then(Value::as_u64)
            {
                self.sequence = Some(self.sequence.map_or(sequence, |old| old.max(sequence)));
            }
        }
        if self.failure.is_some() && self.saw_terminal {
            if let Some(accounting) = self.mode.accounting() {
                accounting.control.failure_terminal();
            }
        }
        self.wire_hasher.update(&bytes);
        if publish_at_terminal {
            self.publish(!self.ended)?;
            if self
                .receipt
                .as_ref()
                .is_some_and(|receipt| receipt.published)
            {
                if let Some(accounting) = self.mode.accounting() {
                    accounting.control.terminal_returned(self.failure);
                }
            }
        }
        Ok(bytes)
    }
    fn publish(&mut self, at_terminal: bool) -> Result<(), ServiceError> {
        let hash = format!(
            "sha256:{}",
            hex::encode(self.wire_hasher.clone().finalize())
        );
        if let Some(receipt) = &mut self.receipt {
            // Publication snapshots ids; later source observations only affect EOF metrics.
            let ids = self.ids.clone();
            receipt.publish(&ids, hash, &self.endpoint, at_terminal)?;
        }
        Ok(())
    }
    fn record_finalizer_error(&self, error: &ServiceError) {
        if let Some(receipt) = &self.receipt {
            receipt.metrics.record_stream_error(
                &self.endpoint,
                match error {
                    ServiceError::E2ee(_) => StreamErrorKind::E2ee,
                    _ => StreamErrorKind::ReceiptFinalize,
                },
            );
        }
    }
    fn encoding_failed(&mut self, error: ServiceError) {
        let failure = finalizer_failure();
        self.failure = Some(failure);
        if let Some(accounting) = self.mode.accounting() {
            accounting.control.abort(failure);
        }
        if let Some(receipt) = &mut self.receipt {
            receipt.suppressed = true;
        }
        self.tail.clear();
        self.record_finalizer_error(&error);
        self.output_error = Some(error);
    }
    fn fail(&mut self, error: &ServiceError) {
        self.finished = true;
        if let Some(accounting) = self.mode.accounting_mut() {
            accounting.settle(Some(finalizer_failure()));
        }
        self.record_finalizer_error(error);
    }
}
impl Stream for ResponseOwner {
    type Item = Result<Bytes, ServiceError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        loop {
            let bytes = if let Some(record) = this.tail.pop_front() {
                this.encode(record)
            } else if this.ended {
                this.finished = true;
                let result = this.publish(false);
                if let Some(receipt) = &this.receipt {
                    if matches!(receipt.draft, ReceiptDraft::Direct { .. })
                        && !receipt.suppressed
                        && result.is_ok()
                    {
                        receipt.metrics.record_upstream_response(
                            &this.endpoint,
                            RequestMode::Streaming,
                            200,
                            this.ids.model_id.as_deref(),
                        );
                    }
                }
                if let Err(error) = &result {
                    this.fail(error);
                } else if let Some(accounting) = this.mode.accounting_mut() {
                    accounting.settle(this.failure);
                }
                return Poll::Ready(this.output_error.take().or_else(|| result.err()).map(Err));
            } else {
                let result = match &mut this.inner {
                    ResponseInput::Events(stream) => match stream.as_mut().poll_next(cx) {
                        Poll::Ready(Some(Ok(record))) => Some(if this.output_error.is_some() {
                            Ok(Bytes::new())
                        } else {
                            this.accept(record)
                        }),
                        Poll::Ready(Some(Err(error))) => {
                            if this.output_error.is_none() {
                                this.stream_error(&error);
                            }
                            None
                        }
                        Poll::Ready(None) => {
                            this.end();
                            None
                        }
                        Poll::Pending => return Poll::Pending,
                    },
                    ResponseInput::Bytes(stream) => match stream.as_mut().poll_next(cx) {
                        Poll::Ready(Some(Ok(bytes))) => {
                            this.wire_hasher.update(&bytes);
                            Some(Ok(bytes))
                        }
                        Poll::Ready(Some(Err(error))) => {
                            if this.output_error.is_none() {
                                this.stream_error(&error);
                            }
                            None
                        }
                        Poll::Ready(None) => {
                            this.end();
                            None
                        }
                        Poll::Pending => return Poll::Pending,
                    },
                };
                let Some(result) = result else { continue };
                result
            };
            match bytes {
                Ok(bytes) if bytes.is_empty() => {}
                Ok(bytes) => {
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Err(error) => {
                    this.encoding_failed(error);
                }
            }
        }
    }
}
impl ResponseOwner {
    fn stream_error(&mut self, error: &ServiceError) {
        let already_failed = self.failure.is_some();
        let upstream = matches!(error, ServiceError::Upstream(_));
        if !upstream || !self.saw_terminal || already_failed {
            self.failure = Some(StreamFailure::from_error(error));
        }
        if let Some(receipt) = &self.receipt {
            receipt.metrics.record_stream_error(
                &self.endpoint,
                if upstream {
                    StreamErrorKind::UpstreamRead
                } else {
                    StreamErrorKind::Transform
                },
            );
        }
        tracing::warn!(
            target: "stream_abort",
            request_id = self.request_id.as_deref().unwrap_or_default(),
            error = %error,
            "response stream failed; ending the body gracefully"
        );
        if let (Some(accounting), Some(failure)) = (self.mode.accounting(), self.failure) {
            accounting.control.abort(failure);
        }
        if matches!(self.inner, ResponseInput::Bytes(_)) {
            self.ended = true;
        }
        if let Some(receipt) = &mut self.receipt {
            if let ReceiptDraft::Middleware(journal) = &receipt.draft {
                if !journal.source_finished() {
                    receipt.suppressed = true;
                }
            }
        }
        if !self.saw_terminal && !already_failed {
            if let Some(protocol) = self.protocol {
                self.tail.extend(error_records(
                    protocol,
                    502,
                    self.request_id.as_deref(),
                    self.sequence,
                ));
            }
        }
    }
}

fn finalizer_failure() -> StreamFailure {
    StreamFailure {
        status: 502,
        class: Some(ErrorClass::DownstreamFinalizerFailed),
        source: Some(ErrorSource::Gateway),
    }
}

fn inband_failure() -> StreamFailure {
    StreamFailure {
        class: Some(ErrorClass::StreamInbandError),
        ..StreamFailure::default()
    }
}

fn responses_failure_error() -> Value {
    json!({
        "code": responses_gateway_code(502),
        "message": crate::error_payload::upstream_message(502)
    })
}

pub(super) fn finalize_sse_body(
    bytes: &[u8],
    e2ee: Option<&E2eeRequestContext>,
    endpoint: &str,
) -> Result<(Vec<u8>, Identifiers), ServiceError> {
    let records = crate::sse::decode_buffered(Bytes::copy_from_slice(bytes), None)?;
    let mut ids = Identifiers::default();
    let mut out = Vec::new();
    for mut record in records {
        ids.observe(&record);
        if let Some(ctx) = e2ee {
            encrypt_e2ee_record(&mut record, ctx, endpoint)?;
        }
        out.extend(serialize(&record)?);
    }
    Ok((out, ids))
}

pub(crate) fn done_record() -> Record {
    Record::generated(
        None,
        Payload::Text("[DONE]".into()),
        EventMeaning::generated(SseProtocol::OpenaiChat, EventKind::Done),
    )
}
fn next_sequence(sequence: Option<u64>) -> Option<u64> {
    sequence.map_or(Some(0), |value| value.checked_add(1))
}

pub(crate) fn error_records(
    protocol: SseProtocol,
    status: u16,
    request_id: Option<&str>,
    sequence: Option<u64>,
) -> Vec<Record> {
    let (name, body) = match protocol {
        SseProtocol::OpenaiChat => (None, json!({"error": chat_gateway_error(status)})),
        SseProtocol::AnthropicMessages => (
            Some("error"),
            crate::error_payload::envelope(
                crate::error_payload::Surface::Anthropic,
                crate::error_payload::error_type(crate::error_payload::Surface::Anthropic, status),
                crate::error_payload::upstream_message(status),
                request_id,
            ),
        ),
        SseProtocol::OpenaiResponses => {
            // There is no representable sequence after u64::MAX. Never emit
            // a duplicate sequence or wrap it to zero.
            let Some(sequence) = next_sequence(sequence) else {
                return Vec::new();
            };
            (
                Some("error"),
                responses_error_event(
                    Some(responses_gateway_code(status)),
                    crate::error_payload::upstream_message(status),
                    None,
                    Some(sequence),
                ),
            )
        }
    };
    let mut out = vec![Record::generated(
        name,
        Payload::Json(body),
        EventMeaning {
            failure: None,
            ..EventMeaning::generated(protocol, EventKind::Error)
        },
    )];
    if protocol == SseProtocol::OpenaiChat {
        out.push(done_record());
    }
    out
}

pub(super) fn direct_response(
    service: &AciService,
    inner: UpstreamBodyStream,
    builder: ReceiptBuilder,
    lease: Option<SessionLease>,
    context: ResponseContext,
    is_sse: bool,
) -> ServiceResponseStream {
    let ResponseContext {
        requester,
        endpoint,
        e2ee,
        request_id,
        ..
    } = context;
    let inner: ServiceResponseStream =
        Box::pin(inner.map(|item| item.map_err(ServiceError::Upstream)));
    let protocol = is_sse.then(|| {
        SseProtocol::from(crate::middleware::request_transform::Endpoint::from_path(
            &endpoint,
        ))
    });
    let inner = match protocol {
        Some(protocol) => ResponseInput::Events(Box::pin(KeepAliveStream::new(
            Box::pin(DecodedStream::new(inner, Some(protocol)).map(|record| {
                record.and_then(|record| {
                    validate_source_record(&record)?;
                    Ok(record)
                })
            })),
            Some(crate::sse::DEFAULT_KEEP_ALIVE),
        ))),
        None => ResponseInput::Bytes(inner),
    };
    let mut owner = ResponseOwner::new(
        inner,
        ResponseMode::Direct,
        e2ee,
        endpoint,
        protocol,
        request_id,
    );
    owner.receipt = Some(ReceiptContext::new(
        service,
        requester,
        ReceiptDraft::Direct {
            builder: Some(Box::new(builder)),
            lease,
        },
    ));
    Box::pin(owner)
}

#[cfg(test)]
mod tests;
