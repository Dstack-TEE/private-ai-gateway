use super::*;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::time::Instant;

use super::super::appraisal::tests::material_appraisal;
use super::super::tests::{
    aci_service::{request, verifier},
    public_key_compressed_hex,
    session_pinning::material_service,
    signing_key,
};

async fn material_event(
    keyset_change: Option<(&str, Value)>,
    nonce: &str,
) -> UpstreamVerifiedEvent {
    let (body, appraisal) = material_appraisal(keyset_change, nonce).await;
    verifier("https://provider.example")
        .finish_verification(Instant::now(), &body, appraisal)
        .unwrap()
        .event_for(
            request("https://provider.example"),
            AciServiceUpstreamVerifier::VERIFIER_ID,
        )
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
