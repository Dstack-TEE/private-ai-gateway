//! Buffered image generation validation and the public response schema.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Map, Value};

/// Protocol maximum for the requested number of images.
pub const MAX_IMAGE_COUNT: i64 = 10;
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
            "safety_identifier",
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
        "safety_identifier",
        "style",
    ] {
        if object.get(key).is_some_and(|v| !v.is_string()) {
            return Err(RequestError::invalid(key, "parameter must be a string"));
        }
    }
    for (key, min, max) in [("n", 1, MAX_IMAGE_COUNT), ("output_compression", 0, 100)] {
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

/// Providers may return fewer images, but never more than the requested maximum.
/// A malformed 2xx is an upstream failure; URL-only responses fail closed.
pub fn validate_response(body: &Value, requested_count: usize) -> Result<(), &'static str> {
    match body.get("data").and_then(Value::as_array) {
        Some(data)
            if !data.is_empty()
                && data.len() <= requested_count
                && data
                    .iter()
                    .all(|item| item.get("b64_json").is_some_and(Value::is_string)) =>
        {
            Ok(())
        }
        _ => Err("upstream returned a malformed image success body"),
    }
}

/// The request has already passed image count validation.
pub fn requested_count(request: &Value) -> usize {
    request.get("n").and_then(Value::as_u64).unwrap_or(1) as usize
}

