//! Identity, keyset digest, and report-data binding tests (ACI §4).

use private_ai_gateway::aci::canonical;
use private_ai_gateway::aci::identity;
use private_ai_gateway::aci::types::{
    KeyedPublicKey, KeysetEpoch, PublicKeyMaterial, TlsSpki, WorkloadIdentity, WorkloadKeyset,
};

fn sample_keyset(subject: Option<&str>, epoch_version: u64) -> WorkloadKeyset {
    WorkloadKeyset {
        workload_identity: WorkloadIdentity {
            public_key: PublicKeyMaterial {
                algo: "ed25519".to_string(),
                public_key_hex: "00".repeat(32),
            },
            subject: subject.map(|s| s.to_string()),
        },
        keyset_epoch: KeysetEpoch {
            version: epoch_version,
            not_after: 2_000_000_000,
        },
        receipt_signing_keys: vec![KeyedPublicKey {
            key_id: "rk-1".to_string(),
            algo: "ed25519".to_string(),
            public_key_hex: "11".repeat(32),
        }],
        e2ee_public_keys: vec![KeyedPublicKey {
            key_id: "ek-1".to_string(),
            algo: "x25519".to_string(),
            public_key_hex: "22".repeat(32),
        }],
        tls_public_keys: vec![TlsSpki {
            domain: None,
            spki_sha256_hex: "33".repeat(32),
        }],
    }
}

#[test]
fn workload_id_is_hash_of_public_key_only() {
    let ks = sample_keyset(None, 1);
    let expected_payload = canonical::canonicalize(&serde_json::json!({
        "algo": "ed25519",
        "public_key": "00".repeat(32),
    }))
    .unwrap();
    let expected = canonical::sha256_hex(&expected_payload);
    assert_eq!(
        identity::workload_id(&ks.workload_identity).unwrap(),
        expected
    );
}

#[test]
fn workload_id_ignores_subject() {
    let a = identity::workload_id(&sample_keyset(None, 1).workload_identity).unwrap();
    let b =
        identity::workload_id(&sample_keyset(Some("dstack://abc/app/xyz"), 1).workload_identity)
            .unwrap();
    assert_eq!(a, b);
}

#[test]
fn keyset_digest_changes_with_subject() {
    let a = identity::workload_keyset_digest(&sample_keyset(None, 1)).unwrap();
    let b =
        identity::workload_keyset_digest(&sample_keyset(Some("dstack://abc/app/xyz"), 1)).unwrap();
    assert_ne!(a, b);
}

#[test]
fn keyset_digest_changes_with_epoch_version() {
    let a = identity::workload_keyset_digest(&sample_keyset(None, 1)).unwrap();
    let b = identity::workload_keyset_digest(&sample_keyset(None, 2)).unwrap();
    assert_ne!(a, b);
}

#[test]
fn attestation_statement_shape() {
    let ks = sample_keyset(None, 1);
    let stmt = identity::attestation_statement(&ks, Some("abc".to_string())).unwrap();
    assert_eq!(
        stmt.workload_id,
        identity::workload_id(&ks.workload_identity).unwrap()
    );
    assert_eq!(
        stmt.workload_keyset_digest,
        identity::workload_keyset_digest(&ks).unwrap()
    );
    assert_eq!(stmt.nonce.as_deref(), Some("abc"));
}

#[test]
fn report_data_is_32_bytes_and_deterministic() {
    let ks = sample_keyset(None, 1);
    let stmt = identity::attestation_statement(&ks, Some("abc".to_string())).unwrap();
    let rd1 = identity::report_data(&stmt).unwrap();
    let rd2 = identity::report_data(&stmt).unwrap();
    assert_eq!(rd1.len(), 32);
    assert_eq!(rd1, rd2);
}

#[test]
fn report_data_distinguishes_null_from_string_null() {
    let ks = sample_keyset(None, 1);
    let rd_null =
        identity::report_data(&identity::attestation_statement(&ks, None).unwrap()).unwrap();
    let rd_str = identity::report_data(
        &identity::attestation_statement(&ks, Some("null".to_string())).unwrap(),
    )
    .unwrap();
    assert_ne!(rd_null, rd_str);
}

#[test]
fn keyset_endorsement_payload_is_named_json_object() {
    let ks = sample_keyset(None, 1);
    let payload = identity::keyset_endorsement_payload(&ks).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "purpose": "aci.keyset.endorsement.v1",
            "workload_keyset_digest": identity::workload_keyset_digest(&ks).unwrap(),
        })
    );
}
