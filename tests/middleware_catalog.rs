//! Router-level tests for the in-process middleware catalog relay.
//!
//! Proves that a router built with the in-process middleware routes the model
//! catalog endpoints to the control plane, and that direct-upstream mode keeps
//! its unchanged sub-catalog behavior (404).

use std::sync::{Arc, Mutex};

mod common;

use axum::{
    body::{to_bytes, Body},
    extract::RawQuery,
    http::{Request, StatusCode},
    routing::{get, post},
    Json, Router,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::aggregator::upstream_config::{
    UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::http::{build_router_with_admin, build_router_with_admin_and_middleware};
use private_ai_gateway::middleware::{hash_api_key, Middleware, MiddlewareConfig};
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
            AciServiceConfig::for_test("private-ai-gateway"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    (service, manager)
}

// Spawn a stub control plane that labels each catalog so the test can prove the
// relay reached the right path, and echoes the query string it received so the
// test can prove catalog filters (e.g. `?zdr=true`) survive the relay. The
// control plane serves catalogs without the `/v1` prefix.
async fn spawn_stub_control() -> String {
    let app = Router::new()
        .route(
            "/models",
            get(|RawQuery(q): RawQuery| async move {
                Json(json!({ "data": ["m1"], "source": "control-models", "query": q }))
            }),
        )
        .route(
            "/models/*rest",
            get(|RawQuery(q): RawQuery| async move {
                Json(json!({ "data": ["ns"], "source": "control-sub", "query": q }))
            }),
        )
        .route(
            "/embeddings/models",
            get(|RawQuery(q): RawQuery| async move {
                Json(json!({ "data": ["e1"], "source": "control-embeddings", "query": q }))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_control_capturing_auth() -> (String, Arc<Mutex<Vec<Value>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_by_route = captured.clone();
    let app = Router::new().route(
        "/consult/pre",
        post(move |Json(body): Json<Value>| {
            let captured = captured_by_route.clone();
            async move {
                captured.lock().unwrap().push(body);
                Json(json!({ "allow": false }))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
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
async fn middleware_preserves_distinct_client_bearers_for_authorization() {
    let (control_url, captured) = spawn_control_capturing_auth().await;
    let middleware = Arc::new(
        Middleware::new(&MiddlewareConfig {
            control_url,
            control_token: None,
            control_timeout_ms: Some(2_000),
            control_post_timeout_ms: Some(2_000),
            sse_keepalive_ms: None,
            tee_only_domains: Vec::new(),
        })
        .unwrap(),
    );
    let (service, manager) = build_service();
    let app = build_router_with_admin_and_middleware(service, manager, None, middleware);

    for token in ["client-one", "client-two"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        br#"{"model":"private-model","messages":[]}"#.to_vec(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "the request must reach the control plane's denial"
        );
    }

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0]["apiKeyHash"], hash_api_key("client-one"));
    assert_eq!(captured[1]["apiKeyHash"], hash_api_key("client-two"));
    assert_ne!(captured[0]["apiKeyHash"], captured[1]["apiKeyHash"]);
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
            tee_only_domains: Vec::new(),
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

// Catalog filters live in the control plane (`?zdr=true` restricts the catalog
// to zero-data-retention providers). The gateway must relay the query string
// verbatim — dropping it would silently serve an unfiltered catalog to a client
// that asked to filter.
#[tokio::test]
async fn relays_catalog_query_string_to_control() {
    let control_url = spawn_stub_control().await;
    let middleware = Arc::new(
        Middleware::new(&MiddlewareConfig {
            control_url,
            control_token: None,
            control_timeout_ms: Some(2_000),
            control_post_timeout_ms: Some(2_000),
            sse_keepalive_ms: None,
            tee_only_domains: Vec::new(),
        })
        .unwrap(),
    );
    let (service, manager) = build_service();
    let app = build_router_with_admin_and_middleware(service, manager, None, middleware);

    let (status, body) = get_json(app.clone(), "/v1/models?zdr=true").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["query"], "zdr=true");

    // Multiple params survive intact; the gateway does not parse or reorder them.
    let (_, body) = get_json(app.clone(), "/v1/models?zdr=true&foo=bar").await;
    assert_eq!(body["query"], "zdr=true&foo=bar");

    // Sub-catalogs relay it too, alongside the path.
    let (_, body) = get_json(app.clone(), "/v1/models/my-namespace?zdr=true").await;
    assert_eq!(body["source"], "control-sub");
    assert_eq!(body["query"], "zdr=true");

    // Every catalog route relays it — embeddings included. Missing one would
    // serve an unfiltered catalog to a client that asked to filter.
    let (_, body) = get_json(app.clone(), "/v1/embeddings/models?zdr=true").await;
    assert_eq!(body["source"], "control-embeddings");
    assert_eq!(body["query"], "zdr=true");

    // No query string means no trailing `?` is invented.
    let (_, body) = get_json(app, "/v1/models").await;
    assert_eq!(body["query"], Value::Null);
}

#[tokio::test]
async fn direct_mode_sub_catalogs_remain_not_found() {
    let (service, manager) = build_service();
    let app = build_router_with_admin(service, manager, None, None);

    let (status, _) = get_json(app.clone(), "/v1/models/my-namespace").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = get_json(app, "/v1/embeddings/models").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
