//! Library SSE decoding at ingress, typed payloads between stages, and canonical
//! library encoding at the response edge. JSON text is retained until edited.
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::Stream;
#[cfg(test)]
use futures_util::StreamExt;
use serde_json::Value;
use sse_stream::{Sse, SseByteStream};

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{ServiceError, ServiceResponseStream};
use crate::sse_protocol::{classify, EventMeaning, SseProtocol};

pub(crate) const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const KEEP_ALIVE_COMMENT: &[u8] = b": PROCESSING\n\n";
pub(crate) type EventStream = Pin<Box<dyn Stream<Item = Result<Record, ServiceError>> + Send>>;

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
    pub data: String,
    pub payload: Payload,
    pub meaning: EventMeaning,
    pub after_terminal: bool,
    pub source_protocol: Option<SseProtocol>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub event: Option<Event>,
    /// The library returns metadata-only blocks, with no dispatched payload.
    metadata: Sse,
    pub residual: bool,
    /// Content forwarded by a bridge after it stopped producing destination records.
    pub unconverted: bool,
    heartbeat: bool,
    edited: bool,
    /// Explicit non-SSE response bytes, including the early-commit handoff.
    pub non_sse: Option<Bytes>,
}
impl Record {
    pub fn from_sse(metadata: Sse, protocol: Option<SseProtocol>) -> Self {
        let event = metadata.data.as_ref().map(|data| {
            let payload = serde_json::from_str(data)
                .map_or_else(|_| Payload::Text(data.clone()), Payload::Json);
            Event {
                data: data.clone(),
                meaning: classify(protocol, metadata.event.as_deref(), &payload),
                after_terminal: false,
                source_protocol: protocol,
                payload,
            }
        });
        Self {
            event,
            metadata,
            residual: false,
            unconverted: false,
            heartbeat: false,
            edited: false,
            non_sse: None,
        }
    }
    pub fn edit_json(&mut self, edit: impl FnOnce(&mut Value)) {
        let Some(event) = self.event.as_mut() else {
            return;
        };
        let Payload::Json(value) = &mut event.payload else {
            return;
        };
        let previous = value.clone();
        edit(value);
        if previous == *value {
            return;
        }
        self.edited = true;
    }
    pub fn json(&self) -> Option<&Value> {
        self.event.as_ref()?.payload.json()
    }
    #[cfg(test)]
    pub fn event_name(&self) -> &str {
        self.metadata
            .event
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or("message")
    }
    pub fn set_event_name(&mut self, name: &str) {
        self.metadata.event = Some(name.into());
    }
    pub fn set_encrypted(&mut self, value: Value) {
        if let Some(event) = &mut self.event {
            event.payload = Payload::Json(value);
            self.edited = true;
        }
    }
    pub fn generated(name: Option<&str>, payload: Payload, meaning: EventMeaning) -> Self {
        let data = match &payload {
            Payload::Text(text) => text.clone(),
            Payload::Json(value) => value.to_string(),
        };
        Self {
            event: Some(Event {
                data,
                payload,
                meaning,
                after_terminal: false,
                source_protocol: None,
            }),
            metadata: Sse {
                event: name.map(str::to_string),
                ..Sse::default()
            },
            residual: false,
            unconverted: false,
            heartbeat: false,
            edited: false,
            non_sse: None,
        }
    }
    pub fn is_heartbeat(&self) -> bool {
        self.heartbeat
    }
    pub fn heartbeat() -> Self {
        Self {
            heartbeat: true,
            ..Self::from_sse(Sse::default(), None)
        }
    }
    pub fn non_sse(bytes: Bytes) -> Self {
        Self {
            non_sse: Some(bytes),
            ..Self::from_sse(Sse::default(), None)
        }
    }
    pub fn buffered_bytes(&self) -> usize {
        self.event.as_ref().map_or(0, |event| event.data.len())
    }
}

/// The only SSE encoding boundary. The heartbeat is the gateway's fixed comment;
/// all fields and payloads are encoded through the library's public encoder.
pub(crate) fn serialize(record: &Record) -> Result<Bytes, ServiceError> {
    if let Some(bytes) = &record.non_sse {
        return Ok(bytes.clone());
    }
    if record.residual {
        return Ok(Bytes::new());
    }
    if record.heartbeat {
        return Ok(Bytes::from_static(KEEP_ALIVE_COMMENT));
    }
    let mut metadata = record.metadata.clone();
    if let Some(event) = &record.event {
        metadata.data = Some(if record.edited {
            match &event.payload {
                Payload::Json(v) => serde_json::to_string(v)
                    .map_err(|_| ServiceError::AdmissionInvariant("SSE JSON encoding failed"))?,
                Payload::Text(s) => s.clone(),
            }
        } else {
            event.data.clone()
        });
    }
    metadata
        .encode()
        .map_err(|_| ServiceError::AdmissionInvariant("SSE encoding failed"))
}

