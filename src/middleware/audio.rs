//! Bounded OpenAI multipart transcription input and sanitized JSON output.

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::{
    collections::HashSet,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_BODY_BYTES: usize = MAX_FILE_BYTES + MAX_TEXT_BYTES + 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
// Charge bytes received, not connections or declared Content-Length.
const UPLOAD_BYTE_BUDGET: usize = 16 * MAX_BODY_BYTES;
const READ_DEADLINE: Duration = Duration::from_secs(60);
static UPLOAD_BYTES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(UPLOAD_BYTE_BUDGET)));

#[derive(Debug)]
pub struct AudioError {
    pub status: StatusCode,
    pub code: &'static str,
    pub param: String,
}
impl AudioError {
    fn invalid(param: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_parameter",
            param: param.into(),
        }
    }
    fn capacity() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "upload_capacity",
            param: "body".into(),
        }
    }
    fn large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "request_too_large",
            param: "file".into(),
        }
    }
    pub fn into_response(self) -> Response {
        (self.status, Json(json!({"error":{"type":"invalid_request_error", "code":self.code,"param":self.param,"message":format!("invalid transcription parameter: {}",self.param)}}))).into_response()
    }
}

/// Account for each received chunk until inference admission takes over, or
/// the upload is refused, times out, or drops. Opening a connection reserves no bytes.
pub async fn read_upload(
    headers: &HeaderMap,
    body: Body,
) -> Result<(Bytes, OwnedSemaphorePermit), AudioError> {
    if headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|v| v > MAX_BODY_BYTES as u64)
    {
        return Err(AudioError::large());
    }
    tokio::time::timeout(READ_DEADLINE, collect_upload(body, UPLOAD_BYTES.clone()))
        .await
        .map_err(|_| AudioError {
            status: StatusCode::REQUEST_TIMEOUT,
            code: "upload_timeout",
            param: "body".into(),
        })?
}

async fn collect_upload(
    body: Body,
    budget: Arc<Semaphore>,
) -> Result<(Bytes, OwnedSemaphorePermit), AudioError> {
    let mut permit = budget
        .clone()
        .try_acquire_many_owned(0)
        .map_err(|_| AudioError::capacity())?;
    let mut chunks = body.into_data_stream();
    let mut bytes = bytes::BytesMut::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| AudioError::invalid("body"))?;
        if chunk.len() > MAX_BODY_BYTES - bytes.len() {
            return Err(AudioError::large());
        }
        let count = u32::try_from(chunk.len()).map_err(|_| AudioError::large())?;
        let chunk_permit = budget
            .clone()
            .try_acquire_many_owned(count)
            .map_err(|_| AudioError::capacity())?;
        permit.merge(chunk_permit);
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes.freeze(), permit))
}

/// Shared slices of the buffered upload; no caller filename or MIME is forwarded.
#[derive(Debug, Clone)]
pub struct AudioForm {
    pub file: Bytes,
    pub extension: String,
}

