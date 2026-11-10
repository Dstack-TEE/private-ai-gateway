use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::Stream;
use sha2::{Digest, Sha256};

use crate::sse_framing::{SseFramingObserver, MAX_OBSERVED_EVENT_BYTES};
use crate::sse_protocol::{stream_error_tail, stream_success_terminator, SseProtocol};

use super::e2ee_crypto::encrypt_e2ee_stream_payload;
use super::{
    AciService, Clock, E2eeError, E2eeRequestContext, MiddlewareReceiptDraft,
    MiddlewareReceiptJournal, ReceiptOwner, ReceiptStore, ServiceError, ServiceResponseStream,
    StreamCompletion,
};
use crate::aci::keys::KeyProvider;
use crate::aci::receipt::{ReceiptBuilder, ReceiptError};
use crate::aci::upstream::UpstreamBodyStream;
use crate::aggregator::metrics::{
    ReceiptFinalizationOutcome, RequestMode, ServiceMetrics, StreamErrorKind,
};
use crate::aggregator::session_store::{SessionLease, SessionStore};

pub(super) struct MiddlewareProviderResponseDraftingStream {
    inner: Option<UpstreamBodyStream>,
    draft: Option<MiddlewareReceiptDraft>,
    journal: MiddlewareReceiptJournal,
    provider_response_hasher: Sha256,
    endpoint_path: String,
    sse_parser: SseFramingObserver,
    metrics: Arc<ServiceMetrics>,
    upstream_status: u16,
    finished: bool,
}

impl Unpin for MiddlewareProviderResponseDraftingStream {}

impl Stream for MiddlewareProviderResponseDraftingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                let boundary = this.sse_parser.observe(&chunk);
                let chunk = match boundary {
                    Some(end) => chunk.slice(..end),
                    None => chunk,
                };
                this.provider_response_hasher.update(&chunk);
                if boundary.is_some() {
                    if let Err(err) = this.finish() {
                        return Poll::Ready(Some(Err(err)));
                    }
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(err))) => {
                this.metrics
                    .record_stream_error(&this.endpoint_path, StreamErrorKind::UpstreamRead);
                this.finished = true;
                this.inner.take();
                Poll::Ready(Some(Err(ServiceError::Upstream(err))))
            }
            Poll::Ready(None) => match this.finish() {
                Ok(()) => Poll::Ready(None),
                Err(err) => Poll::Ready(Some(Err(err))),
            },
        }
    }
}

impl MiddlewareProviderResponseDraftingStream {
    pub(super) fn new(
        inner: UpstreamBodyStream,
        draft: MiddlewareReceiptDraft,
        journal: MiddlewareReceiptJournal,
        metrics: Arc<ServiceMetrics>,
        upstream_status: u16,
        protocol: Option<SseProtocol>,
    ) -> Self {
        Self {
            endpoint_path: draft.endpoint_path.clone(),
            inner: Some(inner),
            draft: Some(draft),
            journal,
            provider_response_hasher: Sha256::new(),
            sse_parser: protocol.map_or_else(
                SseFramingObserver::identifiers_only,
                SseFramingObserver::for_protocol,
            ),
            metrics,
            upstream_status,
            finished: false,
        }
    }

    fn finish(&mut self) -> Result<(), ServiceError> {
        let result = self.publish_draft();
        self.inner.take();
        self.finished = true;
        if result.is_err() {
            self.metrics
                .record_stream_error(&self.endpoint_path, StreamErrorKind::ReceiptFinalize);
        }
        result
    }

    fn publish_draft(&mut self) -> Result<(), ServiceError> {
        let provider_response_hash = format!(
            "sha256:{}",
            hex::encode(self.provider_response_hasher.clone().finalize())
        );
        let response_model = self.sse_parser.model_id();
        let mut draft = self.draft.take().ok_or(ReceiptError::EmptyReceipt)?;
        draft.builder.set_chat_id(self.sse_parser.chat_id());
        draft
            .builder
            .set_upstream_verified_model_id(response_model.clone());
        draft
            .builder
            .add_response_received_hash(provider_response_hash)?;
        draft.response_model = response_model.clone();
        self.journal.set(draft);
        self.metrics.record_upstream_response(
            &self.endpoint_path,
            RequestMode::Streaming,
            self.upstream_status,
            response_model.as_deref(),
        );
        Ok(())
    }
}

