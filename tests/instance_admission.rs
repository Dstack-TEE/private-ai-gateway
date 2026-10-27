//! Admission ownership for independently selectable Chutes and 0G instances.
mod common;

use common::admission::{candidate, request, Attempt, Harness};
use futures_util::StreamExt;
use private_ai_gateway::aci::receipt::{ChannelBinding, SignedReceipt};
use private_ai_gateway::aggregator::service::{
    MiddlewareForwardResult, MiddlewareReceiptJournal, ServiceError, StreamingForwardResult,
};

fn configure_instances(h: &Harness, provider: &str) {
    let mut event = h.verifier.0.lock().unwrap();
    event.provider_type = Some(provider.to_string());
    event.channel_bindings = ["a", "b"]
        .into_iter()
        .map(|id| ChannelBinding::E2eePublicKeySha256 {
            provider: provider.to_string(),
            key_id: Some(id.to_string()),
            algorithm: if provider == "chutes" {
                "chutes-ml-kem-768"
            } else {
                "0g-pc-v1-hpke-x25519-hkdf-sha256-chacha20poly1305"
            }
            .to_string(),
            public_key_sha256: "aa".repeat(32),
        })
        .collect();
}

fn verified_event(receipt: &SignedReceipt) -> serde_json::Value {
    receipt.document_json().unwrap()["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "upstream.verified")
        .unwrap()
        .clone()
}

#[tokio::test]
async fn instances_keep_admitted_candidates_and_cite_the_serving_lease() {
    for provider in ["chutes", "0g-private-computer"] {
        for stream in [false, true] {
            for pinned in [false, true] {
                let h = Harness::new(vec![Attempt::Response {
                    status: 200,
                    at: 120,
                }]);
                configure_instances(&h, provider);
                *h.backend.instance.lock().unwrap() = Some("b".to_string());
                let listed = h.listed();
                assert_eq!(listed.len(), 2);
                let chosen = listed.iter().find(|s| matches!(
                    &s.document().channel_binding[0],
                    ChannelBinding::E2eePublicKeySha256 { key_id: Some(id), .. } if id == "b"
                )).unwrap();
                let pins = if pinned {
                    vec![chosen.session_id().to_string()]
                } else {
                    vec![]
                };
                h.clock.set(109);
                let receipt = if stream {
                    let result = h
                        .service
                        .forward_chat_completion_stream_request(request(pins))
                        .await
                        .unwrap();
                    let StreamingForwardResult::Stream(mut response) = result else {
                        panic!("expected stream")
                    };
                    // The selected lease survives session expiry while the response is pending.
                    assert!(h
                        .service
                        .get_attested_session(chosen.session_id())
                        .is_some());
                    while let Some(chunk) = response.body.next().await {
                        chunk.unwrap();
                    }
                    h.service
                        .get_receipt_by_receipt_id(&response.receipt_id)
                        .unwrap()
                } else {
                    h.service
                        .forward_chat_completion_request(request(pins))
                        .await
                        .unwrap()
                        .receipt
                };
                assert_eq!(
                    h.backend.events.lock().unwrap()[0].channel_bindings.len(),
                    if pinned { 1 } else { 2 }
                );
                assert_eq!(receipt.document_json().unwrap()["served_at"], 109);
                assert_eq!(verified_event(&receipt)["session_id"], chosen.session_id());
                let sibling = listed
                    .iter()
                    .find(|s| s.session_id() != chosen.session_id())
                    .unwrap();
                assert!(h
                    .service
                    .get_attested_session(sibling.session_id())
                    .is_none());
                assert!(h
                    .service
                    .get_attested_session(chosen.session_id())
                    .is_some());
            }
        }
    }
}

#[tokio::test]
async fn zero_g_missing_or_unadmitted_instance_fails_after_forwarding() {
    for served in [Some("unknown"), None] {
        for stream in [false, true] {
            let h = Harness::new(vec![Attempt::Response {
                status: 200,
                at: 120,
            }]);
            configure_instances(&h, "0g-private-computer");
            *h.backend.instance.lock().unwrap() = served.map(str::to_string);
            let listed = h.listed();
            h.clock.set(109);
            let err = if stream {
                match h
                    .service
                    .forward_chat_completion_stream_request(request(vec![]))
                    .await
                {
                    Err(err) => err,
                    Ok(_) => panic!("must reject missing/unadmitted instance"),
                }
            } else {
                h.service
                    .forward_chat_completion_request(request(vec![]))
                    .await
                    .unwrap_err()
            };
            assert!(matches!(err, ServiceError::AdmissionInvariant(_)));
            assert_eq!(h.backend.events.lock().unwrap().len(), 1);
            assert!(listed
                .iter()
                .all(|s| h.service.get_attested_session(s.session_id()).is_none()));
        }
    }
}

#[tokio::test]
async fn withheld_instance_errors_preserve_status_without_citing_an_arbitrary_session() {
    for provider in ["chutes", "0g-private-computer"] {
        for stream in [false, true] {
            for middleware in [false, true] {
                let h = Harness::new(vec![Attempt::Withheld {
                    status: 503,
                    at: 120,
                }]);
                configure_instances(&h, provider);
                let listed = h.listed();
                h.clock.set(109);
                if middleware {
                    let result = h
                        .service
                        .forward_chat_completion_for_middleware(
                            request(vec![]),
                            vec![candidate("instance")],
                            stream,
                            MiddlewareReceiptJournal::default(),
                        )
                        .await
                        .unwrap();
                    match result {
                        MiddlewareForwardResult::Forwarded(response) => {
                            assert_eq!(response.upstream_status, 503);
                            assert!(response.upstream_body.is_empty());
                        }
                        MiddlewareForwardResult::UpstreamError(response) => {
                            assert_eq!(response.error.upstream_status, 503);
                            assert!(response.error.upstream_body.is_empty());
                        }
                        _ => panic!("withheld status must remain an upstream answer"),
                    }
                } else if stream {
                    let result = h
                        .service
                        .forward_chat_completion_stream_request(request(vec![]))
                        .await
                        .unwrap();
                    let StreamingForwardResult::UpstreamError(error) = result else {
                        panic!("expected error status")
                    };
                    assert_eq!(error.upstream_status, 503);
                    assert!(error.upstream_body.is_empty());
                } else {
                    let response = h
                        .service
                        .forward_chat_completion_request(request(vec![]))
                        .await
                        .unwrap();
                    assert_eq!(response.upstream_status, 503);
                    let body: serde_json::Value =
                        serde_json::from_slice(&response.upstream_body).unwrap();
                    assert_eq!(body["error"]["type"], "service_unavailable");
                    let event = verified_event(&response.receipt);
                    assert_eq!(event["result"], "failed");
                    assert!(event["session_id"].is_null());
                    assert!(response.receipt.document_json().unwrap()["event_log"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|e| e["type"] == "request.forwarded"));
                }
                assert!(listed
                    .iter()
                    .all(|s| h.service.get_attested_session(s.session_id()).is_none()));
            }
        }
    }
}

#[test]
fn zero_g_preserves_instance_facts_and_requires_review_before_stable_reuse() {
    use private_ai_gateway::aggregator::session::EvidenceRef;
    let h = Harness::new(vec![]);
    configure_instances(&h, "0g-private-computer");
    {
        let mut event = h.verifier.0.lock().unwrap();
        event.verifier_id = "0g-private-computer/native-e2ee/v1".to_string();
        event.provider_claims = Some(serde_json::json!({
            "trust_boundary": "0g-provider-broker-tee",
            "instances": {
                "a": {"provider_address": "a", "tcb_status": "UpToDate"},
                "b": {"provider_address": "b", "tcb_status": "OutOfDate"}
            }
        }));
    }
    let before = h.listed();
    assert_eq!(before.len(), 2);
    for session in &before {
        let ChannelBinding::E2eePublicKeySha256 {
            key_id: Some(id), ..
        } = &session.document().channel_binding[0]
        else {
            panic!("expected instance binding")
        };
        assert_eq!(
            session.document().claims.extra["provider_address"],
            id.as_str()
        );
        assert_eq!(
            session.document().claims.extra["tcb_status"],
            if id == "a" { "UpToDate" } else { "OutOfDate" }
        );
    }
    let evidence = EvidenceRef::from_bytes("application/json", br#"{"quote":"new round"}"#);
    h.verifier.0.lock().unwrap().evidence = Some(serde_json::json!({
        "digest": evidence.digest, "data": evidence.data_uri
    }));
    let after = h.listed();
    assert!(
        after.iter().all(|new| before
            .iter()
            .all(|old| old.session_id() != new.session_id())),
        "0G has no reviewed stable-material profile; refreshed evidence must remain significant"
    );
}
