//! Buffered image generation validation and the public response schema.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::{Semaphore, SemaphorePermit, TryAcquireError};

/// Bound large buffered image responses without queueing requests in memory.
static IN_FLIGHT: Semaphore = Semaphore::const_new(16);

pub fn try_acquire() -> Result<SemaphorePermit<'static>, TryAcquireError> {
    IN_FLIGHT.try_acquire()
}

#[derive(Debug)]
pub struct RequestError {
    pub code: &'static str,
    pub param: String,
    pub message: String,
}

impl RequestError {
    fn invalid(param: &str, message: &str) -> Self {
        Self {
            code: "invalid_parameter",
            param: param.into(),
            message: message.into(),
        }
    }

    pub fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {
                "type": "invalid_request_error", "code": self.code,
                "param": self.param, "message": self.message
            }})),
        )
            .into_response()
    }
}

/// Validate the complete client body before consulting or forwarding.
pub fn validate_request(body: &Value) -> Result<(), RequestError> {
    let object = body
        .as_object()
        .ok_or_else(|| RequestError::invalid("body", "request body must be a JSON object"))?;
    for key in object.keys() {
        if ![
            "model",
            "prompt",
            "n",
            "size",
            "quality",
            "background",
            "output_format",
            "output_compression",
            "moderation",
            "response_format",
            "user",
            "style",
            "provider",
            "stream",
            "partial_images",
        ]
        .contains(&key.as_str())
        {
            return Err(RequestError {
                code: "unknown_parameter",
                param: key.clone(),
                message: format!("unknown parameter: {key}"),
            });
        }
    }
    for key in ["model", "prompt"] {
        if !object.get(key).is_some_and(Value::is_string) {
            return Err(RequestError::invalid(
                key,
                "required parameter must be a string",
            ));
        }
    }
    if body["prompt"]
        .as_str()
        .is_some_and(|s| s.chars().count() > 32_000)
    {
        return Err(RequestError::invalid(
            "prompt",
            "prompt must be at most 32000 characters",
        ));
    }
    for key in [
        "size",
        "quality",
        "background",
        "output_format",
        "moderation",
        "user",
        "style",
    ] {
        if object.get(key).is_some_and(|v| !v.is_string()) {
            return Err(RequestError::invalid(key, "parameter must be a string"));
        }
    }
    for (key, min, max) in [("n", 1, 10), ("output_compression", 0, 100)] {
        if let Some(value) = object.get(key) {
            if !value.as_i64().is_some_and(|n| (min..=max).contains(&n)) {
                return Err(RequestError::invalid(
                    key,
                    &format!("parameter must be an integer from {min} to {max}"),
                ));
            }
        }
    }
    if object
        .get("response_format")
        .is_some_and(|v| v.as_str() != Some("b64_json"))
    {
        return Err(RequestError::invalid(
            "response_format",
            "only b64_json output is supported",
        ));
    }
    for key in ["stream", "partial_images"] {
        if object.contains_key(key) {
            return Err(RequestError::invalid(
                key,
                "streaming image generation is not supported",
            ));
        }
    }
    Ok(())
}

/// Build from an allowlist, even after a candidate replaces the model id.
pub fn upstream_body(body: &Value) -> Value {
    let mut output = Map::new();
    for key in [
        "model",
        "prompt",
        "n",
        "size",
        "quality",
        "background",
        "output_format",
        "output_compression",
        "moderation",
    ] {
        if let Some(value) = body.get(key) {
            output.insert(key.into(), value.clone());
        }
    }
    output.insert("response_format".into(), json!("b64_json"));
    Value::Object(output)
}

/// A malformed 2xx is an upstream failure; URL-only responses fail closed.
pub fn validate_response(body: &Value) -> Result<(), &'static str> {
    match body.get("data").and_then(Value::as_array) {
        Some(data)
            if !data.is_empty()
                && data
                    .iter()
                    .all(|item| item.get("b64_json").is_some_and(Value::is_string)) =>
        {
            Ok(())
        }
        _ => Err("upstream returned a malformed image success body"),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ImageUsage {
    pub count: usize,
    pub size: String,
    pub quality: String,
}

/// Resolve billing dimensions before reducing the upstream response schema.
pub fn usage(response: &Value, request: &Value) -> ImageUsage {
    let dimension = |key| {
        response
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| request.get(key).and_then(Value::as_str))
            .unwrap_or("auto")
            .to_string()
    };
    ImageUsage {
        count: response
            .get("data")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        size: dimension("size"),
        quality: dimension("quality"),
    }
}

