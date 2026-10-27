//! Buffered image generation validation and the public response schema.

use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};

use axum::{
    body::{Body, Bytes, HttpBody},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

/// Bound admitted image generation and buffered response bodies, without queueing.
static IN_FLIGHT: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(16)));

pub fn try_acquire() -> Result<OwnedSemaphorePermit, TryAcquireError> {
    IN_FLIGHT.clone().try_acquire_owned()
}

pub fn capacity_response() -> Response {
    let reset_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |now| now.as_secs().saturating_add(1) as i64);
    super::errors::rate_limit_response(
        super::errors::Surface::Openai,
        "image generation capacity reached",
        16,
        reset_at,
        None,
    )
}

/// The response body owns admission after the handler returns to Hyper.
/// Completion, body errors, and disconnect/drop all release the slot.
pub fn hold_permit(response: Response, permit: Option<OwnedSemaphorePermit>) -> Response {
    match permit {
        Some(permit) => response.map(|body| {
            Body::new(PermitBody {
                body,
                permit: Some(Arc::new(permit)),
            })
        }),
        None => response,
    }
}

struct PermitBody {
    body: Body,
    permit: Option<Arc<OwnedSemaphorePermit>>,
}

impl HttpBody for PermitBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.body).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                Poll::Ready(Some(Ok(frame.map_data(|bytes| {
                    match self.permit.as_ref() {
                        Some(permit) => Bytes::from_owner(PermitBytes {
                            bytes,
                            _permit: permit.clone(),
                        }),
                        None => bytes,
                    }
                }))))
            }
            frame @ Poll::Ready(None | Some(Err(_))) => {
                self.permit.take();
                frame
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

// Hyper may drop Body as soon as it takes the last frame, while retaining that
// frame in its write buffer. Keep admission with the bytes until the writer
// releases them too. Bytes::from_owner preserves the allocation without copying.
struct PermitBytes {
    bytes: Bytes,
    _permit: Arc<OwnedSemaphorePermit>,
}

impl AsRef<[u8]> for PermitBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
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

    #[tokio::test]
    async fn response_body_owns_capacity_through_consumption_and_disconnect() {
        use axum::body::to_bytes;

        let capacity = Arc::new(Semaphore::new(1));
        let response = hold_permit(
            Response::new(Body::from("image")),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        assert!(capacity.clone().try_acquire_owned().is_err());
        assert_eq!(response.body().size_hint().exact(), Some(5));
        let bytes = to_bytes(response.into_body(), 100).await.unwrap();
        assert_eq!(bytes, "image");
        // The buffered bytes may still belong to Hyper's pending write.
        assert_eq!(capacity.available_permits(), 0);
        drop(bytes);
        assert_eq!(capacity.available_permits(), 1);

        let response = hold_permit(
            Response::new(Body::from("image")),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        assert_eq!(capacity.available_permits(), 0);
        drop(response); // Client disconnected before polling the body.
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn queued_frame_owns_capacity_even_after_hyper_drops_the_body() {
        use http_body_util::BodyExt;

        let capacity = Arc::new(Semaphore::new(1));
        let response = hold_permit(
            Response::new(Body::from("image")),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        let mut body = response.into_body();
        let bytes = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert!(body.is_end_stream());
        drop(body);
        assert_eq!(capacity.available_permits(), 0);
        let pending_write = bytes.clone();
        drop(bytes);
        assert_eq!(capacity.available_permits(), 0);
        drop(pending_write);
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn response_body_errors_release_capacity() {
        use http_body_util::BodyExt;

        let capacity = Arc::new(Semaphore::new(1));
        let failing = futures_util::stream::once(async {
            Err::<Bytes, _>(std::io::Error::other("body failed"))
        });
        let response = hold_permit(
            Response::new(Body::from_stream(failing)),
            Some(capacity.clone().try_acquire_owned().unwrap()),
        );
        let mut body = response.into_body();
        assert_eq!(capacity.available_permits(), 0);
        assert!(body.frame().await.unwrap().is_err());
        assert_eq!(capacity.available_permits(), 1);
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
