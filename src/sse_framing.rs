//! Read-only SSE framing observer, shared by the receipt finalizer and the
//! metering pass.
//!
//! Both need the same answer to "how did this stream end" — did it deliver its
//! protocol terminal, an in-band error, or stop mid-event — and getting it from
//! two independent line parsers let them disagree (one counting a `data:` line
//! the instant it is read, the other only once a blank line dispatches it). One
//! implementation, byte-accurate about SSE framing, removes that gap.
//!
//! It observes; it never rewrites. The finalizer hashes the bytes and the meter
//! injects cost through their own passes — this only watches.

use serde_json::Value;

use crate::sse_protocol::{sse_protocol, SseProtocol};

/// Cap on the reassembly buffers. Without one, a response that never sends a
/// line terminator would grow them until the request runs out of memory. A
/// single SSE event above this is pathological.
const MAX_OBSERVED_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// Watches an SSE stream as it flows past: the identifiers a receipt records,
/// and how the stream ended.
///
/// "Ended" is decided at the framing level, not the content level. A `[DONE]`,
/// `message_stop`, or Responses terminal counts only once a blank line has
/// dispatched the event carrying it; an in-band error likewise. A per-choice
/// `finish_reason` is not a terminal — whether a terminator-less stream is
/// complete is decided by its caller from how the body ended.
pub struct SseFramingObserver {
    line_buffer: Vec<u8>,
    event_data: Vec<u8>,
    /// A `\r` closed the previous line; a `\n` right after it belongs to that
    /// same CRLF terminator rather than opening an empty line.
    pending_cr: bool,
    chat_id: Option<String>,
    model_id: Option<String>,
    /// `None` when these bytes are not the client-facing stream, so no terminal
    /// is meaningful and only the identifiers are collected.
    protocol: Option<SseProtocol>,
    saw_terminal: bool,
    saw_error: bool,
    /// A non-comment field line has been read but not yet dispatched by a blank
    /// line: the stream is mid-event. Tracks the WHATWG "event in progress"
    /// state (any field, not just `data:`), so a pending `event:`/`id:` is not
    /// mistaken for a boundary where a terminator could be spliced in.
    mid_event: bool,
    last_sequence_number: Option<u64>,
    /// A single line or event ran past the cap. The buffers were dropped, so
    /// what the stream did afterwards is no longer known.
    observation_failed: bool,
}

impl SseFramingObserver {
    /// Observe the client-facing stream: identifiers and how it ended.
    pub fn new(endpoint_path: &str) -> Self {
        Self::with_protocol(Some(sse_protocol(endpoint_path)))
    }

    /// Observe a client-facing stream whose protocol is already known.
    pub fn for_protocol(protocol: SseProtocol) -> Self {
        Self::with_protocol(Some(protocol))
    }

    /// Observe bytes that are not the client-facing stream — an upstream-format
    /// draft, or a buffered body. A terminal in some other protocol would mean
    /// nothing here, so only the identifiers are collected.
    pub fn identifiers_only() -> Self {
        Self::with_protocol(None)
    }

    fn with_protocol(protocol: Option<SseProtocol>) -> Self {
        Self {
            line_buffer: Vec::new(),
            event_data: Vec::new(),
            pending_cr: false,
            chat_id: None,
            model_id: None,
            protocol,
            saw_terminal: false,
            saw_error: false,
            mid_event: false,
            last_sequence_number: None,
            observation_failed: false,
        }
    }

    pub fn protocol(&self) -> Option<SseProtocol> {
        self.protocol
    }

    /// The stream delivered its protocol's framing terminal.
    pub fn saw_terminal(&self) -> bool {
        self.saw_terminal
    }

    /// The stream delivered an in-band error event; the upstream told the client
    /// what went wrong, so no gateway error should be appended on top.
    pub fn saw_error(&self) -> bool {
        self.saw_error
    }

    /// The highest `sequence_number` seen, for protocols that number events.
    pub fn last_sequence_number(&self) -> Option<u64> {
        self.last_sequence_number
    }

    pub fn chat_id(&self) -> Option<String> {
        self.chat_id.clone()
    }

    pub fn model_id(&self) -> Option<String> {
        self.model_id.clone()
    }

    /// The stream is between events: no line is half-written and no event is
    /// waiting for its blank line. This is where a missing terminator can be
    /// supplied without splitting an event the client would otherwise discard.
    pub fn at_event_boundary(&self) -> bool {
        self.line_buffer.is_empty() && !self.mid_event
    }

    /// Whether this stream can be judged at all.
    pub fn observation_usable(&self) -> bool {
        !self.observation_failed
    }

