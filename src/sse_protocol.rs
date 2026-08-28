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

/// The chat error object the gateway emits for a failure it will not attribute
/// to the caller. One definition for its two producers: the tail this module
/// appends to a stream that broke, and the rebuild of an upstream's own
/// in-band error in `response_transform` — a client must not be able to tell
/// the two apart, which a differing `code` or field order would give away.
///
/// `code` is the numeric status, unlike the HTTP error envelope's string-or-
/// null: this object rides inside a 200 stream, where consumers read `code` as
/// the status the failed chunk stands for.
pub fn chat_gateway_error(status: u16) -> Value {
    json!({
        "message": upstream_message(status),
        "type": error_type(Surface::Openai, status),
        "code": status,
        "param": Value::Null,
    })
}

/// The Responses `code` the gateway emits for a failure it will not attribute
/// to the caller: the surface's own official `rate_limit_exceeded` for a rate
/// limit, its official generic otherwise. Both are in the documented code
/// enum — a word from another surface's vocabulary would fail a strict
/// client's validation of it.
pub fn responses_gateway_code(status: u16) -> &'static str {
    match status {
        429 => "rate_limit_exceeded",
        // A request fault must not render as a server error: `code` is the only
        // machine-readable field on this event, and `server_error` beside "the
        // request was rejected as invalid" tells the client to retry something
        // that cannot succeed. `invalid_prompt` is the enum's request-fault
        // value. Statuses with no enum value (404) keep the generic.
        400 | 413 | 422 => "invalid_prompt",
        _ => "server_error",
    }
}

/// The Responses in-stream error event. One definition for its two producers:
/// the tail this module appends to a stream that broke, and the rebuild of an
/// upstream's own in-band error in `response_transform` — a client should not
/// be able to tell the two apart.
///
/// `code` and `param` are `string | null` on the wire; the parameter types
/// enforce that, so no caller can smuggle a numeric code or a structured
/// `param` into the event.
pub fn responses_error_event(
    code: Option<&str>,
    message: &str,
    param: Option<&str>,
    sequence_number: Option<u64>,
) -> Value {
    let mut event = json!({
        "type": "error",
        "code": code,
        "message": message,
        "param": param,
    });
    // Only when the upstream numbered its events. Inventing a 0 mid-stream
    // would break the monotonic sequence a client tracks, and claim a position
    // the protocol never gave this event.
    if let Some(sequence_number) = sequence_number {
        event["sequence_number"] = json!(sequence_number);
    }
    event
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
    stream_error_event(protocol, 502, request_id, last_sequence_number)
}

/// Like [`stream_error_tail`], but for a chosen status. Used where the gateway
/// commits a 200 stream before the upstream answers (the pre-first-byte
/// keep-alive) and then has to deliver the real failure in-band: the client
/// reads `code`/`type` as the status the request would otherwise have had.
pub fn stream_error_event(
    protocol: SseProtocol,
    status: u16,
    request_id: Option<&str>,
    last_sequence_number: Option<u64>,
) -> String {
    let message = upstream_message(status);
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
                error_type(Surface::Anthropic, status),
                message,
                request_id,
            );
            format!("\n\nevent: error\ndata: {}\n\n", json_str(&body))
        }
        SseProtocol::OpenaiResponses => {
            // The protocol numbers its events, so the error continues the
            // sequence rather than restarting it. A caller with no view of the
            // stream has none to continue from. The code is the one the
            // gateway emits for any unattributed failure: a null here would
            // let a client tell a gateway-detected break from an
            // upstream-reported error, which are deliberately
            // indistinguishable.
            let body = responses_error_event(
                Some(responses_gateway_code(status)),
                message,
                None,
                Some(last_sequence_number.map_or(0, |last| last.saturating_add(1))),
            );
            format!("\n\nevent: error\ndata: {}\n\n", json_str(&body))
        }
        SseProtocol::OpenaiChat => {
            let body = json!({ "error": chat_gateway_error(status) });
            format!("\n\ndata: {}\n\ndata: [DONE]\n\n", json_str(&body))
        }
    }
}

fn json_str(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