/// Count bytes between blank lines without interpreting fields. Split chunks at
/// block boundaries so a large chunk of small events never trips the event cap.
/// The same state reports an undispatched EOF tail that the library discards.
struct BlockLimit {
    max_bytes: usize,
    bytes: usize,
    line_bytes: usize,
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
            return Err(upstream_error("sse line overflow"));
        }
        if crlf {
            return Ok(false);
        }
        if self.bom_prefix < 3 {
            if b"\xef\xbb\xbf"[self.bom_prefix] == byte {
                self.bom_prefix += 1;
                return Ok(false);
            }
            self.line_bytes += self.bom_prefix;
            self.has_field |= self.bom_prefix != 0;
            self.bom_prefix = 3;
        }
        if matches!(byte, b'\n' | b'\r') {
            self.cr = byte == b'\r';
            if std::mem::take(&mut self.line_bytes) == 0 {
                self.bytes = 0;
                self.has_field = false;
                return Ok(true);
            }
        } else {
            if self.line_bytes == 0 {
                self.comment = byte == b':';
            }
            self.has_field |= !self.comment;
            self.line_bytes += 1;
        }
        Ok(false)
    }
    fn at_boundary(&self) -> bool {
        (self.bytes == 0 || self.bom_prefix == 3) && !self.has_field && self.line_bytes == 0
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
                return Poll::Ready(Some(Ok(this.pending.split_to(count))));
            }
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => this.pending = bytes,
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
        Self::with_limit(inner, protocol, MAX_EVENT_BYTES)
    }
    fn with_limit(
        inner: ServiceResponseStream,
        protocol: Option<SseProtocol>,
        max_bytes: usize,
    ) -> Self {
        let boundary = Arc::new(AtomicBool::new(true));
        Self {
            inner: SseByteStream::new(LimitedInput {
                inner,
                pending: Bytes::new(),
                limit: BlockLimit {
                    max_bytes,
                    bytes: 0,
                    line_bytes: 0,
                    cr: false,
                    bom_prefix: 0,
                    has_field: false,
                    comment: false,
                },
                boundary: boundary.clone(),
                finished: false,
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
                if this.protocol.is_some()
                    && record.event.as_ref().is_some_and(|event| {
                        event.meaning.failure
                            == Some(crate::sse_protocol::FailureEvidence::ConflictingType)
                    })
                {
                    this.finished = true;
                    return Poll::Ready(Some(Err(upstream_error(
                        "provider SSE event has conflicting control types",
                    ))));
                }
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
                    Poll::Ready(Some(Ok(Record {
                        residual: true,
                        ..Record::from_sse(Sse::default(), None)
                    })))
                } else {
                    this.finished = true;
                    Poll::Ready(Some(Err(error)))
                }
            }
            Poll::Ready(None) => {
                this.finished = true;
                Poll::Ready((!this.boundary.load(Ordering::Relaxed)).then(|| {
                    Ok(Record {
                        residual: true,
                        ..Record::from_sse(Sse::default(), None)
                    })
                }))
            }
        }
    }
}

pub(crate) type EnvelopeStream = Pin<Box<dyn Stream<Item = Result<Record, UpstreamError>> + Send>>;
pub(crate) fn decode_envelope(
    inner: crate::aci::upstream::UpstreamBodyStream,
    max_bytes: usize,
) -> EnvelopeStream {
    use futures_util::StreamExt;
    let inner = Box::pin(inner.map(|item| item.map_err(ServiceError::Upstream)));
    Box::pin(
        DecodedStream::with_limit(inner, None, max_bytes).map(|item| {
            item.map_err(|error| match error {
                ServiceError::Upstream(error) => error,
                _ => UpstreamError::Transport("invalid provider SSE envelope".into()),
            })
        }),
    )
}