/// State shared by the two streaming receipt finalizers. The streams differ
/// only in their inner source (and its error type) and how the receipt is
/// drafted; the hashing, SSE chat-id parsing, optional E2EE re-encryption, and
/// the receipt-store/metrics plumbing are identical and live here.
struct FinalizerShared {
    wire_hasher: Sha256,
    keys: Arc<dyn KeyProvider>,
    receipt_store: Arc<dyn ReceiptStore>,
    session_store: Arc<dyn SessionStore>,
    key_id: String,
    requester: Option<ReceiptOwner>,
    receipt_ttl_seconds: u64,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    endpoint_path: String,
    sse_parser: SseFramingObserver,
    e2ee_transformer: Option<E2eeSseTransformer>,
    request_id: Option<String>,
    serving_instance_id: Option<String>,
    polled: bool,
    content_committed: bool,
    outcome_recorded: bool,
    upstream_ended: bool,
    finished: bool,
    /// Set once the clean-EOF check has run, so the tail is considered exactly
    /// once however many times the terminal branch is polled.
    error_tail_settled: bool,
    /// The stream feeding this finalizer failed, rather than ending. E2EE
    /// buffers may not be flushed; receipt publication depends on the draft.
    broken: bool,
}

impl FinalizerShared {
    fn new(
        service: &AciService,
        requester: Option<ReceiptOwner>,
        endpoint_path: String,
        e2ee_transformer: Option<E2eeSseTransformer>,
        request_id: Option<String>,
        is_sse: bool,
    ) -> Self {
        Self {
            wire_hasher: Sha256::new(),
            keys: service.keys.clone(),
            receipt_store: service.receipt_store.clone(),
            session_store: service.session_store.clone(),
            key_id: service.default_receipt_key_id.clone(),
            requester,
            receipt_ttl_seconds: service.config.receipt_ttl_seconds,
            clock: service.clock.clone(),
            metrics: service.metrics.clone(),
            // A response that is not an event stream has no protocol terminal to
            // miss, so it is observed for identifiers only.
            sse_parser: if is_sse {
                SseFramingObserver::new(&endpoint_path)
            } else {
                SseFramingObserver::identifiers_only()
            },
            endpoint_path,
            e2ee_transformer,
            request_id,
            serving_instance_id: service.serving_instance_id.clone(),
            polled: false,
            content_committed: false,
            outcome_recorded: false,
            upstream_ended: false,
            finished: false,
            error_tail_settled: false,
            broken: false,
        }
    }

    /// The bytes to append to a stream that ended without its protocol
    /// terminal, or `None` when nothing may be added.
    ///
    /// The kind of ending decides the kind of tail. A clean end of stream is
    /// the provider closing normally — a completion, never an error. The chat
    /// surface's `[DONE]` is a fixed stateless marker the gateway supplies when
    /// the provider omitted it; Anthropic and Responses terminals carry state
    /// the gateway cannot fabricate, so nothing is appended (the outcome is
    /// still complete). A transport error is an abnormal interruption, so the
    /// client is told with the protocol's error.
    ///
    /// Built here, inside the finalizer, so the tail is hashed and encrypted
    /// like any other chunk and the receipt covers exactly what the client
    /// received.
    fn take_synthesized_tail(&mut self) -> Option<Vec<u8>> {
        if std::mem::replace(&mut self.error_tail_settled, true) {
            return None;
        }
        let protocol = self.sse_parser.protocol()?;
        if self.sse_parser.saw_terminal() || self.sse_parser.saw_error() {
            return None;
        }
        // Nothing may be appended unless the stream stopped cleanly between
        // events and was observed all the way there.
        if !self.sse_parser.at_event_boundary() || !self.sse_parser.observation_usable() {
            return None;
        }

        // A clean end is a completion, never an error: supply the protocol's
        // terminator where it is a fixed marker the gateway can emit, and
        // otherwise append nothing (the outcome is still complete).
        if !self.broken {
            return stream_success_terminator(protocol)
                .map(|terminator| terminator.as_bytes().to_vec());
        }

        // A sequence with no successor cannot be continued, so the error would
        // have to repeat a number the client has already seen. Only the
        // protocol that numbers its events is affected; the field is recorded
        // wherever it appears, so gating on its value alone would silence the
        // others over a number they never use.
        if protocol == SseProtocol::OpenaiResponses
            && self.sse_parser.last_sequence_number() == Some(u64::MAX)
        {
            return None;
        }
        Some(
            stream_error_tail(
                protocol,
                self.request_id.as_deref(),
                self.sse_parser.last_sequence_number(),
            )
            .into_bytes(),
        )
    }

