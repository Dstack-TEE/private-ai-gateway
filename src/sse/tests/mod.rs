mod library_contract;

use super::*;
#[tokio::test]
async fn envelope_truncation_does_not_mask_read_timeouts() {
    let source = futures_util::stream::iter([
        Ok(Bytes::from_static(b"data: {")),
        Err(UpstreamError::Timeout("read timed out".into())),
    ]);
    let mut stream = decode_envelope(Box::pin(source), MAX_EVENT_BYTES);
    assert!(matches!(
        stream.next().await,
        Some(Err(UpstreamError::Timeout(_)))
    ));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn normalization_and_pending_eof_are_independent_of_chunk_splits() {
    for ending in ["\n", "\r\n", "\r"] {
        let input = format!("\u{feff}: dropped{ending}{ending}retry: 07{ending}id: old{ending}id: last{ending}extension: ignored{ending}event: old{ending}event{ending}data: {{ \"x\" : 1 }}{ending}{ending}[DONE]{ending}{ending}data: pending{ending}");
        for split in 0..=input.len() {
            let chunks = [
                Ok(Bytes::copy_from_slice(&input.as_bytes()[..split])),
                Ok(Bytes::copy_from_slice(&input.as_bytes()[split..])),
            ];
            let records: Vec<_> = DecodedStream::new(
                Box::pin(futures_util::stream::iter(chunks)),
                Some(SseProtocol::OpenaiChat),
            )
            .collect()
            .await;
            let records: Vec<_> = records.into_iter().map(Result::unwrap).collect();
            assert_eq!(records.len(), 2, "{ending:?} {split}");
            assert_eq!(
                serialize(&records[0]).unwrap(),
                b"event: \ndata: { \"x\" : 1 }\nid: last\nretry: 7\n\n".as_slice()
            );
            assert!(!matches!(records[1], Record::Residual));
            assert_eq!(
                serialize(&records[1]).unwrap(),
                b"data: pending\n\n".as_slice()
            );
        }
    }
}
#[tokio::test]
async fn clean_eof_completes_only_terminated_field_lines_at_every_split() {
    for ending in ["\n", "\r\n", "\r", ""] {
        for (body, protocol) in [
            ("data: [DONE]".to_string(), SseProtocol::OpenaiChat),
            (
                format!(
                    "event: message_stop{}data: {{\"type\":\"message_stop\"}}",
                    if ending.is_empty() { "\n" } else { ending }
                ),
                SseProtocol::AnthropicMessages,
            ),
        ] {
            for blank in [false, true] {
                let input = format!("{body}{ending}{}", if blank { ending } else { "" });
                for split in 0..=input.len() {
                    let source = futures_util::stream::iter([
                        Ok(Bytes::copy_from_slice(&input.as_bytes()[..split])),
                        Ok(Bytes::copy_from_slice(&input.as_bytes()[split..])),
                    ]);
                    let records: Vec<_> = DecodedStream::new(Box::pin(source), Some(protocol))
                        .collect::<Vec<_>>()
                        .await
                        .into_iter()
                        .map(Result::unwrap)
                        .collect();
                    assert_eq!(records.len(), 1, "{input:?} split {split}");
                    assert_eq!(
                        matches!(records[0], Record::Residual),
                        ending.is_empty(),
                        "{input:?} split {split}"
                    );
                    if !ending.is_empty() {
                        assert!(
                            records[0].event().unwrap().meaning.terminal.is_some(),
                            "{input:?} split {split}"
                        );
                    }
                }
            }
        }
    }
}
#[tokio::test]
async fn bom_and_complete_comment_lines_leave_a_clean_eof() {
    for input in [
        "\u{feff}",
        "\u{feff}\n",
        ": upstream comment\n",
        "\u{feff}: comment\r\n",
    ] {
        for split in 0..=input.len() {
            let chunks = [
                Ok(Bytes::copy_from_slice(&input.as_bytes()[..split])),
                Ok(Bytes::copy_from_slice(&input.as_bytes()[split..])),
            ];
            let records: Vec<_> =
                DecodedStream::new(Box::pin(futures_util::stream::iter(chunks)), None)
                    .collect()
                    .await;
            assert!(records.is_empty(), "{input:?} split {split}: {records:?}");
        }
    }
}
#[tokio::test]
async fn cap_counts_a_block_instead_of_each_line_or_input_chunk() {
    let records: Vec<_> = DecodedStream::with_limit(
        Box::pin(futures_util::stream::iter([Ok(Bytes::from(
            "data: {}\r\n\r\n".repeat(1000),
        ))])),
        None,
        32,
    )
    .collect()
    .await;
    assert_eq!(records.len(), 1000);
    assert!(records.iter().all(Result::is_ok));
    let records: Vec<_> = DecodedStream::with_limit(
        Box::pin(futures_util::stream::iter([Ok(Bytes::from(
            "data: x\n".repeat(20),
        ))])),
        None,
        32,
    )
    .collect()
    .await;
    assert!(records.last().unwrap().is_err());
}
#[test]
fn contradictory_controls_retain_classified_failure_in_a_decoded_record() {
    let result = decode_buffered(
        Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"error\"}\n\n"),
        Some(SseProtocol::AnthropicMessages),
    );
    let records = result.unwrap();
    assert_eq!(
        records[0].event().unwrap().meaning.failure,
        Some(crate::sse_protocol::FailureEvidence::ConflictingType)
    );
}
#[tokio::test]
async fn invalid_utf8_is_an_upstream_failure() {
    let records: Vec<_> = DecodedStream::new(
        Box::pin(futures_util::stream::iter([Ok(Bytes::from_static(
            b"data: \xff\n\n",
        ))])),
        None,
    )
    .collect()
    .await;
    assert_eq!(records.len(), 1);
    assert!(matches!(
        &records[0],
        Err(ServiceError::Upstream(UpstreamError::Transport(_)))
    ));
}
#[tokio::test]
async fn cap_is_per_event_and_error_preserves_timeout_classification() {
    let chunk = Bytes::from_static(b"data: {}\n\n");
    let records: Vec<_> = DecodedStream::new(
        Box::pin(futures_util::stream::iter(
            (0..1000).map(move |_| Ok(chunk.clone())),
        )),
        None,
    )
    .collect()
    .await;
    assert_eq!(records.len(), 1000);
    let records: Vec<_> = DecodedStream::new(
        Box::pin(futures_util::stream::iter([Ok(Bytes::from(vec![
            b'x';
            MAX_EVENT_BYTES
                + 1
        ]))])),
        None,
    )
    .collect()
    .await;
    assert!(matches!(
        records.last().unwrap(),
        Err(ServiceError::Upstream(UpstreamError::SseLineOverflow))
    ));
    let records: Vec<_> = DecodedStream::new(
        Box::pin(futures_util::stream::iter([Err(ServiceError::Upstream(
            UpstreamError::Timeout("read".into()),
        ))])),
        None,
    )
    .collect()
    .await;
    assert!(matches!(
        &records[0],
        Err(ServiceError::Upstream(UpstreamError::Timeout(_)))
    ));
}
