//! Router-level tests for the in-process middleware catalog relay.
//!
//! Proves that a router built with the in-process middleware routes the model
//! catalog endpoints to the control plane, and that direct-upstream mode keeps
//! its unchanged sub-catalog behavior (404).

use std::sync::Arc;

mod common;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::get,
    Json, Router,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::aggregator::upstream_config::{
    UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::http::{build_router_with_admin, build_router_with_admin_and_middleware};
use private_ai_gateway::middleware::{Middleware, MiddlewareConfig};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tower::ServiceExt;

use common::{StaticKeyProvider, StubQuoter};

fn temp_config_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "private-ai-gateway-middleware-catalog-{}-{}.json",
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
        accepted_subjects: vec![],
        accepted_image_digests: vec![],
        accepted_dstack_kms_root_public_keys: vec![],
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 600,
        verifier_request_timeout_seconds: 60,
    }
}

fn build_service() -> (Arc<AciService>, Arc<UpstreamConfigManager>) {
    let path = temp_config_path();
    let manager = Arc::new(UpstreamConfigManager::load(&path, runtime_options()).unwrap());
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            manager.backend(),
            manager.verifier(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    (service, manager)
}

// Spawn a stub control plane that labels each catalog so the test can prove the
// relay reached the right path. The control plane serves catalogs without the
// `/v1` prefix.
async fn spawn_stub_control() -> String {
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({ "data": ["m1"], "source": "control-models" })) }),
        )
        .route(
            "/models/*rest",
            get(|| async { Json(json!({ "data": ["ns"], "source": "control-sub" })) }),
        )
        .route(
            "/embeddings/models",
            get(|| async { Json(json!({ "data": ["e1"], "source": "control-embeddings" })) }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn get_json(app: Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn relays_catalogs_from_control() {
    let control_url = spawn_stub_control().await;
    let middleware = Arc::new(
        Middleware::new(&MiddlewareConfig {
            control_url,
            control_token: None,
            control_timeout_ms: Some(2_000),
            control_post_timeout_ms: Some(2_000),
            sse_keepalive_ms: None,
        })
        .unwrap(),
    );
    let (service, manager) = build_service();
    let app = build_router_with_admin_and_middleware(service, manager, None, middleware);

    let (status, body) = get_json(app.clone(), "/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "control-models");

    let (status, body) = get_json(app.clone(), "/v1/models/my-namespace").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "control-sub");

    let (status, body) = get_json(app, "/v1/embeddings/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "control-embeddings");
}

#[tokio::test]
async fn direct_mode_sub_catalogs_remain_not_found() {
    let (service, manager) = build_service();
    let app = build_router_with_admin(service, manager, None);

    let (status, _) = get_json(app.clone(), "/v1/models/my-namespace").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = get_json(app, "/v1/embeddings/models").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
