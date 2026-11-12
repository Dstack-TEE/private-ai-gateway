use super::*;
use crate::aci::upstream::MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES;

#[tokio::test]
async fn incomplete_envelope_is_an_upstream_transport_failure() {
    use futures_util::StreamExt;
    let (key, _) = MlKem768::generate_keypair();
    let input = Box::pin(futures_util::stream::iter([Ok(Bytes::from_static(
        b"data: {",
    ))]));
    let mut stream = ChutesE2eeDecryptingStream::new(input, key);
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(matches!(error, UpstreamError::EnvelopeTruncated));
    let failure = crate::middleware::sse::StreamFailure::from_error(
        &crate::aggregator::service::ServiceError::Upstream(error),
    );
    assert_eq!(failure.status, 502);
    assert_eq!(failure.source, None);
    assert_eq!(
        failure.class,
        Some(crate::middleware::types::ErrorClass::UpstreamTransport)
    );
    assert!(stream.next().await.is_none());
}

#[test]
fn gzip_round_trip_under_limit() {
    let plaintext = b"buffered Chutes response";
    let compressed = gzip_compress(plaintext).unwrap();
    assert_eq!(
        gzip_decompress(&compressed, MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES).unwrap(),
        plaintext
    );
}

#[test]
fn gzip_accepts_exact_limit_and_rejects_oversized_output() {
    for length in [
        MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES,
        MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES + 1,
    ] {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        std::io::copy(&mut std::io::repeat(b'x').take(length as u64), &mut encoder).unwrap();
        let compressed = encoder.finish().unwrap();
        let result = gzip_decompress(&compressed, MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES);
        if length == MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES {
            let plaintext = result.unwrap();
            assert_eq!(plaintext.len(), length);
            assert!(plaintext.iter().all(|byte| *byte == b'x'));
        } else {
            assert!(matches!(result, Err(UpstreamError::ResponseTooLarge)));
        }
    }
}

#[test]
fn gzip_rejects_invalid_compressed_data() {
    assert!(matches!(
        gzip_decompress(b"not gzip", MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES),
        Err(UpstreamError::Transport(_))
    ));
}

pub(crate) async fn characterization_source() -> String {
    use futures_util::StreamExt;
    let (sk, pk) = MlKem768::generate_keypair();
    let (ct, shared) = pk.encapsulate();
    let key = chutes_derive_key(shared.as_slice(), ct.as_slice(), CHUTES_INFO_STREAM).unwrap();
    let plaintext = br#"data: {"id":"chutes-fixture","model":"fixture-model","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"stop"}]}"#;
    let nonce = [0x31; 12];
    let mut sealed = nonce.to_vec();
    sealed.extend(chacha_encrypt(&key, &nonce, plaintext).unwrap());
    let envelope = format!("data: {}\n\ndata: {}\n\ndata: {{\"usage\":{{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}}}\n\ndata: [DONE]\n\n", json!({"e2e_init":BASE64.encode(ct.as_slice())}), json!({"e2e":BASE64.encode(sealed)}));
    let chunks: Vec<_> = envelope
        .as_bytes()
        .chunks(7)
        .map(|b| Ok(Bytes::copy_from_slice(b)))
        .collect();
    let mut stream =
        ChutesE2eeDecryptingStream::new(Box::pin(futures_util::stream::iter(chunks)), sk);
    let mut result = Vec::new();
    while let Some(chunk) = stream.next().await {
        result.extend_from_slice(&chunk.unwrap());
    }
    String::from_utf8(result).unwrap()
}
