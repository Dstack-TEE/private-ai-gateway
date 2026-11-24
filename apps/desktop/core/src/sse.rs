//! The `data` fields of a server-sent event stream, read from bytes as they
//! arrive (WHATWG HTML, "Interpreting an event stream"). tokio-util's
//! `LinesCodec` splits LF and CRLF lines and skips a line longer than
//! [`MAX_LINE_BYTES`], so memory stays bounded; a stream framed with bare CR
//! line endings, which the format also allows, is not split.

use tokio_util::{
    bytes::BytesMut,
    codec::{Decoder, LinesCodec},
};

/// Matches the gateway's SSE limit: Responses terminal events repeat the full output.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

pub struct DataLines {
    codec: LinesCodec,
    buffer: BytesMut,
    event: String,
    has_data: bool,
    discarding_event: bool,
}

impl Default for DataLines {
    fn default() -> Self {
        Self {
            codec: LinesCodec::new_with_max_length(MAX_LINE_BYTES),
            buffer: BytesMut::new(),
            event: String::new(),
            has_data: false,
            discarding_event: false,
        }
    }
}

impl DataLines {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next bytes, calling `data` for each completed event. Multiple
    /// `data:` lines in an event are joined with a newline.
    pub fn push(&mut self, bytes: &[u8], mut data: impl FnMut(&str)) {
        self.buffer.extend_from_slice(bytes);
        // An over-long or non-UTF-8 line is an error for that line only.
        while let Some(line) = self.codec.decode(&mut self.buffer).transpose() {
            if let Ok(line) = line {
                self.line(&line, &mut data);
            }
        }
    }

    /// Ends the stream; a last line without a line break still counts.
    pub fn finish(&mut self, mut data: impl FnMut(&str)) {
        while let Some(line) = self.codec.decode_eof(&mut self.buffer).transpose() {
            if let Ok(line) = line {
                self.line(&line, &mut data);
            }
        }
        self.dispatch(&mut data);
    }

    fn line(&mut self, line: &str, data: &mut impl FnMut(&str)) {
        if line.is_empty() {
            self.dispatch(data);
        } else if let Some(value) = data_value(line) {
            if self.discarding_event {
                return;
            }
            if self.event.len() + usize::from(self.has_data) + value.len() > MAX_LINE_BYTES {
                self.event.clear();
                self.has_data = false;
                self.discarding_event = true;
                return;
            }
            if self.has_data {
                self.event.push('\n');
            }
            self.event.push_str(value);
            self.has_data = true;
        }
    }

    fn dispatch(&mut self, data: &mut impl FnMut(&str)) {
        if self.has_data {
            data(&self.event);
        }
        self.event.clear();
        self.has_data = false;
        self.discarding_event = false;
    }
}

/// The value of a `data` line, without the one optional space after the colon.
fn data_value(line: &str) -> Option<&str> {
    let value = line.strip_prefix("data:")?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<String> {
        let mut values = Vec::new();
        let mut lines = DataLines::new();
        let mut record = |value: &str| values.push(value.to_string());
        for chunk in chunks {
            lines.push(chunk, &mut record);
        }
        lines.finish(&mut record);
        values
    }

    #[test]
    fn data_values_span_chunks_and_line_endings() {
        assert_eq!(
            collect(&[
                b"event: delta\r\ndata: {\"a\"",
                b":1}\r\n\r\ndata:{\"b\":2}\n: comment\nid: 7\n\ndata:  two spaces",
            ]),
            ["{\"a\":1}", "{\"b\":2}", " two spaces"]
        );
        assert!(collect(&[b"data"]).is_empty());
    }

    #[test]
    fn an_event_joins_data_lines_including_empty_values() {
        let mut parser = DataLines::new();
        let mut values = Vec::new();
        parser.push(b"data: first\r\ndata: sec", |value| {
            values.push(value.to_owned())
        });
        assert!(values.is_empty());
        parser.push(
            b"ond\r\n: comment\r\ndata:\r\ndata: fourth\r\n\r\n\ndata:\n\n",
            |value| values.push(value.to_owned()),
        );
        assert_eq!(values, ["first\nsecond\n\nfourth", ""]);
    }

    #[test]
    fn an_oversized_event_is_skipped_up_to_the_next_event() {
        let line = vec![b'x'; MAX_LINE_BYTES / 2];
        assert_eq!(
            collect(&[
                b"data:",
                &line,
                b"\ndata:",
                &line,
                b"\ndata: tail\n\ndata: next\n\n"
            ]),
            ["next"]
        );
    }

    #[test]
    fn oversized_and_invalid_lines_are_skipped_up_to_the_next_line() {
        let long = vec![b'x'; MAX_LINE_BYTES];
        assert_eq!(
            collect(&[b"data: ", &long, b"\ndata: \xff\ndata: next\n"]),
            ["next"]
        );
        assert!(collect(&[b"data: ", &long]).is_empty());
        let fits = [b"data:".as_slice(), &long[5..]].concat();
        assert_eq!(collect(&[&fits]).len(), 1);
    }
}
