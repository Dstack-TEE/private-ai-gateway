//! Bounded OpenAI multipart transcription input and sanitized JSON output.

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
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
const UPSTREAM_FIELDS: &[&str] = &[
    "model",
    "language",
    "prompt",
    "temperature",
    "response_format",
    "timestamp_granularities[]",
];
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
    message: String,
    error_type: &'static str,
}
impl AudioError {
    fn invalid(param: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_parameter",
            param: param.into(),
            message: format!("invalid transcription parameter: {param}"),
            error_type: "invalid_request_error",
        }
    }
    fn capacity() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limit_exceeded",
            param: "body".into(),
            message: "transcription upload byte capacity reached; retry shortly".into(),
            error_type: "rate_limit_error",
        }
    }
    fn large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "request_too_large",
            param: "file".into(),
            message: "transcription upload exceeds the file or request size limit".into(),
            error_type: "invalid_request_error",
        }
    }
    fn timeout() -> Self {
        Self {
            status: StatusCode::REQUEST_TIMEOUT,
            code: "upload_timeout",
            param: "body".into(),
            message: "transcription upload exceeded the 60-second read deadline".into(),
            error_type: "invalid_request_error",
        }
    }
    pub fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"error":{"type":self.error_type, "code":self.code,"param":self.param,"message":self.message}}))).into_response();
        if matches!(
            self.status,
            StatusCode::PAYLOAD_TOO_LARGE | StatusCode::REQUEST_TIMEOUT
        ) {
            response
                .headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
        }
        if self.status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
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
        .map_err(|_| AudioError::timeout())?
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
        if !UPSTREAM_FIELDS.contains(&name.as_str())
            && !matches!(name.as_str(), "user" | "stream")
            && !timestamp
        {
            return Err(AudioError {
                code: "unknown_parameter",
                ..AudioError::invalid(&name)
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
        if name == "stream" {
            if value != "false" {
                return Err(AudioError::invalid(&name));
            }
        } else if timestamp {
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
    fields.retain(|k, _| UPSTREAM_FIELDS.contains(&k.as_str()));
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
    pub raw_duration: f64,
}

pub fn sanitize_response(
    mut body: Value,
    params: &Value,
) -> Result<(Value, AudioUsage), &'static str> {
    let audio = usage(&body)?;
    canonicalize(&mut body);
    normalize_usage(&mut body, &audio);
    if params.get("response_format").and_then(Value::as_str) == Some("verbose_json") {
        body["duration"] = json!(audio.raw_duration);
        if !body.get("language").is_some_and(Value::is_string) {
            body["language"] = params
                .get("language")
                .filter(|v| v.is_string())
                .cloned()
                .unwrap_or(json!(""));
        }
    }
    reduce_format(&mut body, params);
    Ok((body, audio))
}

/// Preferred duration is authoritative, even when invalid. Never estimate from
/// segments, file size, or latency, and never fall back from an invalid value.
fn usage(body: &Value) -> Result<AudioUsage, &'static str> {
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
        raw_duration: raw,
    })
}

/// Expose the same validated integer duration that is billed. Keep the raw
/// provider duration in AudioUsage and the verbose response's duration field.
fn normalize_usage(body: &mut Value, audio: &AudioUsage) {
    body["usage"] = json!({"type":"duration","seconds":audio.seconds});
}

/// Retain sanitized provider counters for consult/post and internal token pricing.
/// Client usage always describes the gateway's duration billing unit.
pub(crate) fn billing_usage(usage: Option<Value>, audio: &AudioUsage) -> Value {
    let mut usage = usage
        .filter(Value::is_object)
        .unwrap_or(json!({"type":"duration"}));
    canonicalize_usage(&mut usage);
    usage["seconds"] = json!(audio.seconds);
    usage
}

fn reduce_format(body: &mut Value, params: &Value) {
    if params.get("response_format").and_then(Value::as_str) != Some("verbose_json") {
        if let Some(object) = body.as_object_mut() {
            object.retain(|k, _| matches!(k.as_str(), "text" | "usage" | "languages"));
        }
    }
}

fn nonnegative(v: &Value) -> bool {
    v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
}

#[derive(Deserialize, Serialize)]
struct Segment {
    id: u64,
    seek: u64,
    start: f64,
    end: f64,
    text: String,
    tokens: Vec<u64>,
    temperature: f64,
    avg_logprob: f64,
    compression_ratio: f64,
    no_speech_prob: f64,
}

#[derive(Deserialize, Serialize)]
struct Word {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Deserialize, Serialize)]
struct Language {
    code: String,
}

fn canonicalize(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.retain(|k, v| match k.as_str() {
        "text" | "language" | "task" => v.is_string(),
        "duration" => nonnegative(v),
        "segments" | "words" | "languages" => v.is_array(),
        _ => false,
    });
    for array in ["segments", "words", "languages"] {
        if let Some(items) = object.get_mut(array).and_then(Value::as_array_mut) {
            *items = std::mem::take(items)
                .into_iter()
                .filter_map(|item| match array {
                    "segments" => serde_json::from_value::<Segment>(item)
                        .ok()
                        .filter(|s| s.start >= 0.0 && s.end >= 0.0)
                        .map(|s| json!(s)),
                    "words" => serde_json::from_value::<Word>(item)
                        .ok()
                        .filter(|w| w.start >= 0.0 && w.end >= 0.0)
                        .map(|w| json!(w)),
                    _ => serde_json::from_value::<Language>(item)
                        .ok()
                        .map(|l| json!(l)),
                })
                .collect();
            if array != "languages" && items.is_empty() {
                object.remove(array);
            }
        }
    }
}

