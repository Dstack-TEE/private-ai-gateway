//! One lossless WHATWG event-stream decoder and one wire serializer.
//!
//! The decoder retains one input chunk and emits one record at a time. A record
//! keeps the original entity bytes and line/field spans; interpreting an event
//! never changes its representation. Layers edit the parsed JSON in place and
//! choose an output policy, then the response owner serializes once.

use std::ops::Range;

use bytes::Bytes;
use serde_json::Value;

use crate::sse_protocol::{classify, EventMeaning, SseProtocol};

pub(crate) const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delimiter {
    Lf,
    CrLf,
    Cr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub content: Range<usize>,
    pub delimiter: Delimiter,
    pub field: Option<Range<usize>>,
    pub value: Option<Range<usize>>,
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
    /// Empty `event` fields select the default event type, `message`.
    pub name: String,
    pub explicit_name: Option<String>,
    pub data: String,
    pub payload: Payload,
    pub last_event_id: String,
    pub retry: Option<u64>,
    pub meaning: EventMeaning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WirePolicy {
    Original,
    Middleware,
    Encrypted,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub original: Bytes,
    pub lines: Vec<Line>,
    pub event: Option<Event>,
    pub policy: WirePolicy,
    pub bom: bool,
    pub residual: bool,
    replaced_data: bool,
    renamed_event: Option<String>,
    generated: bool,
}

impl Record {
    /// Edit representation without recomputing or clearing protocol meaning.
    pub fn edit_json(&mut self, edit: impl FnOnce(&mut Value)) {
        let Some(event) = self.event.as_mut() else {
            return;
        };
        let Payload::Json(value) = &mut event.payload else {
            return;
        };
        let previous = value.clone();
        edit(value);
        if *value == previous {
            return;
        }
        if let (Some(old), Some(new)) = (
            previous.get("type").and_then(Value::as_str),
            value.get("type").and_then(Value::as_str),
        ) {
            if old != new {
                self.renamed_event = Some(new.to_string());
            }
        }
        self.replaced_data = true;
    }

    pub fn json(&self) -> Option<&Value> {
        self.event.as_ref()?.payload.json()
    }

    pub fn set_encrypted(&mut self, value: Value) {
        if let Some(event) = self.event.as_mut() {
            event.payload = Payload::Json(value);
            self.replaced_data = true;
            self.policy = WirePolicy::Encrypted;
        }
    }

    pub fn generated(name: Option<&str>, payload: Payload, meaning: EventMeaning) -> Self {
        let data = match &payload {
            Payload::Text(text) => text.clone(),
            Payload::Json(_) => String::new(),
        };
        Self {
            original: Bytes::new(),
            lines: Vec::new(),
            event: Some(Event {
                name: name
                    .filter(|s| !s.is_empty())
                    .unwrap_or("message")
                    .to_string(),
                explicit_name: name.map(str::to_string),
                data,
                payload,
                last_event_id: String::new(),
                retry: None,
                meaning,
            }),
            policy: WirePolicy::Middleware,
            bom: false,
            residual: false,
            replaced_data: true,
            renamed_event: None,
            generated: true,
        }
    }

    pub fn comment(text: &'static str) -> Self {
        let original = Bytes::from(format!(": {text}\n\n"));
        let len = original.len();
        Self {
            original,
            lines: vec![
                Line {
                    content: 0..len - 2,
                    delimiter: Delimiter::Lf,
                    field: None,
                    value: None,
                },
                Line {
                    content: len - 1..len - 1,
                    delimiter: Delimiter::Lf,
                    field: None,
                    value: None,
                },
            ],
            event: None,
            policy: WirePolicy::Original,
            bom: false,
            residual: false,
            replaced_data: false,
            renamed_event: None,
            generated: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("SSE event exceeded the size cap")]
    Overflow,
    #[error("SSE decoder still has an input chunk")]
    PendingInput,
}

pub(crate) struct Decoder {
    protocol: Option<SseProtocol>,
    chunk: Bytes,
    offset: usize,
    raw: Vec<u8>,
    lines: Vec<Line>,
    line_start: usize,
    pending_cr: bool,
    first_line: bool,
    last_event_id: String,
    retry: Option<u64>,
    eof: bool,
    failed: bool,
}

impl Decoder {
    pub fn new(protocol: Option<SseProtocol>) -> Self {
        Self {
            protocol,
            chunk: Bytes::new(),
            offset: 0,
            raw: Vec::new(),
            lines: Vec::new(),
            line_start: 0,
            pending_cr: false,
            first_line: true,
            last_event_id: String::new(),
            retry: None,
            eof: false,
            failed: false,
        }
    }

    pub fn push(&mut self, chunk: Bytes) -> Result<(), DecodeError> {
        if self.offset != self.chunk.len() || self.eof {
            return Err(DecodeError::PendingInput);
        }
        self.chunk = chunk;
        self.offset = 0;
        Ok(())
    }

    pub fn end(&mut self) {
        self.eof = true;
    }

    pub fn at_boundary(&self) -> bool {
        !self.pending_cr
            && !self.failed
            && (self.raw.is_empty()
                || (self.line_start == self.raw.len()
                    && self.lines.iter().all(|line| line.field.is_none())))
    }

    /// A partial record is evidence of truncation, never a dispatched event.
    pub fn take_residual(&mut self) -> Option<Record> {
        if !self.eof || self.raw.is_empty() {
            return None;
        }
        let residual = !self.at_boundary();
        Some(Record {
            original: Bytes::from(std::mem::take(&mut self.raw)),
            lines: std::mem::take(&mut self.lines),
            event: None,
            policy: WirePolicy::Original,
            bom: false,
            residual,
            replaced_data: false,
            renamed_event: None,
            generated: false,
        })
    }

    pub fn next(&mut self) -> Result<Option<Record>, DecodeError> {
        if self.failed {
            return Err(DecodeError::Overflow);
        }
        loop {
            let next = self.chunk.get(self.offset).copied();
            if self.pending_cr {
                if next.is_none() && !self.eof {
                    return Ok(None);
                }
                self.pending_cr = false;
                let delimiter = if next == Some(b'\n') {
                    self.offset += 1;
                    self.raw.push(b'\n');
                    Delimiter::CrLf
                } else {
                    Delimiter::Cr
                };
                if self.raw.len() > MAX_EVENT_BYTES {
                    self.failed = true;
                    self.raw.clear();
                    self.lines.clear();
                    return Err(DecodeError::Overflow);
                }
                if let Some(record) = self.end_line(delimiter) {
                    return Ok(Some(record));
                }
                continue;
            }
            let Some(byte) = next else { return Ok(None) };
            self.offset += 1;
            self.raw.push(byte);
            if self.raw.len() > MAX_EVENT_BYTES {
                self.failed = true;
                self.raw.clear();
                self.lines.clear();
                return Err(DecodeError::Overflow);
            }
            match byte {
                b'\r' => self.pending_cr = true,
                b'\n' => {
                    if let Some(record) = self.end_line(Delimiter::Lf) {
                        return Ok(Some(record));
                    }
                }
                _ => {}
            }
        }
    }

    fn end_line(&mut self, delimiter: Delimiter) -> Option<Record> {
        let delimiter_len = if delimiter == Delimiter::CrLf { 2 } else { 1 };
        let content = self.line_start..self.raw.len() - delimiter_len;
        let bom = self.first_line && self.raw[content.clone()].starts_with(b"\xef\xbb\xbf");
        let start = content.start + if bom { 3 } else { 0 };
        self.first_line = false;
        let blank = start == content.end;
        let (field, value) = if blank || self.raw[start] == b':' {
            (None, None)
        } else {
            let colon = self.raw[start..content.end]
                .iter()
                .position(|&b| b == b':')
                .map(|n| start + n);
            let end = colon.unwrap_or(content.end);
            let mut value_start = colon.map_or(content.end, |n| n + 1);
            if self.raw.get(value_start) == Some(&b' ') && value_start < content.end {
                value_start += 1;
            }
            (Some(start..end), Some(value_start..content.end))
        };
        self.lines.push(Line {
            content,
            delimiter,
            field,
            value,
        });
        self.line_start = self.raw.len();
        if !blank {
            return None;
        }
        self.line_start = 0;
        let raw = Bytes::from(std::mem::take(&mut self.raw));
        let lines = std::mem::take(&mut self.lines);
        let mut name = None;
        let mut data: Option<String> = None;
        for line in &lines {
            let (Some(field), Some(value)) = (&line.field, &line.value) else {
                continue;
            };
            let value = String::from_utf8_lossy(&raw[value.clone()]);
            match &raw[field.clone()] {
                b"event" => name = Some(value.into_owned()),
                b"data" => {
                    let data = data.get_or_insert_with(String::new);
                    data.push_str(&value);
                    data.push('\n');
                }
                b"id" if !value.contains('\0') => self.last_event_id = value.into_owned(),
                b"retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                    if let Ok(retry) = value.parse() {
                        self.retry = Some(retry);
                    }
                }
                _ => {}
            }
        }
        let event = data.map(|mut data| {
            data.pop();
            let payload = serde_json::from_str(&data)
                .map_or_else(|_| Payload::Text(data.clone()), Payload::Json);
            let meaning = classify(self.protocol, name.as_deref(), &payload);
            Event {
                name: name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("message")
                    .to_string(),
                explicit_name: name,
                data,
                payload,
                last_event_id: self.last_event_id.clone(),
                retry: self.retry,
                meaning,
            }
        });
        Some(Record {
            bom: raw.starts_with(b"\xef\xbb\xbf"),
            original: raw,
            lines,
            event,
            policy: WirePolicy::Original,
            residual: false,
            replaced_data: false,
            renamed_event: None,
            generated: false,
        })
    }
}