pub async fn parse(body: Bytes, content_type: &str) -> Result<(Value, Arc<AudioForm>), AudioError> {
    let boundary =
        multer::parse_boundary(content_type).map_err(|_| AudioError::invalid("content-type"))?;
    if boundary.is_empty()
        || boundary.len() > 70
        || !boundary.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
    {
        return Err(AudioError::invalid("boundary"));
    }
    let constraints = multer::Constraints::new().size_limit(
        multer::SizeLimit::new()
            .whole_stream(MAX_BODY_BYTES as u64)
            .per_field(MAX_TEXT_BYTES as u64)
            .for_field("file", MAX_FILE_BYTES as u64),
    );
    let mut multipart = multer::Multipart::with_constraints(
        stream::once(async move { Ok::<_, std::convert::Infallible>(body) }),
        boundary,
        constraints,
    );
    let mut params = Map::new();
    let mut seen = HashSet::new();
    let mut timestamps = Vec::new();
    let mut timestamp_name = None;
    let mut file = None;
    let mut count = 0;
    let mut text_bytes = 0;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AudioError::invalid("multipart"))?
    {
        count += 1;
        let name = field
            .name()
            .ok_or_else(|| AudioError::invalid("field"))?
            .to_string();
        // Multer resolves duplicate part headers with last-wins semantics.
        // Only its parsed values are used; caller headers never reach the
        // upstream because reqwest re-encodes every validated field and file.
        if count > 16
            || name.len() > 64
            || field
                .headers()
                .iter()
                .map(|(k, v)| k.as_str().len() + v.len())
                .sum::<usize>()
                > 8192
        {
            return Err(AudioError::invalid("multipart"));
        }
        let timestamp = matches!(
            name.as_str(),
            "timestamp_granularities" | "timestamp_granularities[]"
        );
        if !timestamp && !seen.insert(name.clone()) {
            return Err(AudioError::invalid(&name));
        }
        if name == "file" {
            let filename = field
                .file_name()
                .filter(|v| v.len() <= 255)
                .ok_or_else(|| AudioError::invalid("file"))?;
            let ext = filename
                .rsplit_once('.')
                .map(|(_, e)| e.to_ascii_lowercase())
                .filter(|e| {
                    [
                        "flac", "mp3", "mp4", "mpeg", "mpga", "m4a", "ogg", "wav", "webm",
                    ]
                    .contains(&e.as_str())
                })
                .ok_or_else(|| AudioError::invalid("file"))?;
            let data = field.bytes().await.map_err(|_| AudioError::large())?;
            if data.is_empty() {
                return Err(AudioError::invalid("file"));
            }
            file = Some(Arc::new(AudioForm {
                file: data,
                extension: ext,
            }));
            continue;
        }
        if ![
            "model",
            "language",
            "prompt",
            "temperature",
            "response_format",
            "user",
        ]
        .contains(&name.as_str())
            && !timestamp
        {
            return Err(AudioError {
                status: StatusCode::BAD_REQUEST,
                code: "unknown_parameter",
                param: name,
            });
        }
        if field.file_name().is_some() {
            return Err(AudioError::invalid(&name));
        }
        let data = field
            .bytes()
            .await
            .map_err(|_| AudioError::invalid(&name))?;
        text_bytes += data.len();
        if text_bytes > MAX_TEXT_BYTES {
            return Err(AudioError::invalid("fields"));
        }
        let value = std::str::from_utf8(&data).map_err(|_| AudioError::invalid(&name))?;
        if timestamp {
            if timestamp_name.as_ref().is_some_and(|n| n != &name)
                || timestamps.iter().any(|v| v == value)
                || !["word", "segment"].contains(&value)
                || timestamps.len() >= 2
            {
                return Err(AudioError::invalid(&name));
            }
            timestamp_name = Some(name);
            timestamps.push(value.to_string());
        } else if name == "temperature" {
            let temperature = value
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| AudioError::invalid(&name))?;
            params.insert(name, json!(temperature));
        } else {
            params.insert(name, Value::String(value.into()));
        }
    }
    if !params
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(AudioError::invalid("model"));
    }
    if params.get("language").is_some_and(|v| {
        !v.as_str().is_some_and(|s| {
            (2..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase())
        })
    }) {
        return Err(AudioError::invalid("language"));
    }
    if params
        .get("prompt")
        .and_then(Value::as_str)
        .is_some_and(|s| s.chars().count() > 4096)
    {
        return Err(AudioError::invalid("prompt"));
    }
    let format = params.entry("response_format").or_insert(json!("json"));
    if !matches!(format.as_str(), Some("json" | "verbose_json")) {
        return Err(AudioError::invalid("response_format"));
    }
    if !timestamps.is_empty() {
        if format != "verbose_json" {
            return Err(AudioError::invalid("timestamp_granularities"));
        }
        params.insert("timestamp_granularities[]".into(), json!(timestamps));
    }
    Ok((
        Value::Object(params),
        file.ok_or_else(|| AudioError::invalid("file"))?,
    ))
}

pub fn upstream_fields(params: &Value) -> Value {
    let mut fields = params.as_object().cloned().unwrap_or_default();
    fields.retain(|k, _| {
        [
            "model",
            "language",
            "prompt",
            "temperature",
            "response_format",
            "timestamp_granularities[]",
        ]
        .contains(&k.as_str())
    });
    Value::Object(fields)
}

