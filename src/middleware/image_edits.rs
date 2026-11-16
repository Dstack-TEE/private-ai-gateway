//! OpenAI multipart image edits with shared generation response and billing rules.

use std::{collections::HashSet, sync::Arc};

use axum::body::Bytes;
use serde_json::{Map, Value};

use super::{
    images,
    media::{self, FormKind, ImageFile, ImageFormat, MediaError, MediaForm},
};

pub const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_MASK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_METADATA_BYTES: usize = 1024;
pub const MAX_IMAGES: usize = 16;
const EXTRA_FIELDS: &[&str] = &["response_format", "user", "safety_identifier", "stream"];
const KIND: FormKind = FormKind::ImageEdit;

fn validate_request(params: &Value) -> Result<(), images::RequestError> {
    images::validate_request(params)?;
    if params["prompt"].as_str().is_some_and(str::is_empty) {
        return Err(images::RequestError::invalid(
            "prompt",
            "prompt must not be empty",
        ));
    }
    for &key in images::UPSTREAM_FIELDS.iter().chain(EXTRA_FIELDS) {
        if key != "prompt"
            && params
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
    Ok(())
}

pub async fn parse(body: Bytes, content_type: &str) -> Result<(Value, Arc<MediaForm>), MediaError> {
    let mut multipart = media::multipart(body, content_type, KIND)?;
    let mut params = Map::new();
    let mut seen = HashSet::new();
    let mut images = Vec::new();
    let mut image_name = None;
    let mut mask = None;
    let mut markup = None;
    let mut text_bytes = 0;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| multipart_error(error, "multipart"))?
    {
        let name = media::field_name(&field, KIND)?;
        if name != "image[]" && !seen.insert(name.clone()) {
            return Err(KIND.invalid(&name));
        }
        match name.as_str() {
            "image" | "image[]" => {
                if image_name
                    .as_ref()
                    .is_some_and(|previous| previous != &name)
                    || images.len() >= MAX_IMAGES
                {
                    return Err(KIND.invalid("image"));
                }
                let data = field
                    .bytes()
                    .await
                    .map_err(|error| multipart_error(error, &name))?;
                image_name = Some(name);
                let format = image_format(&data).ok_or_else(|| KIND.invalid("image"))?;
                images.push(ImageFile { data, format });
                continue;
            }
            "mask" | "markup" => {
                if mask.is_some() || markup.is_some() {
                    return Err(KIND.invalid(&name));
                }
                let data = field
                    .bytes()
                    .await
                    .map_err(|error| multipart_error(error, &name))?;
                if image_format(&data) != Some(ImageFormat::Png) {
                    return Err(KIND.invalid(&name));
                }
                if name == "mask" {
                    mask = Some(data);
                } else {
                    markup = Some(data);
                }
                continue;
            }
            _ => {}
        }
        if !images::UPSTREAM_FIELDS.contains(&name.as_str())
            && !EXTRA_FIELDS.contains(&name.as_str())
        {
            return Err(MediaError {
                code: "unknown_parameter",
                ..KIND.invalid(&name)
            });
        }
        if field.file_name().is_some() {
            return Err(KIND.invalid(&name));
        }
        let data = field
            .bytes()
            .await
            .map_err(|error| multipart_error(error, &name))?;
        text_bytes += data.len();
        if text_bytes > MAX_TEXT_BYTES {
            return Err(KIND.invalid("fields"));
        }
        let value = std::str::from_utf8(&data).map_err(|_| KIND.invalid(&name))?;
        match name.as_str() {
            "stream" => {
                if value != "false" {
                    return Err(KIND.invalid(&name));
                }
            }
            "n" | "output_compression" => {
                let number = value.parse::<i64>().map_err(|_| KIND.invalid(&name))?;
                params.insert(name, Value::from(number));
            }
            _ => {
                params.insert(name, Value::String(value.into()));
            }
        }
    }
    let params = Value::Object(params);
    validate_request(&params).map_err(|error| MediaError {
        code: error.code,
        message: error.message,
        ..KIND.invalid(&error.param)
    })?;
    if images.is_empty() {
        return Err(KIND.invalid("image"));
    }
    Ok((
        params,
        Arc::new(MediaForm::ImageEdit {
            images,
            mask,
            markup,
        }),
    ))
}

/// Detect media type from bytes, never the caller's filename or MIME header.
fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(ImageFormat::Webp)
    } else {
        None
    }
}

pub fn upstream_fields(params: &Value) -> Value {
    images::upstream_body(params)
}