    /// `sha256:` hex digest of the wire bytes seen so far — the exact
    /// in-order stream the client received (§7.4 `response.returned`).
    fn wire_hash(&self) -> String {
        format!(
            "sha256:{}",
            hex::encode(self.wire_hasher.clone().finalize())
        )
    }

    /// Sign the receipt and store it under the configured retention window.
    fn sign_and_store(
        &self,
        builder: ReceiptBuilder,
        lease: Option<&SessionLease>,
    ) -> Result<(), ServiceError> {
        let receipt = builder.finalize(self.keys.as_ref(), &self.key_id)?;
        super::receipts::store_receipt(
            self.receipt_store.as_ref(),
            self.session_store.as_ref(),
            receipt,
            self.requester.clone(),
            self.clock.now_secs(),
            self.receipt_ttl_seconds,
            lease,
        )
    }

    fn record_outcome(&mut self, outcome: ReceiptFinalizationOutcome, receipt_id: Option<&str>) {
        if std::mem::replace(&mut self.outcome_recorded, true) {
            return;
        }
        self.metrics
            .record_receipt_finalization(&self.endpoint_path, outcome);
        if outcome != ReceiptFinalizationOutcome::Published {
            tracing::warn!(
                target: "receipt_finalization",
                request_id = self.request_id.as_deref(),
                receipt_id,
                outcome = outcome.as_str(),
                terminal_observed = self.sse_parser.saw_terminal(),
                at_event_boundary = self.sse_parser.at_event_boundary(),
                upstream_ended = self.upstream_ended,
                serving_instance_id = self.serving_instance_id.as_deref(),
                "streaming receipt was not published"
            );
        }
    }
}

