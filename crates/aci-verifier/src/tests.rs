//! Unit tests for §9.1 steps below the appraisal: channel selection, dstack
//! custody and compose checks, and the policy's identity anchor.

use aci_protocol::receipt::ChannelBinding;
use aci_protocol::types::{
    AttestationEnvelope, AttestationReport, KeyedPublicKey, SourceProvenance, TlsSpki,
    WorkloadKeyset,
};
use k256::ecdsa::SigningKey;
use serde_json::{json, Value};
use sha2::{Sha256, Sha384};
use sha3::{Digest, Keccak256};

use crate::appraisal::appraise_provenance;
use crate::channel::declared_tls_channel_bindings;
use crate::dstack::{verify_dstack_app_compose, verify_dstack_kms_receipt_custody};
use crate::*;

fn signing_key(byte: u8) -> SigningKey {
    SigningKey::from_slice(&[byte; 32]).unwrap()
}

fn public_key_uncompressed_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_encoded_point(false).as_bytes())
}

fn public_key_compressed_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_sec1_bytes())
}

fn sign_recoverable(key: &SigningKey, message: &[u8]) -> String {
    let digest = Keccak256::new_with_prefix(message);
    let (signature, recid) = key.sign_digest_recoverable(digest).unwrap();
    let mut out = signature.to_vec();
    out.push(recid.to_byte());
    hex::encode(out)
}

fn keyset_with_tls(tls_public_keys: Vec<TlsSpki>) -> WorkloadKeyset {
    WorkloadKeyset {
        subject: None,
        not_after: u64::MAX,
        receipt_signing_keys: Vec::new(),
        e2ee_public_keys: Vec::new(),
        tls_public_keys,
    }
}

/// A keyset + custody evidence for the receipt key derived from the shared
/// 32-byte KMS scalar in `receipt_scalar`: the keyset lists its Ed25519
/// public key, and the evidence publishes the k256 counterpart the KMS chain
/// covers.
fn receipt_custody_fixture(
    receipt_scalar: [u8; 32],
    signature_chain: Vec<String>,
) -> (WorkloadKeyset, Value) {
    let ed25519 = ed25519_dalek::SigningKey::from_bytes(&receipt_scalar);
    let ed25519_public = hex::encode(ed25519.verifying_key().as_bytes());
    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let keyset = WorkloadKeyset {
        subject: Some("test-subject".to_string()),
        not_after: u64::MAX,
        receipt_signing_keys: vec![KeyedPublicKey {
            key_id: "receipt-1".to_string(),
            algo: "ed25519".to_string(),
            public_key_hex: ed25519_public.clone(),
        }],
        e2ee_public_keys: Vec::new(),
        tls_public_keys: Vec::new(),
    };
    let evidence = json!({
        "key_custody": {
            "provider": "dstack-kms",
            "keys": [{
                "role": "receipt",
                "path": "aci/receipt-ed25519/v1",
                "purpose": "aci.receipt.ed25519.v1",
                "algo": "ed25519",
                "public_key": ed25519_public,
                "kms_public_key": kms_public,
                "signature_chain": signature_chain,
            }]
        }
    });
    (keyset, evidence)
}

#[test]
fn declared_tls_channel_bindings_preserves_service_wide_pins() {
    let keyset = keyset_with_tls(vec![
        TlsSpki {
            domain: None,
            spki_sha256_hex: "AA".repeat(32),
        },
        TlsSpki {
            domain: None,
            spki_sha256_hex: "bb".repeat(32),
        },
    ]);

    let bindings =
        declared_tls_channel_bindings(&keyset, &json!({}), "https://gateway.example").unwrap();

    assert_eq!(
        bindings,
        vec![
            ChannelBinding::TlsSpkiSha256 {
                origin: "https://gateway.example".to_string(),
                spki_sha256: "aa".repeat(32),
            },
            ChannelBinding::TlsSpkiSha256 {
                origin: "https://gateway.example".to_string(),
                spki_sha256: "bb".repeat(32),
            },
        ]
    );
}

