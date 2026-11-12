//! Buffered speech synthesis: JSON input, exact audio output and character usage.

use std::collections::HashMap;

use axum::http::HeaderValue;
use serde_json::{json, Value};

use super::images::{max_chars, object_with, pick, require_strings, RequestError};

const UPSTREAM_FIELDS: &[&str] = &[
    "model",
    "input",
    "voice",
    "response_format",
    "speed",
    "instructions",
];

pub fn validate_request(body: &Value) -> Result<(), RequestError> {
    object_with(
        body,
        &[
            "model",
            "input",
            "voice",
            "response_format",
            "speed",
            "instructions",
            "stream_format",
            "provider",
        ],
    )?;
    require_strings(body, &["model", "input", "voice"])?;
    max_chars(
        body,
        &["input", "instructions"],
        4096,
        "parameter must be a string of at most 4096 characters",
    )?;
    if body.get("response_format").is_some_and(|v| {
        !v.as_str()
            .is_some_and(|s| ["mp3", "opus", "aac", "flac", "wav", "pcm"].contains(&s))
    }) {
        return Err(RequestError::invalid(
            "response_format",
            "unsupported audio format",
        ));
    }
    if body.get("speed").is_some_and(|v| {
        !v.as_f64()
            .is_some_and(|n| n.is_finite() && (0.25..=4.0).contains(&n))
    }) {
        return Err(RequestError::invalid(
            "speed",
            "speed must be a number from 0.25 to 4",
        ));
    }
    if body
        .get("stream_format")
        .is_some_and(|v| v.as_str() != Some("audio"))
    {
        return Err(RequestError::invalid(
            "stream_format",
            "only buffered audio is supported",
        ));
    }
    Ok(())
}

pub fn upstream_body(body: &Value) -> Value {
    Value::Object(pick(body, UPSTREAM_FIELDS))
}

pub fn usage(body: &Value) -> Option<Value> {
    let characters = body.get("input")?.as_str()?.chars().count();
    Some(json!({"type":"characters", "characters":characters}))
}

/// Return one canonical audio media type, without provider parameters.
pub fn audio_content_type(headers: &HashMap<String, String>) -> Option<String> {
    let value = headers.get("content-type")?;
    if value.contains(',') || HeaderValue::from_str(value).is_err() {
        return None;
    }
    let media_type = value.parse::<mime::Mime>().ok()?;
    (media_type.type_() == mime::AUDIO && !media_type.subtype().as_str().is_empty())
        .then(|| media_type.essence_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_speech_fields_and_counts_unicode_scalars() {
        let request = json!({"model":"tts", "input":"界🙂e\u{301}", "voice":"ryan", "instructions":"Speak clearly", "speed":0.25, "response_format":"wav", "stream_format":"audio", "provider":{}});
        validate_request(&request).unwrap();
        assert_eq!(
            usage(&request),
            Some(json!({"type":"characters", "characters":4}))
        );
        let upstream = upstream_body(&request);
        assert!(upstream.get("provider").is_none());
        assert!(upstream.get("stream_format").is_none());
        for patch in [
            json!({"input":null}),
            json!({"voice":1}),
            json!({"input":"🙂".repeat(4097)}),
            json!({"instructions":"界".repeat(4097)}),
            json!({"response_format":"json"}),
            json!({"stream_format":"sse"}),
            json!({"speed":0.24}),
            json!({"speed":4.01}),
            json!({"speed":"1"}),
            json!({"stream":false}),
        ] {
            let mut body = request.clone();
            body.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(validate_request(&body).is_err(), "{body}");
        }
        validate_request(&json!({"model":"tts","input":"🙂".repeat(4096),"voice":"ryan","instructions":"界".repeat(4096),"speed":4,"response_format":"wav"})).unwrap();
    }
}
