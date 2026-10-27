use super::*;
use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256, Sha384};
use tokio::time::Instant;

use super::super::current_unix_secs;
use super::super::tests::{
    aci_service::{request, verifier},
    public_key_compressed_hex, receipt_custody_fixture,
    session_pinning::material_service,
    sign_recoverable, signing_key,
};
use crate::aci::receipt::UpstreamVerifiedEvent;
use crate::aci::types::{AttestationEnvelope, TlsSpki};

/// Construct the real binding, measured log, compose and signed custody chain.
/// Only the already-authenticated DCAP boundary is supplied by this fixture.
async fn material_event(
    keyset_change: Option<(&str, Value)>,
    nonce: &str,
) -> UpstreamVerifiedEvent {
    let root = signing_key(1);
    let app = signing_key(2);
    let scalar_byte = keyset_change
        .as_ref()
        .filter(|(key, _)| *key == "receipt_scalar")
        .map(|(_, value)| u8::try_from(value.as_u64().unwrap()).unwrap())
        .unwrap_or(3);
    let scalar = [scalar_byte; 32];
    let kms = signing_key(scalar_byte);
    let app_id = vec![4; 20];
    let purpose = "aci.receipt.ed25519.v1";
    let purpose_signature = sign_recoverable(
        &app,
        format!("{purpose}:{}", public_key_compressed_hex(&kms)).as_bytes(),
    );
    let root_message = [
        b"dstack-kms-issued:".as_slice(),
        &app_id,
        app.verifying_key().to_sec1_bytes().as_ref(),
    ]
    .concat();
    let (mut keyset, mut evidence) = receipt_custody_fixture(
        scalar,
        vec![purpose_signature, sign_recoverable(&root, &root_message)],
    );
    keyset.subject = None;
    keyset.not_after = 4_000_000_000;
    keyset.tls_public_keys = vec![TlsSpki {
        domain: Some("provider.example".into()),
        spki_sha256_hex: "aa".repeat(32),
    }];
    // ACI allows a downstream selector; its declared set remains unchanged
    // when the receipt key or another identity member changes.
    evidence["downstream_tls_binding"] =
        json!({"domain": "provider.example", "spki_sha256": "aa".repeat(32)});
    let compose = "services: gateway";
    let compose_hash = hex::encode(Sha256::digest(compose.as_bytes()));
    let mut events = Vec::new();
    let mut rtmr = vec![0; 48];
    for (name, payload) in [
        ("app-id", hex::encode(&app_id)),
        ("compose-hash", compose_hash),
    ] {
        let digest = Sha384::digest(
            [
                0x0800_0001u32.to_ne_bytes().as_slice(),
                b":",
                name.as_bytes(),
                b":",
                &hex::decode(&payload).unwrap(),
            ]
            .concat(),
        );
        rtmr = Sha384::digest([rtmr.as_slice(), &digest[..]].concat()).to_vec();
        events.push(json!({"imr": 3, "event_type": 0x0800_0001u32, "event": name, "event_payload": payload, "digest": hex::encode(digest)}));
    }
    evidence["event_log"] = serde_json::to_string(&events).unwrap().into();
    evidence["app_compose"] = compose.into();
    let mut original = serde_json::to_value(keyset).unwrap();
    if let Some((key, value)) = keyset_change {
        if key != "receipt_scalar" {
            original[key] = value;
        }
    }
    let keyset_digest =
        crate::aci::digest::sha256_hex(&crate::aci::digest::jcs_bytes(&original).unwrap());
    let statement =
        crate::aci::identity::attestation_statement(&keyset_digest, Some(nonce)).unwrap();
    let report = AttestationReport {
        api_version: "aci/1".into(),
        workload_keyset_digest: keyset_digest,
        attestation: AttestationEnvelope {
            tee_type: "tdx".into(),
            workload_keyset: original,
            report_data_hex: hex::encode(crate::aci::identity::report_data(&statement)),
            source_provenance: SourceProvenance {
                image_digest: Some("sha256:published-image".into()),
                ..Default::default()
            },
            evidence,
        },
        service_capabilities: Default::default(),
    };
    let policy = AciServiceVerifierPolicy::new(
        Vec::new(),
        vec!["sha256:published-image".into()],
        vec![public_key_compressed_hex(&root)],
    )
    .unwrap();
    let inputs = AppraisalInputs {
        report: &report,
        nonce: Some(nonce),
        now_secs: current_unix_secs(),
        expiry_waived: false,
        quote: QuoteSource::Offline {
            reason: "verified boundary supplied below",
        },
        accepted_composes: &[],
        custody: CustodyEvidence::DstackKms { policy: &policy },
        channel: ChannelEvidence::DeclaredFor {
            origin: "https://provider.example",
        },
        explain: true,
    };
    let quote = VerifiedQuote {
        report: td10_report(rtmr.try_into().unwrap()),
        status: "UpToDate".into(),
        tee_type: "tdx",
        advisory_ids: vec!["INTEL-SA-B".into(), "INTEL-SA-A".into()],
    };
    let (binding_result, identity) = appraise_binding(&inputs);
    let keyset = &identity.as_ref().unwrap().keyset;
    let (provenance_result, provenance) = appraise_provenance(&inputs, Some(&quote.report)).await;
    let (custody_result, custody) = appraise_custody(
        &inputs,
        Some(keyset),
        provenance.as_ref().unwrap().app_id.as_deref(),
    );
    let (channel_result, channel_bindings) = appraise_channel(&inputs, Some(keyset));
    let mut results = vec![
        CheckResult {
            id: CheckId::Quote,
            outcome: Outcome::Pass,
            detail: "opaque diagnostic".into(),
            explain: None,
        },
        binding_result,
        appraise_expiry(&inputs, Some(keyset)),
        provenance_result,
        custody_result,
        channel_result,
    ];
    let session_material = authenticated_session_material(
        &results,
        Some(&quote),
        provenance.as_ref(),
        custody.as_ref(),
        &report.attestation.source_provenance,
    )
    .unwrap();
    assert_eq!(
        session_material["advisory_ids"],
        json!(["INTEL-SA-A", "INTEL-SA-B"])
    );
    assert!(crate::aggregator::service::reviewed_session_material(
        Some("aci-service"),
        "aci-service/v2",
        &session_material
    ));
    // A diagnostic string cannot restore eligibility to a failed/unevaluable
    // check, or replace absent authenticated structured material.
    for id in [
        CheckId::Quote,
        CheckId::Binding,
        CheckId::Provenance,
        CheckId::Custody,
    ] {
        let index = results.iter().position(|r| r.id == id).unwrap();
        for outcome in [
            Outcome::Unevaluable("offline".into()),
            Outcome::Failed(FailureCause::Policy("rejected".into())),
        ] {
            results[index].outcome = outcome;
            results[index].detail = "all checks passed; TCB UpToDate".into();
            assert!(authenticated_session_material(
                &results,
                Some(&quote),
                provenance.as_ref(),
                custody.as_ref(),
                &report.attestation.source_provenance
            )
            .is_none());
        }
        results[index].outcome = Outcome::Pass;
    }
    assert!(authenticated_session_material(
        &results,
        None,
        provenance.as_ref(),
        custody.as_ref(),
        &report.attestation.source_provenance
    )
    .is_none());
    assert!(authenticated_session_material(
        &results,
        Some(&quote),
        None,
        custody.as_ref(),
        &report.attestation.source_provenance
    )
    .is_none());
    assert!(authenticated_session_material(
        &results,
        Some(&quote),
        provenance.as_ref(),
        None,
        &report.attestation.source_provenance
    )
    .is_none());
    let accepted = Appraisal {
        results,
        identity,
        channel_bindings,
        session_material: Some(session_material),
    };
    let body = serde_json::to_vec(&report).unwrap();
    verifier("https://provider.example")
        .finish_verification(Instant::now(), &body, accepted)
        .unwrap()
        .event_for(request("https://provider.example"), "aci-service/v2")
}