fn multipart_error(error: multer::Error, param: &str) -> MediaError {
    match error {
        multer::Error::FieldSizeExceeded { field_name, .. } => MediaError {
            param: field_name.unwrap_or_else(|| param.into()),
            ..KIND.large()
        },
        multer::Error::StreamSizeExceeded { .. } => MediaError {
            param: param.into(),
            ..KIND.large()
        },
        _ => KIND.invalid(param),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn request(fields: &[(&str, &str)], files: &[(&str, Bytes)]) -> (Bytes, String) {
        let files: Vec<_> = files
            .iter()
            .map(|(name, data)| (*name, data.clone(), "private.exe"))
            .collect();
        media::test_multipart(fields, &files).await
    }

    #[tokio::test]
    async fn sdk_arrays_preserve_image_order_and_use_safe_upstream_parts() {
        for (part, count) in [
            ("image", 1),
            ("image[]", 1),
            ("image[]", 2),
            ("image[]", 16),
        ] {
            let files: Vec<_> = (0..count)
                .map(|index| {
                    let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
                    data.extend_from_slice(index.to_string().as_bytes());
                    (part, Bytes::from(data))
                })
                .collect();
            let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &files).await;
            let (params, form) = parse(raw, &ct).await.unwrap();
            let (encoded, ct) = form.encode(&params).await.unwrap();
            let mut multipart = media::multipart(encoded, &ct, KIND).unwrap();
            let mut forwarded = Vec::new();
            while let Some(field) = multipart.next_field().await.unwrap() {
                if field.file_name().is_some() {
                    assert_eq!(
                        field.name(),
                        Some(if count == 1 { "image" } else { "image[]" })
                    );
                    assert_eq!(field.file_name(), Some("image.png"));
                    assert_eq!(
                        field.content_type().map(ToString::to_string).as_deref(),
                        Some("image/png")
                    );
                    forwarded.push(field.bytes().await.unwrap());
                }
            }
            assert_eq!(
                forwarded,
                files
                    .iter()
                    .map(|(_, bytes)| bytes.clone())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[tokio::test]
    async fn forwards_standard_text_values_and_strips_private_and_no_op_fields() {
        let fields = [
            ("model", "alias"),
            ("prompt", "edit"),
            ("n", "10"),
            ("size", "auto"),
            ("quality", "provider-quality"),
            ("background", "transparent"),
            ("output_format", "provider-format"),
            ("output_compression", "0"),
            ("input_fidelity", "high"),
            ("moderation", "provider-mode"),
            ("user", "alice"),
            ("safety_identifier", "private-user"),
            ("stream", "false"),
            ("response_format", "b64_json"),
        ];
        for (data, format) in [
            (b"\x89PNG\r\n\x1a\nimage".as_slice(), ImageFormat::Png),
            (b"\xff\xd8\xffimage", ImageFormat::Jpeg),
            (b"RIFF0000WEBPimage", ImageFormat::Webp),
        ] {
            let (raw, ct) = request(&fields, &[("image", Bytes::copy_from_slice(data))]).await;
            let (mut params, form) = parse(raw, &ct).await.unwrap();
            params["model"] = json!("upstream-model");
            let (encoded, ct) = form.encode(&params).await.unwrap();
            assert!(!encoded.windows(11).any(|s| s == b"private.exe"));
            let (forwarded, form) = parse(encoded, &ct).await.unwrap();
            assert_eq!(
                forwarded,
                json!({"model":"upstream-model","prompt":"edit","n":10,"response_format":"b64_json",
                "quality":"provider-quality","background":"transparent","output_format":"provider-format",
                "output_compression":0,"input_fidelity":"high","moderation":"provider-mode"})
            );
            let MediaForm::ImageEdit { images, .. } = form.as_ref() else {
                panic!("expected image edits")
            };
            assert_eq!(images[0].data, data);
            assert_eq!(images[0].format, format);
        }
    }

    #[tokio::test]
    async fn rejects_invalid_fields_image_counts_and_mixed_sdk_names() {
        for patch in [
            ("n", "11"),
            ("n", "0"),
            ("n", "1.5"),
            ("output_compression", "101"),
            ("output_compression", "-1"),
            ("stream", "true"),
            ("partial_images", "1"),
            ("provider", "{}"),
            ("response_format", "url"),
            ("model", "duplicate"),
            ("file_id", "file-123"),
            ("image_url", "https://example.com/image"),
            ("svg", "true"),
        ] {
            let (raw, ct) = request(
                &[("model", "alias"), ("prompt", "edit"), patch],
                &[("image", Bytes::from_static(b"\xff\xd8\xff"))],
            )
            .await;
            assert!(parse(raw, &ct).await.is_err(), "{patch:?}");
        }
        for names in [
            vec!["image[]"; 17],
            vec!["image", "image[]"],
            vec!["image[]", "image"],
            vec!["image", "image"],
            vec![],
        ] {
            let files: Vec<_> = names
                .into_iter()
                .map(|name| (name, Bytes::from_static(b"\xff\xd8\xff")))
                .collect();
            let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &files).await;
            let error = parse(raw, &ct).await.unwrap_err();
            assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
        }
        let (raw, ct) = request(
            &[("model", "alias"), ("prompt", "edit")],
            &[("image", Bytes::from_static(b"not an image"))],
        )
        .await;
        assert!(parse(raw, &ct).await.is_err());
    }

    #[tokio::test]
    async fn masks_and_markup_are_png_files_and_mutually_exclusive() {
        for (parts, valid) in [
            (vec![("mask", b"\x89PNG\r\n\x1a\nmask".as_slice())], true),
            (
                vec![("markup", b"\x89PNG\r\n\x1a\nmarkup".as_slice())],
                true,
            ),
            (vec![("mask", b"\xff\xd8\xff".as_slice())], false),
            (vec![("markup", b"\xff\xd8\xff".as_slice())], false),
            (
                vec![
                    ("mask", b"\x89PNG\r\n\x1a\n".as_slice()),
                    ("markup", b"\x89PNG\r\n\x1a\n".as_slice()),
                ],
                false,
            ),
            (
                vec![
                    ("markup", b"\x89PNG\r\n\x1a\n".as_slice()),
                    ("mask", b"\x89PNG\r\n\x1a\n".as_slice()),
                ],
                false,
            ),
            (
                vec![
                    ("mask", b"\x89PNG\r\n\x1a\n".as_slice()),
                    ("mask", b"\x89PNG\r\n\x1a\n".as_slice()),
                ],
                false,
            ),
        ] {
            let mut files = vec![("image", Bytes::from_static(b"\xff\xd8\xffimage"))];
            files.extend(
                parts
                    .iter()
                    .map(|(name, data)| (*name, Bytes::copy_from_slice(data))),
            );
            let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &files).await;
            let result = parse(raw, &ct).await;
            assert_eq!(result.is_ok(), valid);
            if let Ok((params, form)) = result {
                let (encoded, ct) = form.encode(&params).await.unwrap();
                let (_, forwarded) = parse(encoded, &ct).await.unwrap();
                let MediaForm::ImageEdit { mask, markup, .. } = forwarded.as_ref() else {
                    panic!("expected image edits")
                };
                assert_eq!(
                    mask.as_deref(),
                    parts
                        .iter()
                        .find(|(name, _)| *name == "mask")
                        .map(|(_, data)| *data)
                );
                assert_eq!(
                    markup.as_deref(),
                    parts
                        .iter()
                        .find(|(name, _)| *name == "markup")
                        .map(|(_, data)| *data)
                );
            }
        }
    }

    #[tokio::test]
    async fn enforces_file_mask_and_unicode_prompt_limits() {
        for (name, limit) in [
            ("image", MAX_FILE_BYTES),
            ("image[]", MAX_FILE_BYTES),
            ("mask", MAX_MASK_BYTES),
        ] {
            for (length, valid) in [(limit, true), (limit + 1, false)] {
                let mut data = vec![0; length];
                data[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
                let mut files = vec![(name, Bytes::from(data))];
                if name == "mask" {
                    files.push(("image", Bytes::from_static(b"\xff\xd8\xff")));
                }
                let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &files).await;
                let result = parse(raw, &ct).await;
                assert_eq!(result.is_ok(), valid, "{name}: {length}");
                if !valid {
                    let error = result.unwrap_err();
                    assert_eq!(error.status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
                    assert_eq!(error.param, name);
                }
            }
        }
        for (length, valid) in [(0, false), (1, true), (32000, true), (32001, false)] {
            let (raw, ct) = request(
                &[("model", "alias"), ("prompt", &"界".repeat(length))],
                &[("image", Bytes::from_static(b"\xff\xd8\xff"))],
            )
            .await;
            assert_eq!(parse(raw, &ct).await.is_ok(), valid);
        }
        for n in ["1", "10"] {
            let (raw, ct) = request(
                &[
                    ("model", "alias"),
                    ("prompt", "edit"),
                    ("n", n),
                    ("output_compression", "100"),
                ],
                &[("image", Bytes::from_static(b"\xff\xd8\xff"))],
            )
            .await;
            assert!(parse(raw, &ct).await.is_ok());
        }
    }

    #[tokio::test]
    async fn truncated_file_parts_are_invalid_requests_not_size_overflows() {
        for name in ["image", "mask", "markup"] {
            let mut files = vec![];
            if name != "image" {
                files.push(("image", Bytes::from_static(b"\xff\xd8\xff")));
            }
            files.push((name, Bytes::from_static(b"\x89PNG\r\n\x1a\nimage")));
            let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &files).await;
            let boundary = multer::parse_boundary(&ct).unwrap();
            let truncated = raw.slice(..raw.len() - boundary.len() - 8);
            let error = parse(truncated, &ct).await.unwrap_err();
            assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(error.param, name);
        }
    }

    #[tokio::test]
    async fn caps_non_prompt_metadata_at_the_upload_boundary() {
        for key in ["model", "size", "user", "input_fidelity"] {
            for (length, valid) in [(MAX_METADATA_BYTES, true), (MAX_METADATA_BYTES + 1, false)] {
                let value = "x".repeat(length);
                let mut fields = vec![("prompt", "edit")];
                if key != "model" {
                    fields.push(("model", "alias"));
                }
                fields.push((key, &value));
                let (raw, ct) =
                    request(&fields, &[("image", Bytes::from_static(b"\xff\xd8\xff"))]).await;
                let parsed = parse(raw, &ct).await;
                assert_eq!(parsed.is_ok(), valid, "{key}: {length}");
            }
        }
    }
}
