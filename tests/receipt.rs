//! Receipt construction, event ordering, and signed-bytes tests (ACI §7)
//! against the shared test key provider.

mod common;

use private_ai_gateway::aci::keys::{verify_receipt_signature, KeyProvider};
use private_ai_gateway::aci::receipt::{
    receipt_signing_input, ChannelBinding, ReceiptBuilder, ReceiptError,
};

use common::StaticKeyProvider;

fn keys() -> StaticKeyProvider {
    StaticKeyProvider::default()
}

fn builder() -> ReceiptBuilder {
    ReceiptBuilder::new(
        "rcpt-test-1".to_string(),
        Some("chat-xyz".to_string()),
        Some("demo-model".to_string()),
        format!("sha256:{}", "deadbeef".repeat(8)),
        "/v1/chat/completions".to_string(),
        "POST".to_string(),
        1_700_000_500,
    )
}

#[test]
fn signed_receipt_verifies_under_keyset_receipt_key() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder();
    b.add_request_received(br#"{"model":"x","messages":[]}"#)
        .unwrap();
    b.add_request_forwarded(br#"{"model":"x","messages":[]}"#)
        .unwrap();
    b.add_response_returned(br#"{"id":"chat-xyz"}"#).unwrap();

    let receipt = b.finalize(&keys, &key_id).unwrap();
    // The signature covers JCS(document minus `signature`) (§7.2); Ed25519
    // signatures are a raw 64-byte RFC 8032 pair.
    let document = receipt.document_json().unwrap();
    assert_eq!(document["key_id"].as_str().unwrap(), key_id);
    let sig = hex::decode(&receipt.signature_hex).unwrap();
    assert_eq!(sig.len(), 64);
    assert!(verify_receipt_signature(
        &keys.receipt_keys()[0],
        &receipt_signing_input(&document).unwrap(),
        &sig
    ));
}

#[test]
fn event_order_is_array_order_and_first_is_request_received() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder();
    b.add_request_received(b"a").unwrap();
    b.add_request_forwarded(b"a").unwrap();
    b.add_upstream_verified_with_session(
        &common::verified_event("openai-compatible", "x"),
        &"cd".repeat(32),
    )
    .unwrap();
    b.add_response_returned(b"b").unwrap();
    let receipt = b.finalize(&keys, &key_id).unwrap();

    let payload = receipt.document_json().unwrap();
    let types: Vec<&str> = payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "request.received",
            "request.forwarded",
            "upstream.verified",
            "response.returned"
        ]
    );
    // Events carry no seq: the array order is the event order (§7.3).
    assert!(payload["event_log"][0].get("seq").is_none());
}

#[test]
fn channel_binding_shapes_serialize_to_spec_wire_form() {
    // The §8.2 binding shapes are flat, self-describing objects; they land in
    // session documents, so their serde form is part of the wire contract.
    let bindings = vec![
        ChannelBinding::TlsSpkiSha256 {
            origin: "https://upstream.example".to_string(),
            spki_sha256: "aa".repeat(32),
        },
        ChannelBinding::E2eePublicKeySha256 {
            provider: "chutes".to_string(),
            key_id: Some("instance-a".to_string()),
            algorithm: "chutes-ml-kem-768".to_string(),
            public_key_sha256: "cc".repeat(32),
        },
    ];
    let value = serde_json::to_value(&bindings).unwrap();
    assert_eq!(value[0]["type"], "tls_spki_sha256");
    assert_eq!(value[0]["spki_sha256"], "aa".repeat(32));
    assert_eq!(value[1]["type"], "e2ee_public_key_sha256");
    assert_eq!(value[1]["public_key_sha256"], "cc".repeat(32));
}

#[test]
fn finalize_requires_required_events() {
    let keys = keys();
    let key_id = keys.receipt_key_id().to_string();
    let mut b = builder();
    b.add_request_received(b"a").unwrap();
    let err = b.finalize(&keys, &key_id).unwrap_err();
    assert!(matches!(err, ReceiptError::MissingRequiredEvent(_)));
}

#[test]
fn unknown_receipt_key_id_is_rejected_at_finalize() {
    let keys = keys();
    let mut b = builder();
    b.add_request_received(b"a").unwrap();
    b.add_request_forwarded(b"a").unwrap();
    b.add_response_returned(b"b").unwrap();
    let err = b.finalize(&keys, "nonexistent").unwrap_err();
    assert!(matches!(err, ReceiptError::Key(_)));
}
