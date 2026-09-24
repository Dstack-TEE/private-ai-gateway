//! Published ACI wire fixtures consumed by Private AI Proxy tests.
//!
//! Producer-side construction is intentionally outside this crate. These
//! constants keep PAP tests on the relying-party boundary: parse and verify
//! artifacts exactly as a remote ACI service would send them.

use crate::aci::types::{
    AttestationEnvelope, AttestationReport, ServiceCapabilities, SourceProvenance,
};
use serde_json::{json, Value};

const WIRE_FIXTURES: &str = include_str!("tests/fixtures/aci_wire_fixtures.json");

pub const TEST_NONCE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
pub const KEYSET_NOT_AFTER: u64 = 1_800_000_000;
pub const SERVED_AT: u64 = 1_750_000_000;

pub const REQUEST_BODY: &[u8] =
    br#"{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}"#;
pub const RESPONSE_BODY: &[u8] = br#"{"choices":[],"id":"chatcmpl-123"}"#;

pub fn vector_report() -> AttestationReport {
    AttestationReport {
        api_version: "aci/1".to_string(),
        workload_keyset_digest:
            "sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371".to_string(),
        attestation: AttestationEnvelope {
            tee_type: "tdx".to_string(),
            workload_keyset: json!({
                "subject": "dstack-app://example-app",
                "not_after": KEYSET_NOT_AFTER,
                "receipt_signing_keys": [{
                    "key_id": "receipt-1",
                    "algo": "ed25519",
                    "public_key": "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394",
                }],
                "e2ee_public_keys": [{
                    "key_id": "e2ee-1",
                    "algo": "x25519-aes-256-gcm-hkdf-sha256",
                    "public_key": "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22",
                }],
                "tls_public_keys": [{
                    "domain": "api.example.com",
                    "spki_sha256": "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0",
                }],
            }),
            report_data_hex: "df2174d28130852b413646a3786927b93e94c11d770268b65def8bdba45cb49e"
                .to_string(),
            source_provenance: SourceProvenance::default(),
            evidence: json!({}),
        },
        service_capabilities: ServiceCapabilities {
            supported_e2ee_versions: vec!["2".to_string()],
            serving: "aggregator".to_string(),
        },
    }
}

pub fn vector_session_bytes() -> Vec<u8> {
    crate::aci::digest::jcs_bytes(&wire_fixture("session"))
        .expect("published session fixture canonicalizes")
}

pub fn vector_receipt_envelope() -> Value {
    wire_fixture("receipt")
}

pub fn vector_receipt_envelope_rewritten() -> Value {
    wire_fixture("rewritten_receipt")
}

fn wire_fixture(name: &str) -> Value {
    serde_json::from_str::<Value>(WIRE_FIXTURES).expect("published ACI wire fixtures parse")[name]
        .clone()
}
