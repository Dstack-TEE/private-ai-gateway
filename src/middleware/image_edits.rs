//! Single-image multipart edits with the image generation response/billing schema.

use std::{collections::HashSet, sync::Arc};

use axum::body::Bytes;
use serde_json::{Map, Value};

use super::{
    images,
    media::{self, FormKind, MediaError, MediaForm},
};

pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_BODY_BYTES: usize = 21 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = MAX_BODY_BYTES - MAX_FILE_BYTES;
pub const MAX_METADATA_BYTES: usize = 1024;
const FIELDS: &[&str] = &[
    "model",
    "prompt",
    "size",
    "response_format",
    "n",
    "user",
    "provider",
    "quality",
    "background",
    "output_format",
    "safety_identifier",
    "stream",
];
const KIND: FormKind = FormKind::ImageEdit;

pub fn validate_request(params: &Value) -> Result<(), images::RequestError> {
    if let Some(object) = params.as_object() {
        images::reject_unknown(object, FIELDS)?;
    }
    images::validate_request(params)?;
    for &key in FIELDS {
        if key == "prompt" {
            continue;
        }
        if params
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| value.len() > MAX_METADATA_BYTES)
        {
            return Err(images::RequestError::invalid(
                key,
                &format!("parameter must be at most {MAX_METADATA_BYTES} bytes"),
            ));
        }
    }
    for key in ["quality", "background"] {
        if params.get(key).is_some_and(|value| value != "auto") {
            return Err(images::RequestError::invalid(
                key,
                "only auto is supported for image edits",
            ));
        }
    }
    if params.get("n").is_some_and(|n| n.as_u64() != Some(1)) {
        return Err(images::RequestError::invalid(
            "n",
            "image edits support exactly one output image",
        ));
    }
    Ok(())
}

pub async fn parse(body: Bytes, content_type: &str) -> Result<(Value, Arc<MediaForm>), MediaError> {
    let mut multipart = media::multipart(body, content_type, KIND)?;
    let mut params = Map::new();
    let mut seen = HashSet::new();
    let mut image = None;
    let mut text_bytes = 0;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| KIND.invalid("multipart"))?
    {
        let name = media::field_name(&field, KIND)?;
        let name = if name == "image[]" {
            "image".into()
        } else {
            name
        };
        if !seen.insert(name.clone()) {
            return Err(KIND.invalid(&name));
        }
        if name == "image" {
            let data = field.bytes().await.map_err(|_| KIND.large())?;
            let extension = image_extension(&data).ok_or_else(|| KIND.invalid("image"))?;
            image = Some(Arc::new(MediaForm {
                kind: KIND,
                file: data,
                extension: extension.into(),
            }));
            continue;
        }
        if name == "provider" || !FIELDS.contains(&name.as_str()) {
            return Err(MediaError {
                code: "unknown_parameter",
                ..KIND.invalid(&name)
            });
        }
        if field.file_name().is_some() {
            return Err(KIND.invalid(&name));
        }
        let data = field.bytes().await.map_err(|_| KIND.invalid(&name))?;
        text_bytes += data.len();
        if text_bytes > MAX_TEXT_BYTES {
            return Err(KIND.invalid("fields"));
        }
        let value = std::str::from_utf8(&data).map_err(|_| KIND.invalid(&name))?;
        if name == "stream" {
            if value != "false" {
                return Err(KIND.invalid(&name));
            }
        } else if name == "n" {
            if value != "1" {
                return Err(KIND.invalid("n"));
            }
            params.insert(name, Value::from(1));
        } else {
            params.insert(name, Value::String(value.into()));
        }
    }
    let params = Value::Object(params);
    validate_request(&params).map_err(|error| MediaError {
        code: error.code,
        message: error.message,
        ..KIND.invalid(&error.param)
    })?;
    Ok((params, image.ok_or_else(|| KIND.invalid("image"))?))
}

/// Detect media type from bytes, never the caller's filename or MIME header.
fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else {
        None
    }
}