/// Serialization happens at the response boundary, after all payload edits.
pub(crate) fn serialize(record: &Record) -> Result<Bytes, serde_json::Error> {
    if record.policy == WirePolicy::Original && !record.replaced_data {
        return Ok(record.original.clone());
    }
    if record.lines.iter().all(|line| line.content.is_empty()) && !record.generated {
        return Ok(Bytes::new());
    }
    if record.residual {
        return Ok(Bytes::new());
    }
    let payload = if record.replaced_data {
        record
            .event
            .as_ref()
            .map(|event| match &event.payload {
                Payload::Json(value) => serde_json::to_vec(value),
                Payload::Text(text) => Ok(text.as_bytes().to_vec()),
            })
            .transpose()?
    } else {
        None
    };
    let mut out = Vec::new();
    if record.generated {
        if let Some(event) = &record.event {
            if let Some(name) = event
                .explicit_name
                .as_deref()
                .filter(|name| !name.is_empty())
            {
                out.extend_from_slice(b"event: ");
                out.extend_from_slice(name.as_bytes());
                out.push(b'\n');
            }
            if let Some(payload) = payload {
                write_data(&mut out, &payload);
            }
        }
        out.push(b'\n');
        return Ok(Bytes::from(out));
    }
    let mut replaced = false;
    for line in &record.lines {
        if line.content.is_empty() {
            continue;
        }
        let field = line
            .field
            .as_ref()
            .map(|span| &record.original[span.clone()]);
        if field == Some(b"data".as_slice()) && payload.is_some() {
            if !replaced && record.policy != WirePolicy::Encrypted {
                if let Some(payload) = &payload {
                    write_data(&mut out, payload);
                }
                replaced = true;
            }
        } else if field == Some(b"event".as_slice()) && record.renamed_event.is_some() {
            out.extend_from_slice(b"event: ");
            out.extend_from_slice(
                record
                    .renamed_event
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes(),
            );
            out.push(b'\n');
        } else {
            let start = line.content.start
                + if record.bom && line.content.start == 0 {
                    3
                } else {
                    0
                };
            out.extend_from_slice(&record.original[start..line.content.end]);
            out.push(b'\n');
        }
    }
    if record.policy == WirePolicy::Encrypted {
        if let Some(payload) = payload {
            write_data(&mut out, &payload);
        }
    }
    out.push(b'\n');
    Ok(Bytes::from(out))
}