/// Nothing outside this public schema (including upstream URLs) can escape.
pub fn canonicalize(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.retain(|key, _| {
        [
            "created",
            "data",
            "usage",
            "size",
            "quality",
            "output_format",
            "background",
        ]
        .contains(&key.as_str())
    });
    if let Some(data) = object.get_mut("data").and_then(Value::as_array_mut) {
        for item in data {
            if let Some(item) = item.as_object_mut() {
                item.retain(|key, _| ["b64_json", "revised_prompt"].contains(&key.as_str()));
            }
        }
    }
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        usage.retain(|key, _| {
            [
                "input_tokens",
                "output_tokens",
                "total_tokens",
                "input_tokens_details",
                "output_tokens_details",
                "cost",
            ]
            .contains(&key.as_str())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_fields_and_builds_only_upstream_allowlist() {
        let body = json!({"model":"image", "prompt":"hello", "n":10, "size":"auto", "quality":"auto", "output_compression":100, "style":"vivid", "user":"alice", "provider":{}});
        validate_request(&body).unwrap();
        let upstream = upstream_body(&body);
        assert_eq!(upstream["response_format"], "b64_json");
        for key in ["user", "style", "provider"] {
            assert!(upstream.get(key).is_none());
        }
    }

    #[test]
    fn rejects_invalid_parameters_without_clamping() {
        for patch in [
            json!({"response_format":"url"}),
            json!({"stream":false}),
            json!({"partial_images":1}),
            json!({"n":0}),
            json!({"n":11}),
            json!({"n":1.5}),
            json!({"n":"1"}),
            json!({"output_compression":101}),
            json!({"quality":2}),
            json!({"prompt":null}),
        ] {
            let mut body = json!({"model":"image", "prompt":"hello"});
            body.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(validate_request(&body).is_err(), "{body}");
        }
        assert!(validate_request(&json!({"model":"x"})).is_err());
        assert!(validate_request(&json!({"model":"x", "prompt":"界".repeat(32001)})).is_err());
        validate_request(
            &json!({"model":"x", "prompt":"界".repeat(32000), "n":1, "output_compression":0}),
        )
        .unwrap();
        let err = validate_request(&json!({"model":"x", "prompt":"hi", "seed":1})).unwrap_err();
        assert_eq!(
            (err.code, err.param.as_str()),
            ("unknown_parameter", "seed")
        );
    }

    #[test]
    fn success_requires_inline_images_and_drops_provider_fields() {
        for body in [
            json!({"data":[]}),
            json!({"data":[{"url":"https://private"}]}),
            json!({"data":[{"b64_json":"ok"}, {}]}),
        ] {
            assert!(validate_response(&body).is_err());
        }
        let mut body = json!({"created":1,"id":"private","model":"private","unknown":1,"data":[{"b64_json":"ok","url":"https://private","revised_prompt":"hi","other":true}],"usage":{"input_tokens":1,"private":2,"input_tokens_details":{"text_tokens":1}}});
        validate_response(&body).unwrap();
        canonicalize(&mut body);
        assert_eq!(
            body,
            json!({"created":1,"data":[{"b64_json":"ok","revised_prompt":"hi"}],"usage":{"input_tokens":1,"input_tokens_details":{"text_tokens":1}}})
        );
    }

    #[test]
    fn image_capacity_is_bounded_and_never_queues() {
        let permits: Vec<_> = (0..16).map(|_| try_acquire().unwrap()).collect();
        assert!(matches!(try_acquire(), Err(TryAcquireError::NoPermits)));
        drop(permits);
        assert!(try_acquire().is_ok());
    }

    #[test]
    fn billing_dimensions_prefer_response_then_request_then_auto() {
        let usage = usage(
            &json!({"data":[{},{}],"quality":"high"}),
            &json!({"size":"512x512","quality":"low"}),
        );
        assert_eq!(
            usage,
            ImageUsage {
                count: 2,
                size: "512x512".into(),
                quality: "high".into()
            }
        );
        assert_eq!(super::usage(&json!({}), &json!({})).size, "auto");
    }
}
