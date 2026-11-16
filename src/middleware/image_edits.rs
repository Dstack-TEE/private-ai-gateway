//! OpenAI multipart image edits with shared generation response and billing rules.

use std::{collections::HashSet, sync::Arc};

use axum::body::Bytes;
use serde_json::{Map, Value};

use super::{
    images,
    media::{self, ImageFile, ImageFormat, MediaError, MediaForm, IMAGE_EDIT},
};

pub const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_MASK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_IMAGES: usize = 16;
const EXTRA_FIELDS: &[&str] = &["response_format", "user", "safety_identifier", "stream"];

fn validate_request(params: &Value) -> Result<(), images::RequestError> {
    images::validate_request(params)?;
    if params["prompt"].as_str().is_some_and(str::is_empty) {
        return Err(images::RequestError::invalid(
            "prompt",
            "prompt must not be empty",
        ));
    }
    Ok(())
}

pub async fn parse(body: Bytes, content_type: &str) -> Result<(Value, Arc<MediaForm>), MediaError> {
    let mut multipart = media::multipart(body, content_type, &IMAGE_EDIT)?;
    let mut params = Map::new();
    let mut seen = HashSet::new();
    let mut images = Vec::new();
    let mut image_name = None;
    let mut overlay = None;
    let mut text_bytes = 0;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| multipart_error(error, "multipart"))?
    {
        let name = media::field_name(&field, &IMAGE_EDIT)?;
        if name != "image[]" && !seen.insert(name.clone()) {
            return Err(IMAGE_EDIT.invalid(&name));
        }
        match name.as_str() {
            "image" | "image[]"
                if image_name
                    .as_ref()
                    .is_some_and(|previous| previous != &name)
                    || images.len() >= MAX_IMAGES =>
            {
                return Err(IMAGE_EDIT.invalid("image"));
            }
            "mask" | "markup" if overlay.is_some() => return Err(IMAGE_EDIT.invalid(&name)),
            "image" | "image[]" | "mask" | "markup" => {}
            _ => {
                if !images::UPSTREAM_FIELDS.contains(&name.as_str())
                    && !EXTRA_FIELDS.contains(&name.as_str())
                {
                    return Err(MediaError {
                        code: "unknown_parameter",
                        ..IMAGE_EDIT.invalid(&name)
                    });
                }
                if field.file_name().is_some() {
                    return Err(IMAGE_EDIT.invalid(&name));
                }
            }
        }
        let data = field
            .bytes()
            .await
            .map_err(|error| multipart_error(error, &name))?;
        match name.as_str() {
            "image" | "image[]" => {
                image_name = Some(name);
                let format = image_format(&data).ok_or_else(|| IMAGE_EDIT.invalid("image"))?;
                images.push(ImageFile { data, format });
                continue;
            }
            "mask" | "markup" => {
                if image_format(&data) != Some(ImageFormat::Png) {
                    return Err(IMAGE_EDIT.invalid(&name));
                }
                overlay = Some((if name == "mask" { "mask" } else { "markup" }, data));
                continue;
            }
            _ => {}
        }
        text_bytes += data.len();
        if text_bytes > MAX_TEXT_BYTES {
            return Err(IMAGE_EDIT.invalid("fields"));
        }
        let value = std::str::from_utf8(&data).map_err(|_| IMAGE_EDIT.invalid(&name))?;
        match name.as_str() {
            "stream" => {
                if value != "false" {
                    return Err(IMAGE_EDIT.invalid(&name));
                }
            }
            "n" | "output_compression" => {
                let number = value
                    .parse::<i64>()
                    .map_err(|_| IMAGE_EDIT.invalid(&name))?;
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
        ..IMAGE_EDIT.invalid(&error.param)
    })?;
    if images.is_empty() {
        return Err(IMAGE_EDIT.invalid("image"));
    }
    Ok((params, Arc::new(MediaForm::ImageEdit { images, overlay })))
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

fn multipart_error(error: multer::Error, param: &str) -> MediaError {
    match error {
        multer::Error::FieldSizeExceeded { field_name, .. } => MediaError {
            param: field_name.unwrap_or_else(|| param.into()),
            ..IMAGE_EDIT.large()
        },
        _ => IMAGE_EDIT.invalid(param),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG: &[u8] = b"\xff\xd8\xff";

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
                    let mut data = PNG.to_vec();
                    data.extend_from_slice(index.to_string().as_bytes());
                    (part, Bytes::from(data))
                })
                .collect();
            let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &files).await;
            let (params, form) = parse(raw, &ct).await.unwrap();
            let (encoded, ct) = form.encode(&params).await.unwrap();
            let mut multipart = media::multipart(encoded, &ct, &IMAGE_EDIT).unwrap();
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
            (PNG, ImageFormat::Png),
            (JPEG, ImageFormat::Jpeg),
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
                &[("image", Bytes::from_static(JPEG))],
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
                .map(|name| (name, Bytes::from_static(JPEG)))
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
            (vec![("mask", PNG)], true),
            (vec![("markup", PNG)], true),
            (vec![("mask", JPEG)], false),
            (vec![("markup", JPEG)], false),
            (vec![("mask", PNG), ("markup", PNG)], false),
            (vec![("markup", PNG), ("mask", PNG)], false),
            (vec![("mask", PNG), ("mask", PNG)], false),
        ] {
            let mut files = vec![("image", Bytes::from_static(JPEG))];
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
                let MediaForm::ImageEdit { overlay, .. } = forwarded.as_ref() else {
                    panic!("expected image edits")
                };
                assert_eq!(
                    overlay.as_ref().map(|(name, data)| (*name, data.as_ref())),
                    parts.first().copied()
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
                data[..8].copy_from_slice(PNG);
                let mut files = vec![(name, Bytes::from(data))];
                if name == "mask" {
                    files.push(("image", Bytes::from_static(JPEG)));
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
                &[("image", Bytes::from_static(JPEG))],
            )
            .await;
            assert_eq!(parse(raw, &ct).await.is_ok(), valid);
        }
    }

    #[tokio::test]
    async fn truncated_file_parts_are_invalid_requests_not_size_overflows() {
        for name in ["image", "mask", "markup"] {
            let mut files = vec![];
            if name != "image" {
                files.push(("image", Bytes::from_static(JPEG)));
            }
            files.push((name, Bytes::from_static(PNG)));
            let (raw, ct) = request(&[("model", "alias"), ("prompt", "edit")], &files).await;
            let boundary = multer::parse_boundary(&ct).unwrap();
            let truncated = raw.slice(..raw.len() - boundary.len() - 8);
            let error = parse(truncated, &ct).await.unwrap_err();
            assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(error.param, name);
        }
    }
}