fn write_data(out: &mut Vec<u8>, payload: &[u8]) {
    for line in payload.split(|&b| b == b'\n') {
        out.extend_from_slice(b"data: ");
        out.extend_from_slice(line);
        out.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decode(input: &[u8], split: usize) -> (Vec<Record>, Option<Record>) {
        let mut decoder = Decoder::new(Some(SseProtocol::OpenaiChat));
        let mut records = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            decoder.push(Bytes::copy_from_slice(chunk)).unwrap();
            while let Some(record) = decoder.next().unwrap() {
                records.push(record);
            }
        }
        decoder.end();
        while let Some(record) = decoder.next().unwrap() {
            records.push(record);
        }
        (records, decoder.take_residual())
    }
    #[test]
    fn lossless_at_every_split_and_line_ending() {
        for ending in ["\n", "\r\n", "\r"] {
            let input=format!("\u{feff}: comment{ending}id: first{ending}id: last{ending}id: ignored\0{ending}retry: 7{ending}retry: nope{ending}extension{ending}event: old{ending}event{ending}data:{ending}data: [DONE]{ending}{ending}: heartbeat{ending}{ending}data{ending}{ending}");
            for split in 0..=input.len() {
                let (records, residual) = decode(input.as_bytes(), split);
                assert!(residual.is_none());
                let original: Vec<_> = records.iter().flat_map(|r| serialize(r).unwrap()).collect();
                assert_eq!(original, input.as_bytes(), "{ending:?} at {split}");
                let event = records[0].event.as_ref().unwrap();
                assert_eq!(event.data, "\n[DONE]");
                assert_eq!(event.name, "message");
                assert_eq!(event.last_event_id, "last");
                assert_eq!(event.retry, Some(7));
                assert!(event.meaning.terminal.is_none());
                assert!(records[1].event.is_none());
                assert_eq!(records[2].event.as_ref().unwrap().data, "");
            }
        }
    }
    #[test]
    fn unknown_fields_never_dispatch_and_eof_discards_pending_data() {
        let (records, residual) = decode(
            b"[DONE]\n\n{\"type\":\"message_stop\"}\n\ndata: [DONE]\n",
            7,
        );
        assert!(records.iter().all(|r| r.event.is_none()));
        let residual = residual.unwrap();
        assert!(residual.event.is_none());
        assert_eq!(residual.original, b"data: [DONE]\n".as_slice());
    }
    #[test]
    fn utf8_replacement_does_not_change_original_bytes() {
        let input = b"data: a\xff\xf0\x9f\x92\xa9\n\n";
        for split in 0..=input.len() {
            let (records, _) = decode(input, split);
            assert_eq!(
                records[0].event.as_ref().unwrap().data,
                "a\u{fffd}\u{1f4a9}"
            );
            assert_eq!(serialize(&records[0]).unwrap(), input.as_slice());
        }
    }
    #[test]
    fn comments_and_control_blocks_keep_duplicate_fields_and_spans() {
        let input = b": comment\nevent: ignored\nid\nretry: 9\nunknown: value\n\n";
        let (records, _) = decode(input, 3);
        assert!(records[0].event.is_none());
        assert_eq!(records[0].lines.len(), 6);
        assert_eq!(serialize(&records[0]).unwrap(), input.as_slice());
    }
    #[test]
    fn edits_keep_unknown_fields_and_failure_meaning() {
        let (mut records,_) = decode(b"event: response.error\nid: first\ndata: {\"type\":\"response.error\",\"error\":{}}\nid: last\ndata: \n\n", 10);
        let record = &mut records[0];
        let meaning = record.event.as_ref().unwrap().meaning;
        record.policy = WirePolicy::Middleware;
        record.edit_json(|value| {
            value["type"] = Value::String("error".into());
        });
        assert_eq!(record.event.as_ref().unwrap().meaning, meaning);
        assert_eq!(
            serialize(record).unwrap(),
            b"event: error\nid: first\ndata: {\"type\":\"error\",\"error\":{}}\nid: last\n\n"
                .as_slice()
        );
        record.set_encrypted(serde_json::json!({"encrypted":"value"}));
        assert_eq!(record.event.as_ref().unwrap().meaning, meaning);
        assert_eq!(
            serialize(record).unwrap(),
            b"event: error\nid: first\nid: last\ndata: {\"encrypted\":\"value\"}\n\n".as_slice()
        );
    }

    #[test]
    fn bounded_event_and_comment_eof() {
        let mut decoder = Decoder::new(None);
        decoder
            .push(Bytes::from(vec![b'x'; MAX_EVENT_BYTES + 1]))
            .unwrap();
        assert_eq!(decoder.next(), Err(DecodeError::Overflow));
        let mut decoder = Decoder::new(None);
        decoder.push(Bytes::from_static(b": heartbeat\r")).unwrap();
        assert!(decoder.next().unwrap().is_none());
        decoder.end();
        assert!(decoder.next().unwrap().is_none());
        assert!(decoder.at_boundary());
        let mut comment = decoder.take_residual().unwrap();
        comment.policy = WirePolicy::Middleware;
        assert!(!comment.residual);
        assert_eq!(serialize(&comment).unwrap(), b": heartbeat\n\n".as_slice());
        let comment = Record::comment("PROCESSING");
        assert_eq!(serialize(&comment).unwrap(), b": PROCESSING\n\n".as_slice());
    }
}

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{ServiceError, ServiceResponseStream};

