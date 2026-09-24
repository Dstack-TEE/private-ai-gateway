//! The `data` fields of a server-sent event stream, read from bytes as they
//! arrive (WHATWG HTML, "Interpreting an event stream"). Lines end in LF or
//! CRLF. Memory stays bounded: a line longer than [`MAX_LINE_BYTES`] is
//! skipped whole.

/// Matches the gateway's SSE limit: Responses terminal events repeat the full output.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub struct DataLines {
    line: Vec<u8>,
    overflow: bool,
}

impl DataLines {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next bytes, calling `data` with the value of every `data`
    /// line they complete.
    pub fn push(&mut self, mut bytes: &[u8], mut data: impl FnMut(&[u8])) {
        while let Some(end) = bytes.iter().position(|byte| *byte == b'\n') {
            self.append(&bytes[..end]);
            self.end_line(&mut data);
            bytes = &bytes[end + 1..];
        }
        self.append(bytes);
    }

    /// Ends the stream; a last line without a line break still counts.
    pub fn finish(&mut self, mut data: impl FnMut(&[u8])) {
        if !self.line.is_empty() || self.overflow {
            self.end_line(&mut data);
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        if self.overflow {
            return;
        }
        if self.line.len().saturating_add(bytes.len()) > MAX_LINE_BYTES {
            self.line = Vec::new();
            self.overflow = true;
        } else {
            self.line.extend_from_slice(bytes);
        }
    }

    fn end_line(&mut self, data: &mut impl FnMut(&[u8])) {
        if !self.overflow {
            if let Some(value) = data_value(&self.line) {
                data(value);
            }
        }
        self.line.clear();
        self.overflow = false;
    }
}

/// The value of a `data` line, without the one optional space after the colon.
fn data_value(line: &[u8]) -> Option<&[u8]> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let value = line.strip_prefix(b"data:")?;
    Some(value.strip_prefix(b" ").unwrap_or(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<String> {
        let mut values = Vec::new();
        let mut lines = DataLines::new();
        let mut record = |value: &[u8]| values.push(String::from_utf8_lossy(value).into_owned());
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
    fn oversized_lines_are_skipped_up_to_the_next_line() {
        let long = vec![b'x'; MAX_LINE_BYTES];
        assert_eq!(collect(&[b"data: ", &long, b"\ndata: next\n"]), ["next"]);
        assert!(collect(&[b"data: ", &long]).is_empty());
        let fits = [b"data:".as_slice(), &long[5..]].concat();
        assert_eq!(collect(&[&fits]).len(), 1);
    }
}
