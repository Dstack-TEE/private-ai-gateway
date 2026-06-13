use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::Stream;
use sha2::{Digest, Sha256};

use super::e2ee_crypto::encrypt_e2ee_stream_payload;
use super::{
    AciService, Clock, E2eeError, E2eeRequestContext, MiddlewareReceiptDraft,
    MiddlewareReceiptJournal, ReceiptOwner, ReceiptStore, ServiceError, ServiceResponseStream,
};
use crate::aci::keys::KeyProvider;
use crate::aci::receipt::{ReceiptBuilder, ReceiptError, TransparencyEventKind};
use crate::aci::upstream::UpstreamBodyStream;
use crate::aggregator::metrics::{RequestMode, ServiceMetrics, StreamErrorKind};

pub(super) struct MiddlewareProviderResponseDraftingStream {
    inner: UpstreamBodyStream,
    builder: Option<ReceiptBuilder>,
    journal: MiddlewareReceiptJournal,
    provider_response_hasher: Sha256,
    receipt_id: String,
    endpoint_path: String,
    sse_parser: SseChatIdParser,
    metrics: Arc<ServiceMetrics>,
    upstream_status: u16,
    upstream_ended: bool,
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

        loop {
            if this.upstream_ended {
                this.finished = true;
                return match this.publish_draft() {
                    Ok(()) => Poll::Ready(None),
                    Err(err) => {
                        this.metrics.record_stream_error(
                            &this.endpoint_path,
                            StreamErrorKind::ReceiptFinalize,
                        );
                        Poll::Ready(Some(Err(err)))
                    }
                };
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.provider_response_hasher.update(&chunk);
                    this.sse_parser.observe(&chunk);
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(err))) => {
                    this.metrics
                        .record_stream_error(&this.endpoint_path, StreamErrorKind::UpstreamRead);
                    this.finished = true;
                    return Poll::Ready(Some(Err(ServiceError::Upstream(err))));
                }
                Poll::Ready(None) => {
                    this.upstream_ended = true;
                }
            }
        }
    }
}

impl MiddlewareProviderResponseDraftingStream {
    pub(super) fn new(
        inner: UpstreamBodyStream,
        builder: ReceiptBuilder,
        journal: MiddlewareReceiptJournal,
        receipt_id: String,
        endpoint_path: String,
        metrics: Arc<ServiceMetrics>,
        upstream_status: u16,
    ) -> Self {
        Self {
            inner,
            builder: Some(builder),
            journal,
            provider_response_hasher: Sha256::new(),
            receipt_id,
            endpoint_path,
            sse_parser: SseChatIdParser::default(),
            metrics,
            upstream_status,
            upstream_ended: false,
            finished: false,
        }
    }

    fn publish_draft(&mut self) -> Result<(), ServiceError> {
        let provider_response_hash = format!(
            "sha256:{}",
            hex::encode(self.provider_response_hasher.clone().finalize())
        );
        let response_model = self.sse_parser.model_id.clone();
        let mut builder = self.builder.take().ok_or(ReceiptError::EmptyReceipt)?;
        builder.set_chat_id(self.sse_parser.chat_id.clone());
        builder.set_upstream_verified_model_id(response_model.clone());
        builder.add_response_received_hash(provider_response_hash.clone())?;
        self.journal.set(MiddlewareReceiptDraft {
            receipt_id: self.receipt_id.clone(),
            builder,
            provider_response_hash,
            endpoint_path: self.endpoint_path.clone(),
            request_mode: RequestMode::Streaming,
            response_model: response_model.clone(),
        });
        self.metrics.record_upstream_response(
            &self.endpoint_path,
            RequestMode::Streaming,
            self.upstream_status,
            response_model.as_deref(),
        );
        Ok(())
    }
}