#[test]
fn declared_tls_channel_bindings_selects_domain_binding_for_origin_host() {
    let keyset = keyset_with_tls(vec![
        TlsSpki {
            domain: Some("api.example.com".to_string()),
            spki_sha256_hex: "AA".repeat(32),
        },
        TlsSpki {
            domain: Some("chat.example.com".to_string()),
            spki_sha256_hex: "bb".repeat(32),
        },
    ]);
    let evidence = json!({
        "downstream_tls_binding": {
            "domain": "API.EXAMPLE.COM.",
            "spki_sha256": "AA".repeat(32),
        }
    });

    let bindings =
        declared_tls_channel_bindings(&keyset, &evidence, "https://api.example.com").unwrap();

    assert_eq!(
        bindings,
        vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://api.example.com".to_string(),
            spki_sha256: "aa".repeat(32),
        }]
    );
}

#[test]
fn declared_tls_channel_bindings_rejects_domain_keyset_without_selected_binding() {
    let keyset = keyset_with_tls(vec![TlsSpki {
        domain: Some("api.example.com".to_string()),
        spki_sha256_hex: "aa".repeat(32),
    }]);

    let err =
        declared_tls_channel_bindings(&keyset, &json!({}), "https://api.example.com").unwrap_err();

    assert!(err
        .to_string()
        .contains("did not select a downstream TLS binding"));
}

#[test]
fn declared_tls_channel_bindings_rejects_selected_binding_for_other_host() {
    let keyset = keyset_with_tls(vec![
        TlsSpki {
            domain: Some("api.example.com".to_string()),
            spki_sha256_hex: "aa".repeat(32),
        },
        TlsSpki {
            domain: Some("chat.example.com".to_string()),
            spki_sha256_hex: "bb".repeat(32),
        },
    ]);
    let evidence = json!({
        "downstream_tls_binding": {
            "domain": "chat.example.com",
            "spki_sha256": "bb".repeat(32),
        }
    });

    let err =
        declared_tls_channel_bindings(&keyset, &evidence, "https://api.example.com").unwrap_err();

    assert!(err.to_string().contains("does not match upstream host"));
}

#[test]
fn declared_tls_channel_bindings_rejects_selected_binding_outside_keyset() {
    let keyset = keyset_with_tls(vec![TlsSpki {
        domain: Some("api.example.com".to_string()),
        spki_sha256_hex: "aa".repeat(32),
    }]);
    let evidence = json!({
        "downstream_tls_binding": {
            "domain": "api.example.com",
            "spki_sha256": "bb".repeat(32),
        }
    });

    let err =
        declared_tls_channel_bindings(&keyset, &evidence, "https://api.example.com").unwrap_err();

    assert!(err
        .to_string()
        .contains("not present in the attested keyset"));
}

#[test]
fn verifies_dstack_kms_receipt_key_custody_chain() {
    let root = signing_key(1);
    let app = signing_key(2);
    let receipt_scalar = [3u8; 32];
    let app_id = [0xab; 20];

    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let purpose_message = format!("aci.receipt.ed25519.v1:{kms_public}");
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let (keyset, evidence) =
        receipt_custody_fixture(receipt_scalar, vec![purpose_signature, app_signature]);
    let policy = AciServiceVerifierPolicy::new(
        vec!["test-subject".to_string()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&root)],
    )
    .unwrap();

    verify_dstack_kms_receipt_custody(&evidence, &keyset, &app_id, &policy).unwrap();
}

#[test]
fn rejects_dstack_kms_receipt_key_custody_under_unaccepted_root() {
    let root = signing_key(1);
    let other_root = signing_key(4);
    let app = signing_key(2);
    let receipt_scalar = [3u8; 32];
    let app_id = [0xab; 20];

    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let purpose_message = format!("aci.receipt.ed25519.v1:{kms_public}");
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let (keyset, evidence) =
        receipt_custody_fixture(receipt_scalar, vec![purpose_signature, app_signature]);
    let policy = AciServiceVerifierPolicy::new(
        vec!["test-subject".to_string()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&other_root)],
    )
    .unwrap();

    let err = verify_dstack_kms_receipt_custody(&evidence, &keyset, &app_id, &policy)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "dstack KMS root public key is not accepted by verifier policy"
    );
}

#[test]
fn verifies_dstack_app_compose_preimage_against_measured_hash() {
    let app_compose = r#"{"manifest_version":"2","name":"gateway"}"#;
    let compose_hash: [u8; 32] = sha2::Sha256::digest(app_compose.as_bytes()).into();
    let evidence = json!({ "app_compose": app_compose });

    verify_dstack_app_compose(&evidence, &compose_hash).unwrap();
}

