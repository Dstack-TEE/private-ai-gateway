//! Validate buffered request carriers and inspect models without rewriting bytes.

use super::{Rejection, StatusCode, Surface, Value, MAX_BODY_BYTES};
use axum::{body::Bytes, http::HeaderMap};
use multer::{Constraints, SizeLimit};

/// Keep body semantics and serving constraints intact across HTTP forwarding.
/// Returns the sole Content-Type value, or an empty string when absent.
pub fn check_carrier_headers(headers: &HeaderMap) -> Result<&str, &'static str> {
    let dropped = super::hop_by_hop_names(headers);
    if dropped.contains("x-aci-provider") || dropped.contains("content-type") {
        return Err("serving constraint carriers cannot be hop-by-hop headers");
    }
    let mut content_types = headers.get_all("content-type").iter();
    let content_type = content_types.next();
    if content_types.next().is_some() {
        return Err("duplicate Content-Type headers");
    }
    content_type
        .map(|value| value.to_str().map_err(|_| "invalid Content-Type header"))
        .transpose()
        .map(|value| value.unwrap_or(""))
}

/// Parse buffered multipart bytes under caller-supplied field and size limits.
pub fn multipart(
    bytes: Bytes,
    content_type: &str,
    constraints: multer::Constraints,
) -> Result<multer::Multipart<'static>, &'static str> {
    let boundary =
        multer::parse_boundary(content_type).map_err(|_| "invalid multipart boundary")?;
    if boundary.is_empty() || boundary.len() > 70 {
        return Err("invalid multipart boundary");
    }
    let stream =
        futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(bytes) });
    Ok(multer::Multipart::with_constraints(
        stream,
        boundary,
        constraints,
    ))
}

const MAX_PARTS: usize = 16;
const MAX_TEXT_BYTES: usize = 64 * 1024;

fn invalid(message: &'static str) -> Rejection {
    Rejection::new(StatusCode::BAD_REQUEST, "invalid_request_error", message)
}

pub(super) async fn model_of(
    headers: &HeaderMap,
    bytes: &Bytes,
    surface: Surface,
) -> Result<Option<String>, Rejection> {
    let content_type = check_carrier_headers(headers).map_err(invalid)?;
    let multipart = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("multipart/form-data");
    if surface == Surface::AudioTranscriptions {
        if !multipart {
            return Err(invalid("Audio transcription requires multipart/form-data"));
        }
        return multipart_model(bytes, content_type).await.map(Some);
    }
    if multipart {
        return Err(invalid(
            "Multipart is only supported on /v1/audio/transcriptions",
        ));
    }
    Ok(serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        }))
}

async fn multipart_model(bytes: &Bytes, content_type: &str) -> Result<String, Rejection> {
    let limits = Constraints::new().size_limit(
        SizeLimit::new()
            .whole_stream(MAX_BODY_BYTES as u64)
            .per_field(MAX_TEXT_BYTES as u64)
            .for_field("file", MAX_BODY_BYTES as u64),
    );
    let mut multipart = multipart(bytes.clone(), content_type, limits).map_err(invalid)?;
    let mut model = None;
    let mut count = 0;
    let mut text_bytes = 0;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid("Malformed or oversized multipart body"))?
    {
        count += 1;
        if count > MAX_PARTS {
            return Err(invalid("Multipart body exceeds 16 parts"));
        }
        let is_model = field.name() == Some("model");
        let is_file = field.name() == Some("file");
        if is_model && (model.is_some() || field.file_name().is_some()) {
            return Err(invalid("Multipart model must be a unique text field"));
        }
        let mut value = Vec::new();
        // Drain file chunks without collecting them. Only the bounded model
        // field is retained; the existing upload buffer is forwarded unchanged.
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| invalid("Malformed or oversized multipart field"))?
        {
            if !is_file {
                text_bytes += chunk.len();
                if text_bytes > MAX_TEXT_BYTES {
                    return Err(invalid("Multipart text exceeds 64 KiB"));
                }
            }
            if is_model {
                value.extend_from_slice(&chunk);
            }
        }
        if is_model {
            let value =
                String::from_utf8(value).map_err(|_| invalid("Multipart model must be UTF-8"))?;
            if value.trim().is_empty() {
                return Err(invalid("Multipart model must not be empty"));
            }
            model = Some(value);
        }
    }
    model.ok_or_else(|| invalid("Multipart body must contain a text `model` field"))
}