pub(super) struct MiddlewareResponseFinalizingStream {
    inner: ServiceResponseStream,
    journal: MiddlewareReceiptJournal,
    cleartext_hasher: Sha256,
    wire_hasher: Sha256,
    keys: Arc<dyn KeyProvider>,
    receipt_store: Arc<dyn ReceiptStore>,
    key_id: String,
    requester: Option<ReceiptOwner>,
    receipt_ttl_seconds: u64,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    endpoint_path: String,
    sse_parser: SseChatIdParser,
    e2ee_transformer: Option<E2eeSseTransformer>,
    response_modified_by_wire: bool,
    upstream_ended: bool,
    finished: bool,
}

impl Unpin for MiddlewareResponseFinalizingStream {}

impl Stream for MiddlewareResponseFinalizingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            if this.upstream_ended {
                if let Some(mut transformer) = this.e2ee_transformer.take() {
                    let wire = match transformer.finish() {
                        Ok(wire) => wire,
                        Err(err) => {
                            this.metrics
                                .record_stream_error(&this.endpoint_path, StreamErrorKind::E2ee);
                            this.finished = true;
                            return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                        }
                    };
                    if !wire.is_empty() {
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }
                }
                this.finished = true;
                return match this.finalize_receipt() {
                    Ok(()) => Poll::Ready(None),
                    Err(err) => {
                        this.metrics.record_stream_error(
                            &this.endpoint_path,
                            StreamErrorKind::ReceiptFinalize,
                        );
                        Poll::Ready(Some(Err(err)))
                    }
                };
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.cleartext_hasher.update(&chunk);
                    this.sse_parser.observe(&chunk);

                    if let Some(transformer) = this.e2ee_transformer.as_mut() {
                        let wire = match transformer.push_chunk(&chunk) {
                            Ok(wire) => wire,
                            Err(err) => {
                                this.metrics.record_stream_error(
                                    &this.endpoint_path,
                                    StreamErrorKind::E2ee,
                                );
                                this.finished = true;
                                return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                            }
                        };
                        if wire.is_empty() {
                            continue;
                        }
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }

                    this.wire_hasher.update(&chunk);
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(err))) => {
                    this.metrics
                        .record_stream_error(&this.endpoint_path, StreamErrorKind::UpstreamRead);
                    this.finished = true;
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(None) => {
                    this.upstream_ended = true;
                }
            }
        }
    }
}

impl MiddlewareResponseFinalizingStream {
    pub(super) fn new(
        service: &AciService,
        inner: ServiceResponseStream,
        journal: MiddlewareReceiptJournal,
        requester: Option<ReceiptOwner>,
        endpoint_path: String,
        e2ee_transformer: Option<E2eeSseTransformer>,
        response_modified_by_wire: bool,
    ) -> Self {
        Self {
            inner,
            journal,
            cleartext_hasher: Sha256::new(),
            wire_hasher: Sha256::new(),
            keys: service.keys.clone(),
            receipt_store: service.receipt_store.clone(),
            key_id: service.default_receipt_key_id.clone(),
            requester,
            receipt_ttl_seconds: service.config.receipt_ttl_seconds,
            clock: service.clock.clone(),
            metrics: service.metrics.clone(),
            endpoint_path,
            sse_parser: SseChatIdParser::default(),
            e2ee_transformer,
            response_modified_by_wire,
            upstream_ended: false,
            finished: false,
        }
    }

