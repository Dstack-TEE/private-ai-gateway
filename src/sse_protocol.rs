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

use crate::error_payload::{error_type, upstream_message, Surface};
use crate::middleware::{request_transform::Endpoint, types::ProviderFormat};

/// Streaming protocol of a downstream endpoint. Narrower than [`Surface`],
/// which groups all OpenAI-compatible endpoints together: the chat/completions
/// and responses streams carry errors and terminate differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseProtocol {
    OpenaiChat,
    OpenaiResponses,
    AnthropicMessages,
}

impl From<Endpoint> for SseProtocol {
    fn from(endpoint: Endpoint) -> Self {
        match endpoint {
            Endpoint::Messages => Self::AnthropicMessages,
            Endpoint::CreateModelResponse => Self::OpenaiResponses,
            _ => Self::OpenaiChat,
        }
    }
}

impl From<(ProviderFormat, Endpoint)> for SseProtocol {
    fn from((format, endpoint): (ProviderFormat, Endpoint)) -> Self {
        match (format, endpoint) {
            (ProviderFormat::Anthropic, endpoint) if endpoint != Endpoint::Complete => {
                Self::AnthropicMessages
            }
            (ProviderFormat::Openai, Endpoint::CreateModelResponse) => Self::OpenaiResponses,
            _ => Self::OpenaiChat,
        }
    }
}