#[test]
fn rejects_dstack_app_compose_that_is_not_the_measured_preimage() {
    let measured_app_compose = r#"{"manifest_version":"2","name":"gateway"}"#;
    let compose_hash: [u8; 32] = sha2::Sha256::digest(measured_app_compose.as_bytes()).into();
    let evidence = json!({
        "app_compose": r#"{"manifest_version":"2","name":"other"}"#,
    });

    let err = verify_dstack_app_compose(&evidence, &compose_hash)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "dstack app_compose preimage does not match the RTMR3-bound compose hash"
    );
}

#[test]
fn rejects_receipt_custody_whose_key_is_not_in_the_keyset() {
    let root = signing_key(1);
    let app = signing_key(2);
    let receipt_scalar = [3u8; 32];
    let app_id = [0xab; 20];

    let kms_public = public_key_compressed_hex(&SigningKey::from_slice(&receipt_scalar).unwrap());
    let purpose_message = format!("aci.receipt.ed25519.v1:{kms_public}");
    let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id.as_slice(),
        &app.verifying_key().to_sec1_bytes(),
    ]
    .concat();
    let app_signature = sign_recoverable(&root, &root_message);
    let (mut keyset, evidence) =
        receipt_custody_fixture(receipt_scalar, vec![purpose_signature, app_signature]);
    // The attested keyset lists a different receipt key than the custody entry.
    keyset.receipt_signing_keys[0].public_key_hex = "ff".repeat(32);
    let policy = AciServiceVerifierPolicy::new(
        vec!["test-subject".to_string()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&root)],
    )
    .unwrap();

    let err = verify_dstack_kms_receipt_custody(&evidence, &keyset, &app_id, &policy)
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the attested keyset"));
}

#[test]
fn subject_anchor_requires_the_measured_app_id() {
    let measured = [0xab; 20];
    let measured_subject = format!("app-id:0x{}", hex::encode(measured));
    let policy = AciServiceVerifierPolicy::new(
        vec![measured_subject.clone(), "app-id:0xother".to_string()],
        Vec::new(),
        vec![public_key_uncompressed_hex(&signing_key(1))],
    )
    .unwrap();
    let provenance = SourceProvenance::default();

    let mut keyset = keyset_with_tls(Vec::new());
    keyset.subject = Some(measured_subject);
    assert!(policy.accepts_measured(&keyset, &provenance, &measured));

    // Accepted by the allowlist, but not what the event log measured: the
    // subject is the workload's own claim and must not anchor on its own.
    keyset.subject = Some("app-id:0xother".to_string());
    assert!(!policy.accepts_measured(&keyset, &provenance, &measured));

    // No declared subject: the measured app-id is the anchor, and it is
    // allowlisted. Requiring the workload to also name itself would rest the
    // decision on its own claim.
    keyset.subject = None;
    assert!(policy.accepts_measured(&keyset, &provenance, &measured));

    // A measurement the operator never allowlisted is rejected either way.
    let unlisted = [0xcd; 20];
    assert!(!policy.accepts_measured(&keyset, &provenance, &unlisted));
    keyset.subject = Some(format!("app-id:0x{}", hex::encode(unlisted)));
    assert!(!policy.accepts_measured(&keyset, &provenance, &unlisted));
}

// ---- §9.1(4) compose measurement (moved with the logic from the CLI) ----

fn td10_report(rt_mr3: [u8; 48]) -> dcap_qvl::quote::Report {
    dcap_qvl::quote::Report::TD10(dcap_qvl::quote::TDReport10 {
        tee_tcb_svn: [0; 16],
        mr_seam: [0; 48],
        mr_signer_seam: [0; 48],
        seam_attributes: [0; 8],
        td_attributes: [0; 8],
        xfam: [0; 8],
        mr_td: [0; 48],
        mr_config_id: [0; 48],
        mr_owner: [0; 48],
        mr_owner_config: [0; 48],
        rt_mr0: [0; 48],
        rt_mr1: [0; 48],
        rt_mr2: [0; 48],
        rt_mr3,
        report_data: [0; 64],
    })
}

