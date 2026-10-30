//! Bounded collection for upstream HTTP responses and buffered streams.

use futures_util::StreamExt;

use super::{transport_error, UpstreamBodyStream, UpstreamError};

/// Default operator limit for buffered upstream responses, including decompressed
/// Chutes responses. Streaming responses are not collected under this limit.
pub const MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) async fn read_buffered_upstream_response(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, UpstreamError> {
    let content_length = response.content_length();
    let body = response
        .bytes_stream()
        .map(|chunk| chunk.map_err(transport_error));
    collect_buffered_upstream_body(Box::pin(body), content_length, limit).await
}

pub(crate) async fn collect_buffered_upstream_body(
    mut body: UpstreamBodyStream,
    content_length: Option<u64>,
    limit: usize,
) -> Result<Vec<u8>, UpstreamError> {
    let capacity = match content_length {
        Some(length) if length > limit as u64 => {
            return Err(UpstreamError::ResponseTooLarge);
        }
        Some(length) => (length as usize).min(1024 * 1024),
        None => 0,
    };
    let mut out = Vec::with_capacity(capacity);
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        if chunk.len() > limit - out.len() {
            return Err(UpstreamError::ResponseTooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::stream;

    use super::*;

    fn full_body() -> UpstreamBodyStream {
        let chunk = Bytes::from(vec![b'x'; 1024 * 1024]);
        Box::pin(stream::iter((0..64).map(move |_| Ok(chunk.clone()))))
    }

    #[tokio::test]
    async fn accepts_body_under_limit_with_and_without_content_length() {
        for content_length in [None, Some(5)] {
            let body = Box::pin(stream::iter([
                Ok(Bytes::from_static(b"he")),
                Ok(Bytes::from_static(b"llo")),
            ]));
            assert_eq!(
                collect_buffered_upstream_body(
                    body,
                    content_length,
                    MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES
                )
                .await
                .unwrap(),
                b"hello"
            );
        }
    }

    #[tokio::test]
    async fn accepts_body_exactly_at_limit() {
        let body = collect_buffered_upstream_body(
            full_body(),
            Some(MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES as u64),
            MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(body.len(), MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES);
        assert!(body.iter().all(|byte| *byte == b'x'));
    }

    #[tokio::test]
    async fn rejects_body_over_limit_and_stops_reading() {
        for content_length in [None, Some(1)] {
            let body = full_body()
                .chain(stream::once(async { Ok(Bytes::from_static(b"x")) }))
                .chain(stream::poll_fn(|_| panic!("read beyond the size limit")));
            assert!(matches!(
                collect_buffered_upstream_body(
                    Box::pin(body),
                    content_length,
                    MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES
                )
                .await,
                Err(UpstreamError::ResponseTooLarge)
            ));
        }
    }

    #[tokio::test]
    async fn rejects_oversized_content_length_before_reading() {
        let body = Box::pin(stream::poll_fn(|_| panic!("read an oversized response")));
        assert!(matches!(
            collect_buffered_upstream_body(
                body,
                Some(MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES as u64 + 1),
                MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES,
            )
            .await,
            Err(UpstreamError::ResponseTooLarge)
        ));
    }

    #[tokio::test]
    async fn preserves_stream_errors() {
        let body = Box::pin(stream::once(async {
            Err(UpstreamError::Timeout("read deadline".to_string()))
        }));
        assert!(matches!(
            collect_buffered_upstream_body(body, None, MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES).await,
            Err(UpstreamError::Timeout(_))
        ));
    }
}