#[tokio::test]
async fn aci_identity_and_authenticated_material_control_snapshot_reuse_and_expiry() {
    use crate::aggregator::session_store::InMemorySessionStore;
    let store = Arc::new(InMemorySessionStore::default());
    let event = material_event(None, &"01".repeat(32)).await;
    let now = current_unix_secs();
    let sessions = |event: UpstreamVerifiedEvent, at| {
        let service = material_service(event, at, store.clone());
        service.list_current_sessions(&[request("https://provider.example")])
    };
    let first = sessions(event.clone(), now).unwrap().remove(0);
    let fresh = material_event(None, &"02".repeat(32)).await;
    assert_ne!(fresh.evidence, event.evidence);
    assert_eq!(sessions(fresh, now).unwrap()[0].bytes(), first.bytes());
    let identity = first.document().identity.as_ref().unwrap();
    assert_eq!(identity.extra["subject"], Value::Null);
    assert_eq!(
        identity.extra["workload_keyset_digest"],
        event.aci_identity.as_ref().unwrap().keyset_digest
    );
    for (key, value) in [
        ("subject", json!(format!("app-id:0x{}", "04".repeat(20)))),
        ("not_after", json!(4_000_000_001u64)),
        ("future_member", json!({"must_keep": true})),
    ] {
        let changed = material_event(Some((key, value)), &"01".repeat(32)).await;
        if key == "future_member" {
            assert_eq!(
                changed.aci_identity.as_ref().unwrap().keyset,
                event.aci_identity.as_ref().unwrap().keyset
            );
            assert_ne!(
                changed.aci_identity.as_ref().unwrap().keyset_digest,
                event.aci_identity.as_ref().unwrap().keyset_digest
            );
        }
        assert_ne!(
            sessions(changed, now).unwrap()[0].session_id(),
            first.session_id(),
            "{key}"
        );
    }
    // Rotating the receipt key changes accepted keyset identity even though
    // the downstream TLS channel is identical.
    let rotated = material_event(Some(("receipt_scalar", json!(8))), &"01".repeat(32)).await;
    assert_eq!(rotated.channel_bindings, event.channel_bindings);
    assert_ne!(
        sessions(rotated, now).unwrap()[0].session_id(),
        first.session_id()
    );
    for (key, value) in [
        ("measured_compose_hash", json!("ff".repeat(32))),
        ("measured_app_id", json!("ff".repeat(20))),
        ("tcb_status", json!("OutOfDate")),
        ("advisory_ids", json!(["INTEL-SA-C"])),
        (
            "published_source_provenance",
            json!({"image_digest": "sha256:other-image"}),
        ),
    ] {
        let mut changed = event.clone();
        changed.provider_claims.as_mut().unwrap()[key] = value;
        assert_ne!(
            sessions(changed, now).unwrap()[0].session_id(),
            first.session_id(),
            "{key}"
        );
    }
    for field in ["kms_public_key", "app_public_key", "root_public_key"] {
        let mut changed = event.clone();
        changed.provider_claims.as_mut().unwrap()["receipt_key_custody"][field] =
            json!(public_key_compressed_hex(&signing_key(9)));
        assert_ne!(
            sessions(changed, now).unwrap()[0].session_id(),
            first.session_id(),
            "{field}"
        );
    }
    let mut changed = event.clone();
    let claims = changed.provider_claims.as_mut().unwrap();
    claims["tdx_report"]["mr_owner"] = json!("ff".repeat(48));
    claims["tdx_report_sha256"] = json!(crate::aci::digest::sha256_hex(
        &crate::aci::digest::jcs_bytes(&claims["tdx_report"]).unwrap()
    ));
    assert_ne!(
        sessions(changed, now).unwrap()[0].session_id(),
        first.session_id()
    );
    let capped = material_event(Some(("not_after", json!(now + 5))), &"01".repeat(32)).await;
    assert_eq!(
        sessions(capped.clone(), now).unwrap()[0]
            .document()
            .expires_at,
        now + 5
    );
    assert!(sessions(capped.clone(), now + 5).is_err());
    let service = material_service(capped.clone(), now + 5, store.clone());
    assert!(service
        .forward_chat_completion(b"{}", None, false, Some(capped))
        .await
        .is_err());
    // Even a copied native profile cannot supply accepted identity via claims.
    let mut untrusted = event.clone();
    untrusted.aci_identity = None;
    untrusted.provider_claims.as_mut().unwrap()["keyset_not_after"] = json!(now);
    let untrusted_first = sessions(untrusted.clone(), now).unwrap().remove(0);
    assert_eq!(untrusted_first.document().expires_at, now + 3600);
    untrusted.evidence = Some(
        serde_json::to_value(crate::aggregator::session::EvidenceRef::from_bytes(
            "text/plain",
            b"new untrusted evidence",
        ))
        .unwrap(),
    );
    assert_ne!(
        sessions(untrusted, now).unwrap()[0].session_id(),
        untrusted_first.session_id()
    );
    // Arbitrary claims cannot impose an ACI validity cap on another adapter.
    let mut custom = event;
    custom.provider_type = Some("custom".into());
    custom.provider_claims.as_mut().unwrap()["keyset_not_after"] = json!(now);
    assert_eq!(
        sessions(custom, now).unwrap()[0].document().expires_at,
        now + 3600
    );
}