    pub fn observe(&mut self, chunk: &[u8]) {
        if self.done() {
            return;
        }
        for &byte in chunk {
            // LF, CRLF and a lone CR all terminate a line. Splitting only on
            // `\n` would leave a CR-framed stream as one endless line, so its
            // terminal would never be seen and a complete response would look
            // truncated.
            let was_pending_cr = std::mem::take(&mut self.pending_cr);
            if was_pending_cr && byte == b'\n' {
                continue;
            }
            match byte {
                b'\r' | b'\n' => {
                    self.pending_cr = byte == b'\r';
                    let line = std::mem::take(&mut self.line_buffer);
                    self.observe_line(&line);
                    if self.done() {
                        return;
                    }
                }
                _ => self.line_buffer.push(byte),
            }
            if self.line_buffer.len() + self.event_data.len() > MAX_OBSERVED_EVENT_BYTES {
                // Give up rather than keep buffering. The bytes still reach the
                // client untouched; only the judgement about how the stream
                // ended is lost, and a lost judgement must not become a guess.
                self.observation_failed = true;
                self.line_buffer = Vec::new();
                self.event_data = Vec::new();
                return;
            }
        }
    }

    /// Nothing left to learn: the identifiers are known and the stream has
    /// ended, so no later byte can change the answer.
    fn done(&self) -> bool {
        self.observation_failed
            || (self.chat_id.is_some()
                && self.model_id.is_some()
                && (self.protocol.is_none() || self.saw_terminal || self.saw_error))
    }

    fn observe_line(&mut self, line: &[u8]) {
        // A blank line ends the event and resets the framing state, whether or
        // not a message was dispatched.
        if line.is_empty() {
            self.dispatch_event();
            self.mid_event = false;
            return;
        }
        // Comments do not start an event.
        if line.starts_with(b":") {
            return;
        }
        // Any field line (`data:`, `event:`, `id:`, an unknown field) puts an
        // event in progress; only `data:` contributes to the payload watched
        // for identifiers and `[DONE]`.
        self.mid_event = true;
        let Some(rest) = line.strip_prefix(b"data:") else {
            return;
        };
        // WHATWG: append the value and a single LF for every `data:` field,
        // including an empty one. `dispatch` strips one trailing LF. So a lone
        // empty `data:` before `data: [DONE]` yields the payload "\n[DONE]",
        // which is not the terminator — as the client's own parser sees it.
        let data = rest.strip_prefix(b" ").unwrap_or(rest);
        self.event_data.extend_from_slice(data);
        self.event_data.push(b'\n');
    }