    fn finalize_receipt(&mut self) -> Result<(), ServiceError> {
        let Some(mut draft) = self.journal.take() else {
            return Ok(());
        };
        let cleartext_hash = format!(
            "sha256:{}",
            hex::encode(self.cleartext_hasher.clone().finalize())
        );
        let wire_hash = format!(
            "sha256:{}",
            hex::encode(self.wire_hasher.clone().finalize())
        );

        if self.sse_parser.chat_id.is_some() {
            draft.builder.set_chat_id(self.sse_parser.chat_id.clone());
        }
        if draft.provider_response_hash != cleartext_hash || self.response_modified_by_wire {
            draft
                .builder
                .add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        draft
            .builder
            .add_response_returned_hashes(cleartext_hash, wire_hash)?;
        let receipt = draft.builder.finalize(self.keys.as_ref(), &self.key_id)?;

        let now = self.clock.now_secs();
        let expires_at = now.saturating_add(self.receipt_ttl_seconds);
        self.receipt_store
            .put(receipt, self.requester.clone(), expires_at);

        self.metrics.record_receipt_issued(
            &draft.endpoint_path,
            draft.request_mode,
            draft.response_model.as_deref(),
        );
        Ok(())
    }
}

pub(super) struct ReceiptFinalizingStream {
    inner: UpstreamBodyStream,
    builder: Option<ReceiptBuilder>,
    cleartext_hasher: Sha256,
    wire_hasher: Sha256,
    keys: Arc<dyn KeyProvider>,
    receipt_store: Arc<dyn ReceiptStore>,
    key_id: String,
    requester: Option<ReceiptOwner>,
    receipt_ttl_seconds: u64,
    clock: Arc<dyn Clock>,
    metrics: Arc<ServiceMetrics>,
    endpoint_path: String,
    sse_parser: SseChatIdParser,
    e2ee_transformer: Option<E2eeSseTransformer>,
    response_modified: bool,
    upstream_ended: bool,
    finished: bool,
}

impl Unpin for ReceiptFinalizingStream {}

impl Stream for ReceiptFinalizingStream {
    type Item = Result<Bytes, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            if this.upstream_ended {
                if let Some(mut transformer) = this.e2ee_transformer.take() {
                    let wire = match transformer.finish() {
                        Ok(wire) => wire,
                        Err(err) => {
                            this.metrics
                                .record_stream_error(&this.endpoint_path, StreamErrorKind::E2ee);
                            this.finished = true;
                            return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                        }
                    };
                    if !wire.is_empty() {
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }
                }
                this.finished = true;
                return match this.finalize_receipt() {
                    Ok(()) => Poll::Ready(None),
                    Err(err) => {
                        this.metrics.record_stream_error(
                            &this.endpoint_path,
                            StreamErrorKind::ReceiptFinalize,
                        );
                        Poll::Ready(Some(Err(err)))
                    }
                };
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.cleartext_hasher.update(&chunk);
                    this.sse_parser.observe(&chunk);

                    if let Some(transformer) = this.e2ee_transformer.as_mut() {
                        let wire = match transformer.push_chunk(&chunk) {
                            Ok(wire) => wire,
                            Err(err) => {
                                this.metrics.record_stream_error(
                                    &this.endpoint_path,
                                    StreamErrorKind::E2ee,
                                );
                                this.finished = true;
                                return Poll::Ready(Some(Err(ServiceError::E2ee(err))));
                            }
                        };
                        if wire.is_empty() {
                            continue;
                        }
                        this.wire_hasher.update(&wire);
                        return Poll::Ready(Some(Ok(Bytes::from(wire))));
                    }

                    this.wire_hasher.update(&chunk);
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(err))) => {
                    this.metrics
                        .record_stream_error(&this.endpoint_path, StreamErrorKind::UpstreamRead);
                    this.finished = true;
                    return Poll::Ready(Some(Err(ServiceError::Upstream(err))));
                }
                Poll::Ready(None) => {
                    this.upstream_ended = true;
                }
            }
        }
    }
}

