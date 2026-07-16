//! Admin upstream config control-plane tests.

use std::sync::Arc;

mod common;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::aggregator::upstream_config::{
    parse_config_text, UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::http::{build_router_with_admin, build_router_with_admin_and_api};
use serde_json::Value;
use tower::ServiceExt;

use common::{StaticKeyProvider, StubQuoter};

fn temp_config_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "private-ai-gateway-upstreams-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
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
    }
}

async fn call(
    app: axum::Router,
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
        .oneshot(req.body(Body::from(body.into())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

async fn call_raw(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: impl Into<Vec<u8>>,
    auth: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = auth {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let resp = app
        .oneshot(req.body(Body::from(body.into())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, bytes.to_vec())
}

#[tokio::test]
async fn admin_can_replace_single_upstream_config_file_at_runtime() {
    let path = temp_config_path();
    let manager = Arc::new(UpstreamConfigManager::load(&path, runtime_options()).unwrap());
    let keys = Arc::new(StaticKeyProvider::default());
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            Arc::new(StubQuoter::default()),
            manager.backend(),
            manager.verifier(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router_with_admin(service, manager, Some("admin-secret".to_string()));

    let (status, models) = call(app.clone(), "GET", "/v1/models", Vec::new(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models["data"].as_array().unwrap().len(), 0);

    let (status, _) = call_raw(app.clone(), "GET", "/v1/metrics", Vec::new(), None).await;
    assert_eq!(status, StatusCode::OK);

    let config = br#"[
      {
        "name": "gpu-a",
        "base_url": "https://gpu-a.example",
        "models": {"public-a": "upstream-a"},
        "bearer_token": "secret-token"
      }
    ]"#;

    let (status, body) = call(
        app.clone(),
        "PUT",
        "/v1/admin/upstreams",
        config.to_vec(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "unauthorized");
    assert!(!path.exists());

    let (status, body) = call(
        app.clone(),
        "PUT",
        "/v1/admin/upstreams",
        config.to_vec(),
        Some("admin-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["upstreams"][0]["name"], "gpu-a");
    assert_eq!(body["upstreams"][0]["bearer_token_configured"], true);
    assert!(body["upstreams"][0].get("bearer_token").is_none());
    assert!(body["config_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("\"public-a\""));

    let (status, body) = call(
        app.clone(),
        "GET",
        "/v1/admin/upstreams",
        Vec::new(),
        Some("admin-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["upstreams"][0]["models"]["public-a"], "upstream-a");
    assert!(body["upstreams"][0].get("bearer_token").is_none());

    let (status, models) = call(app, "GET", "/v1/models", Vec::new(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models["data"][0]["id"], "public-a");

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn api_token_protects_catalog_and_completion_validates_model() {
    let path = temp_config_path();
    let manager = Arc::new(UpstreamConfigManager::load(&path, runtime_options()).unwrap());
    manager
        .replace(
            parse_config_text(
                r#"[
                  {
                    "name": "gpu-a",
                    "base_url": "https://gpu-a.example",
                    "models": {"public-a": "upstream-a"}
                  }
                ]"#,
            )
            .unwrap(),
        )
        .unwrap();
    let keys = Arc::new(StaticKeyProvider::default());
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            keys,
            Arc::new(StubQuoter::default()),
            manager.backend(),
            manager.verifier(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router_with_admin_and_api(
        service,
        manager,
        Some("admin-secret".to_string()),
        Some("api-secret".to_string()),
    );

    let (status, body) = call(app.clone(), "GET", "/v1/models", Vec::new(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], "api bearer token required");

    let (status, body) = call(app.clone(), "GET", "/v1/metrics", Vec::new(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], "api bearer token required");

    let (status, body) = call(
        app.clone(),
        "GET",
        "/v1/models",
        Vec::new(),
        Some("wrong-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["message"], "invalid api bearer token");

    let (status, body) = call(
        app.clone(),
        "GET",
        "/v1/metrics",
        Vec::new(),
        Some("wrong-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["message"], "invalid api bearer token");

    let (status, models) = call(
        app.clone(),
        "GET",
        "/v1/models",
        Vec::new(),
        Some("api-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models["data"][0]["id"], "public-a");

    let (status, body) = call(
        app.clone(),
        "POST",
        "/v1/chat/completions",
        br#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#.to_vec(),
        Some("api-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["message"], "Unknown model: test");

    let (status, body) = call(
        app,
        "POST",
        "/v1/chat/completions",
        br#"{"messages":[{"role":"user","content":"hi"}]}"#.to_vec(),
        Some("api-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "Model parameter is required");

    let _ = std::fs::remove_file(path);
}