/// Per-stream behavior the shared [`poll_finalizing_stream`] loop needs: access
/// to the shared state, how to pull the next inner chunk (mapping its error into
/// [`ServiceError`]), and how to finalize the receipt at end of stream.
trait FinalizingStream {
    fn shared(&mut self) -> &mut FinalizerShared;
    fn poll_inner(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, ServiceError>>>;
    fn finalize_receipt(&mut self) -> Result<ReceiptFinalizationOutcome, ServiceError>;
    fn receipt_id(&self) -> Option<String>;
    fn release_inner(&mut self);
    fn settle(&mut self, _outcome: StreamCompletion) {}
}

fn record_finalization_outcome<F: FinalizingStream>(
    f: &mut F,
    outcome: ReceiptFinalizationOutcome,
) {
    if !f.shared().outcome_recorded {
        let receipt_id = f.receipt_id();
        f.shared().record_outcome(outcome, receipt_id.as_deref());
    }
}

fn record_abandoned_stream<F: FinalizingStream>(f: &mut F) {
    let s = f.shared();
    if !s.outcome_recorded && (s.polled || s.content_committed) {
        record_finalization_outcome(f, ReceiptFinalizationOutcome::DroppedBeforeFinalization);
    }
}

/// Fail before exposing the final bytes. Accounting is committed before the
/// pipeline drops, so its abandonment guard cannot emit a second report.
fn fail_finalization<F: FinalizingStream>(
    f: &mut F,
    err: ServiceError,
    kind: StreamErrorKind,
) -> Poll<Option<Result<Bytes, ServiceError>>> {
    let s = f.shared();
    s.metrics.record_stream_error(&s.endpoint_path, kind);
    s.e2ee_transformer.take();
    record_finalization_outcome(f, ReceiptFinalizationOutcome::FinalizeFailed);
    f.settle(StreamCompletion::FinalizeFailed);
    f.release_inner();
    f.shared().finished = true;
    Poll::Ready(Some(Err(err)))
}

/// Finish output, hash it, publish, settle, then release the upstream. This
/// operation runs in the poll that returns the terminal, including EOF tails.
fn complete_finalizing_stream<F: FinalizingStream>(
    f: &mut F,
    mut wire: Vec<u8>,
) -> Poll<Option<Result<Bytes, ServiceError>>> {
    if let Some(tail) = f.shared().take_synthesized_tail() {
        let tail = match f.shared().e2ee_transformer.as_mut() {
            Some(transformer) => match transformer.push_chunk(&tail) {
                Ok(tail) => tail,
                Err(err) => return fail_finalization(f, err.into(), StreamErrorKind::E2ee),
            },
            None => tail,
        };
        wire.extend_from_slice(&tail);
    }
    // Never dispatch a partial E2EE event after cancellation or a read failure.
    let may_finish = !f.shared().broken
        && f.shared().sse_parser.observation_usable()
        && f.shared().sse_parser.at_event_boundary();
    if let Some(mut transformer) = f.shared().e2ee_transformer.take().filter(|_| may_finish) {
        match transformer.finish() {
            Ok(tail) => wire.extend_from_slice(&tail),
            Err(err) => return fail_finalization(f, err.into(), StreamErrorKind::E2ee),
        }
    }
    f.shared().wire_hasher.update(&wire);
    match f.finalize_receipt() {
        Ok(outcome) => record_finalization_outcome(f, outcome),
        Err(err) => return fail_finalization(f, err, StreamErrorKind::ReceiptFinalize),
    }
    let s = f.shared();
    let failed = s.broken
        || s.sse_parser.saw_error()
        || (!s.sse_parser.saw_terminal()
            && (!s.sse_parser.at_event_boundary() || !s.sse_parser.observation_usable()));
    f.settle(if failed {
        StreamCompletion::Failed
    } else {
        StreamCompletion::Completed
    });
    f.release_inner();
    f.shared().finished = true;
    Poll::Ready((!wire.is_empty()).then(|| Ok(Bytes::from(wire))))
}

/// Both finalizers use the same boundary, encryption and completion operation.
fn poll_finalizing_stream<F: FinalizingStream>(
    f: &mut F,
    cx: &mut Context<'_>,
) -> Poll<Option<Result<Bytes, ServiceError>>> {
    if f.shared().finished {
        return Poll::Ready(None);
    }
    f.shared().polled = true;
    loop {
        if f.shared().upstream_ended {
            return complete_finalizing_stream(f, Vec::new());
        }
        match f.poll_inner(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                let s = f.shared();
                let boundary = s.sse_parser.observe(&chunk);
                let chunk = match boundary {
                    Some(end) => chunk.slice(..end),
                    None => chunk,
                };
                let wire = match s.e2ee_transformer.as_mut() {
                    Some(transformer) => match transformer.push_chunk(&chunk) {
                        Ok(wire) => Bytes::from(wire),
                        Err(err) => return fail_finalization(f, err.into(), StreamErrorKind::E2ee),
                    },
                    None => chunk,
                };
                if boundary.is_some() {
                    return complete_finalizing_stream(f, wire.to_vec());
                }
                if !wire.is_empty() {
                    f.shared().wire_hasher.update(&wire);
                    return Poll::Ready(Some(Ok(wire)));
                }
            }
            Poll::Ready(Some(Err(err))) => {
                let s = f.shared();
                s.metrics
                    .record_stream_error(&s.endpoint_path, StreamErrorKind::UpstreamRead);
                tracing::warn!(
                    target: "stream_abort",
                    request_id = s.request_id.as_deref().unwrap_or_default(),
                    error = %err,
                    "response stream failed; ending the body instead of aborting the connection"
                );
                s.broken = true;
                s.upstream_ended = true;
            }
            Poll::Ready(None) => f.shared().upstream_ended = true,
        }
    }
}

pub(super) struct MiddlewareResponseFinalizingStream {
    inner: Option<ServiceResponseStream>,
    journal: MiddlewareReceiptJournal,
    shared: FinalizerShared,
}

impl Unpin for MiddlewareResponseFinalizingStream {}

impl Drop for MiddlewareResponseFinalizingStream {
    fn drop(&mut self) {
        record_abandoned_stream(self);
    }
}

impl Stream for MiddlewareResponseFinalizingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        poll_finalizing_stream(self.get_mut(), cx)
    }
}

impl FinalizingStream for MiddlewareResponseFinalizingStream {
    fn shared(&mut self) -> &mut FinalizerShared {
        &mut self.shared
    }