impl ReceiptFinalizingStream {
    pub(super) fn new(
        service: &AciService,
        inner: UpstreamBodyStream,
        builder: ReceiptBuilder,
        requester: Option<ReceiptOwner>,
        endpoint_path: String,
        e2ee_transformer: Option<E2eeSseTransformer>,
        response_modified: bool,
    ) -> Self {
        Self {
            inner,
            builder: Some(builder),
            cleartext_hasher: Sha256::new(),
            wire_hasher: Sha256::new(),
            keys: service.keys.clone(),
            receipt_store: service.receipt_store.clone(),
            key_id: service.default_receipt_key_id.clone(),
            requester,
            receipt_ttl_seconds: service.config.receipt_ttl_seconds,
            clock: service.clock.clone(),
            metrics: service.metrics.clone(),
            endpoint_path,
            sse_parser: SseChatIdParser::default(),
            e2ee_transformer,
            response_modified,
            upstream_ended: false,
            finished: false,
        }
    }

    fn finalize_receipt(&mut self) -> Result<(), ServiceError> {
        let cleartext_hash = format!(
            "sha256:{}",
            hex::encode(self.cleartext_hasher.clone().finalize())
        );
        let wire_hash = format!(
            "sha256:{}",
            hex::encode(self.wire_hasher.clone().finalize())
        );
        let mut builder = self.builder.take().ok_or(ReceiptError::EmptyReceipt)?;
        builder.set_chat_id(self.sse_parser.chat_id.clone());
        builder.set_upstream_verified_model_id(self.sse_parser.model_id.clone());
        if self.response_modified {
            builder.add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        builder.add_response_returned_hashes(cleartext_hash, wire_hash)?;
        let receipt = builder.finalize(self.keys.as_ref(), &self.key_id)?;

        let now = self.clock.now_secs();
        let expires_at = now.saturating_add(self.receipt_ttl_seconds);
        self.receipt_store
            .put(receipt, self.requester.clone(), expires_at);

        self.metrics.record_upstream_response(
            &self.endpoint_path,
            RequestMode::Streaming,
            200,
            self.sse_parser.model_id.as_deref(),
        );
        self.metrics.record_receipt_issued(
            &self.endpoint_path,
            RequestMode::Streaming,
            self.sse_parser.model_id.as_deref(),
        );

        Ok(())
    }
}

pub(super) struct E2eeSseTransformer {
    line_buffer: Vec<u8>,
    event_lines: Vec<Vec<u8>>,
    ctx: E2eeRequestContext,
    endpoint_path: String,
}

impl E2eeSseTransformer {
    pub(super) fn new(ctx: E2eeRequestContext, endpoint_path: String) -> Self {
        Self {
            line_buffer: Vec::new(),
            event_lines: Vec::new(),
            ctx,
            endpoint_path,
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<u8>, E2eeError> {
        let mut out = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line_buffer);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                out.extend(self.observe_line(line)?);
            } else {
                self.line_buffer.push(byte);
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

#[derive(Default)]
pub(super) struct SseChatIdParser {
    pub(super) line_buffer: Vec<u8>,
    pub(super) event_data: Vec<u8>,
    pub(super) chat_id: Option<String>,
    pub(super) model_id: Option<String>,
}

impl SseChatIdParser {
    pub(super) fn observe(&mut self, chunk: &[u8]) {
        if self.chat_id.is_some() && self.model_id.is_some() {
            return;
        }
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line_buffer);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.observe_line(&line);
                if self.chat_id.is_some() && self.model_id.is_some() {
                    return;
                }
            } else {
                self.line_buffer.push(byte);
            }
        }
    }

    fn observe_line(&mut self, line: &[u8]) {
        if line.is_empty() {
            self.dispatch_event();
            return;
        }
        if line.starts_with(b":") {
            return;
        }
        let Some(rest) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = rest.strip_prefix(b" ").unwrap_or(rest);
        if !self.event_data.is_empty() {
            self.event_data.push(b'\n');
        }
        self.event_data.extend_from_slice(data);
    }

    fn dispatch_event(&mut self) {
        if self.event_data.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.event_data);
        if data.as_slice() == b"[DONE]" {
            return;
        }
        let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&data) else {
            return;
        };
        if self.chat_id.is_none() {
            self.chat_id = parsed
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
        if self.model_id.is_none() {
            self.model_id = parsed
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
    }
}
