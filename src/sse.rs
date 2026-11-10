//! Library SSE decoding at ingress, typed payloads between stages, and canonical
//! library encoding at the response edge. JSON text is retained until edited.
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
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
    pub name: String,
    pub explicit_name: Option<String>,
    pub data: String,
    pub payload: Payload,
    pub meaning: EventMeaning,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub event: Option<Event>,
    /// The library returns metadata-only blocks, with no dispatched payload.
    metadata: Sse,
    pub residual: bool,
    heartbeat: bool,
    edited: bool,
}
impl Record {
    pub fn from_sse(metadata: Sse, protocol: Option<SseProtocol>) -> Self {
        let event = metadata.data.as_ref().map(|data| {
            let payload = serde_json::from_str(data)
                .map_or_else(|_| Payload::Text(data.clone()), Payload::Json);
            Event {
                name: metadata
                    .event
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("message")
                    .into(),
                explicit_name: metadata.event.clone(),
                data: data.clone(),
                meaning: classify(protocol, metadata.event.as_deref(), &payload),
                payload,
            }
        });
        Self {
            event,
            metadata,
            residual: false,
            heartbeat: false,
            edited: false,
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
        if let (Some(old), Some(new)) = (
            previous.get("type").and_then(Value::as_str),
            value.get("type").and_then(Value::as_str),
        ) {
            if old != new && self.metadata.event.is_some() {
                self.metadata.event = Some(new.into());
                event.explicit_name = Some(new.into());
                event.name = new.into();
            }
        }
        self.edited = true;
    }
    pub fn json(&self) -> Option<&Value> {
        self.event.as_ref()?.payload.json()
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
            Payload::Json(_) => String::new(),
        };
        Self {
            event: Some(Event {
                name: name.filter(|s| !s.is_empty()).unwrap_or("message").into(),
                explicit_name: name.map(str::to_string),
                data,
                payload,
                meaning,
            }),
            metadata: Sse {
                event: name.map(str::to_string),
                ..Sse::default()
            },
            residual: false,
            heartbeat: false,
            edited: true,
        }
    }
    pub fn heartbeat() -> Self {
        Self {
            heartbeat: true,
            ..Self::from_sse(Sse::default(), None)
        }
    }
    pub fn buffered_bytes(&self) -> usize {
        fn size(value: &Value) -> usize {
            match value {
                Value::String(s) => s.len(),
                Value::Array(v) => v.iter().map(size).fold(0, usize::saturating_add),
                Value::Object(v) => v
                    .iter()
                    .map(|(k, v)| k.len().saturating_add(size(v)))
                    .fold(0, usize::saturating_add),
                _ => 16,
            }
        }
        self.event.as_ref().map_or(0, |event| {
            event.data.len().saturating_add(match &event.payload {
                Payload::Json(v) => size(v),
                Payload::Text(s) => s.len(),
            })
        })
    }
}

/// The only SSE encoding boundary. The heartbeat is the gateway's fixed comment;
/// all fields and payloads are encoded through the library's public encoder.
pub(crate) fn serialize(record: &Record) -> Result<Bytes, ServiceError> {
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
#[derive(Default)]
struct BlockLimit {
    bytes: usize,
    line_bytes: usize,
    cr: bool,
}
impl BlockLimit {
    fn observe(&mut self, byte: u8) -> Result<bool, ServiceError> {
        if std::mem::take(&mut self.cr) && byte == b'\n' {
            return Ok(false);
        }
        self.bytes += 1;
        if self.bytes > MAX_EVENT_BYTES {
            return Err(upstream_error("sse line overflow"));
        }
        if matches!(byte, b'\n' | b'\r') {
            self.cr = byte == b'\r';
            if std::mem::take(&mut self.line_bytes) == 0 {
                self.bytes = 0;
                return Ok(true);
            }
        } else {
            self.line_bytes += 1;
        }
        Ok(false)
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
                            this.pending = Bytes::new();
                            return Poll::Ready(Some(Err(error)));
                        }
                    }
                }
                this.boundary
                    .store(this.limit.bytes == 0, Ordering::Relaxed);
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
    finished: bool,
}
impl DecodedStream {
    pub fn new(inner: ServiceResponseStream, protocol: Option<SseProtocol>) -> Self {
        let boundary = Arc::new(AtomicBool::new(true));
        Self {
            inner: SseByteStream::new(LimitedInput {
                inner,
                pending: Bytes::new(),
                limit: BlockLimit::default(),
                boundary: boundary.clone(),
                finished: false,
            }),
            protocol,
            boundary,
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
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => {
                Poll::Ready(Some(Ok(Record::from_sse(event, this.protocol))))
            }
            Poll::Ready(Some(Err(error))) => {
                this.finished = true;
                let error = match error {
                    sse_stream::Error::Body(error) => match error.downcast::<ServiceError>() {
                        Ok(error) => *error,
                        Err(_) => upstream_error("SSE source read failed"),
                    },
                    _ => upstream_error("provider sent invalid SSE encoding"),
                };
                Poll::Ready(Some(Err(error)))
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

pub(crate) struct SerializedStream {
    inner: EventStream,
}
impl SerializedStream {
    pub fn new(inner: EventStream) -> Self {
        Self { inner }
    }
}
impl Stream for SerializedStream {
    type Item = Result<Bytes, ServiceError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.inner.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(record))) => match serialize(&record) {
                    Ok(bytes) if bytes.is_empty() => {}
                    other => return Poll::Ready(Some(other)),
                },
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
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
            matches!(&records[0], Err(ServiceError::Upstream(UpstreamError::Transport(message))) if message == "sse line overflow")
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
