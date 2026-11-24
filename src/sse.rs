//! Library SSE decoding at ingress, typed payloads between stages, and canonical
//! library encoding at the response edge. JSON text is retained until edited.
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::{FutureExt, Stream, StreamExt, TryStreamExt};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sse_stream::{Sse, SseByteStream};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{ServiceError, ServiceResponseStream};
use crate::sse_protocol::{classify, EventMeaning, SseProtocol};

pub(crate) const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const DEFAULT_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const KEEP_ALIVE_COMMENT: &[u8] = b": PROCESSING\n\n";
pub(crate) type EventStream = Pin<Box<dyn Stream<Item = Result<Record, ServiceError>> + Send>>;

/// Accepted source bytes, shared with the receipt draft. The decoder updates it
/// at block boundaries, before a record can produce destination output.
/// Publication freezes the hash while source consumption and accounting continue.
#[derive(Clone, Default)]
pub(crate) struct SourceHash(Arc<Mutex<SourceHashState>>);
#[derive(Default)]
struct SourceHashState {
    hasher: Sha256,
    frozen: bool,
}
impl SourceHash {
    pub fn update(&self, bytes: &[u8]) {
        let mut hash = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !hash.frozen {
            hash.hasher.update(bytes);
        }
    }
    pub fn finish(&self) -> String {
        let mut hasher = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        hasher.frozen = true;
        format!("sha256:{}", hex::encode(hasher.hasher.clone().finalize()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Payload {
    Json(Value),
    Text(String),
}
impl Payload {
    pub fn json(&self) -> Option<&Value> {
        match self {
            Self::Json(value) => Some(value),
            Self::Text(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    /// Original decoded text, also the exact size used by conversion limits.
    pub data: String,
    pub payload: Payload,
    pub meaning: EventMeaning,
    pub after_terminal: bool,
    pub source_protocol: Option<SseProtocol>,
    metadata: Sse,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Record {
    Event(Event),
    EditedEvent(Event),
    /// The library returns metadata-only blocks, with no dispatched payload.
    Metadata(Sse),
    Residual,
    Heartbeat,
    /// Explicit non-SSE response bytes, including the early-commit handoff.
    NonSse(Bytes),
    BeginNonSse,
}
impl Record {
    pub fn from_sse(mut metadata: Sse, protocol: Option<SseProtocol>) -> Self {
        let Some(data) = metadata.data.take() else {
            return Self::Metadata(metadata);
        };
        let payload =
            serde_json::from_str(&data).map_or_else(|_| Payload::Text(data.clone()), Payload::Json);
        let event = Event {
            data,
            meaning: classify(protocol, metadata.event.as_deref(), &payload),
            after_terminal: false,
            source_protocol: protocol,
            payload,
            metadata,
        };
        Self::Event(event)
    }
    pub fn event(&self) -> Option<&Event> {
        match self {
            Self::Event(event) | Self::EditedEvent(event) => Some(event),
            _ => None,
        }
    }
    pub fn event_mut(&mut self) -> Option<&mut Event> {
        match self {
            Self::Event(event) | Self::EditedEvent(event) => Some(event),
            _ => None,
        }
    }
    fn mark_edited(&mut self) {
        *self = match std::mem::replace(self, Self::Residual) {
            Self::Event(event) | Self::EditedEvent(event) => Self::EditedEvent(event),
            other => other,
        };
    }
    /// The closure must return true whenever it mutates the value; otherwise
    /// serialization emits the original text unchanged.
    pub fn edit_json(&mut self, edit: impl FnOnce(&mut Value) -> bool) {
        let Some(event) = self.event_mut() else {
            return;
        };
        let Payload::Json(value) = &mut event.payload else {
            return;
        };
        if edit(value) {
            self.mark_edited();
        }
    }
    pub fn json(&self) -> Option<&Value> {
        self.event()?.payload.json()
    }
    #[cfg(test)]
    pub fn event_name(&self) -> &str {
        let metadata = match self {
            Self::Event(event) | Self::EditedEvent(event) => &event.metadata,
            Self::Metadata(metadata) => metadata,
            _ => return "message",
        };
        metadata
            .event
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or("message")
    }
    pub fn set_event_name(&mut self, name: &str) {
        match self {
            Self::Event(event) | Self::EditedEvent(event) => {
                event.metadata.event = Some(name.into())
            }
            Self::Metadata(metadata) => metadata.event = Some(name.into()),
            _ => {}
        }
    }
    pub fn set_encrypted(&mut self, value: Value) {
        if let Some(event) = self.event_mut() {
            event.payload = Payload::Json(value);
            self.mark_edited();
        }
    }
    pub fn generated(name: Option<&str>, payload: Payload, meaning: EventMeaning) -> Self {
        let data = match &payload {
            Payload::Text(text) => text.clone(),
            Payload::Json(value) => value.to_string(),
        };
        Self::Event(Event {
            data,
            payload,
            meaning,
            after_terminal: false,
            source_protocol: None,
            metadata: Sse {
                event: name.map(str::to_string),
                ..Sse::default()
            },
        })
    }
    pub fn buffered_bytes(&self) -> usize {
        self.event().map_or(0, |event| event.data.len())
    }
}

/// The only SSE encoding boundary. The heartbeat is the gateway's fixed comment;
/// all fields and payloads are encoded through the library's public encoder.
pub(crate) fn serialize(record: &Record) -> Result<Bytes, ServiceError> {
    let metadata = match record {
        Record::NonSse(bytes) => return Ok(bytes.clone()),
        Record::Residual | Record::BeginNonSse => return Ok(Bytes::new()),
        Record::Heartbeat => return Ok(Bytes::from_static(KEEP_ALIVE_COMMENT)),
        Record::Event(event) => Sse {
            data: Some(event.data.clone()),
            ..event.metadata.clone()
        },
        Record::EditedEvent(event) => Sse {
            data: Some(match &event.payload {
                Payload::Json(value) => serde_json::to_string(value)
                    .map_err(|_| ServiceError::AdmissionInvariant("SSE JSON encoding failed"))?,
                Payload::Text(text) => text.clone(),
            }),
            ..event.metadata.clone()
        },
        Record::Metadata(metadata) => metadata.clone(),
    };
    metadata
        .encode()
        .map_err(|_| ServiceError::AdmissionInvariant("SSE encoding failed"))
}

/// The library exposes neither size caps nor source block boundaries. This
/// wrapper bounds buffering and splits blocks for clean-EOF completion and source
/// hashing. It recognizes line endings, the leading BOM and comments only;
/// it must never parse SSE fields or payloads.
struct BlockLimit {
    max_bytes: usize,
    bytes: usize,
    line_nonempty: bool,
    cr: bool,
    bom_prefix: usize,
    has_field: bool,
    comment: bool,
}
impl BlockLimit {
    fn observe(&mut self, byte: u8) -> Result<bool, ServiceError> {
        let crlf = std::mem::take(&mut self.cr) && byte == b'\n';
        if crlf && self.bytes == 0 {
            return Ok(false);
        }
        self.bytes += 1;
        if self.bytes > self.max_bytes {
            return Err(ServiceError::Upstream(UpstreamError::SseLineOverflow));
        }
        if crlf {
            return Ok(false);
        }
        if self.bom_prefix < 3 {
            if b"\xef\xbb\xbf"[self.bom_prefix] == byte {
                self.bom_prefix += 1;
                return Ok(false);
            }
            self.line_nonempty |= self.bom_prefix != 0;
            self.has_field |= self.bom_prefix != 0;
            self.bom_prefix = 3;
        }
        if matches!(byte, b'\n' | b'\r') {
            self.cr = byte == b'\r';
            if !std::mem::take(&mut self.line_nonempty) {
                self.bytes = 0;
                self.has_field = false;
                return Ok(true);
            }
        } else {
            if !self.line_nonempty {
                self.comment = byte == b':';
            }
            self.has_field |= !self.comment;
            self.line_nonempty = true;
        }
        Ok(false)
    }
    fn at_boundary(&self) -> bool {
        (self.bytes == 0 || self.bom_prefix == 3) && !self.has_field && !self.line_nonempty
    }
}
fn upstream_error(message: &'static str) -> ServiceError {
    ServiceError::Upstream(UpstreamError::Transport(message.into()))
}
struct LimitedInput {
    inner: ServiceResponseStream,
    pending: Bytes,
    limit: BlockLimit,
    boundary: Arc<AtomicBool>,
    finished: bool,
    source_hash: Option<SourceHash>,
}
impl Stream for LimitedInput {
    type Item = Result<Bytes, ServiceError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        loop {
            if !this.pending.is_empty() {
                let mut count = 0;
                for &byte in this.pending.as_ref() {
                    count += 1;
                    match this.limit.observe(byte) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(error) => {
                            this.finished = true;
                            this.boundary.store(false, Ordering::Relaxed);
                            this.pending = Bytes::new();
                            return Poll::Ready(Some(Err(error)));
                        }
                    }
                }
                this.boundary
                    .store(this.limit.at_boundary(), Ordering::Relaxed);
                let bytes = this.pending.split_to(count);
                if let Some(hash) = &this.source_hash {
                    hash.update(&bytes);
                }
                return Poll::Ready(Some(Ok(bytes)));
            }
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => this.pending = bytes,
                Poll::Ready(None) if !this.limit.at_boundary() && !this.limit.line_nonempty => {
                    // The library discards pending blocks at EOF. Complete the
                    // delimiter only when the source ended on a complete line;
                    // a CR needs another CR, since LF would only finish CRLF.
                    this.finished = true;
                    this.boundary.store(true, Ordering::Relaxed);
                    return Poll::Ready(Some(Ok(Bytes::from_static(if this.limit.cr {
                        b"\r"
                    } else {
                        b"\n"
                    }))));
                }
                Poll::Ready(other) => {
                    this.finished = true;
                    return Poll::Ready(other);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

pub(crate) struct DecodedStream {
    inner: SseByteStream<LimitedInput>,
    protocol: Option<SseProtocol>,
    boundary: Arc<AtomicBool>,
    pending_error: Option<ServiceError>,
    finished: bool,
}
impl DecodedStream {
    pub fn new(inner: ServiceResponseStream, protocol: Option<SseProtocol>) -> Self {
        Self::with_options(inner, protocol, MAX_EVENT_BYTES, None)
    }
    pub fn with_source_hash(
        inner: ServiceResponseStream,
        protocol: SseProtocol,
        hash: SourceHash,
    ) -> Self {
        Self::with_options(inner, Some(protocol), MAX_EVENT_BYTES, Some(hash))
    }
    fn with_limit(
        inner: ServiceResponseStream,
        protocol: Option<SseProtocol>,
        max_bytes: usize,
    ) -> Self {
        Self::with_options(inner, protocol, max_bytes, None)
    }
    fn with_options(
        inner: ServiceResponseStream,
        protocol: Option<SseProtocol>,
        max_bytes: usize,
        source_hash: Option<SourceHash>,
    ) -> Self {
        let boundary = Arc::new(AtomicBool::new(true));
        Self {
            inner: SseByteStream::new(LimitedInput {
                inner,
                pending: Bytes::new(),
                limit: BlockLimit {
                    max_bytes,
                    bytes: 0,
                    line_nonempty: false,
                    cr: false,
                    bom_prefix: 0,
                    has_field: false,
                    comment: false,
                },
                boundary: boundary.clone(),
                finished: false,
                source_hash,
            }),
            protocol,
            boundary,
            pending_error: None,
            finished: false,
        }
    }
}
impl Stream for DecodedStream {
    type Item = Result<Record, ServiceError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        if let Some(error) = this.pending_error.take() {
            this.finished = true;
            return Poll::Ready(Some(Err(error)));
        }
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => {
                let record = Record::from_sse(event, this.protocol);
                Poll::Ready(Some(Ok(record)))
            }
            Poll::Ready(Some(Err(error))) => {
                let error = match error {
                    sse_stream::Error::Body(error) => match error.downcast::<ServiceError>() {
                        Ok(error) => *error,
                        Err(_) => upstream_error("SSE source read failed"),
                    },
                    _ => upstream_error("provider sent invalid SSE encoding"),
                };
                if !this.boundary.load(Ordering::Relaxed) {
                    this.pending_error = Some(error);
                    Poll::Ready(Some(Ok(Record::Residual)))
                } else {
                    this.finished = true;
                    Poll::Ready(Some(Err(error)))
                }
            }
            Poll::Ready(None) => {
                this.finished = true;
                Poll::Ready((!this.boundary.load(Ordering::Relaxed)).then(|| Ok(Record::Residual)))
            }
        }
    }
}

pub(crate) type EnvelopeStream = Pin<Box<dyn Stream<Item = Result<Record, UpstreamError>> + Send>>;
pub(crate) fn decode_envelope(
    inner: crate::aci::upstream::UpstreamBodyStream,
    max_bytes: usize,
) -> EnvelopeStream {
    let inner = Box::pin(inner.map(|item| item.map_err(ServiceError::Upstream)));
    Box::pin(async_stream::try_stream! {
        let mut decoder = DecodedStream::with_limit(inner, None, max_bytes);
        while let Some(item) = decoder.next().await {
            let record = item
                .and_then(|record| match record {
                    Record::Residual => Err(decoder.pending_error.take().unwrap_or(
                        ServiceError::Upstream(UpstreamError::EnvelopeTruncated),
                    )),
                    record => Ok(record),
                })
                .map_err(|error| match error {
                    ServiceError::Upstream(error) => error,
                    _ => UpstreamError::Transport("invalid provider SSE envelope".into()),
                })?;
            yield record;
        }
    })
}

/// Buffered bodies use the same public stream decoder; an in-memory input never
/// waits, so no executor or second parser is needed.
pub(crate) fn decode_buffered(
    bytes: Bytes,
    protocol: Option<SseProtocol>,
) -> Result<Vec<Record>, ServiceError> {
    DecodedStream::new(Box::pin(futures_util::stream::iter([Ok(bytes)])), protocol)
        .try_collect()
        .now_or_never()
        .ok_or(ServiceError::AdmissionInvariant(
            "buffered SSE input waited",
        ))?
}

#[cfg(test)]
mod tests;
