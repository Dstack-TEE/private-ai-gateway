use super::super::wire::E2eeAadMode;
use super::*;

#[test]
fn generated_error_frames_use_canonical_framing_without_leading_blank_lines() {
    for protocol in [
        SseProtocol::OpenaiChat,
        SseProtocol::AnthropicMessages,
        SseProtocol::OpenaiResponses,
    ] {
        let bytes: Vec<_> = error_records(protocol, 502, Some("fixture"), None)
            .iter()
            .flat_map(|record| serialize(record).unwrap())
            .collect();
        assert!(!bytes.starts_with(b"\n"));
        assert!(bytes.ends_with(b"\n\n"));
        if protocol == SseProtocol::OpenaiChat {
            assert_eq!(bytes, b"data: {\"error\":{\"message\":\"The upstream provider returned an error\",\"type\":\"upstream_error\",\"code\":502,\"param\":null}}\n\ndata: [DONE]\n\n");
        } else {
            assert!(bytes.starts_with(b"event: error\ndata: "));
        }
    }
}

#[tokio::test]
async fn e2ee_only_passes_the_exact_done_marker_and_dispatches_complete_final_lines() {
    for (wire, failed, expected) in [
        ("data: [DONE]\n\n", false, "data: [DONE]\n\n"),
        ("data: [DONE] \n\n", true, ""),
        ("data:\ndata: [DONE]\n\n", true, ""),
        ("[DONE]\n\n", false, "data: [DONE]\n\n"),
        ("data: [DONE]\n", false, "data: [DONE]\n\n"),
    ] {
        let input = Box::pin(futures_util::stream::iter([Ok(Bytes::from(wire))]));
        let context = E2eeRequestContext {
            version: "2".into(),
            algo: crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM.into(),
            aad_mode: E2eeAadMode::AciV2,
            request_model: "fixture".into(),
            client_public_key_hex: "11".repeat(32),
            nonce: None,
            timestamp: None,
        };
        let mut owner = ResponseOwner::new(
            ResponseInput::Events(Box::pin(DecodedStream::new(
                input,
                Some(SseProtocol::OpenaiChat),
            ))),
            ResponseMode::Direct,
            Some(context),
            "/v1/chat/completions".into(),
            Some(SseProtocol::OpenaiChat),
            None,
        );
        let mut bytes = Vec::new();
        let mut errors = 0;
        while let Some(item) = owner.next().await {
            match item {
                Ok(chunk) => bytes.extend(chunk),
                Err(ServiceError::E2ee(_)) => errors += 1,
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(errors != 0, failed, "{wire:?}");
        assert_eq!(bytes, expected.as_bytes(), "{wire:?}");
    }
}

mod accounting;

#[test]
fn buffered_e2ee_preserves_upstream_decoding_failures() {
    let context = E2eeRequestContext {
        version: "2".into(),
        algo: crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM.into(),
        aad_mode: E2eeAadMode::AciV2,
        request_model: "fixture".into(),
        client_public_key_hex: "11".repeat(32),
        nonce: None,
        timestamp: None,
    };
    let error = super::super::e2ee_crypto::encrypt_e2ee_final_response(
        b"data: \xff\n\n",
        &context,
        "/v1/chat/completions",
        true,
    )
    .unwrap_err();
    assert!(matches!(error, ServiceError::Upstream(_)));
    let failure = StreamFailure::from_error(&error);
    assert_eq!(failure.status, 502);
    assert_eq!(failure.source, None);
    assert_eq!(failure.class, Some(ErrorClass::UpstreamTransport));
}