impl AudioForm {
    /// Encode once after model selection. These exact immutable bytes are both
    /// hashed and transmitted, including on channel binding retries.
    pub async fn encode(
        &self,
        params: &Value,
    ) -> Result<(Bytes, String), crate::aci::upstream::UpstreamError> {
        use crate::aci::upstream::UpstreamError;
        let mut form = reqwest::multipart::Form::new();
        for (key, value) in upstream_fields(params).as_object().into_iter().flatten() {
            if let Some(values) = value.as_array() {
                for value in values {
                    form = form.text(key.clone(), value.as_str().unwrap_or_default().to_string());
                }
            } else {
                form = form.text(
                    key.clone(),
                    value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string()),
                );
            }
        }
        let file =
            reqwest::multipart::Part::stream_with_length(self.file.clone(), self.file.len() as u64)
                .file_name(format!("audio.{}", self.extension))
                .mime_str("application/octet-stream")
                .map_err(|e| UpstreamError::InvalidRequest(e.to_string()))?;
        form = form.part("file", file);
        let content_type = format!("multipart/form-data; boundary={}", form.boundary());
        let chunks = form.into_stream();
        futures_util::pin_mut!(chunks);
        let mut bytes = bytes::BytesMut::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|e| UpstreamError::Transport(e.to_string()))?;
            if chunk.len() > MAX_BODY_BYTES - bytes.len() {
                return Err(UpstreamError::InvalidRequest(
                    "encoded multipart body exceeds limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes.freeze(), content_type))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AudioUsage {
    pub seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_duration: Option<f64>,
}

/// Preferred duration is authoritative, even when invalid. Never estimate from
/// segments, file size, or latency, and never fall back from an invalid value.
pub fn usage(body: &Value) -> Result<AudioUsage, &'static str> {
    if !body.get("text").is_some_and(Value::is_string) {
        return Err("missing transcription text");
    }
    let usage = body.get("usage");
    let duration = if usage.is_some_and(|v| {
        v.get("seconds").is_some() || v.get("type").and_then(Value::as_str) == Some("duration")
    }) {
        let usage = usage.ok_or("missing duration usage")?;
        if usage.get("type").and_then(Value::as_str) != Some("duration") {
            return Err("invalid duration usage type");
        }
        usage.get("seconds")
    } else {
        body.get("duration")
    };
    let raw = duration
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= u32::MAX as f64)
        .ok_or("missing or invalid transcription duration")?;
    Ok(AudioUsage {
        seconds: raw.ceil() as u64,
        raw_duration: Some(raw),
    })
}

/// Expose the same validated integer duration that is billed. Keep the raw
/// provider duration in AudioUsage and the verbose response's duration field.
pub fn normalize_usage(body: &mut Value, audio: &AudioUsage) {
    if !body.get("usage").is_some_and(Value::is_object) {
        body["usage"] = json!({"type":"duration"});
    }
    body["usage"]["seconds"] = json!(audio.seconds);
}

pub fn reduce_format(body: &mut Value, params: &Value) {
    if params.get("response_format").and_then(Value::as_str) != Some("verbose_json") {
        if let Some(object) = body.as_object_mut() {
            object.retain(|k, _| matches!(k.as_str(), "text" | "usage"));
        }
    }
}