#[tokio::test]
async fn offline_or_failed_quote_appraisal_never_emits_stable_material() {
    // Parseable TD10 fixture generated by session_material_soundness.quote_fixture;
    // the genuine parser/binding checks run, while its signatures are invalid.
    let report: AttestationReport = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/aci_service_offline.json"
    ))
    .unwrap();
    for (offline, quote) in [
        (
            true,
            QuoteSource::Offline {
                reason: "offline audit",
            },
        ),
        (
            false,
            QuoteSource::Online {
                pccs_url: "http://127.0.0.1:1",
            },
        ),
    ] {
        let appraisal = appraise_report(AppraisalInputs {
            report: &report,
            nonce: Some(&"01".repeat(32)),
            now_secs: 1_700_000_000,
            expiry_waived: false,
            quote,
            accepted_composes: &[],
            custody: CustodyEvidence::Unimplemented {
                reason: "no accepted custody",
            },
            channel: ChannelEvidence::DeclaredFor {
                origin: "https://provider.example",
            },
            explain: true,
        })
        .await
        .unwrap();
        assert!(
            appraisal.identity.is_some(),
            "parsed binding is diagnostic, not authentication"
        );
        let quote_outcome = &appraisal
            .results
            .iter()
            .find(|r| r.id == CheckId::Quote)
            .unwrap()
            .outcome;
        if offline {
            assert!(matches!(quote_outcome, Outcome::Unevaluable(_)));
        } else {
            assert!(matches!(quote_outcome, Outcome::Failed(_)));
        }
        assert!(appraisal.session_material.is_none());
    }
}

