//! Shared bounded media uploads, multipart guards and immutable upstream encoding.

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    Transcription,
    ImageEdit,
}

pub(crate) struct FormSpec {
    label: &'static str,
    file_field: &'static str,
    filename_stem: &'static str,
    max_file_bytes: usize,
    pub(crate) max_body_bytes: usize,
    max_text_bytes: usize,
    upstream_fields: fn(&Value) -> Value,
}

const TRANSCRIPTION: FormSpec = FormSpec {
    label: "transcription",
    file_field: "file",
    filename_stem: "audio",
    max_file_bytes: super::audio::MAX_FILE_BYTES,
    max_body_bytes: super::audio::MAX_BODY_BYTES,
    max_text_bytes: super::audio::MAX_TEXT_BYTES,
    upstream_fields: super::audio::upstream_fields,
};
const IMAGE_EDIT: FormSpec = FormSpec {
    label: "image edit",
    file_field: "image",
    filename_stem: "image",
    max_file_bytes: super::image_edits::MAX_FILE_BYTES,
    max_body_bytes: super::image_edits::MAX_BODY_BYTES,
    max_text_bytes: super::image_edits::MAX_TEXT_BYTES,
    upstream_fields: super::image_edits::upstream_fields,
};

impl FormKind {
    pub(crate) fn spec(self) -> &'static FormSpec {
        match self {
            Self::Transcription => &TRANSCRIPTION,
            Self::ImageEdit => &IMAGE_EDIT,
        }
    }
}

// Account for audio and image bytes together, including uploads awaiting consult.
const UPLOAD_BYTE_BUDGET: usize = 16 * super::audio::MAX_BODY_BYTES;
const READ_DEADLINE: Duration = Duration::from_secs(60);
pub(crate) static UPLOAD_BYTES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(UPLOAD_BYTE_BUDGET)));

#[derive(Debug)]
pub struct MediaError {
    pub status: StatusCode,
    pub code: &'static str,
    pub param: String,
    pub(crate) message: String,
    pub(crate) error_type: &'static str,
}
impl FormKind {
    pub(crate) fn invalid(self, param: &str) -> MediaError {
        MediaError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_parameter",
            param: param.into(),
            message: format!("invalid {} parameter: {param}", self.spec().label),
            error_type: "invalid_request_error",
        }
    }
    fn capacity(self) -> MediaError {
        MediaError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limit_exceeded",
            param: "body".into(),
            message: format!(
                "{} upload byte capacity reached; retry shortly",
                self.spec().label
            ),
            error_type: "rate_limit_error",
        }
    }
    pub(crate) fn large(self) -> MediaError {
        MediaError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "request_too_large",
            param: self.spec().file_field.into(),
            message: format!(
                "{} upload exceeds the file or request size limit",
                self.spec().label
            ),
            error_type: "invalid_request_error",
        }
    }
    fn timeout(self) -> MediaError {
        MediaError {
            status: StatusCode::REQUEST_TIMEOUT,
            code: "upload_timeout",
            param: "body".into(),
            message: format!(
                "{} upload exceeded the 60-second read deadline",
                self.spec().label
            ),
            error_type: "invalid_request_error",
        }
    }
}

impl IntoResponse for MediaError {
    fn into_response(self) -> Response {
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
    kind: FormKind,
) -> Result<(Bytes, OwnedSemaphorePermit), MediaError> {
    if headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|v| v > kind.spec().max_body_bytes as u64)
    {
        return Err(kind.large());
    }
    tokio::time::timeout(
        READ_DEADLINE,
        collect_upload(body, UPLOAD_BYTES.clone(), kind),
    )
    .await
    .map_err(|_| kind.timeout())?
}

pub(crate) async fn collect_upload(
    body: Body,
    budget: Arc<Semaphore>,
    kind: FormKind,
) -> Result<(Bytes, OwnedSemaphorePermit), MediaError> {
    let mut permit = budget
        .clone()
        .try_acquire_many_owned(0)
        .map_err(|_| kind.capacity())?;
    let mut chunks = body.into_data_stream();
    let mut bytes = bytes::BytesMut::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| kind.invalid("body"))?;
        if chunk.len() > kind.spec().max_body_bytes - bytes.len() {
            return Err(kind.large());
        }
        let count = u32::try_from(chunk.len()).map_err(|_| kind.large())?;
        let chunk_permit = budget
            .clone()
            .try_acquire_many_owned(count)
            .map_err(|_| kind.capacity())?;
        permit.merge(chunk_permit);
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes.freeze(), permit))
}

/// Shared slices of the buffered upload; no caller filename or MIME is forwarded.
#[derive(Debug, Clone)]
pub struct MediaForm {
    pub kind: FormKind,
    pub file: Bytes,
    pub extension: String,
}

pub(crate) fn multipart(
    body: Bytes,
    content_type: &str,
    kind: FormKind,
) -> Result<multer::Multipart<'static>, MediaError> {
    let boundary =
        multer::parse_boundary(content_type).map_err(|_| kind.invalid("content-type"))?;
    if boundary.is_empty()
        || boundary.len() > 70
        || !boundary.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
    {
        return Err(kind.invalid("boundary"));
    }
    let spec = kind.spec();
    let mut limits = multer::SizeLimit::new()
        .whole_stream(spec.max_body_bytes as u64)
        .per_field(spec.max_text_bytes as u64)
        .for_field(spec.file_field, spec.max_file_bytes as u64);
    if kind == FormKind::ImageEdit {
        limits = limits.for_field("image[]", spec.max_file_bytes as u64);
    }
    let constraints = multer::Constraints::new().size_limit(limits);
    Ok(multer::Multipart::with_constraints(
        stream::once(async move { Ok::<_, std::convert::Infallible>(body) }),
        boundary,
        constraints,
    ))
}

pub(crate) fn field_name(field: &multer::Field<'_>, kind: FormKind) -> Result<String, MediaError> {
    let name = field.name().ok_or_else(|| kind.invalid("field"))?;
    if name.len() > 64
        || field
            .headers()
            .iter()
            .map(|(k, v)| k.as_str().len() + v.len())
            .sum::<usize>()
            > 8192
    {
        return Err(kind.invalid("multipart"));
    }
    Ok(name.to_string())
}

impl MediaForm {
    /// Encode once after model selection. These exact immutable bytes are both
    /// hashed and transmitted, including on channel binding retries.
    pub async fn encode(
        &self,
        params: &Value,
    ) -> Result<(Bytes, String), crate::aci::upstream::UpstreamError> {
        use crate::aci::upstream::UpstreamError;
        let kind = self.kind;
        let mut form = reqwest::multipart::Form::new();
        for (key, value) in (kind.spec().upstream_fields)(params)
            .as_object()
            .into_iter()
            .flatten()
        {
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
                .file_name(format!("{}.{}", kind.spec().filename_stem, self.extension))
                .mime_str("application/octet-stream")
                .map_err(|e| UpstreamError::InvalidRequest(e.to_string()))?;
        form = form.part(kind.spec().file_field, file);
        let content_type = format!("multipart/form-data; boundary={}", form.boundary());
        let chunks = form.into_stream();
        futures_util::pin_mut!(chunks);
        let mut bytes = bytes::BytesMut::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|e| UpstreamError::Transport(e.to_string()))?;
            if chunk.len() > kind.spec().max_body_bytes - bytes.len() {
                return Err(UpstreamError::InvalidRequest(
                    "encoded multipart body exceeds limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes.freeze(), content_type))
    }
}