    fn dispatch_event(&mut self) {
        // Empty means no `data:` field was seen (each appends at least an LF).
        if self.event_data.is_empty() {
            return;
        }
        let mut data = std::mem::take(&mut self.event_data);
        if data.last() == Some(&b'\n') {
            data.pop();
        }
        if data.as_slice() == b"[DONE]" {
            self.saw_terminal |= self.protocol == Some(SseProtocol::OpenaiChat);
            return;
        }
        let Ok(parsed) = serde_json::from_slice::<Value>(&data) else {
            return;
        };
        self.observe_terminal(&parsed);
        if self.chat_id.is_none() {
            self.chat_id = parsed.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if self.model_id.is_none() {
            self.model_id = parsed
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }

    /// Note the protocol's framing terminal or in-band error, and the event
    /// numbering a Responses error tail continues from.
    ///
    /// A per-choice `finish_reason` / `stop_reason` is deliberately not a
    /// terminal: whether the stream is complete is decided by how it ended
    /// (a clean end vs a transport error), not by a content-level signal that
    /// says nothing about the framing around it.
    fn observe_terminal(&mut self, parsed: &Value) {
        if let Some(sequence) = parsed.get("sequence_number").and_then(Value::as_u64) {
            self.last_sequence_number = Some(match self.last_sequence_number {
                Some(previous) => previous.max(sequence),
                None => sequence,
            });
        }
        let Some(protocol) = self.protocol else {
            return;
        };
        let event_type = parsed.get("type").and_then(Value::as_str);
        let terminal = match protocol {
            SseProtocol::OpenaiChat => false,
            SseProtocol::AnthropicMessages => event_type == Some("message_stop"),
            SseProtocol::OpenaiResponses => matches!(
                event_type,
                Some("response.completed" | "response.incomplete" | "response.failed")
            ),
        };
        self.saw_terminal |= terminal;
        // An error is an error however the protocol frames it: a top-level
        // `error`, an `error`-typed event, a Responses `response.failed`, the
        // error object nested under `response`, or an error-shaped finish reason
        // (an upstream error smuggled through `finish_reason` / `stop_reason` —
        // not a framing terminal, but a failure the finalizer must not stamp a
        // success terminator over, matching the meter's own detection).
        let responses_failed = event_type == Some("response.failed");
        let nested_error = parsed
            .get("response")
            .and_then(|r| r.get("error"))
            .is_some_and(|e| !e.is_null());
        let is_error_reason = |r: &str| r == "error" || r.ends_with("_error");
        let choices_error =
            parsed
                .get("choices")
                .and_then(Value::as_array)
                .is_some_and(|choices| {
                    choices.iter().any(|choice| {
                        choice
                            .get("finish_reason")
                            .and_then(Value::as_str)
                            .is_some_and(is_error_reason)
                    })
                });
        let stop_reason_error = parsed
            .get("delta")
            .and_then(|d| d.get("stop_reason"))
            .and_then(Value::as_str)
            .is_some_and(is_error_reason);
        self.saw_error |= event_type == Some("error")
            || parsed.get("error").is_some_and(|e| !e.is_null())
            || responses_failed
            || nested_error
            || choices_error
            || stop_reason_error;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::service::{CHAT_COMPLETIONS_PATH, MESSAGES_PATH, RESPONSES_PATH};

    fn observe(path: &str, chunks: &[&str]) -> SseFramingObserver {
        let mut parser = SseFramingObserver::new(path);
        for chunk in chunks {
            parser.observe(chunk.as_bytes());
        }
        parser
    }

    // Every SSE line terminator has to split lines. Splitting only on `\n`
    // would leave a CR-framed stream as one endless line whose terminal is
    // never seen — and an unseen terminal makes a complete response look
    // truncated, the one direction that corrupts good output.
    #[test]
    fn every_line_terminator_reveals_the_terminal() {
        for framing in ["\n\n", "\r\n\r\n", "\r\r"] {
            let stream =
                format!("data: {{\"id\":\"a\",\"model\":\"m\"}}{framing}data: [DONE]{framing}");
            let parser = observe(CHAT_COMPLETIONS_PATH, &[&stream]);
            assert!(
                parser.saw_terminal(),
                "terminal missed with framing {framing:?}"
            );
            assert_eq!(parser.chat_id().as_deref(), Some("a"));
        }
    }

    // A terminator split across chunks is still one terminator.
    #[test]
    fn a_split_crlf_is_not_two_line_endings() {
        let parser = observe(
            MESSAGES_PATH,
            &[
                "event: message_stop\r",
                "\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
            ],
        );
        assert!(parser.saw_terminal());
    }

    // A terminal counts only once a blank line has dispatched the event that
    // carried it. A `[DONE]` line with no blank line after it is still pending,
    // and the stream is not at a boundary.
    #[test]
    fn a_terminal_counts_only_once_dispatched() {
        let dispatched = observe(CHAT_COMPLETIONS_PATH, &["data: [DONE]\n\n"]);
        assert!(dispatched.saw_terminal());
        assert!(dispatched.at_event_boundary());

        let pending = observe(CHAT_COMPLETIONS_PATH, &["data: [DONE]\n"]);
        assert!(!pending.saw_terminal(), "no blank line dispatched it");
        assert!(!pending.at_event_boundary(), "the event is still pending");
    }

    // The observer tracks only the protocol's framing terminal, each on its own
    // surface. A per-choice finish reason is not one.
    #[test]
    fn only_the_framing_terminal_settles_the_observer() {
        let chat_finish = observe(
            CHAT_COMPLETIONS_PATH,
            &["data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n"],
        );
        assert!(
            !chat_finish.saw_terminal(),
            "a finish reason is not the terminal"
        );

        let anthropic_stop_reason = observe(
            MESSAGES_PATH,
            &["data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"],
        );
        assert!(!anthropic_stop_reason.saw_terminal());

        let anthropic_stop = observe(MESSAGES_PATH, &["data: {\"type\":\"message_stop\"}\n\n"]);
        assert!(anthropic_stop.saw_terminal());

        // A framing terminal from another protocol does not count.
        let anthropic_done = observe(MESSAGES_PATH, &["data: [DONE]\n\n"]);
        assert!(!anthropic_done.saw_terminal());

        let responses_done = observe(
            RESPONSES_PATH,
            &["data: {\"type\":\"response.completed\",\"sequence_number\":7}\n\n"],
        );
        assert!(responses_done.saw_terminal());
    }

    // An in-band error is tracked apart from the framing terminal: the finalizer
    // suppresses its own error on either, but the meter fails the outcome on an
    // error and completes it on a terminal.
    #[test]
    fn an_in_band_error_is_distinct_from_a_terminal() {
        let parser = observe(
            CHAT_COMPLETIONS_PATH,
            &["data: {\"error\":{\"message\":\"boom\"}}\n\n"],
        );
        assert!(parser.saw_error());
        assert!(!parser.saw_terminal());
    }

    // An error-shaped finish reason (an upstream error smuggled through
    // `finish_reason`/`stop_reason`) is a failure, though not a framing
    // terminal — so the finalizer agrees with the meter and stamps no success
    // terminator over it.
    #[test]
    fn an_error_shaped_finish_reason_is_an_error_not_a_terminal() {
        let chat = observe(
            CHAT_COMPLETIONS_PATH,
            &["data: {\"choices\":[{\"finish_reason\":\"upstream_error\"}]}\n\n"],
        );
        assert!(chat.saw_error());
        assert!(!chat.saw_terminal(), "a finish reason is not a terminal");

        let anthropic = observe(
            MESSAGES_PATH,
            &["data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"error\"}}\n\n"],
        );
        assert!(anthropic.saw_error());
        assert!(!anthropic.saw_terminal());

        // A normal finish reason is neither.
        let ok = observe(
            CHAT_COMPLETIONS_PATH,
            &["data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n"],
        );
        assert!(!ok.saw_error());
        assert!(!ok.saw_terminal());
    }

    // The error tail continues the event numbering rather than restarting it.
    #[test]
    fn the_last_sequence_number_is_tracked() {
        let parser = observe(
            RESPONSES_PATH,
            &[
                "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":3}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":9}\n\n",
            ],
        );
        assert!(!parser.saw_terminal(), "truncated before its terminal");
        assert_eq!(parser.last_sequence_number(), Some(9));
    }

    // A stream cut mid-line or mid-event is not at a boundary.
    #[test]
    fn a_partial_event_is_not_a_boundary() {
        let complete = observe(CHAT_COMPLETIONS_PATH, &["data: {\"a\":1}\n\n"]);
        assert!(complete.at_event_boundary());

        let mid_line = observe(CHAT_COMPLETIONS_PATH, &["data: {\"a\":"]);
        assert!(!mid_line.at_event_boundary());

        let mid_event = observe(CHAT_COMPLETIONS_PATH, &["data: {\"a\":1}\n"]);
        assert!(!mid_event.at_event_boundary());
    }

    // A pending `event:` field with no dispatching blank line is not a boundary:
    // splicing a terminator in would fold it into that event.
    #[test]
    fn a_pending_event_field_is_not_a_boundary() {
        let pending = observe(CHAT_COMPLETIONS_PATH, &["event: custom\n"]);
        assert!(!pending.at_event_boundary());

        // A blank line resets the event state, dispatched or not.
        let reset = observe(CHAT_COMPLETIONS_PATH, &["event: custom\n\n"]);
        assert!(reset.at_event_boundary());

        // A lone comment does not start an event.
        let comment = observe(CHAT_COMPLETIONS_PATH, &[": keep-alive\n"]);
        assert!(comment.at_event_boundary());
    }

    // A Responses failure is an error however it is framed: the `response.failed`
    // event and the error nested under `response` both count.
    #[test]
    fn a_responses_failure_is_an_error() {
        let failed = observe(
            RESPONSES_PATH,
            &["data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"x\"}}}\n\n"],
        );
        assert!(
            failed.saw_error(),
            "response.failed is a failure, not a success"
        );
    }

    // WHATWG appends an LF per `data:` field, so an empty `data:` before
    // `data: [DONE]` makes the payload "\n[DONE]" — not the terminator, exactly
    // as the client's own parser would decode it.
    #[test]
    fn an_empty_data_line_is_not_folded_away() {
        let terminal = observe(CHAT_COMPLETIONS_PATH, &["data: [DONE]\n\n"]);
        assert!(terminal.saw_terminal());

        let not_terminal = observe(CHAT_COMPLETIONS_PATH, &["data:\ndata: [DONE]\n\n"]);
        assert!(
            !not_terminal.saw_terminal(),
            "payload is \\n[DONE], not the [DONE] sentinel"
        );

        // Multi-line data joins with LF (each line its own `data:` field),
        // forming one JSON payload — matching the client's decode.
        let multi = observe(
            CHAT_COMPLETIONS_PATH,
            &["data: {\"id\":\"a\",\ndata: \"model\":\"m\"}\n\n"],
        );
        assert_eq!(multi.chat_id().as_deref(), Some("a"));
    }

    // Past the cap the stream is no longer judged at all; a lost judgement must
    // not become a guess.
    #[test]
    fn an_unbounded_line_ends_observation_rather_than_memory() {
        let mut parser = SseFramingObserver::new(CHAT_COMPLETIONS_PATH);
        parser.observe(&vec![b'x'; MAX_OBSERVED_EVENT_BYTES + 1]);
        assert!(!parser.observation_usable());
    }

    // Bytes that are not the client-facing stream carry no meaningful terminal.
    #[test]
    fn identifier_only_parsing_tracks_no_terminal() {
        let mut parser = SseFramingObserver::identifiers_only();
        parser.observe(b"data: {\"id\":\"a\",\"model\":\"m\"}\n\ndata: [DONE]\n\n");
        assert_eq!(parser.chat_id().as_deref(), Some("a"));
        assert!(!parser.saw_terminal());
    }
}