    fn poll_inner(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, ServiceError>>> {
        // The middleware response stream already yields `ServiceError`.
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };
        inner.as_mut().poll_next(cx)
    }

    fn release_inner(&mut self) {
        self.inner.take();
    }

    fn settle(&mut self, outcome: StreamCompletion) {
        self.journal.complete_stream(outcome);
    }

    fn receipt_id(&self) -> Option<String> {
        self.journal.diagnostic_receipt_id()
    }

    fn finalize_receipt(&mut self) -> Result<ReceiptFinalizationOutcome, ServiceError> {
        let Some(mut draft) = self.journal.take() else {
            if self.shared.sse_parser.saw_terminal() && self.journal.peek_receipt_id().is_some() {
                return Err(ReceiptError::EmptyReceipt.into());
            }
            return Ok(ReceiptFinalizationOutcome::NoDraft);
        };
        if self.shared.sse_parser.chat_id().is_some() {
            draft.builder.set_chat_id(self.shared.sse_parser.chat_id());
        }
        draft
            .builder
            .add_response_returned_hash(self.shared.wire_hash())?;
        self.shared
            .sign_and_store(draft.builder, draft.session_lease.as_ref())?;

        self.shared.metrics.record_receipt_issued(
            &draft.endpoint_path,
            draft.request_mode,
            draft.response_model.as_deref(),
        );
        Ok(ReceiptFinalizationOutcome::Published)
    }
}

impl MiddlewareResponseFinalizingStream {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        service: &AciService,
        inner: ServiceResponseStream,
        journal: MiddlewareReceiptJournal,
        requester: Option<ReceiptOwner>,
        endpoint_path: String,
        e2ee_transformer: Option<E2eeSseTransformer>,
        request_id: Option<String>,
        is_sse: bool,
    ) -> Self {
        let content_committed = journal.early_response_keyset().is_some();
        let mut shared = FinalizerShared::new(
            service,
            requester,
            endpoint_path,
            e2ee_transformer,
            request_id,
            is_sse,
        );
        shared.content_committed = content_committed;
        Self {
            inner: Some(inner),
            journal,
            shared,
        }
    }
}

pub(super) struct ReceiptFinalizingStream {
    inner: Option<UpstreamBodyStream>,
    builder: Option<ReceiptBuilder>,
    receipt_id: String,
    session_lease: Option<SessionLease>,
    shared: FinalizerShared,
}

impl Unpin for ReceiptFinalizingStream {}

impl Drop for ReceiptFinalizingStream {
    fn drop(&mut self) {
        record_abandoned_stream(self);
    }
}

impl Stream for ReceiptFinalizingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        poll_finalizing_stream(self.get_mut(), cx)
    }
}

impl FinalizingStream for ReceiptFinalizingStream {
    fn shared(&mut self) -> &mut FinalizerShared {
        &mut self.shared
    }

    fn poll_inner(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, ServiceError>>> {
        // The upstream body stream yields `UpstreamError`; lift it to `ServiceError`.
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(ServiceError::Upstream(err)))),
        }
    }

    fn release_inner(&mut self) {
        self.inner.take();
    }

    fn receipt_id(&self) -> Option<String> {
        Some(self.receipt_id.clone())
    }

    fn finalize_receipt(&mut self) -> Result<ReceiptFinalizationOutcome, ServiceError> {
        let mut builder = self.builder.take().ok_or(ReceiptError::EmptyReceipt)?;
        builder.set_chat_id(self.shared.sse_parser.chat_id());
        builder.set_upstream_verified_model_id(self.shared.sse_parser.model_id());
        builder.add_response_returned_hash(self.shared.wire_hash())?;
        let lease = self.session_lease.take();
        self.shared.sign_and_store(builder, lease.as_ref())?;

        self.shared.metrics.record_upstream_response(
            &self.shared.endpoint_path,
            RequestMode::Streaming,
            200,
            self.shared.sse_parser.model_id().as_deref(),
        );
        self.shared.metrics.record_receipt_issued(
            &self.shared.endpoint_path,
            RequestMode::Streaming,
            self.shared.sse_parser.model_id().as_deref(),
        );

        Ok(ReceiptFinalizationOutcome::Published)
    }
}