fn provenance_report(evidence: Value, provenance: SourceProvenance) -> AttestationReport {
    AttestationReport {
        api_version: "aci/1".to_string(),
        workload_keyset_digest: String::new(),
        attestation: AttestationEnvelope {
            tee_type: "tdx".to_string(),
            workload_keyset: Value::Null,
            report_data_hex: String::new(),
            source_provenance: provenance,
            evidence,
        },
        service_capabilities: Default::default(),
    }
}

fn provenance_inputs<'a>(
    report: &'a AttestationReport,
    accepted_composes: &'a [String],
) -> AppraisalInputs<'a> {
    AppraisalInputs {
        report,
        nonce: None,
        now_secs: 0,
        expiry_waived: false,
        quote: QuoteSource::Online {
            pccs_url: "https://pccs.invalid",
        },
        accepted_composes,
        custody: CustodyEvidence::NotConfigured {
            reason: "not under test",
        },
        channel: ChannelEvidence::Unobservable {
            reason: "not under test",
        },
        explain: false,
    }
}

#[tokio::test]
async fn compose_measurement_passes_and_an_allowlist_pins_the_measured_value() {
    // Two RTMR3 boot events, measured the way dstack does: a runtime event's
    // digest is SHA-384 over `event_type:event:payload`, and RTMR3 is the
    // SHA-384 chain over those digests from a 48-byte-zero start.
    let app_compose = "services:\n  gateway:\n    image: demo\n";
    let compose_hash = hex::encode(Sha256::digest(app_compose.as_bytes()));
    const RUNTIME_EVENT: u32 = 0x0800_0001;
    let event_digest = |event: &str, payload_hex: &str| -> Vec<u8> {
        let mut hasher = Sha384::new();
        hasher.update(RUNTIME_EVENT.to_ne_bytes());
        hasher.update(b":");
        hasher.update(event.as_bytes());
        hasher.update(b":");
        hasher.update(hex::decode(payload_hex).unwrap());
        hasher.finalize().to_vec()
    };
    let digests = [
        event_digest("compose-hash", &compose_hash),
        event_digest("system-ready", ""),
    ];
    let mut mr = vec![0u8; 48];
    for d in &digests {
        mr.extend_from_slice(d);
        mr = Sha384::digest(&mr).to_vec();
    }
    let quote_report = td10_report(mr.as_slice().try_into().unwrap());
    let mut evidence = json!({
        "event_log": serde_json::to_string(&json!([
            { "imr": 3, "event_type": RUNTIME_EVENT, "digest": hex::encode(&digests[0]),
              "event": "compose-hash", "event_payload": compose_hash },
            { "imr": 3, "event_type": RUNTIME_EVENT, "digest": hex::encode(&digests[1]),
              "event": "system-ready", "event_payload": "" },
        ]))
        .unwrap(),
        "app_compose": app_compose,
    });
    let provenance = SourceProvenance {
        repo_url: Some("https://example.com/repo".to_string()),
        repo_commit: Some("abc123".to_string()),
        image_digest: None,
        image_provenance: None,
    };

    let report = provenance_report(evidence.clone(), provenance.clone());
    let (result, app_id) =
        appraise_provenance(&provenance_inputs(&report, &[]), Some(&quote_report)).await;
    assert!(result.passed(), "{}", result.detail);
    assert!(app_id.is_none(), "this log carries no app-id event");

    // An allowlist pins the measured value: the real hash passes, any other
    // list fails even though the measurement itself verified.
    let accepted = vec![compose_hash.clone()];
    let (result, _) =
        appraise_provenance(&provenance_inputs(&report, &accepted), Some(&quote_report)).await;
    assert!(result.passed(), "{}", result.detail);

    let rejected = vec!["00".repeat(32)];
    let (result, _) =
        appraise_provenance(&provenance_inputs(&report, &rejected), Some(&quote_report)).await;
    assert!(!result.passed(), "an unlisted compose must not pass");

    // A different app_compose no longer matches the measured compose-hash.
    evidence["app_compose"] = Value::String("tampered".to_string());
    let tampered = provenance_report(evidence, provenance);
    let (result, _) =
        appraise_provenance(&provenance_inputs(&tampered, &[]), Some(&quote_report)).await;
    assert!(!result.passed(), "a tampered compose must not pass");
}