/// Buffered bodies use the same public stream decoder; an in-memory input never
/// waits, so no executor or second parser is needed.
pub(crate) fn decode_buffered(
    bytes: Bytes,
    protocol: Option<SseProtocol>,
) -> Result<Vec<Record>, ServiceError> {
    let mut stream =
        DecodedStream::new(Box::pin(futures_util::stream::iter([Ok(bytes)])), protocol);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let mut records = Vec::new();
    loop {
        match Pin::new(&mut stream).poll_next(&mut cx) {
            Poll::Ready(Some(record)) => records.push(record?),
            Poll::Ready(None) => return Ok(records),
            Poll::Pending => {
                return Err(ServiceError::AdmissionInvariant(
                    "buffered SSE input waited",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn normalization_and_pending_eof_are_independent_of_chunk_splits() {
        for ending in ["\n", "\r\n", "\r"] {
            let input = format!("\u{feff}: dropped{ending}{ending}retry: 07{ending}id: old{ending}id: last{ending}extension: ignored{ending}event: old{ending}event{ending}data: {{ \"x\" : 1 }}{ending}{ending}[DONE]{ending}{ending}data: pending{ending}");
            for split in 0..=input.len() {
                let chunks = [
                    Ok(Bytes::copy_from_slice(&input.as_bytes()[..split])),
                    Ok(Bytes::copy_from_slice(&input.as_bytes()[split..])),
                ];
                let records: Vec<_> = DecodedStream::new(
                    Box::pin(futures_util::stream::iter(chunks)),
                    Some(SseProtocol::OpenaiChat),
                )
                .collect()
                .await;
                let records: Vec<_> = records.into_iter().map(Result::unwrap).collect();
                assert_eq!(records.len(), 2, "{ending:?} {split}");
                assert_eq!(
                    serialize(&records[0]).unwrap(),
                    b"event: \ndata: { \"x\" : 1 }\nid: last\nretry: 7\n\n".as_slice()
                );
                assert!(records[1].residual);
                assert!(serialize(&records[1]).unwrap().is_empty());
            }
        }
    }
    #[tokio::test]
    async fn bom_and_complete_comment_lines_leave_a_clean_eof() {
        for input in [
            "\u{feff}",
            "\u{feff}\n",
            ": upstream comment\n",
            "\u{feff}: comment\r\n",
        ] {
            for split in 0..=input.len() {
                let chunks = [
                    Ok(Bytes::copy_from_slice(&input.as_bytes()[..split])),
                    Ok(Bytes::copy_from_slice(&input.as_bytes()[split..])),
                ];
                let records: Vec<_> =
                    DecodedStream::new(Box::pin(futures_util::stream::iter(chunks)), None)
                        .collect()
                        .await;
                assert!(records.is_empty(), "{input:?} split {split}: {records:?}");
            }
        }
    }
    #[tokio::test]
    async fn cap_counts_a_block_instead_of_each_line_or_input_chunk() {
        let records: Vec<_> = DecodedStream::with_limit(
            Box::pin(futures_util::stream::iter([Ok(Bytes::from(
                "data: {}\r\n\r\n".repeat(1000),
            ))])),
            None,
            32,
        )
        .collect()
        .await;
        assert_eq!(records.len(), 1000);
        assert!(records.iter().all(Result::is_ok));
        let records: Vec<_> = DecodedStream::with_limit(
            Box::pin(futures_util::stream::iter([Ok(Bytes::from(
                "data: x\n".repeat(20),
            ))])),
            None,
            32,
        )
        .collect()
        .await;
        assert!(records.last().unwrap().is_err());
    }
    #[test]
    fn contradictory_named_control_types_are_an_upstream_failure() {
        let result = decode_buffered(
            Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"error\"}\n\n"),
            Some(SseProtocol::AnthropicMessages),
        );
        assert!(matches!(result, Err(ServiceError::Upstream(_))));
    }
    #[tokio::test]
    async fn invalid_utf8_is_an_upstream_failure() {
        let records: Vec<_> = DecodedStream::new(
            Box::pin(futures_util::stream::iter([Ok(Bytes::from_static(
                b"data: \xff\n\n",
            ))])),
            None,
        )
        .collect()
        .await;
        assert_eq!(records.len(), 1);
        assert!(matches!(
            &records[0],
            Err(ServiceError::Upstream(UpstreamError::Transport(_)))
        ));
    }
    #[tokio::test]
    async fn cap_is_per_event_and_error_preserves_timeout_classification() {
        let chunk = Bytes::from_static(b"data: {}\n\n");
        let records: Vec<_> = DecodedStream::new(
            Box::pin(futures_util::stream::iter(
                (0..1000).map(move |_| Ok(chunk.clone())),
            )),
            None,
        )
        .collect()
        .await;
        assert_eq!(records.len(), 1000);
        let records: Vec<_> = DecodedStream::new(
            Box::pin(futures_util::stream::iter([Ok(Bytes::from(vec![
                b'x';
                MAX_EVENT_BYTES
                    + 1
            ]))])),
            None,
        )
        .collect()
        .await;
        assert!(
            matches!(records.last().unwrap(), Err(ServiceError::Upstream(UpstreamError::Transport(message))) if message == "sse line overflow")
        );
        let records: Vec<_> = DecodedStream::new(
            Box::pin(futures_util::stream::iter([Err(ServiceError::Upstream(
                UpstreamError::Timeout("read".into()),
            ))])),
            None,
        )
        .collect()
        .await;
        assert!(matches!(
            &records[0],
            Err(ServiceError::Upstream(UpstreamError::Timeout(_)))
        ));
    }
}