pub(crate) type EventStream = Pin<Box<dyn Stream<Item = Result<Record, ServiceError>> + Send>>;

/// Byte adapters are used only at ingress and at public byte-stream API boundaries.
/// The application pipeline passes `Record` between all of its stages.
pub(crate) struct DecodedStream {
    inner: ServiceResponseStream,
    decoder: Decoder,
    ended: bool,
    finished: bool,
}

impl DecodedStream {
    pub fn new(inner: ServiceResponseStream, protocol: Option<SseProtocol>) -> Self {
        Self {
            inner,
            decoder: Decoder::new(protocol),
            ended: false,
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
        loop {
            match this.decoder.next() {
                Ok(Some(record)) => return Poll::Ready(Some(Ok(record))),
                Err(_) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(ServiceError::Upstream(
                        UpstreamError::Transport("provider SSE event exceeded the size cap".into()),
                    ))));
                }
                Ok(None) => {}
            }
            if this.ended {
                this.finished = true;
                return Poll::Ready(this.decoder.take_residual().map(Ok));
            }
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) => {
                    if this.decoder.push(bytes).is_err() {
                        this.finished = true;
                        return Poll::Ready(Some(Err(ServiceError::AdmissionInvariant(
                            "SSE ingress has pending input",
                        ))));
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => {
                    this.ended = true;
                    this.decoder.end();
                }
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
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(Some(Ok(record))) => match serialize(&record) {
                    Ok(bytes) if bytes.is_empty() => {}
                    Ok(bytes) => return Poll::Ready(Some(Ok(bytes))),
                    Err(_) => {
                        return Poll::Ready(Some(Err(ServiceError::AdmissionInvariant(
                            "SSE serialization failed",
                        ))))
                    }
                },
            }
        }
    }
}

impl Record {
    /// Conservative size of owned payload storage, used to bound conversion queues.
    pub fn buffered_bytes(&self) -> usize {
        fn size(value: &Value) -> usize {
            match value {
                Value::String(text) => text.len(),
                Value::Array(values) => values.iter().map(size).fold(0, usize::saturating_add),
                Value::Object(values) => values
                    .iter()
                    .map(|(key, value)| key.len().saturating_add(size(value)))
                    .fold(0, usize::saturating_add),
                _ => 16,
            }
        }
        self.original
            .len()
            .saturating_add(self.event.as_ref().map_or(0, |event| {
                event.data.len().saturating_add(match &event.payload {
                    Payload::Json(value) => size(value),
                    Payload::Text(text) => text.len(),
                })
            }))
    }
}
