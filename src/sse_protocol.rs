//! Streaming protocol of a downstream endpoint, and the in-stream error it
//! carries.
//!
//! Which protocol an endpoint speaks decides the event that ends a stream and
//! the shape an error takes inside one. The receipt finalizer uses this to end
//! a stream that stopped without its terminal, choosing by how it ended: supply
//! the protocol's success terminator where it is a fixed marker, append the
//! protocol's error on a transport failure, or leave the stream as-is when a
//! clean end has no terminator the gateway may fabricate.

use serde_json::{json, Value};

use crate::aggregator::service::{MESSAGES_PATH, RESPONSES_PATH};
use crate::error_payload::{envelope, error_type, upstream_message, Surface};

/// Streaming protocol of a downstream endpoint. Narrower than [`Surface`],
/// which groups all OpenAI-compatible endpoints together: the chat/completions
/// and responses streams carry errors and terminate differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseProtocol {
    OpenaiChat,
    OpenaiResponses,
    AnthropicMessages,
}

/// The streaming protocol an endpoint speaks.
pub fn sse_protocol(endpoint_path: &str) -> SseProtocol {
    match endpoint_path {
        MESSAGES_PATH => SseProtocol::AnthropicMessages,
        RESPONSES_PATH => SseProtocol::OpenaiResponses,
        _ => SseProtocol::OpenaiChat,
    }
}

/// SSE bytes that complete the framing of a clean but terminator-less stream,
/// or `None` if the protocol's terminal carries state the gateway cannot
/// fabricate. A clean end is a success either way; this only decides whether a
/// missing terminator can be filled in for the client.
pub fn stream_success_terminator(protocol: SseProtocol) -> Option<&'static str> {
    match protocol {
        // `[DONE]` is a fixed marker carrying no state, so a chat provider that
        // closed normally without it has still finished; the gateway completes
        // the framing.
        SseProtocol::OpenaiChat => Some("data: [DONE]\n\n"),
        // The gateway does not fabricate a terminator for the other surfaces:
        // supplying an event the upstream never sent would assert state it did
        // not (a `message_stop` with no preceding `stop_reason`, or a
        // `response.completed` with no usage). A clean end is still recorded
        // complete; the client simply receives what the upstream sent.
        SseProtocol::AnthropicMessages | SseProtocol::OpenaiResponses => None,
    }
}

/// SSE bytes that end a broken response stream visibly to the client.
///
/// A broken stream is ended gracefully rather than failed — a body `Err` would
/// make hyper abort the connection — but a graceful end alone reads as a
/// complete response. These events say it failed, in the shape the protocol
/// defines. Only the chat stream has a `[DONE]` sentinel to follow with; on the
/// other two the error event is itself terminal.
///
/// The message is the generic 502 text; the underlying error is only logged.
pub fn stream_error_tail(
    protocol: SseProtocol,
    request_id: Option<&str>,
    last_sequence_number: Option<u64>,
) -> String {
    let message = upstream_message(502);
    // The leading blank line dispatches an event written but not yet dispatched
    // (a `data:` line with no blank line after it), so these start their own
    // event instead of folding into it as extra `data:` lines. Two newlines,
    // not one: after a `\r` a single `\n` completes a CRLF, which is one line
    // terminator, and the next `data:` line would join the same event. Against
    // an already dispatched stream the extra blank lines are ignored.
    match protocol {
        SseProtocol::AnthropicMessages => {
            let body = envelope(
                Surface::Anthropic,
                error_type(Surface::Anthropic, 502),
                message,
                request_id,
            );
            format!("\n\nevent: error\ndata: {}\n\n", json_str(&body))
        }
        SseProtocol::OpenaiResponses => {
            // The protocol numbers its events, so the error continues the
            // sequence rather than restarting it. A caller with no view of the
            // stream has none to continue from.
            let body = json!({
                "type": "error",
                "code": Value::Null,
                "message": message,
                "param": Value::Null,
                "sequence_number": last_sequence_number.map_or(0, |last| last.saturating_add(1)),
            });
            format!("\n\nevent: error\ndata: {}\n\n", json_str(&body))
        }
        SseProtocol::OpenaiChat => {
            let body = envelope(
                Surface::Openai,
                error_type(Surface::Openai, 502),
                message,
                request_id,
            );
            format!("\n\ndata: {}\n\ndata: [DONE]\n\n", json_str(&body))
        }
    }
}

fn json_str(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
