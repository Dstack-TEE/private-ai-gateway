//! Test fixtures for the check engine: the `spec/test-vectors.md` fixture
//! family (fixed seeds, no randomness), built with the lib's own
//! constructions — a sealed keyset, a report bound to nonce `test-nonce`, a
//! §9 session document, and §8.2 receipt envelopes citing it. The unit tests
//! below pin the published constants so these fixtures cannot drift from the
//! doc; the full byte-exact pins live in `tests/spec_vectors.rs`.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey as Ed25519SigningKey};
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    x25519_public_key_hex, x25519_secret_key_from_bytes, E2EE_ALGO_X25519_AESGCM,
};
use private_ai_gateway::aci::identity::{attestation_statement, report_data, SealedWorkloadKeyset};
use private_ai_gateway::aci::keys::{KeyError, KeyProvider};
use private_ai_gateway::aci::receipt::{
    ChannelBinding, ReceiptBuilder, UpstreamVerifiedEvent, VerificationResult,
};
use private_ai_gateway::aci::types::{
    AttestationEnvelope, AttestationReport, KeyedPublicKey, ServiceCapabilities, SourceProvenance,
    TlsSpki, WorkloadKeyset,
};
use private_ai_gateway::aggregator::session::{
    AttestedSession, Claim, ClaimSource, EvidenceRef, SessionClaims, SessionDocument,
};
use serde_json::{json, Value};

pub const TEST_NONCE: &str = "test-nonce";
pub const KEYSET_NOT_AFTER: u64 = 1_800_000_000;
pub const SERVED_AT: u64 = 1_750_000_000;

pub const REQUEST_BODY: &[u8] =
    br#"{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}"#;
pub const RESPONSE_BODY: &[u8] = br#"{"choices":[],"id":"chatcmpl-123"}"#;

fn receipt_signing_key() -> Ed25519SigningKey {
    Ed25519SigningKey::from_bytes(&[0x02; 32])
}

/// The fixture keyset, sealed once (§4.1): its digest is over these exact
/// serialized bytes for every fixture that references it.
pub fn vector_sealed_keyset() -> SealedWorkloadKeyset {
    let e2ee = x25519_secret_key_from_bytes(&[0x03; 32]).expect("fixture x25519 seed");
    SealedWorkloadKeyset::seal(WorkloadKeyset {
        subject: Some("dstack-app://example-app".to_string()),
        not_after: KEYSET_NOT_AFTER,
        receipt_signing_keys: vec![KeyedPublicKey {
            key_id: "receipt-1".to_string(),
            algo: "ed25519".to_string(),
            public_key_hex: hex::encode(receipt_signing_key().verifying_key().as_bytes()),
        }],
        e2ee_public_keys: vec![KeyedPublicKey {
            key_id: "e2ee-1".to_string(),
            algo: E2EE_ALGO_X25519_AESGCM.to_string(),
            public_key_hex: x25519_public_key_hex(&e2ee),
        }],
        tls_public_keys: vec![TlsSpki {
            domain: Some("api.example.com".to_string()),
            spki_sha256_hex: "c0".repeat(32),
        }],
    })
    .expect("fixture keyset seals")
}

/// A self-consistent `aci/1` report over the fixture keyset, bound to
/// [`TEST_NONCE`]. It carries no hardware quote and no provenance — the
/// fail-closed checks are expected to say so.
pub fn vector_report() -> AttestationReport {
    let sealed = vector_sealed_keyset();
    let statement =
        attestation_statement(sealed.digest(), Some(TEST_NONCE)).expect("fixture nonce is valid");
    AttestationReport {
        api_version: "aci/1".to_string(),
        workload_keyset_digest: sealed.digest().to_string(),
        attestation: AttestationEnvelope {
            tee_type: "tdx".to_string(),
            workload_keyset_b64: BASE64.encode(sealed.bytes()),
            report_data_hex: hex::encode(report_data(&statement)),
            source_provenance: SourceProvenance::default(),
            evidence: json!({}),
        },
        service_capabilities: ServiceCapabilities {
            supported_e2ee_versions: vec!["3".to_string()],
        },
    }
}

