//! One response driver owns receipt publication, wire hashing and accounting.
//! Sources hash accepted provider bytes; typed stages never serialize/reparse.
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
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
use crate::middleware::types::{ErrorClass, ErrorSource};
use crate::sse::{serialize, DecodedStream, EventStream, Payload, Record};
use crate::sse_protocol::{
    chat_gateway_error, responses_error_event, responses_gateway_code, EventKind, EventMeaning,
    SseProtocol, TerminalKind,
};

#[derive(Default)]
pub(crate) struct Identifiers {
    pub chat_id: Option<String>,
    pub model_id: Option<String>,
}
impl Identifiers {
    pub fn observe(&mut self, record: &Record) {
        let Some(value) = record.json() else { return };
        if self.chat_id.is_none() {
            self.chat_id = value.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if self.model_id.is_none() {
            self.model_id = value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
}

/// Raw source hashing has no lease, accounting guard or reporting destructor.
/// The draft and its lease belong to the response owner's journal from selection.
pub(super) struct MiddlewareProviderResponseDraftingStream {
    inner: UpstreamBodyStream,
    journal: MiddlewareReceiptJournal,
    hasher: Sha256,
    finished: bool,
    metrics: Arc<ServiceMetrics>,
    endpoint: String,
}
impl MiddlewareProviderResponseDraftingStream {
    pub(super) fn new(
        inner: UpstreamBodyStream,
        draft: MiddlewareReceiptDraft,
        journal: MiddlewareReceiptJournal,
        metrics: Arc<ServiceMetrics>,
    ) -> Self {
        let endpoint = draft.endpoint_path.clone();
        journal.set(draft);
        Self {
            inner,
            journal,
            hasher: Sha256::new(),
            finished: false,
            metrics,
            endpoint,
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
                this.hasher.update(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.finished = true;
                Poll::Ready(Some(Err(ServiceError::Upstream(error))))
            }
            Poll::Ready(None) => {
                this.finished = true;
                let result = this.journal.finish_source(format!(
                    "sha256:{}",
                    hex::encode(this.hasher.clone().finalize())
                ));
                this.metrics.record_upstream_response(
                    &this.endpoint,
                    RequestMode::Streaming,
                    this.journal.source_status(),
                    this.journal.response_model().as_deref(),
                );
                Poll::Ready(result.err().map(Err))
            }
        }
    }
}

pub(crate) fn decode_source(
    inner: ServiceResponseStream,
    protocol: SseProtocol,
    journal: MiddlewareReceiptJournal,
    accounting: AccountingHandle,
) -> EventStream {
    let mut ids = Identifiers::default();
    Box::pin(
        DecodedStream::new(inner, Some(protocol)).filter_map(move |mut record| {
            if let Ok(record) = &mut record {
                let forward = accounting.ingress(record);
                ids.observe(record);
                journal.observe_source(&ids);
                if !forward {
                    return std::future::ready(None);
                }
            }
            std::future::ready(Some(record))
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
    keys: Arc<dyn KeyProvider>,
    receipt_store: Arc<dyn ReceiptStore>,
    session_store: Arc<dyn SessionStore>,
    key_id: String,
    requester: Option<ReceiptOwner>,
    ttl: u64,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    draft: ReceiptDraft,
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
            keys: service.keys.clone(),
            receipt_store: service.receipt_store.clone(),
            session_store: service.session_store.clone(),
            key_id: service.default_receipt_key_id.clone(),
            requester,
            ttl: service.config.receipt_ttl_seconds,
            clock: service.clock.clone(),
            metrics: service.metrics.clone(),
            draft,
        }
    }
    fn publish(
        &mut self,
        ids: &Identifiers,
        hash: String,
        endpoint: &str,
    ) -> Result<(), ServiceError> {
        let (mut builder, lease, response_model, status) = match &mut self.draft {
            ReceiptDraft::Middleware(journal) => {
                let Some(draft) = journal.take() else {
                    return Ok(());
                };
                if !journal.source_finished() {
                    return Ok(());
                }
                (
                    draft.builder,
                    draft.session_lease,
                    draft.response_model,
                    journal.source_status(),
                )
            }
            ReceiptDraft::Direct { builder, lease } => (
                *builder.take().ok_or(ReceiptError::EmptyReceipt)?,
                lease.take(),
                ids.model_id.clone(),
                200,
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
        if matches!(self.draft, ReceiptDraft::Direct { .. }) {
            self.metrics.record_upstream_response(
                endpoint,
                RequestMode::Streaming,
                status,
                response_model.as_deref(),
            );
        }
        self.metrics.record_receipt_issued(
            endpoint,
            RequestMode::Streaming,
            response_model.as_deref(),
        );
        Ok(())
    }
}

/// The sole response lifetime owner. EOF remains the publication boundary; a
/// terminal records protocol meaning but never stops polling the source.
pub(crate) struct ResponseOwner {
    inner: ResponseInput,
    receipt: Option<ReceiptContext>,
    accounting: Option<Accounting>,
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
}
impl ResponseOwner {
    fn new(
        inner: ResponseInput,
        accounting: Option<Accounting>,
        e2ee: Option<E2eeRequestContext>,
        endpoint: String,
        protocol: Option<SseProtocol>,
        request_id: Option<String>,
    ) -> Self {
        Self {
            inner,
            receipt: None,
            accounting,
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
        }
    }
    pub(crate) fn middleware(
        service: &AciService,
        inner: ResponseInput,
        journal: MiddlewareReceiptJournal,
        context: ResponseContext,
        accounting: Option<Accounting>,
    ) -> Self {
        let ResponseContext {
            requester,
            endpoint,
            e2ee,
            request_id,
            identity,
        } = context;
        let protocol = matches!(&inner, ResponseInput::Events(_))
            .then(|| crate::sse_protocol::sse_protocol(&endpoint));
        let mut owner = Self::new(inner, accounting, e2ee, endpoint, protocol, request_id);
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
            .accounting
            .as_ref()
            .and_then(|accounting| accounting.handle.observe(&mut record, failure));
        let Some(event) = record.event().filter(|_| failure) else {
            return self.encode(record);
        };
        if self.accounting.is_none() {
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
                    .accounting
                    .as_ref()
                    .is_some_and(|accounting| accounting.handle.has_bridge()))
    }

    fn sanitize_native_failure(&self, record: &mut Record, protocol: SseProtocol) {
        let Some(event) = record.event() else { return };
        let meaning = event.meaning;
        record.edit_json(|body| {
            if let Some(identity) = &self.identity {
                response_transform::rewrite_identity(body, identity);
            }
            match (protocol, meaning.terminal) {
                (SseProtocol::AnthropicMessages, Some(TerminalKind::Error)) => {
                    if let Some(object) = body.as_object_mut() {
                        response_transform::sanitize_messages_error_event(
                            object,
                            self.request_id.as_deref(),
                        );
                    }
                }
                (SseProtocol::OpenaiResponses, Some(TerminalKind::Error)) => {
                    if let Some(object) = body.as_object_mut() {
                        response_transform::sanitize_responses_error_event(object);
                    }
                }
                _ => response_transform::canonicalize_protocol_stream(
                    body,
                    protocol,
                    self.request_id.as_deref(),
                ),
            }
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
        let Some(event) = record.event() else {
            return Vec::new();
        };
        let source = event.source_protocol;
        let error = record
            .json()
            .and_then(|body| body.get("error"))
            .filter(|error| !error.is_null())
            .cloned();
        let request_id = self.request_id.as_deref();
        let closed = self
            .accounting
            .as_ref()
            .map(|accounting| accounting.handle.close_bridge())
            .unwrap_or_default();
        let mut output = closed.records;
        let failure = match protocol {
            SseProtocol::OpenaiChat | SseProtocol::AnthropicMessages => {
                // The Chat envelope is omitted for a bridged failure, but its
                // receipt still identifies the same response as before.
                if protocol == SseProtocol::OpenaiChat
                    && source == Some(SseProtocol::AnthropicMessages)
                    && self.ids.chat_id.is_none()
                {
                    self.ids.chat_id = self.request_id.clone();
                }
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
                    });
                }
                records
            }
            SseProtocol::OpenaiResponses => {
                let usage = usage.or_else(|| {
                    self.accounting
                        .as_ref()
                        .map(|accounting| accounting.handle.responses_usage())
                });
                let mut error = error.unwrap_or_else(responses_failure_error);
                response_transform::sanitize_responses_error_field(Some(&mut error));
                let body = json!({"response":{
                    "store":false,
                    "id":request_id,
                    "object":"response",
                    "created_at":response_transform::now_secs(),
                    "model":self.identity.as_ref().and_then(|i| i.user_model.as_deref()),
                    "status":"failed",
                    "incomplete_details":null,
                    "error":error,
                    "output":[],
                    "usage":usage,
                }, "type":"response.failed", "sequence_number":self.sequence.map_or(0, |n| n.saturating_add(1))});
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
                });
                vec![terminal]
            }
        };
        output.extend(failure);
        output
    }

    fn encode(&mut self, mut record: Record) -> Result<Bytes, ServiceError> {
        self.non_sse |= matches!(record, Record::NonSse(_));
        self.ids.observe(&record);
        if let Some(ctx) = &self.e2ee {
            encrypt_e2ee_record(&mut record, ctx, &self.endpoint)?;
        }
        let bytes = serialize(&record)?;
        if let Some(event) = record.event() {
            self.saw_terminal |= event.meaning.terminal.is_some();
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
            if let Some(accounting) = &self.accounting {
                accounting.handle.failure_terminal();
            }
        }
        Ok(bytes)
    }
    fn fail(&mut self, error: &ServiceError) {
        self.finished = true;
        if let Some(accounting) = &mut self.accounting {
            accounting.settle(Some(StreamFailure {
                status: 502,
                class: Some(ErrorClass::DownstreamFinalizerFailed),
                source: Some(ErrorSource::Gateway),
            }));
        }
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
                let hash = format!(
                    "sha256:{}",
                    hex::encode(this.wire_hasher.clone().finalize())
                );
                let result = this.receipt.as_mut().map_or(Ok(()), |receipt| {
                    receipt.publish(&this.ids, hash, &this.endpoint)
                });
                if let Err(error) = &result {
                    this.fail(error);
                } else if let Some(accounting) = &mut this.accounting {
                    accounting.settle(this.failure);
                }
                return Poll::Ready(result.err().map(Err));
            } else {
                let result = match &mut this.inner {
                    ResponseInput::Events(stream) => match stream.as_mut().poll_next(cx) {
                        Poll::Ready(Some(Ok(record))) => Some(this.accept(record)),
                        Poll::Ready(Some(Err(error))) => {
                            this.stream_error(&error);
                            None
                        }
                        Poll::Ready(None) => {
                            this.end();
                            None
                        }
                        Poll::Pending => return Poll::Pending,
                    },
                    ResponseInput::Bytes(stream) => match stream.as_mut().poll_next(cx) {
                        Poll::Ready(Some(Ok(bytes))) => Some(Ok(bytes)),
                        Poll::Ready(Some(Err(error))) => {
                            this.stream_error(&error);
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
                    this.wire_hasher.update(&bytes);
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Err(error) => {
                    this.fail(&error);
                    return Poll::Ready(Some(Err(error)));
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
        tracing::warn!(target: "stream_abort", request_id = self.request_id.as_deref().unwrap_or_default(), error = %error, "response stream failed; ending the body gracefully");
        self.ended = true;
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

fn inband_failure() -> StreamFailure {
    StreamFailure {
        class: Some(ErrorClass::StreamInbandError),
        ..StreamFailure::default()
    }
}

fn responses_failure_error() -> Value {
    json!({"code":responses_gateway_code(502),"message":crate::error_payload::upstream_message(502)})
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
            if sequence == Some(u64::MAX) {
                return Vec::new();
            }
            (
                Some("error"),
                responses_error_event(
                    Some(responses_gateway_code(status)),
                    crate::error_payload::upstream_message(status),
                    None,
                    Some(sequence.map_or(0, |n| n + 1)),
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
    let protocol = is_sse.then(|| crate::sse_protocol::sse_protocol(&endpoint));
    let inner = match protocol {
        Some(protocol) => ResponseInput::Events(Box::pin(KeepAliveStream::new(
            Box::pin(DecodedStream::new(inner, Some(protocol))),
            Some(std::time::Duration::from_secs(5)),
        ))),
        None => ResponseInput::Bytes(inner),
    };
    let mut owner = ResponseOwner::new(inner, None, e2ee, endpoint, protocol, request_id);
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
mod tests {
    use super::super::wire::E2eeAadMode;
    use super::*;

    #[test]
    fn generated_error_frames_use_canonical_framing_without_leading_blank_lines() {
        for protocol in [
            SseProtocol::OpenaiChat,
            SseProtocol::AnthropicMessages,
            SseProtocol::OpenaiResponses,
        ] {
            let bytes: Vec<_> = error_records(protocol, 502, Some("fixture"), None)
                .iter()
                .flat_map(|record| serialize(record).unwrap())
                .collect();
            assert!(!bytes.starts_with(b"\n"));
            assert!(bytes.ends_with(b"\n\n"));
            if protocol == SseProtocol::OpenaiChat {
                assert_eq!(bytes, b"data: {\"error\":{\"message\":\"The upstream provider returned an error\",\"type\":\"upstream_error\",\"code\":502,\"param\":null}}\n\ndata: [DONE]\n\n");
            } else {
                assert!(bytes.starts_with(b"event: error\ndata: "));
            }
        }
    }

    #[tokio::test]
    async fn e2ee_only_passes_the_exact_done_marker_and_dispatches_complete_final_lines() {
        for (wire, failed, expected) in [
            ("data: [DONE]\n\n", false, "data: [DONE]\n\n"),
            ("data: [DONE] \n\n", true, ""),
            ("data:\ndata: [DONE]\n\n", true, ""),
            ("[DONE]\n\n", false, "data: [DONE]\n\n"),
            ("data: [DONE]\n", false, "data: [DONE]\n\n"),
        ] {
            let input = Box::pin(futures_util::stream::iter([Ok(Bytes::from(wire))]));
            let context = E2eeRequestContext {
                version: "2".into(),
                algo: crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM.into(),
                aad_mode: E2eeAadMode::AciV2,
                request_model: "fixture".into(),
                client_public_key_hex: "11".repeat(32),
                nonce: None,
                timestamp: None,
            };
            let mut owner = ResponseOwner::new(
                ResponseInput::Events(Box::pin(DecodedStream::new(
                    input,
                    Some(SseProtocol::OpenaiChat),
                ))),
                None,
                Some(context),
                "/v1/chat/completions".into(),
                Some(SseProtocol::OpenaiChat),
                None,
            );
            let mut bytes = Vec::new();
            let mut errors = 0;
            while let Some(item) = owner.next().await {
                match item {
                    Ok(chunk) => bytes.extend(chunk),
                    Err(ServiceError::E2ee(_)) => errors += 1,
                    Err(error) => panic!("{error}"),
                }
            }
            assert_eq!(errors != 0, failed, "{wire:?}");
            assert_eq!(bytes, expected.as_bytes(), "{wire:?}");
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/streaming/accounting.rs"]
mod accounting_tests;