/// Validate and reduce a direct upstream success before signing its receipt.
pub fn response_body(response: &[u8], request: &[u8]) -> Option<Vec<u8>> {
    let request: Value = serde_json::from_slice(request).ok()?;
    let mut body: Value = serde_json::from_slice(response).ok()?;
    validate_response(&body, requested_count(&request)).ok()?;
    canonicalize(&mut body);
    Some(serde_json::to_vec(&body).unwrap_or_default())
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
        request
            .get(key)
            .and_then(Value::as_str)
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
    object.retain(|key, value| match key.as_str() {
        "created" => value.is_i64() || value.is_u64(),
        "data" => value.is_array(),
        "usage" => value.is_object(),
        "size" | "quality" | "output_format" | "background" => value.is_string(),
        _ => false,
    });
    object.entry("created").or_insert_with(|| {
        json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs())
    });
    if let Some(data) = object.get_mut("data").and_then(Value::as_array_mut) {
        for item in data {
            if let Some(item) = item.as_object_mut() {
                item.retain(|key, value| match key.as_str() {
                    "b64_json" => value.is_string(),
                    "revised_prompt" => value.is_string() || value.is_null(),
                    _ => false,
                });
            }
        }
    }
    // SDKs require the complete token shape even for providers that meter only
    // images. This happens after middleware captures raw usage for billing.
    let usage = object.entry("usage").or_insert_with(|| json!({}));
    if let Some(usage) = usage.as_object_mut() {
        usage.retain(|key, value| match key.as_str() {
            "input_tokens" | "output_tokens" | "total_tokens" => value.is_u64(),
            "input_tokens_details" | "output_tokens_details" => value.is_object(),
            "cost" => value.is_number(),
            _ => false,
        });
        for key in ["input_tokens", "output_tokens", "total_tokens"] {
            usage.entry(key).or_insert(json!(0));
        }
        usage
            .entry("input_tokens_details")
            .or_insert_with(|| json!({}));
        for key in ["input_tokens_details", "output_tokens_details"] {
            if let Some(details) = usage.get_mut(key).and_then(Value::as_object_mut) {
                details.retain(|key, value| {
                    matches!(
                        key.as_str(),
                        "text_tokens" | "image_tokens" | "cached_tokens"
                    ) && value.is_u64()
                });
                for key in ["text_tokens", "image_tokens"] {
                    details.entry(key).or_insert(json!(0));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_fields_and_builds_only_upstream_allowlist() {
        let body = json!({"model":"image", "prompt":"hello", "n":MAX_IMAGE_COUNT, "size":"auto", "quality":"auto", "output_compression":100, "style":"vivid", "user":"alice", "safety_identifier":"hashed-user", "provider":{}});
        validate_request(&body).unwrap();
        let upstream = upstream_body(&body);
        assert_eq!(upstream["response_format"], "b64_json");
        for key in ["user", "safety_identifier", "style", "provider"] {
            assert!(upstream.get(key).is_none());
        }
    }

    #[test]
    fn safety_identifier_must_be_a_string() {
        for value in [Value::Null, json!(1), json!({"id":"private"})] {
            let error = validate_request(
                &json!({"model":"image", "prompt":"hello", "safety_identifier":value}),
            )
            .unwrap_err();
            assert_eq!(error.code, "invalid_parameter");
            assert_eq!(error.param, "safety_identifier");
        }
    }

    #[test]
    fn accepts_protocol_image_counts_and_preserves_type_and_range_errors() {
        validate_request(&json!({"model":"image","prompt":"hello"})).unwrap();
        for n in 1..=MAX_IMAGE_COUNT {
            let body = json!({"model":"image","prompt":"hello","n":n});
            validate_request(&body).unwrap();
            assert_eq!(upstream_body(&body)["n"], n);
        }
        for n in [
            json!(-1),
            json!(0),
            json!(11),
            json!(1.0),
            json!(1.5),
            json!("1"),
            json!(true),
            Value::Null,
        ] {
            let error =
                validate_request(&json!({"model":"image","prompt":"hello","n":n})).unwrap_err();
            assert_eq!(error.code, "invalid_parameter", "{n}");
            assert_eq!(error.param, "n");
            assert_eq!(error.message, "parameter must be an integer from 1 to 10");
        }
    }

    #[test]
    fn rejects_invalid_parameters_without_clamping() {
        for patch in [
            json!({"response_format":"url"}),
            json!({"stream":false}),
            json!({"partial_images":1}),
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
    fn response_count_may_be_lower_but_never_exceed_the_requested_maximum() {
        for (requested, returned, valid) in
            [(2, 1, true), (2, 2, true), (2, 3, false), (1, 2, false)]
        {
            let body = json!({"data":vec![json!({"b64_json":"ok"}); returned]});
            assert_eq!(validate_response(&body, requested).is_ok(), valid);
        }
    }

    #[test]
    fn response_allowlist_strips_provider_data_and_urls() {
        for body in [
            json!({"data":[]}),
            json!({"data":[{"url":"https://private"}]}),
            json!({"data":[{"b64_json":"ok"}, {}]}),
            json!({"data":[{"b64_json":{"url":"https://private"}}]}),
        ] {
            assert!(validate_response(&body, 1).is_err());
        }
        for (mut body, expected) in [
            (
                json!({"created":1,"id":"private","model":"private","unknown":1,"data":[{"b64_json":"ok","url":"https://private","revised_prompt":"hi","other":true}],"usage":{"input_tokens":1,"private":2,"input_tokens_details":{"text_tokens":1}}}),
                json!({"data":[{"b64_json":"ok","revised_prompt":"hi"}],"usage":{"input_tokens":1,"output_tokens":0,"total_tokens":0,"input_tokens_details":{"text_tokens":1,"image_tokens":0}}}),
            ),
            (
                json!({"created":{"url":"https://private"},"data":[{"b64_json":"ok","revised_prompt":{"url":"https://private/image","provider":"private"}}],"size":[],"quality":1,"output_format":null,"background":{},"usage":{"input_tokens":-1,"output_tokens":1.5,"total_tokens":"2","input_tokens_details":{"url":"https://private/image","text_tokens":2,"image_tokens":3,"cached_tokens":1,"other":4},"output_tokens_details":{"text_tokens":-1,"image_tokens":1.5,"cached_tokens":"1"},"cost":{"url":"https://private"}}}),
                json!({"data":[{"b64_json":"ok"}],"usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0,"input_tokens_details":{"text_tokens":2,"image_tokens":3,"cached_tokens":1},"output_tokens_details":{"text_tokens":0,"image_tokens":0}}}),
            ),
            (
                json!({"created":1,"data":[{"b64_json":"ok","revised_prompt":null}],"size":"512x512","quality":"high","output_format":"png","background":"opaque","usage":{"input_tokens":0,"output_tokens":2,"total_tokens":2,"input_tokens_details":[],"output_tokens_details":null,"cost":0.1}}),
                json!({"data":[{"b64_json":"ok","revised_prompt":null}],"size":"512x512","quality":"high","output_format":"png","background":"opaque","usage":{"input_tokens":0,"output_tokens":2,"total_tokens":2,"input_tokens_details":{"text_tokens":0,"image_tokens":0},"cost":0.1}}),
            ),
        ] {
            validate_response(&body, 1).unwrap();
            canonicalize(&mut body);
            assert!(body["created"].is_u64());
            body.as_object_mut().unwrap().remove("created");
            assert_eq!(body, expected);
        }
    }

    #[test]
    fn sdk_fields_complete_absent_invalid_and_partial_values() {
        let empty = json!({"input_tokens":0,"output_tokens":0,"total_tokens":0,"input_tokens_details":{"text_tokens":0,"image_tokens":0}});
        let full = json!({"input_tokens":5,"output_tokens":3,"total_tokens":8,"input_tokens_details":{"text_tokens":2,"image_tokens":3,"cached_tokens":1},"output_tokens_details":{"text_tokens":0,"image_tokens":3},"cost":0.1});
        for (usage, expected) in [
            (None, empty.clone()),
            (Some(json!([])), empty.clone()),
            (Some(json!("private")), empty.clone()),
            (Some(Value::Null), empty.clone()),
            (
                Some(json!({"cost":0.1})),
                json!({"input_tokens":0,"output_tokens":0,"total_tokens":0,"input_tokens_details":{"text_tokens":0,"image_tokens":0},"cost":0.1}),
            ),
            (
                Some(json!({"input_tokens":5,"input_tokens_details":{"text_tokens":2}})),
                json!({"input_tokens":5,"output_tokens":0,"total_tokens":0,"input_tokens_details":{"text_tokens":2,"image_tokens":0}}),
            ),
            (Some(full.clone()), full),
        ] {
            let mut body = json!({"created":1,"data":[{"b64_json":"ok"}]});
            if let Some(usage) = usage {
                body["usage"] = usage;
            }
            canonicalize(&mut body);
            assert_eq!(body["created"], 1);
            assert_eq!(body["usage"], expected);
        }
        for created in [
            None,
            Some(Value::Null),
            Some(json!(1.5)),
            Some(json!({"url":"private"})),
        ] {
            let before = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let mut body = json!({"data":[{"b64_json":"ok"}]});
            if let Some(created) = created {
                body["created"] = created;
            }
            canonicalize(&mut body);
            let after = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            assert!((before..=after).contains(&body["created"].as_u64().unwrap()));
            assert_eq!(body["usage"], empty);
        }
        for (details, expected) in [
            (None, None),
            (
                Some(json!({"image_tokens":7})),
                Some(json!({"text_tokens":0,"image_tokens":7})),
            ),
            (
                Some(json!({})),
                Some(json!({"text_tokens":0,"image_tokens":0})),
            ),
            (
                Some(
                    json!({"text_tokens":-1,"image_tokens":"7","cached_tokens":2,"url":"private"}),
                ),
                Some(json!({"text_tokens":0,"image_tokens":0,"cached_tokens":2})),
            ),
        ] {
            let mut body = json!({"data":[{"b64_json":"ok"}],"usage":{}});
            if let Some(details) = details {
                body["usage"]["output_tokens_details"] = details;
            }
            canonicalize(&mut body);
            assert_eq!(
                body["usage"].get("output_tokens_details"),
                expected.as_ref()
            );
        }
    }

    #[test]
    fn billing_dimensions_use_request_then_auto() {
        let usage = usage(
            &json!({"data":[{},{}],"quality":"high"}),
            &json!({"size":"512x512","quality":"low"}),
        );
        assert_eq!(
            usage,
            ImageUsage {
                count: 2,
                size: "512x512".into(),
                quality: "low".into()
            }
        );
        assert_eq!(super::usage(&json!({}), &json!({})).size, "auto");
    }
}