pub fn upstream_fields(params: &Value) -> Value {
    let mut fields = images::upstream_body(params);
    if let Some(fields) = fields.as_object_mut() {
        fields.retain(|key, _| {
            [
                "model",
                "prompt",
                "size",
                "response_format",
                "output_format",
            ]
            .contains(&key.as_str())
        });
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use serde_json::json;

    async fn request(fields: &[(&str, &str)], images: &[(&str, Bytes)]) -> (Bytes, String) {
        let mut form = reqwest::multipart::Form::new();
        for (key, value) in fields {
            form = form.text(key.to_string(), value.to_string());
        }
        for (key, data) in images {
            form = form.part(
                key.to_string(),
                reqwest::multipart::Part::stream(data.clone())
                    .file_name("private.exe")
                    .mime_str("text/private")
                    .unwrap(),
            );
        }
        let ct = format!("multipart/form-data; boundary={}", form.boundary());
        let stream = form.into_stream();
        futures_util::pin_mut!(stream);
        let mut bytes = bytes::BytesMut::new();
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk.unwrap());
        }
        (bytes.freeze(), ct)
    }

    #[tokio::test]
    async fn encodes_image_magic_with_safe_filename_and_allowlisted_fields() {
        for (part, data, extension) in [
            ("image", b"\x89PNG\r\n\x1a\nimage".as_slice(), "png"),
            ("image[]", b"\xff\xd8\xffimage", "jpg"),
            ("image[]", b"RIFF0000WEBPimage", "webp"),
        ] {
            let (raw, ct) = request(
                &[
                    ("model", "alias"),
                    ("prompt", "edit"),
                    ("user", "alice"),
                    ("size", "auto"),
                    ("n", "1"),
                ],
                &[(part, Bytes::copy_from_slice(data))],
            )
            .await;
            let (mut params, form) = parse(raw, &ct).await.unwrap();
            assert_eq!(form.extension, extension);
            params["model"] = json!("upstream-model");
            let (encoded, ct) = form.encode(&params).await.unwrap();
            assert!(!encoded.windows(11).any(|s| s == b"private.exe"));
            assert!(!encoded.windows(5).any(|s| s == b"alice"));
            let (forwarded, image) = parse(encoded, &ct).await.unwrap();
            assert_eq!(
                forwarded,
                json!({"model":"upstream-model","prompt":"edit","response_format":"b64_json"})
            );
            assert_eq!(image.file, data);
        }
    }

    #[tokio::test]
    async fn accepts_sdk_no_ops_and_reuses_generation_output_shaping() {
        let (raw, ct) = request(
            &[
                ("model", "alias"),
                ("prompt", "edit"),
                ("quality", "auto"),
                ("background", "auto"),
                ("output_format", "png"),
                ("safety_identifier", "private-user"),
                ("stream", "false"),
            ],
            &[("image[]", Bytes::from_static(b"\xff\xd8\xffimage"))],
        )
        .await;
        let (params, form) = parse(raw, &ct).await.unwrap();
        assert!(params.get("stream").is_none());
        let expected = json!({"model":"alias", "prompt":"edit", "output_format":"png", "response_format":"b64_json"});
        assert_eq!(upstream_fields(&params), expected);
        let (encoded, ct) = form.encode(&params).await.unwrap();
        let (forwarded, _) = parse(encoded, &ct).await.unwrap();
        assert_eq!(forwarded, expected);
    }

    #[tokio::test]
    async fn rejects_unsupported_fields_duplicates_and_invalid_files() {
        for patch in [
            ("mask", "x"),
            ("n", "2"),
            ("stream", "true"),
            ("partial_images", "1"),
            ("quality", "high"),
            ("background", "opaque"),
            ("provider", "{}"),
            ("response_format", "url"),
            ("model", "duplicate"),
        ] {
            let (raw, ct) = request(
                &[("model", "alias"), ("prompt", "edit"), patch],
                &[("image", Bytes::from_static(b"\x89PNG\r\n\x1a\nimage"))],
            )
            .await;
            assert!(parse(raw, &ct).await.is_err(), "{patch:?}");
        }
        for images in [
            vec![
                ("image[]", Bytes::from_static(b"\xff\xd8\xff")),
                ("image[]", Bytes::from_static(b"\xff\xd8\xff")),
            ],
            vec![
                ("image", Bytes::from_static(b"\xff\xd8\xff")),
                ("image[]", Bytes::from_static(b"\xff\xd8\xff")),
            ],
            vec![
                ("image[]", Bytes::from_static(b"\xff\xd8\xff")),
                ("image", Bytes::from_static(b"\xff\xd8\xff")),
            ],
            vec![],
            vec![("image", Bytes::from_static(b"not an image"))],
            vec![
                ("image", Bytes::from_static(b"\xff\xd8\xff")),
                ("image", Bytes::from_static(b"\xff\xd8\xff")),
            ],
        ] {
            let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &images).await;
            assert!(parse(raw, &ct).await.is_err());
        }
    }

    #[tokio::test]
    async fn enforces_file_body_and_unicode_prompt_limits() {
        for part in ["image", "image[]"] {
            for (len, valid) in [(MAX_FILE_BYTES, true), (MAX_FILE_BYTES + 1, false)] {
                let mut data = vec![0; len];
                data[..3].copy_from_slice(b"\xff\xd8\xff");
                let (raw, ct) = request(
                    &[("model", "alias"), ("prompt", "edit")],
                    &[(part, data.into())],
                )
                .await;
                assert_eq!(parse(raw, &ct).await.is_ok(), valid);
            }
        }
        for (len, valid) in [(32000, true), (32001, false)] {
            let (raw, ct) = request(
                &[("model", "alias"), ("prompt", &"界".repeat(len))],
                &[("image", Bytes::from_static(b"\xff\xd8\xff"))],
            )
            .await;
            assert_eq!(parse(raw, &ct).await.is_ok(), valid);
        }
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("content-length", (MAX_BODY_BYTES + 1).into());
        assert_eq!(
            media::read_upload(&headers, axum::body::Body::empty(), KIND)
                .await
                .unwrap_err()
                .status,
            axum::http::StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    #[tokio::test]
    async fn caps_non_prompt_metadata_at_the_upload_boundary() {
        for key in ["model", "size", "user"] {
            for (length, valid) in [(MAX_METADATA_BYTES, true), (MAX_METADATA_BYTES + 1, false)] {
                let value = "x".repeat(length);
                let mut fields = vec![("prompt", "edit")];
                if key != "model" {
                    fields.push(("model", "alias"));
                }
                fields.push((key, &value));
                let (raw, ct) = request(
                    &fields,
                    &[("image", Bytes::from_static(b"\xff\xd8\xffimage"))],
                )
                .await;
                let parsed = parse(raw, &ct).await;
                if valid {
                    parsed.unwrap();
                } else {
                    let error = parsed.unwrap_err();
                    assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
                    assert_eq!(error.param, key);
                }
            }
        }
    }
}
