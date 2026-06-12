//! Receipt construction, event ordering, and signing-bytes tests
//! (ACI §9).

mod common;

use private_ai_gateway::aci::canonical;
use private_ai_gateway::aci::keys::{verify_receipt_signature, KeyProvider};
use private_ai_gateway::aci::receipt::{
    canonical_bytes_for_signing, ChannelBinding, ReceiptBuilder, ReceiptError,
    TransparencyEventKind, UpstreamVerifiedEvent, VerificationResult,
    EVENT_TRANSPARENCY_REQUEST_MODIFIED,
};

use common::StaticKeyProvider;

fn keys() -> StaticKeyProvider {
    StaticKeyProvider::default()
}

fn builder(_keys: &StaticKeyProvider) -> ReceiptBuilder {
    ReceiptBuilder::new(
        "rcpt-test-1".to_string(),
        Some("chat-xyz".to_string()),
        format!(
            "sha256:{}",
            "deadbeef".repeat(8) // fake but valid-shaped digest
        ),
        format!("sha256:{}", "feedface".repeat(8)),
        "/v1/chat/completions".to_string(),
        "POST".to_string(),
        1_700_000_500,
    )
}

#[test]
fn signed_receipt_verifies_under_keyset_receipt_key() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder(&keys);
    b.add_request_received(br#"{"model":"x","messages":[]}"#)
        .unwrap();
    b.add_request_forwarded(br#"{"model":"x","messages":[]}"#)
        .unwrap();
    b.add_response_returned(br#"{"id":"chat-xyz"}"#, br#"{"id":"chat-xyz"}"#)
        .unwrap();

    let receipt = b.finalize(&keys, &key_id).unwrap();
    let canonical_bytes = canonical_bytes_for_signing(&receipt).unwrap();
    let sig = hex::decode(&receipt.signature.value_hex).unwrap();

    let receipt_keys = keys.receipt_keys();
    assert!(verify_receipt_signature(
        &receipt_keys[0],
        &canonical_bytes,
        &sig
    ));
}

#[test]
fn signed_receipt_canonical_bytes_omit_signature_value() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder(&keys);
    b.add_request_received(b"a").unwrap();
    b.add_request_forwarded(b"a").unwrap();
    b.add_response_returned(b"b", b"b").unwrap();
    let receipt = b.finalize(&keys, &key_id).unwrap();

    let with_value = canonical::canonicalize(&receipt.to_canonical_value(true)).unwrap();
    let without_value = canonical_bytes_for_signing(&receipt).unwrap();
    assert_ne!(with_value, without_value);
    assert!(!without_value.windows(8).any(|w| w == br#""value":"#));
    assert!(with_value.windows(8).any(|w| w == br#""value":"#));
}

#[test]
fn event_seqs_strictly_increasing_and_first_is_request_received() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder(&keys);
    b.add_request_received(b"a").unwrap();
    b.add_request_forwarded(b"a").unwrap();
    b.add_upstream_verified(UpstreamVerifiedEvent {
        upstream_name: "openai-compatible".to_string(),
        provider: None,
        model_id: "x".to_string(),
        url_origin: Some("http://upstream".to_string()),
        verifier_id: "verifier-stub-1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: None,
        channel_bindings: Vec::new(),
        provider_claims: None,
    })
    .unwrap();
    b.add_response_returned(b"b", b"b").unwrap();
    let receipt = b.finalize(&keys, &key_id).unwrap();

    let seqs: Vec<u64> = receipt.event_log.iter().map(|e| e.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(seqs, sorted);
    assert_eq!(receipt.event_log[0].event_type, "request.received");
}

#[test]
fn upstream_verified_event_records_channel_bindings() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder(&keys);
    b.add_request_received(b"a").unwrap();
    b.add_request_forwarded(b"a").unwrap();
    b.add_upstream_verified(UpstreamVerifiedEvent {
        upstream_name: "openai-compatible".to_string(),
        provider: None,
        model_id: "x".to_string(),
        url_origin: Some("https://upstream.example".to_string()),
        verifier_id: "external/verifier/v1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: None,
        channel_bindings: vec![
            ChannelBinding::TlsSpkiSha256 {
                origin: "https://upstream.example".to_string(),
                spki_sha256: "aa".repeat(32),
            },
            ChannelBinding::TlsCertificateSha256 {
                origin: "https://upstream.example".to_string(),
                certificate_sha256: "bb".repeat(32),
            },
            ChannelBinding::E2eePublicKeySha256 {
                provider: "chutes".to_string(),
                key_id: Some("instance-a".to_string()),
                algorithm: "chutes-ml-kem-768".to_string(),
                public_key_sha256: "cc".repeat(32),
            },
        ],
        provider_claims: Some(serde_json::json!({
            "trust_boundary": "fixture",
            "model_evidence_present": true,
        })),
    })
    .unwrap();
    b.add_response_returned(b"b", b"b").unwrap();
    let receipt = b.finalize(&keys, &key_id).unwrap();
    let upstream = receipt
        .event_log
        .iter()
        .find(|event| event.event_type == "upstream.verified")
        .unwrap();
    assert_eq!(
        upstream.fields["channel_bindings"][0]["type"],
        "tls_spki_sha256"
    );
    assert_eq!(
        upstream.fields["channel_bindings"][0]["spki_sha256"],
        "aa".repeat(32)
    );
    assert_eq!(
        upstream.fields["channel_bindings"][1]["type"],
        "tls_certificate_sha256"
    );
    assert_eq!(
        upstream.fields["channel_bindings"][1]["certificate_sha256"],
        "bb".repeat(32)
    );
    assert_eq!(
        upstream.fields["channel_bindings"][2]["type"],
        "e2ee_public_key_sha256"
    );
    assert_eq!(
        upstream.fields["channel_bindings"][2]["public_key_sha256"],
        "cc".repeat(32)
    );
    assert_eq!(
        upstream.fields["provider_claims"]["trust_boundary"],
        "fixture"
    );
}

#[test]
fn first_event_must_be_request_received() {
    let keys = keys();
    let mut b = builder(&keys);
    let err = b.add_request_forwarded(b"a").unwrap_err();
    matches!(err, ReceiptError::FirstEventMustBeRequestReceived(_));
}

#[test]
fn finalize_requires_required_events() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder(&keys);
    b.add_request_received(b"a").unwrap();
    let err = b.finalize(&keys, &key_id).unwrap_err();
    matches!(err, ReceiptError::MissingRequiredEvent(_));
}

#[test]
fn request_received_hash_matches_observed_bytes() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder(&keys);
    let body = br#"{"model":"x","messages":[{"role":"user","content":"hi"}]}"#;
    b.add_request_received(body).unwrap();
    b.add_request_forwarded(body).unwrap();
    b.add_response_returned(b"b", b"b").unwrap();
    let receipt = b.finalize(&keys, &key_id).unwrap();

    let received_event = receipt
        .event_log
        .iter()
        .find(|e| e.event_type == "request.received")
        .unwrap();
    let expected = canonical::sha256_hex(body);
    assert_eq!(
        received_event
            .fields
            .get("body_hash")
            .unwrap()
            .as_str()
            .unwrap(),
        expected
    );
}

#[test]
fn extension_event_cannot_collide_with_required_type() {
    let keys = keys();
    let mut b = builder(&keys);
    b.add_request_received(b"a").unwrap();
    let err = b
        .add_extension_event("request.received", serde_json::json!({ "x": 1 }))
        .unwrap_err();
    matches!(err, ReceiptError::ReservedEventType(_));
}

#[test]
fn transparency_event_names_operation_without_parameters() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder(&keys);
    b.add_request_received(b"a").unwrap();
    b.add_request_forwarded(b"b").unwrap();
    b.add_transparency_event(TransparencyEventKind::RequestModified)
        .unwrap();
    b.add_response_returned(b"c", b"c").unwrap();
    let receipt = b.finalize(&keys, &key_id).unwrap();

    let transparency = receipt
        .event_log
        .iter()
        .find(|e| e.event_type == EVENT_TRANSPARENCY_REQUEST_MODIFIED)
        .unwrap();
    assert_eq!(transparency.fields, serde_json::json!({}));
}

#[test]
fn unknown_receipt_key_id_is_rejected_at_finalize() {
    let keys = keys();
    let mut b = builder(&keys);
    b.add_request_received(b"a").unwrap();
    b.add_request_forwarded(b"a").unwrap();
    b.add_response_returned(b"b", b"b").unwrap();
    let err = b.finalize(&keys, "nonexistent").unwrap_err();
    matches!(err, ReceiptError::Key(_));
}