fn canonicalize_usage(usage: &mut Value) {
    if let Some(usage) = usage.as_object_mut() {
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
                ("stream", "false"),
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
        assert!(params.get("stream").is_none());
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
            vec![("model", "a"), ("include[]", "true")],
            vec![("model", "a"), ("chunking_strategy", "true")],
            vec![("model", "a"), ("known_speaker_names[]", "true")],
            vec![("model", "a"), ("stream", "true")],
            vec![("model", "a"), ("stream", "0")],
            vec![("model", "a"), ("temperature", "NaN")],
            vec![("model", "a"), ("temperature", "1.1")],
            vec![("model", "a"), ("language", "EN")],
            vec![("model", "a"), ("response_format", "srt")],
            vec![("model", "a"), ("response_format", "text")],
            vec![("model", "a"), ("response_format", "vtt")],
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
        let error = result.unwrap_err();
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(response.headers()[header::CONNECTION], "close");
        assert_eq!(UPLOAD_BYTES.available_permits(), available);
    }

    #[tokio::test]
    async fn declared_and_received_oversized_uploads_close_the_connection() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, (MAX_BODY_BYTES + 1).into());
        let unread = Body::from_stream(stream::poll_fn(
            |_| -> std::task::Poll<Option<Result<Bytes, std::io::Error>>> {
                panic!("declared oversize upload must not be read")
            },
        ));
        let declared = read_upload(&headers, unread).await.unwrap_err();
        let received = read_upload(&HeaderMap::new(), Body::from(vec![0; MAX_BODY_BYTES + 1]))
            .await
            .unwrap_err();
        for error in [declared, received] {
            let response = error.into_response();
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(response.headers()[header::CONNECTION], "close");
        }
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
        let response = rejected.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
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
                raw_duration: 1.01
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
    fn sanitizer_normalizes_usage_metadata_and_requested_format() {
        for (provider_usage, language, requested, expected_language) in [
            (Value::Null, Value::Null, Value::Null, ""),
            (json!({}), json!("English"), json!("en"), "English"),
            (
                json!({"type":"tokens","input_tokens":2}),
                Value::Null,
                json!("en"),
                "en",
            ),
            (
                json!({"type":"duration","seconds":2.965,"cost":99}),
                json!({"private":1}),
                Value::Null,
                "",
            ),
        ] {
            let upstream =
                json!({"text":"hello","duration":2.965,"language":language,"usage":provider_usage});
            let params = json!({"response_format":"verbose_json","language":requested});
            let (verbose, audio) = sanitize_response(upstream.clone(), &params).unwrap();
            assert_eq!(
                audio,
                AudioUsage {
                    seconds: 3,
                    raw_duration: 2.965
                }
            );
            assert_eq!(verbose["duration"], 2.965);
            assert_eq!(verbose["language"], expected_language);
            assert_eq!(verbose["usage"], json!({"type":"duration","seconds":3}));
            let (reduced, _) = sanitize_response(upstream, &Value::Null).unwrap();
            assert_eq!(reduced, json!({"text":"hello","usage":verbose["usage"]}));
        }
    }

    #[test]
    fn timestamp_items_require_every_sdk_field_and_keep_no_private_fields() {
        let segment = json!({"id":0,"seek":0,"start":0.0,"end":1.0,"text":"hello","tokens":[1],"temperature":0.0,"avg_logprob":-0.1,"compression_ratio":1.0,"no_speech_prob":0.0});
        let mut invalid = segment.clone();
        invalid["tokens"] = json!([1, "private"]);
        let mut private = segment.clone();
        private["url"] = json!("private");
        let mut negative = segment.clone();
        negative["start"] = json!(-1);
        let upstream = json!({"text":"hello","duration":1,"private":{},"segments":[private,invalid,negative,{"text":"hello"}],"words":[{"word":"hello","start":0,"end":1,"url":"private"},{"word":"incomplete"},{"word":"negative","start":0,"end":-1}],"languages":[{"code":"en","private":1},{"code":7},"private"]});
        let (body, _) =
            sanitize_response(upstream.clone(), &json!({"response_format":"verbose_json"}))
                .unwrap();
        assert_eq!(body["segments"], json!([segment]));
        assert_eq!(
            body["words"],
            json!([{"word":"hello","start":0.0,"end":1.0}])
        );
        assert_eq!(body["languages"], json!([{"code":"en"}]));
        assert!(!body.to_string().contains("private"));
        let (reduced, _) = sanitize_response(upstream, &Value::Null).unwrap();
        assert_eq!(
            reduced,
            json!({"text":"hello","usage":{"type":"duration","seconds":1},"languages":[{"code":"en"}]})
        );
    }
}
