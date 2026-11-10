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
use crate::middleware::sse::{Accounting, AccountingHandle};
use crate::sse::{serialize, DecodedStream, EventStream, Payload, Record};
use crate::sse_protocol::{
    chat_gateway_error, responses_error_event, responses_gateway_code, EventKind, EventMeaning,
    SseProtocol,
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
        DecodedStream::new(inner, Some(protocol)).map(move |mut record| {
            if let Ok(record) = &mut record {
                accounting.ingress(record);
                ids.observe(record);
                journal.observe_source(&ids);
            }
            record
        }),
    )
}

pub(crate) struct ResponseContext {
    pub requester: Option<ReceiptOwner>,
    pub endpoint: String,
    pub e2ee: Option<E2eeRequestContext>,
    pub request_id: Option<String>,
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
    wire_hasher: Sha256,
    saw_terminal: bool,
    saw_error: bool,
    truncated: bool,
    sequence: Option<u64>,
    broken: bool,
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
            wire_hasher: Sha256::new(),
            saw_terminal: false,
            saw_error: false,
            truncated: false,
            sequence: None,
            broken: false,
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
        } = context;
        let protocol = matches!(&inner, ResponseInput::Events(_))
            .then(|| crate::sse_protocol::sse_protocol(&endpoint));
        let mut owner = Self::new(inner, accounting, e2ee, endpoint, protocol, request_id);
        owner.receipt = Some(ReceiptContext::new(
            service,
            requester,
            ReceiptDraft::Middleware(journal),
        ));
        owner
    }
    fn end(&mut self) {
        self.ended = true;
        if let Some(accounting) = &self.accounting {
            let (terminal, failure, truncated) = accounting.handle.protocol_facts();
            self.saw_terminal |= terminal;
            self.saw_error |= failure;
            self.truncated |= truncated;
        }
        if self.saw_terminal || self.saw_error || self.truncated {
            return;
        }
        if let Some(protocol) = self.protocol {
            self.tail.extend(if self.broken {
                error_records(protocol, 502, self.request_id.as_deref(), self.sequence)
            } else if !self.non_sse && protocol == SseProtocol::OpenaiChat {
                vec![done_record()]
            } else {
                Vec::new()
            });
        }
    }
    fn encode(&mut self, mut record: Record) -> Result<Bytes, ServiceError> {
        if record.non_sse.is_some() {
            self.non_sse = true;
        }
        self.truncated |= record.residual;
        self.ids.observe(&record);
        if let Some(event) = &record.event {
            self.saw_terminal |= event.meaning.terminal.is_some();
            self.saw_error |= !event.after_terminal && event.meaning.failure.is_some();
            if let Some(sequence) = event
                .payload
                .json()
                .and_then(|v| v.get("sequence_number"))
                .and_then(Value::as_u64)
            {
                self.sequence = Some(self.sequence.map_or(sequence, |old| old.max(sequence)));
            }
        }
        if let Some(ctx) = &self.e2ee {
            encrypt_e2ee_record(&mut record, ctx, &self.endpoint)?;
        }
        serialize(&record)
    }
    fn fail(&mut self, error: &ServiceError) {
        self.finished = true;
        if let Some(accounting) = &mut self.accounting {
            accounting.settle(false, true);
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
                    accounting.settle(!this.broken, false);
                }
                return Poll::Ready(result.err().map(Err));
            } else {
                let result = match &mut this.inner {
                    ResponseInput::Events(stream) => match stream.as_mut().poll_next(cx) {
                        Poll::Ready(Some(Ok(record))) => Some(this.encode(record)),
                        Poll::Ready(Some(Err(error))) => {
                            this.source_error(&error);
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
                            this.source_error(&error);
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
    fn source_error(&mut self, error: &ServiceError) {
        if let Some(accounting) = &self.accounting {
            accounting.handle.error(error);
        }
        if let Some(receipt) = &self.receipt {
            receipt
                .metrics
                .record_stream_error(&self.endpoint, StreamErrorKind::UpstreamRead);
        }
        tracing::warn!(target: "stream_abort", request_id = self.request_id.as_deref().unwrap_or_default(), error = %error, "response stream failed; ending the body gracefully");
        self.broken = true;
        self.end();
    }
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
        EventMeaning::generated(protocol, EventKind::Error),
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
    } = context;
    let inner: ServiceResponseStream =
        Box::pin(inner.map(|item| item.map_err(ServiceError::Upstream)));
    let protocol = is_sse.then(|| crate::sse_protocol::sse_protocol(&endpoint));
    let inner = match protocol {
        Some(protocol) => {
            ResponseInput::Events(Box::pin(DecodedStream::new(inner, Some(protocol))))
        }
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
    async fn e2ee_only_passes_the_exact_done_marker_and_discards_pending_data() {
        for (wire, failed, expected) in [
            ("data: [DONE]\n\n", false, "data: [DONE]\n\n"),
            ("data: [DONE] \n\n", true, ""),
            ("data:\ndata: [DONE]\n\n", true, ""),
            ("[DONE]\n\n", false, "data: [DONE]\n\n"),
            ("data: [DONE]\n", false, ""),
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