impl From<SseProtocol> for Endpoint {
    fn from(protocol: SseProtocol) -> Self {
        match protocol {
            SseProtocol::OpenaiChat => Self::ChatComplete,
            SseProtocol::AnthropicMessages => Self::Messages,
            SseProtocol::OpenaiResponses => Self::CreateModelResponse,
        }
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

/// Protocol meaning is assigned at application ingress and survives all edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum EventKind {
    #[default]
    Data,
    Done,
    MessageStart,
    MessageDelta,
    MessageStop,
    Ping,
    ContentBlockStart,
    ContentBlockDelta,
    ContentBlockStop,
    ResponseCreated,
    ResponseInProgress,
    ResponseCompleted,
    ResponseIncomplete,
    ResponseFailed,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalKind {
    Success,
    Incomplete,
    Failed,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureEvidence {
    Inband,
    ConflictingType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct EventMeaning {
    pub kind: EventKind,
    pub terminal: Option<TerminalKind>,
    pub failure: Option<FailureEvidence>,
}

impl EventMeaning {
    /// Constructors for converted/generated events use their known kind. They
    /// never parse wire bytes or reclassify an edited payload.
    pub fn generated(protocol: SseProtocol, kind: EventKind) -> Self {
        let terminal = match (protocol, kind) {
            (SseProtocol::OpenaiChat, EventKind::Done)
            | (SseProtocol::AnthropicMessages, EventKind::MessageStop)
            | (SseProtocol::OpenaiResponses, EventKind::ResponseCompleted) => {
                Some(TerminalKind::Success)
            }
            (SseProtocol::OpenaiResponses, EventKind::ResponseIncomplete) => {
                Some(TerminalKind::Incomplete)
            }
            (SseProtocol::OpenaiResponses, EventKind::ResponseFailed) => Some(TerminalKind::Failed),
            (SseProtocol::OpenaiResponses | SseProtocol::AnthropicMessages, EventKind::Error) => {
                Some(TerminalKind::Error)
            }
            _ => None,
        };
        let failure = matches!(kind, EventKind::Error | EventKind::ResponseFailed)
            .then_some(FailureEvidence::Inband);
        Self {
            kind,
            terminal,
            failure,
        }
    }
}

pub(crate) fn event_kind(name: &str) -> EventKind {
    match name {
        "message_start" => EventKind::MessageStart,
        "message_delta" => EventKind::MessageDelta,
        "message_stop" => EventKind::MessageStop,
        "ping" => EventKind::Ping,
        "content_block_start" => EventKind::ContentBlockStart,
        "content_block_delta" => EventKind::ContentBlockDelta,
        "content_block_stop" => EventKind::ContentBlockStop,
        "response.created" => EventKind::ResponseCreated,
        "response.in_progress" => EventKind::ResponseInProgress,
        "response.completed" => EventKind::ResponseCompleted,
        "response.incomplete" => EventKind::ResponseIncomplete,
        "response.failed" => EventKind::ResponseFailed,
        "response.error" | "error" => EventKind::Error,
        _ => EventKind::Data,
    }
}

pub(crate) fn classify(
    protocol: Option<SseProtocol>,
    name: Option<&str>,
    payload: &crate::sse::Payload,
) -> EventMeaning {
    use crate::sse::Payload;
    if matches!(payload, Payload::Text(text) if text == "[DONE]") {
        return protocol.map_or_else(
            || EventMeaning {
                kind: EventKind::Done,
                ..EventMeaning::default()
            },
            |protocol| EventMeaning::generated(protocol, EventKind::Done),
        );
    }
    let value = payload.json();
    let payload_type = value.and_then(|v| v.get("type")).and_then(Value::as_str);
    let name = name.filter(|name| !name.is_empty());
    let payload_kind = event_kind(payload_type.unwrap_or_default());
    let kind = name.map_or(payload_kind, event_kind);
    let mut meaning = protocol.map_or(
        EventMeaning {
            kind,
            ..EventMeaning::default()
        },
        |protocol| EventMeaning::generated(protocol, kind),
    );
    if let (Some(name), Some(payload_type)) = (name, payload_type) {
        let named_kind = kind;
        // Known control names and payload types must agree. Unknown extensions
        // remain opaque; a trailing space never acquires a control meaning.
        if named_kind != EventKind::Data && payload_kind != EventKind::Data && name != payload_type
        {
            meaning.failure = Some(FailureEvidence::ConflictingType);
        }
    }
    if let Some(value) = value {
        let error_reason = |reason: &str| reason == "error" || reason.ends_with("_error");
        let choices_error = value
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                choices.iter().any(|choice| {
                    choice
                        .get("finish_reason")
                        .and_then(Value::as_str)
                        .is_some_and(error_reason)
                })
            });
        let stop_error = value
            .get("delta")
            .and_then(|delta| delta.get("stop_reason"))
            .and_then(Value::as_str)
            .is_some_and(error_reason);
        let nested_error = value
            .get("response")
            .and_then(|response| response.get("error"))
            .is_some_and(|error| !error.is_null());
        if value.get("error").is_some_and(|error| !error.is_null())
            || nested_error
            || choices_error
            || stop_error
            || matches!(payload_kind, EventKind::Error | EventKind::ResponseFailed)
        {
            meaning.failure.get_or_insert(FailureEvidence::Inband);
            // An extension event can still carry a protocol failure terminal.
            if meaning.terminal.is_none() {
                meaning.terminal = match (protocol, payload_kind) {
                    (Some(SseProtocol::OpenaiResponses), EventKind::ResponseFailed) => {
                        Some(TerminalKind::Failed)
                    }
                    (Some(SseProtocol::OpenaiResponses), EventKind::Error)
                    | (Some(SseProtocol::AnthropicMessages), EventKind::Error) => {
                        Some(TerminalKind::Error)
                    }
                    _ => None,
                };
            }
        }
    }
    meaning
}

#[cfg(test)]
mod classification_tests {
    use super::*;
    use crate::sse::Payload;
    #[test]
    fn terminals_and_names_are_exact() {
        for protocol in [
            SseProtocol::OpenaiChat,
            SseProtocol::AnthropicMessages,
            SseProtocol::OpenaiResponses,
        ] {
            assert!(
                classify(Some(protocol), None, &Payload::Text(" [DONE] ".into()))
                    .terminal
                    .is_none()
            );
        }
        assert_eq!(
            classify(
                Some(SseProtocol::OpenaiChat),
                None,
                &Payload::Text("[DONE]".into())
            )
            .terminal,
            Some(TerminalKind::Success)
        );
        let payload = Payload::Json(json!({"type":"message_stop"}));
        assert!(classify(
            Some(SseProtocol::AnthropicMessages),
            Some("message_stop "),
            &payload
        )
        .terminal
        .is_none());
        assert!(classify(
            Some(SseProtocol::AnthropicMessages),
            Some("message_stop"),
            &payload
        )
        .terminal
        .is_some());
    }
    #[test]
    fn failure_is_independent_of_terminal_and_conflicts_fail() {
        let error = Payload::Json(json!({"error":{"message":"failed"}}));
        let meaning = classify(Some(SseProtocol::OpenaiChat), None, &error);
        assert!(meaning.failure.is_some());
        assert!(meaning.terminal.is_none());
        let conflict = classify(
            Some(SseProtocol::AnthropicMessages),
            Some("message_stop"),
            &Payload::Json(json!({"type":"error"})),
        );
        assert_eq!(conflict.failure, Some(FailureEvidence::ConflictingType));
        assert_eq!(conflict.terminal, Some(TerminalKind::Success));
        let named_error = classify(
            Some(SseProtocol::AnthropicMessages),
            Some("error"),
            &Payload::Json(json!({})),
        );
        assert_eq!(named_error.terminal, Some(TerminalKind::Error));
        assert!(named_error.failure.is_some());
    }
}