fn nonnegative(v: &Value) -> bool {
    v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
}
pub fn canonicalize(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.retain(|k, v| match k.as_str() {
        "text" | "language" | "task" => v.is_string(),
        "duration" => nonnegative(v),
        "segments" | "words" => v.is_array(),
        "usage" => v.is_object(),
        _ => false,
    });
    for array in ["segments", "words"] {
        if let Some(items) = object.get_mut(array).and_then(Value::as_array_mut) {
            items.retain(Value::is_object);
            for item in items {
                if let Some(fields) = item.as_object_mut() {
                    fields.retain(|k, v| match k.as_str() {
                        "text" if array == "segments" => v.is_string(),
                        "word" if array == "words" => v.is_string(),
                        "id" | "seek" if array == "segments" => v.is_u64(),
                        "start" | "end" => nonnegative(v),
                        "temperature" | "avg_logprob" | "compression_ratio" | "no_speech_prob"
                            if array == "segments" =>
                        {
                            v.is_number()
                        }
                        "tokens" if array == "segments" => {
                            v.as_array().is_some_and(|a| a.iter().all(Value::is_u64))
                        }
                        _ => false,
                    });
                }
            }
        }
    }
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        usage.retain(|k, v| match k.as_str() {
            "type" => matches!(v.as_str(), Some("duration" | "tokens")),
            "seconds" | "cost" => nonnegative(v),
            "input_tokens" | "output_tokens" | "total_tokens" => v.is_u64(),
            "input_token_details" => v.is_object(),
            _ => false,
        });
        if let Some(details) = usage
            .get_mut("input_token_details")
            .and_then(Value::as_object_mut)
        {
            details.retain(|k, v| {
                matches!(k.as_str(), "audio_tokens" | "text_tokens" | "cached_tokens") && v.is_u64()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(fields: &[(&str, &str)], data: Bytes, filename: &str) -> (Bytes, String) {
        let mut form = reqwest::multipart::Form::new();
        for (k, v) in fields {
            form = form.text(k.to_string(), v.to_string());
        }
        form = form.part(
            "file",
            reqwest::multipart::Part::stream(data).file_name(filename.to_string()),
        );
        let content_type = format!("multipart/form-data; boundary={}", form.boundary());
        let stream = form.into_stream();
        futures_util::pin_mut!(stream);
        let mut bytes = bytes::BytesMut::new();
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk.unwrap());
        }
        (bytes.freeze(), content_type)
    }

    #[tokio::test]
    async fn multipart_roundtrip_validates_fields_and_generates_safe_upstream_filename() {
        let (raw, ct) = request(
            &[
                ("model", "alias"),
                ("user", "alice"),
                ("prompt", "hello"),
                ("response_format", "verbose_json"),
                ("timestamp_granularities[]", "word"),
                ("timestamp_granularities[]", "segment"),
            ],
            Bytes::from_static(b"audio"),
            "private.wav",
        )
        .await;
        let (params, form) = parse(raw, &ct).await.unwrap();
        assert_eq!(
            params["timestamp_granularities[]"],
            json!(["word", "segment"])
        );
        let mut rewritten = params.clone();
        rewritten["model"] = json!("upstream-model");
        let (encoded, ct) = form.encode(&rewritten).await.unwrap();
        let text = std::str::from_utf8(&encoded).unwrap();
        assert!(text.contains("audio.wav"));
        assert!(!text.contains("private.wav"));
        assert!(!text.contains("alice"));
        let (forwarded, _) = parse(encoded, &ct).await.unwrap();
        assert_eq!(forwarded["model"], "upstream-model");
        assert_eq!(forwarded["response_format"], "verbose_json");
    }

    #[tokio::test]
    async fn rejects_ambiguous_unsupported_and_invalid_fields() {
        for fields in [
            vec![("model", "a"), ("model", "b")],
            vec![("model", "a"), ("provider", "{}")],
            vec![("model", "a"), ("stream", "false")],
            vec![("model", "a"), ("temperature", "NaN")],
            vec![("model", "a"), ("temperature", "1.1")],
            vec![("model", "a"), ("language", "EN")],
            vec![("model", "a"), ("response_format", "srt")],
            vec![("model", "a"), ("timestamp_granularities[]", "word")],
            vec![
                ("model", "a"),
                ("response_format", "verbose_json"),
                ("timestamp_granularities[]", "word"),
                ("timestamp_granularities", "segment"),
            ],
            vec![
                ("model", "a"),
                ("response_format", "verbose_json"),
                ("timestamp_granularities[]", "word"),
                ("timestamp_granularities[]", "word"),
            ],
        ] {
            let (raw, ct) = request(&fields, Bytes::from_static(b"audio"), "audio.wav").await;
            assert!(parse(raw, &ct).await.is_err(), "{fields:?}");
        }
        for filename in ["audio.exe", "audio", "audio."] {
            let (raw, ct) = request(&[("model", "a")], Bytes::from_static(b"a"), filename).await;
            assert!(parse(raw, &ct).await.is_err());
        }
        let (raw, ct) = request(&[("model", "a")], Bytes::new(), "audio.wav").await;
        assert!(parse(raw, &ct).await.is_err());
        let (raw, ct) = request(
            &[("model", "a"), ("prompt", &"a".repeat(4097))],
            Bytes::from_static(b"a"),
            "audio.wav",
        )
        .await;
        assert!(parse(raw, &ct).await.is_err());
        let fields = vec![("timestamp_granularities[]", "word"); 17];
        let (raw, ct) = request(&fields, Bytes::from_static(b"a"), "a.wav").await;
        assert!(parse(raw, &ct).await.is_err());
    }

    #[tokio::test]
    async fn file_limit_and_utf8_and_text_aggregate_are_enforced() {
        let (raw, ct) = request(
            &[("model", "a")],
            Bytes::from(vec![0; MAX_FILE_BYTES]),
            "a.wav",
        )
        .await;
        assert!(parse(raw, &ct).await.is_ok());
        let (raw, ct) = request(
            &[("model", "a")],
            Bytes::from(vec![0; MAX_FILE_BYTES + 1]),
            "a.wav",
        )
        .await;
        assert_eq!(
            parse(raw, &ct).await.unwrap_err().status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let (raw, ct) = request(
            &[("model", "a"), ("user", &"a".repeat(MAX_TEXT_BYTES))],
            Bytes::from_static(b"a"),
            "a.wav",
        )
        .await;
        assert!(parse(raw, &ct).await.is_err());
        let raw = Bytes::from_static(
            b"--x\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n\xff\r\n--x--\r\n",
        );
        assert!(parse(raw, "multipart/form-data; boundary=x").await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_upload_has_deadline_and_releases_buffered_bytes() {
        let available = UPLOAD_BYTES.available_permits();
        let body = Body::from_stream(
            stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"x")) })
                .chain(stream::pending()),
        );
        let result = read_upload(&HeaderMap::new(), body).await;
        assert_eq!(result.unwrap_err().code, "upload_timeout");
        assert_eq!(UPLOAD_BYTES.available_permits(), available);
    }

    #[tokio::test]
    async fn upload_budget_counts_chunks_and_is_held_until_handoff() {
        let budget = Arc::new(Semaphore::new(7));
        let body = Body::from_stream(stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"ab")),
            Ok(Bytes::from_static(b"cde")),
        ]));
        let (bytes, permit) = collect_upload(body, budget.clone()).await.unwrap();
        assert_eq!(bytes, "abcde");
        assert_eq!(permit.num_permits(), 5);
        assert_eq!(budget.available_permits(), 2);

        let rejected = collect_upload(Body::from("xyz"), budget.clone())
            .await
            .unwrap_err();
        assert_eq!(rejected.code, "upload_capacity");
        assert_eq!(budget.available_permits(), 2);
        drop(permit);
        assert_eq!(budget.available_permits(), 7);
    }

    #[tokio::test]
    async fn upload_errors_and_disconnects_release_partial_byte_reservations() {
        let budget = Arc::new(Semaphore::new(7));
        let body = Body::from_stream(stream::iter([
            Ok(Bytes::from_static(b"ab")),
            Err(std::io::Error::other("upload interrupted")),
        ]));
        assert_eq!(
            collect_upload(body, budget.clone()).await.unwrap_err().code,
            "invalid_parameter"
        );
        assert_eq!(budget.available_permits(), 7);

        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut tx = Some(tx);
        let body = Body::from_stream(
            stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"ab")) }).chain(
                stream::poll_fn(move |_| {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                    std::task::Poll::Pending
                }),
            ),
        );
        let task = tokio::spawn(collect_upload(body, budget.clone()));
        rx.await.unwrap();
        assert_eq!(budget.available_permits(), 5);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(budget.available_permits(), 7);
    }

    #[test]
    fn duration_is_authoritative_positive_bounded_and_rounded_up() {
        let body =
            json!({"text":"hello","duration":10.1,"usage":{"type":"duration","seconds":1.01}});
        assert_eq!(
            usage(&body).unwrap(),
            AudioUsage {
                seconds: 2,
                raw_duration: Some(1.01)
            }
        );
        for seconds in [
            Value::Null,
            json!("1"),
            json!(-1),
            json!(0),
            json!(4294967296u64),
        ] {
            assert!(usage(
                &json!({"text":"secret","duration":1,"usage":{"type":"duration","seconds":seconds}})
            )
            .is_err());
        }
        assert!(usage(&json!({"text":"hello","usage":{"type":"duration"},"duration":1})).is_err());
        assert!(
            usage(&json!({"text":"hello","usage":{"type":"tokens","seconds":1},"duration":1}))
                .is_err()
        );
        assert!(usage(&json!({"text":"hello"})).is_err());
        assert!(usage(&json!({"duration":1})).is_err());
        assert_eq!(
            usage(&json!({"text":"hello","duration":1.2}))
                .unwrap()
                .seconds,
            2
        );
    }

    #[test]
    fn nested_allowlists_withhold_private_structures() {
        let mut body = json!({"text":"hello","duration":1,"secret":{},"segments":[{"id":0,"text":"hello","start":0,"end":1,"url":"private","tokens":[1,2]},{"text":{"url":"private"},"start":-1},"private"],"words":[{"word":"hello","start":0,"end":1,"secret":1}],"usage":{"type":"tokens","input_tokens":1,"input_token_details":{"audio_tokens":1,"url":"private"},"private":{}}});
        canonicalize(&mut body);
        assert!(!body.to_string().contains("private"));
        assert_eq!(body["segments"].as_array().unwrap().len(), 2);
        assert_eq!(
            body["usage"]["input_token_details"],
            json!({"audio_tokens":1})
        );
        reduce_format(&mut body, &json!({"response_format":"json"}));
        assert!(body.get("duration").is_none());
    }
}
