//! Verify the public API of the pinned library before migrating the pipeline.
use std::convert::Infallible;

use bytes::Bytes;
use futures_util::StreamExt;
use sse_stream::{Error, Sse, SseByteStream};

async fn decode(input: &[u8], split: usize) -> Vec<Result<Sse, Error>> {
    let chunks = [&input[..split], &input[split..]]
        .into_iter()
        .map(|bytes| Ok::<_, Infallible>(Bytes::copy_from_slice(bytes)));
    SseByteStream::new(futures_util::stream::iter(chunks))
        .collect()
        .await
}

#[tokio::test]
async fn public_decoder_preserves_empty_fields_and_whatwg_boundaries() {
    for ending in ["\n", "\r\n", "\r"] {
        let input = format!("\u{feff}: upstream comment{ending}{ending}event: old{ending}event{ending}id: first{ending}id: last{ending}id: ignored\0{ending}retry: 7{ending}retry: nope{ending}data:{ending}data: [DONE]{ending}{ending}event: metadata{ending}{ending}data{ending}{ending}[DONE]{ending}{ending}data: pending{ending}");
        for split in 0..=input.len() {
            let actual: Vec<_> = decode(input.as_bytes(), split)
                .await
                .into_iter()
                .map(Result::unwrap)
                .collect();
            assert_eq!(
                actual,
                vec![
                    Sse::default()
                        .event("")
                        .id("last")
                        .retry(7)
                        .data("\n[DONE]"),
                    Sse::default().event("metadata"),
                    Sse::default().data(""),
                ],
                "{ending:?} split {split}"
            );
        }
    }
}

#[tokio::test]
async fn public_decoder_rejects_invalid_utf8_instead_of_replacing_it() {
    // This is a documented library policy, differing from WHATWG UTF-8 decode.
    // It needs an explicit compatibility decision before the gateway uses it.
    for input in [b"data: \xff\n\n".as_slice(), b"event: \xff\ndata: {}\n\n"] {
        for split in 0..=input.len() {
            let mut records = decode(input, split).await.into_iter();
            assert!(matches!(records.next(), Some(Err(Error::Utf8Parse(_)))));
            assert!(records.next().is_none());
        }
    }
}

#[test]
fn public_encoder_canonicalizes_fields_without_touching_data_text() {
    let data = " {\"value\" : 1 }\n";
    let bytes = Sse::default()
        .retry(7)
        .id("last")
        .data(data)
        .event("")
        .encode()
        .unwrap();
    assert_eq!(
        bytes,
        b"event: \ndata:  {\"value\" : 1 }\ndata: \nid: last\nretry: 7\n\n".as_slice()
    );
    assert!(Sse::default().id("bad\0id").encode().is_err());
    assert!(Sse::default().event("bad\nevent").encode().is_err());
}
