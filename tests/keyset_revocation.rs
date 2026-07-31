//! Keyset revocation surface (§4.7): admin revoke endpoint, public revocations
//! list, and the "stop serving a revoked keyset" behavior.

use std::sync::Arc;

mod common;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use private_ai_gateway::aci::identity::keyset_revocation_payload;
use private_ai_gateway::aci::keys::{verify_keyset_endorsement, KeyProvider};
use private_ai_gateway::aggregator::revocation_store::RevocationStore;
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::aggregator::upstream_config::{
    UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::http::build_router_with_admin;
use serde_json::Value;
use tower::ServiceExt;

use common::{StaticKeyProvider, StubQuoter};

const ADMIN_TOKEN: &str = "admin-secret";

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "keyset-revocation-{name}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn runtime_options() -> UpstreamRuntimeOptions {
    UpstreamRuntimeOptions {
        verifier_mode: UpstreamVerifierMode::Preverified,
        accepted_workload_ids: vec![],
        accepted_image_digests: vec![],
        accepted_dstack_kms_root_public_keys: vec![],
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 600,
        verifier_request_timeout_seconds: 60,
        privatemode_proxy: None,
    }
}

struct Harness {
    app: axum::Router,
    revocations_path: std::path::PathBuf,
}

fn build_harness(dir: &std::path::Path) -> Harness {
    let upstream_path = dir.join("upstreams.json");
    let revocations_path = dir.join("revocations.json");
    let manager = Arc::new(UpstreamConfigManager::load(&upstream_path, runtime_options()).unwrap());
    let revocation_store = Arc::new(RevocationStore::open(&revocations_path).unwrap());
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            manager.backend(),
            manager.verifier(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap()
        .with_revocation_store(revocation_store),
    );
    Harness {
        app: build_router_with_admin(service, manager, Some(ADMIN_TOKEN.to_string()), None),
        revocations_path,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: impl Into<Vec<u8>>,
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = auth {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.into())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn revocation_lifecycle_end_to_end() {
    let dir = temp_dir("lifecycle");
    let Harness {
        app,
        revocations_path,
    } = build_harness(&dir);

    // Before revocation: an empty transparency list and a served report.
    let (status, list) = call(&app, "GET", "/v1/aci/revocations", Vec::new(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["revocations"].as_array().unwrap().len(), 0);

    let (status, _) = call(&app, "GET", "/v1/aci/attestation", Vec::new(), None).await;
    assert_eq!(status, StatusCode::OK);

    // The revoke endpoint is admin-guarded.
    let (status, body) = call(&app, "POST", "/v1/admin/revoke-keyset", Vec::new(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "unauthorized");

    let (status, _) = call(
        &app,
        "POST",
        "/v1/admin/revoke-keyset",
        Vec::new(),
        Some("wrong-token"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A revoked keyset is not yet in the list, so serving still works.
    let (status, _) = call(&app, "GET", "/v1/aci/attestation", Vec::new(), None).await;
    assert_eq!(status, StatusCode::OK);

    // Revoke: the returned statement verifies under the identity key.
    let (status, body) = call(
        &app,
        "POST",
        "/v1/admin/revoke-keyset",
        Vec::new(),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let revoked = &body["revoked"];
    assert_eq!(revoked["purpose"], "aci.keyset.revocation.v1");
    assert_eq!(revoked["revocation"]["algo"], "ecdsa-secp256k1");

    let identity_pk = StaticKeyProvider::default().identity_public_key();
    let digest = revoked["workload_keyset_digest"].as_str().unwrap();
    let payload = keyset_revocation_payload(digest).unwrap();
    let signature = hex::decode(revoked["revocation"]["value"].as_str().unwrap()).unwrap();
    assert!(
        verify_keyset_endorsement(&identity_pk, &payload, &signature),
        "revocation signature must verify under the identity key"
    );

    // The statement is now public and persisted to disk.
    let (status, list) = call(&app, "GET", "/v1/aci/revocations", Vec::new(), None).await;
    assert_eq!(status, StatusCode::OK);
    let statements = list["revocations"].as_array().unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0]["workload_keyset_digest"], digest);
    let reopened = RevocationStore::open(&revocations_path).unwrap();
    assert!(reopened.is_revoked(digest));

    // Now serving stops: reports and inference both fail closed.
    let (status, body) = call(&app, "GET", "/v1/aci/attestation", Vec::new(), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["type"], "keyset_revoked");

    let (status, body) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        br#"{"model":"demo","messages":[{"role":"user","content":"hi"}]}"#.to_vec(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["type"], "keyset_revoked");

    // Re-revoking the same digest is idempotent — still one statement.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/admin/revoke-keyset",
        Vec::new(),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(&app, "GET", "/v1/aci/revocations", Vec::new(), None).await;
    assert_eq!(list["revocations"].as_array().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}
