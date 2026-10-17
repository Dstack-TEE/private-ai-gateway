use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;

fn verifier(origin: &str) -> AciServiceUpstreamVerifier {
    AciServiceUpstreamVerifier::new_with_timeouts(
        origin,
        "http://127.0.0.1:1",
        AciServiceVerifierPolicy::new(
            vec!["test-subject".to_string()],
            Vec::new(),
            vec![public_key_uncompressed_hex(&signing_key(1))],
        )
        .unwrap(),
        300,
        1,
        60,
    )
    .unwrap()
}

fn request(origin: &str) -> UpstreamVerificationRequest {
    UpstreamVerificationRequest {
        upstream_name: "aci".to_string(),
        model_id: "model".to_string(),
        url_origin: Some(origin.to_string()),
        forwarded_body_hash: "22".repeat(32),
        required: true,
    }
}

async fn wait_until_wall(not_after: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
            <= Duration::from_secs(not_after)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wall clock did not pass keyset expiry");
}

struct Control {
    calls: AtomicUsize,
    entered: Notify,
    release: Semaphore,
    not_after: u64,
}

struct Fixture {
    verifier: Arc<AciServiceUpstreamVerifier>,
    request: UpstreamVerificationRequest,
    control: Arc<Control>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(not_after: u64) -> Self {
        use crate::aci::identity::{self, SealedWorkloadKeyset};
        use axum::{
            extract::{Query, State},
            routing::get,
            Json, Router,
        };
        async fn report(
            State(control): State<Arc<Control>>,
            Query(query): Query<std::collections::HashMap<String, String>>,
        ) -> Json<AttestationReport> {
            control.calls.fetch_add(1, Ordering::SeqCst);
            control.entered.notify_one();
            control.release.acquire().await.unwrap().forget();
            let nonce = &query["nonce"];
            let mut keyset = keyset_with_tls(Vec::new());
            keyset.not_after = control.not_after;
            let sealed = SealedWorkloadKeyset::seal(keyset).unwrap();
            let statement = identity::attestation_statement(sealed.digest(), Some(nonce)).unwrap();
            Json(AttestationReport {
                api_version: "aci/1".to_string(),
                workload_keyset_digest: sealed.digest().to_string(),
                attestation: AttestationEnvelope {
                    tee_type: "tdx".to_string(),
                    workload_keyset: sealed.to_value(),
                    report_data_hex: hex::encode(identity::report_data(&statement)),
                    source_provenance: SourceProvenance::default(),
                    evidence: json!({}),
                },
                service_capabilities: Default::default(),
            })
        }
        // Mock cryptographic appraisal only; public methods still fetch, bind the nonce,
        // serialize verification, check completion-time expiry, and cache the response.
        fn appraise(report: &AttestationReport, nonce: &str) -> super::super::appraisal::Appraisal {
            super::super::appraisal::Appraisal {
                results: Vec::new(),
                session_material: None,
                channel_bindings: Vec::new(),
                identity: Some(
                    aci_verify::report::verify_report_binding(report, Some(nonce)).unwrap(),
                ),
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let control = Arc::new(Control {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Semaphore::new(0),
            not_after,
        });
        let app = Router::new()
            .route("/v1/aci/attestation", get(report))
            .with_state(control.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            verifier: Arc::new(verifier(&origin).with_appraisal(appraise)),
            request: request(&origin),
            control,
            server,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn public_cold_verification_is_single_flight_and_refresh_shares_the_lock() {
    let fixture = Fixture::new(u64::MAX).await;
    let initial = {
        let mut calls = Box::pin(futures_util::future::join_all(
            (0..10).map(|_| fixture.verifier.verify(fixture.request.clone())),
        ));
        tokio::select! {
            _ = fixture.control.entered.notified() => {},
            _ = &mut calls => panic!("verification must await the upstream response"),
        }
        fixture.control.release.add_permits(1);
        calls.await
    };
    assert!(initial
        .iter()
        .all(|event| event.result == VerificationResult::Verified));
    assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 1);
    fixture.verifier.invalidate(&fixture.request);
    let verifier = fixture.verifier.clone();
    let request = fixture.request.clone();
    let refresh = tokio::spawn(async move { verifier.refresh(request).await });
    fixture.control.entered.notified().await;
    let mut cold = Box::pin(fixture.verifier.verify(fixture.request.clone()));
    assert!(futures_util::poll!(&mut cold).is_pending());
    assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 2);
    fixture.control.release.add_permits(1);
    let (refreshed, verified) = tokio::join!(refresh, cold);
    let refreshed = refreshed.unwrap();
    assert_eq!(refreshed.result, VerificationResult::Verified);
    assert_eq!(verified.result, VerificationResult::Verified);
    assert_eq!(verified.evidence, refreshed.evidence);
    assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn public_verification_rejects_a_keyset_expiring_before_the_response() {
    let not_after = current_unix_secs() + 2;
    let fixture = Fixture::new(not_after).await;
    let verifier = fixture.verifier.clone();
    let request = fixture.request.clone();
    let pending = tokio::spawn(async move { verifier.verify(request).await });
    fixture.control.entered.notified().await;
    assert!(current_unix_secs() < not_after);
    wait_until_wall(not_after).await;
    fixture.control.release.add_permits(1);
    let result = pending.await.unwrap();
    assert_eq!(result.result, VerificationResult::Failed);
    assert!(result.reason.unwrap().contains("expired"));
    assert!(fixture.verifier.cached(&fixture.request).is_none());
}

#[tokio::test]
async fn absolute_keyset_expiry_invalidates_an_unexpired_monotonic_cache() {
    let origin = "http://127.0.0.1:1";
    let deadline = Instant::now() + Duration::from_secs(300);
    let not_after = current_unix_secs() + 2;
    let verifier = verifier(origin).with_cached(CachedAciServiceVerification {
        identity: None,
        session_material: None,
        expires_at: deadline,
        not_after,
        evidence: None,
        channel_bindings: Vec::new(),
    });
    let request = request(origin);
    assert_eq!(
        verifier.verify(request.clone()).await.result,
        VerificationResult::Verified
    );
    assert!(verifier.cache_remaining(&request).unwrap() <= Duration::from_secs(2));
    wait_until_wall(not_after).await;
    assert!(Instant::now() < deadline);
    assert!(verifier.cached(&request).is_none());
    assert!(verifier.cache_remaining(&request).is_none());
}

/// Construct the real binding, measured log, compose and signed custody chain.
/// Only the already-authenticated DCAP boundary is supplied by this fixture.
async fn material_event(
    keyset_change: Option<(&str, Value)>,
    nonce: &str,
) -> UpstreamVerifiedEvent {
    use super::super::appraisal::{
        appraise_binding, appraise_channel, appraise_custody, appraise_expiry,
        authenticated_session_material, CheckId, CheckResult, Outcome,
    };
    use super::super::quote::VerifiedQuote;
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
        let service = super::session_pinning::material_service(event, at, store.clone());
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
    let service = super::session_pinning::material_service(capped.clone(), now + 5, store.clone());
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
