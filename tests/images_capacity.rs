//! Isolated binary so saturating image admission cannot affect other image tests.
mod common;

use axum::{
    body::{to_bytes, Body},
    http::Request,
    routing::post,
    Json, Router,
};
use private_ai_gateway::{
    http::build_router_with_admin_and_middleware,
    middleware::{admission, request_transform::Endpoint, Middleware, PrefixHashKey},
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn middleware_capacity_refusal_reports_gateway_failure_with_request_id() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let control = Router::new()
        .route("/consult/pre", post(|| async { Json(json!({"allow":true,"candidates":[
            {"routeId":"image:model","format":"openai","supportedEndpoints":["/v1/images/generations"]}
        ]})) }))
        .route("/consult/post", post(move |Json(body): Json<Value>| {
            let tx = tx.clone();
            async move { tx.send(body).await.unwrap(); }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, control).await.unwrap() });
    let middleware = Middleware::new(
        PrefixHashKey::new([7; 32]),
        &serde_json::from_value(json!({
            "control_url":format!("http://{address}")
        }))
        .unwrap(),
    )
    .unwrap();
    let h = common::admission::Harness::new(vec![]);
    let app = build_router_with_admin_and_middleware(
        Arc::new(h.service),
        common::session_config(json!([])),
        None,
        Arc::new(middleware),
    );
    let permits: Vec<_> = (0..admission::MAX_IN_FLIGHT)
        .map(|_| admission::try_acquire(Endpoint::ImagesGenerations).unwrap())
        .collect();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/images/generations")
                .header("authorization", "Bearer tenant-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"image","prompt":"test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    drop(permits);
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()["retry-after"], "1");
    assert!(!response.headers().contains_key("x-ratelimit-limit"));
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");
    let report = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    assert_eq!(body["request_id"], report["requestId"]);
    assert!(body["request_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(report["status"], 429);
    assert_eq!(report["errorSource"], "gateway");
    assert_eq!(report["errorMessage"], "gateway_capacity");
    assert!(h.backend.events.lock().unwrap().is_empty());
}