impl ReceiptFinalizingStream {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        service: &AciService,
        inner: UpstreamBodyStream,
        builder: ReceiptBuilder,
        receipt_id: String,
        session_lease: Option<SessionLease>,
        requester: Option<ReceiptOwner>,
        endpoint_path: String,
        e2ee_transformer: Option<E2eeSseTransformer>,
        request_id: Option<String>,
        is_sse: bool,
    ) -> Self {
        Self {
            inner: Some(inner),
            builder: Some(builder),
            receipt_id,
            session_lease,
            shared: FinalizerShared::new(
                service,
                requester,
                endpoint_path,
                e2ee_transformer,
                request_id,
                is_sse,
            ),
        }
    }
}

pub(super) struct E2eeSseTransformer {
    line_buffer: Vec<u8>,
    event_lines: Vec<Vec<u8>>,
    pending_cr: bool,
    buffered_bytes: usize,
    ctx: E2eeRequestContext,
    endpoint_path: String,
}

impl E2eeSseTransformer {
    pub(super) fn new(ctx: E2eeRequestContext, endpoint_path: String) -> Self {
        Self {
            line_buffer: Vec::new(),
            event_lines: Vec::new(),
            pending_cr: false,
            buffered_bytes: 0,
            ctx,
            endpoint_path,
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<u8>, E2eeError> {
        let mut out = Vec::new();
        for &byte in chunk {
            if std::mem::take(&mut self.pending_cr) && byte == b'\n' {
                continue;
            }
            self.buffered_bytes += 1;
            if self.buffered_bytes > MAX_OBSERVED_EVENT_BYTES {
                return Err(E2eeError::EncryptionFailed);
            }
            match byte {
                b'\r' | b'\n' => {
                    self.pending_cr = byte == b'\r';
                    let line = std::mem::take(&mut self.line_buffer);
                    out.extend(self.observe_line(line)?);
                }
                _ => self.line_buffer.push(byte),
            }
        }
        Ok(out)
    }

    pub(super) fn finish(&mut self) -> Result<Vec<u8>, E2eeError> {
        let mut out = Vec::new();
        if !self.line_buffer.is_empty() {
            let mut line = std::mem::take(&mut self.line_buffer);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            out.extend(self.observe_line(line)?);
        }
        if !self.event_lines.is_empty() {
            out.extend(self.dispatch_event()?);
        }
        Ok(out)
    }

    fn observe_line(&mut self, line: Vec<u8>) -> Result<Vec<u8>, E2eeError> {
        if line.is_empty() {
            return self.dispatch_event();
        }
        self.event_lines.push(line);
        Ok(Vec::new())
    }

    fn dispatch_event(&mut self) -> Result<Vec<u8>, E2eeError> {
        self.buffered_bytes = 0;
        let lines = std::mem::take(&mut self.event_lines);
        if lines.is_empty() {
            return Ok(Vec::new());
        }

        let Some(data) = sse_event_data(&lines) else {
            return Ok(serialize_original_sse_event(&lines));
        };
        if data.as_slice() == b"[DONE]" {
            return Ok(serialize_original_sse_event(&lines));
        }

        let encrypted_payload = encrypt_e2ee_stream_payload(&data, &self.ctx, &self.endpoint_path)?;
        let mut out = Vec::new();
        for line in &lines {
            if !is_sse_data_line(line) {
                out.extend_from_slice(line);
                out.push(b'\n');
            }
        }
        out.extend_from_slice(b"data: ");
        out.extend_from_slice(&encrypted_payload);
        out.extend_from_slice(b"\n\n");
        Ok(out)
    }
}

pub(super) fn sse_event_data(lines: &[Vec<u8>]) -> Option<Vec<u8>> {
    let mut found = false;
    let mut out = Vec::new();
    for line in lines {
        if line.starts_with(b":") {
            continue;
        }
        let Some(rest) = line.strip_prefix(b"data:") else {
            continue;
        };
        let data = rest.strip_prefix(b" ").unwrap_or(rest);
        if found {
            out.push(b'\n');
        }
        out.extend_from_slice(data);
        found = true;
    }
    found.then_some(out)
}

pub(super) fn is_sse_data_line(line: &[u8]) -> bool {
    line.strip_prefix(b"data:").is_some()
}

pub(super) fn serialize_original_sse_event(lines: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out.push(b'\n');
    out
}
