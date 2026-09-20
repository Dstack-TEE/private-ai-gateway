//! Reviewer-only invariant: a live receipt's cited session remains retrievable.
mod common;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use private_ai_gateway::{
    aci::{
        receipt::{UpstreamVerifiedEvent, VerificationResult},
        upstream::{
            PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest,
            UpstreamResponse,
        },
    },
    aggregator::service::{
        AciService, AciServiceConfig, Clock, InMemoryReceiptStore, UpstreamVerificationRequest,
        UpstreamVerifier,
    },
    http::build_router,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tower::ServiceExt;
struct Time(AtomicU64);
impl Clock for Time {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Backend;
#[async_trait]
impl UpstreamBackend for Backend {
    fn name(&self) -> &str {
        "test"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://example.com")
    }
    fn prepare(&self, request: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        Ok(PreparedUpstreamRequest {
            request,
            upstream_name: "test".into(),
            url_origin: Some("https://example.com".into()),
            model_id: "test".into(),
            route_id: None,
            is_tee: Some(true),
        })
    }
    async fn forward(&self, _: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Ok(UpstreamResponse {
            status_code: 200,
            body: b"{}".to_vec(),
            headers: HashMap::new(),
            served_instance_id: None,
        })
    }
    async fn forward_verified_prepared(
        &self,
        r: PreparedUpstreamRequest,
        _: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(r).await
    }
}
struct Verifier;
#[async_trait]
impl UpstreamVerifier for Verifier {
    async fn verify(&self, r: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        common::event_from_request(&r, VerificationResult::Verified)
    }
}
#[tokio::test]
async fn configured_retention_cannot_undercut_live_receipt() {
    for retention in [0, 1, 3600, 30 * 86400] {
        let time = Arc::new(Time(AtomicU64::new(1_700_000_000)));
        let mut cfg = AciServiceConfig::for_test();
        cfg.receipt_ttl_seconds = 3600;
        cfg.session_retention_seconds = retention;
        let result = AciService::new_with_upstream_verifier(
            Arc::new(common::StaticKeyProvider::default()),
            Arc::new(common::StubQuoter::default()),
            Arc::new(Backend),
            Arc::new(Verifier),
            Arc::new(InMemoryReceiptStore::default()),
            cfg,
            time.clone(),
        );
        if result.is_err() {
            assert!(retention < 3600);
            continue;
        }
        let service = Arc::new(result.unwrap());
        let app = build_router(service.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"test","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let receipt_id = response.headers()["x-receipt-id"].to_str().unwrap();
        let receipt = service.get_receipt_by_receipt_id(receipt_id).unwrap();
        let value = receipt.document_json().unwrap();
        let id = value["event_log"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "upstream.verified")
            .unwrap()["session_id"]
            .as_str()
            .unwrap();
        time.0.fetch_add(2, Ordering::SeqCst);
        assert!(service.get_receipt_by_receipt_id(receipt_id).is_some());
        assert!(
            service.get_attested_session(id).is_some(),
            "session vanished while citing receipt is live, configured retention={retention}"
        );
    }
}