#[test]
fn tdx_projection_matches_stage2_fields_and_excludes_only_fresh_report_data() {
    use super::super::quote::tdx_session_material;
    use dcap_qvl::quote::{Report, TDReport15};
    let Report::TD10(mut base) = td10_report([0; 48]) else {
        unreachable!()
    };
    let first = tdx_session_material(&Report::TD10(base)).unwrap();
    base.report_data = [9; 64];
    assert_eq!(tdx_session_material(&Report::TD10(base)).unwrap(), first);
    let td15 = tdx_session_material(&Report::TD15(TDReport15 {
        base,
        tee_tcb_svn2: [3; 16],
        mr_service_td: [4; 48],
    }))
    .unwrap();
    assert_eq!(td15["tee_tcb_svn2"], "03".repeat(16));
    assert_eq!(td15["mr_service_td"], "04".repeat(48));
    assert_eq!(td15["report_format"], "td15");
    let stage2: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/session_material.json"
    ))
    .unwrap();
    assert_eq!(
        first
            .as_object()
            .unwrap()
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        stage2["near-ai"]["tdx_report"]
            .as_object()
            .unwrap()
            .keys()
            .collect()
    );
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
        custody: CustodyEvidence::Unimplemented {
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
    assert!(
        app_id.unwrap().app_id.is_none(),
        "this log carries no app-id event"
    );

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
