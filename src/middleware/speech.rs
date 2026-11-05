//! Buffered speech synthesis: JSON input, exact audio output and character usage.

use std::collections::HashMap;

use axum::http::HeaderValue;
use serde_json::{json, Map, Value};

use super::images::{reject_unknown, RequestError};

const UPSTREAM_FIELDS: &[&str] = &[
    "model",
    "input",
    "voice",
    "response_format",
    "speed",
    "instructions",
];

pub fn validate_request(body: &Value) -> Result<(), RequestError> {
    let object = body
        .as_object()
        .ok_or_else(|| RequestError::invalid("body", "request body must be a JSON object"))?;
    reject_unknown(
        object,
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
    for key in ["model", "input", "voice"] {
        if !body.get(key).is_some_and(Value::is_string) {
            return Err(RequestError::invalid(
                key,
                "required parameter must be a string",
            ));
        }
    }
    for key in ["input", "instructions"] {
        if body
            .get(key)
            .is_some_and(|v| !v.as_str().is_some_and(|s| s.chars().count() <= 4096))
        {
            return Err(RequestError::invalid(
                key,
                "parameter must be a string of at most 4096 characters",
            ));
        }
    }
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
    let mut output = Map::new();
    for &key in UPSTREAM_FIELDS {
        if let Some(value) = body.get(key) {
            output.insert(key.into(), value.clone());
        }
    }
    Value::Object(output)
}

pub fn usage(body: &Value) -> Option<Value> {
    let characters = body.get("input")?.as_str()?.chars().count();
    Some(json!({"type":"characters", "characters":characters}))
}

/// Preserve the upstream media type only for a valid audio header.
pub fn audio_content_type(headers: &HashMap<String, String>) -> Option<&str> {
    let value = headers.get("content-type")?;
    let media_type = value.split(';').next()?.trim();
    (media_type.len() > 6
        && media_type
            .get(..6)
            .is_some_and(|s| s.eq_ignore_ascii_case("audio/"))
        && HeaderValue::from_str(value).is_ok())
    .then_some(value.as_str())
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
        for response_format in ["mp3", "opus", "aac", "flac", "wav", "pcm"] {
            validate_request(&json!({"model":"tts","input":"🙂".repeat(4096),"voice":"ryan","instructions":"界".repeat(4096),"speed":4,"response_format":response_format})).unwrap();
        }
    }
}