/// The §9 session document, sealed once through the lib: the served bytes are
/// the artifact and the id is the hash over them.
pub fn vector_session() -> AttestedSession {
    let evidence = b"example-evidence";
    AttestedSession::seal(SessionDocument {
        api_version: "aci/1".to_string(),
        upstream_name: "demo-upstream".to_string(),
        endpoint: Some("https://upstream.example.com".to_string()),
        verifier_id: "example/1".to_string(),
        established_at: SERVED_AT,
        expires_at: SERVED_AT + 3_600,
        identity: None,
        channel_binding: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://upstream.example.com".to_string(),
            spki_sha256: "d1".repeat(32),
        }],
        claims: SessionClaims {
            tee_attested: Claim::asserted(ClaimSource::HardwareProven, "example quote verified"),
            extra: [
                ("gpu_arch".to_string(), "HOPPER".into()),
                ("tcb_status".to_string(), "UpToDate".into()),
            ]
            .into_iter()
            .collect(),
            ..SessionClaims::default()
        },
        evidence: EvidenceRef {
            digest: Some(sha256_hex(evidence)),
            data_uri: Some(format!(
                "data:text/plain;base64,{}",
                BASE64.encode(evidence)
            )),
        },
    })
    .expect("fixture session seals")
}

/// The exact served session bytes (§9.1).
pub fn vector_session_bytes() -> Vec<u8> {
    vector_session().bytes().to_vec()
}

/// `sha256:` content id over the exact served bytes (§9).
pub fn vector_session_id() -> String {
    vector_session().session_id().to_string()
}

/// Minimal provider over the fixture receipt key (test-only custody).
struct FixtureKeys;

impl KeyProvider for FixtureKeys {
    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vector_sealed_keyset().keyset().receipt_signing_keys.clone()
    }

    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id != "receipt-1" {
            return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
        }
        Ok(receipt_signing_key().sign(payload).to_bytes().to_vec())
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        Vec::new()
    }

    fn tls_spkis(&self) -> Vec<TlsSpki> {
        Vec::new()
    }

    fn is_test_only(&self) -> bool {
        true
    }
}

/// The §8.2 receipt envelope: payload built and serialized once by the lib's
/// own [`ReceiptBuilder`], Ed25519-signed over those exact bytes.
pub fn vector_receipt_envelope() -> Value {
    receipt_envelope_forwarding(REQUEST_BODY)
}

/// Like [`vector_receipt_envelope`] but recording a service-side rewrite:
/// `request.forwarded` hashes different bytes than `request.received` (§10.2
/// rewrite note).
pub fn vector_receipt_envelope_rewritten() -> Value {
    receipt_envelope_forwarding(b"rewritten-request-bytes")
}

fn receipt_envelope_forwarding(forwarded_body: &[u8]) -> Value {
    let mut builder = ReceiptBuilder::new(
        "rcpt-0001".to_string(),
        Some("chatcmpl-123".to_string()),
        Some("demo-model".to_string()),
        vector_sealed_keyset().digest().to_string(),
        "/v1/chat/completions".to_string(),
        "POST".to_string(),
        SERVED_AT,
    );
    builder
        .add_request_received(REQUEST_BODY)
        .expect("fixture event");
    builder
        .add_request_forwarded(forwarded_body)
        .expect("fixture event");
    builder
        .add_upstream_verified_with_session(
            &UpstreamVerifiedEvent {
                upstream_name: "demo-upstream".to_string(),
                model_id: "demo-model".to_string(),
                verifier_id: "example/1".to_string(),
                result: VerificationResult::Verified,
                required: true,
                ..Default::default()
            },
            &vector_session_id(),
        )
        .expect("fixture event");
    builder
        .add_response_returned(RESPONSE_BODY)
        .expect("fixture event");
    builder
        .finalize(&FixtureKeys, "receipt-1")
        .expect("fixture receipt finalizes")
        .envelope()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published `spec/test-vectors.md` constants: if a fixture stops
    /// reproducing them, the fixtures (or the lib) drifted from the doc.
    #[test]
    fn fixtures_reproduce_the_published_test_vector_constants() {
        assert_eq!(
            vector_sealed_keyset().digest(),
            "sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4"
        );
        assert_eq!(
            vector_report().attestation.report_data_hex,
            "8b899aae55437dec4d1d0d435920e112aca2a74d17595eeb601a7764d901ea07"
        );
        assert_eq!(
            vector_session_id(),
            "sha256:a595d269728e15fe8236af46586fe84f220696c0d7d4e647eed36922b7b20cb6"
        );

        let envelope = vector_receipt_envelope();
        let payload = BASE64
            .decode(envelope["payload_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            sha256_hex(&payload),
            "sha256:5a04d7ce350a09a9faa4f32e5a21790cd1080a46239039538bac98c798dc2dab"
        );
        assert_eq!(
            envelope["signature"],
            "b0b2c830be73d6b6ad9a90b75b9c347a930e6a918e6e4f70ad1c3ce0d3dbfe67\
             89504be5f7d317d24ba9eb84cd8bf634d58e898de89baa7fc939abd12e1b7400"
        );
    }
}