#[cfg(test)]
mod tests {
    use super::super::header;
    use super::*;

    const CONTENT_TYPE: &str = "multipart/form-data; boundary=\"test-boundary\"";

    fn body(parts: &[(&str, Option<&str>, &[u8])]) -> Bytes {
        let mut bytes = Vec::new();
        for (name, filename, data) in parts {
            bytes.extend_from_slice(
                format!("--test-boundary\r\nContent-Disposition: form-data; name=\"{name}\"")
                    .as_bytes(),
            );
            if let Some(filename) = filename {
                bytes.extend_from_slice(format!("; filename=\"{filename}\"").as_bytes());
            }
            bytes.extend_from_slice(b"\r\n\r\n");
            bytes.extend_from_slice(data);
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(b"--test-boundary--\r\n");
        bytes.into()
    }

    #[tokio::test]
    async fn rejects_ambiguous_missing_or_invalid_model_parts() {
        let cases = [
            vec![("file", Some("clip.wav"), b"audio".as_slice())],
            vec![("model", None, b"".as_slice())],
            vec![("model", None, b" \t".as_slice())],
            vec![("model", None, b"\xff".as_slice())],
            vec![("model", Some("model.txt"), b"whisper".as_slice())],
            vec![
                ("model", None, b"whisper".as_slice()),
                ("model", None, b"other".as_slice()),
            ],
        ];
        for parts in cases {
            assert!(multipart_model(&body(&parts), CONTENT_TYPE).await.is_err());
        }
    }

    #[tokio::test]
    async fn enforces_part_count_and_aggregate_text_limits() {
        let mut parts = vec![("model", None, b"whisper".as_slice())];
        parts.extend(std::iter::repeat_n(
            ("unused", None, b"".as_slice()),
            MAX_PARTS - 1,
        ));
        assert!(multipart_model(&body(&parts), CONTENT_TYPE).await.is_ok());
        parts.push(("unused", None, b""));
        assert!(multipart_model(&body(&parts), CONTENT_TYPE).await.is_err());

        let text = vec![b'a'; MAX_TEXT_BYTES];
        assert!(
            multipart_model(&body(&[("model", None, &text)]), CONTENT_TYPE)
                .await
                .is_ok()
        );
        let too_large = vec![b'a'; MAX_TEXT_BYTES + 1];
        assert!(multipart_model(
            &body(&[("model", None, b"whisper"), ("prompt", None, &text)]),
            CONTENT_TYPE,
        )
        .await
        .is_err());
        // Binary uploads are not subject to the text limit or collected again.
        assert!(multipart_model(
            &body(&[
                ("model", None, b"whisper"),
                ("file", Some("clip.wav"), &too_large)
            ]),
            CONTENT_TYPE,
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn requires_unambiguous_multipart_only_on_the_audio_surface() {
        let bytes = body(&[("model", None, b"whisper")]);
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, CONTENT_TYPE.parse().unwrap());
        assert_eq!(
            model_of(&headers, &bytes, Surface::AudioTranscriptions)
                .await
                .ok()
                .flatten()
                .as_deref(),
            Some("whisper")
        );
        for surface in [
            Surface::Images,
            Surface::SystemOne,
            Surface::Messages,
            Surface::Responses,
            Surface::ChatCompletions,
        ] {
            assert!(model_of(&headers, &bytes, surface).await.is_err());
        }
        for content_type in [
            "application/json",
            "multipart/form-data",
            "multipart/form-data; boundary=\"\"",
        ] {
            headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
            assert!(model_of(&headers, &bytes, Surface::AudioTranscriptions)
                .await
                .is_err());
        }
        headers.insert(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={}", "x".repeat(71))
                .parse()
                .unwrap(),
        );
        assert!(model_of(&headers, &bytes, Surface::AudioTranscriptions)
            .await
            .is_err());
        headers.insert(header::CONTENT_TYPE, CONTENT_TYPE.parse().unwrap());
        headers.append(header::CONTENT_TYPE, CONTENT_TYPE.parse().unwrap());
        assert!(model_of(&headers, &bytes, Surface::AudioTranscriptions)
            .await
            .is_err());
    }
}
