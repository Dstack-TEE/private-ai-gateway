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

/// Timeline proof that the default 30-day knob (not the receipt TTL) governs
/// session survival, and that the *last* citation renews it:
///   T0            first citation seals the session (deadline T0+30d)
///   T0+3500       second citation within validity renews (deadline T0+3500+30d)
///   T0+3601       first receipt expired at its 1h TTL; session still served
///   T0+30d        still served — only the renewal makes this true
///   T0+3500+30d   renewed deadline reached; eviction is exact
#[tokio::test]
async fn last_citation_renews_default_thirty_day_retention() {
    const DAY: u64 = 86_400;
    const RETENTION: u64 = 30 * DAY; // DEFAULT_SESSION_RETENTION_SECONDS
    let t0 = 1_700_000_000u64;
    let time = Arc::new(Time(AtomicU64::new(t0)));
    let cfg = AciServiceConfig::for_test();
    assert_eq!(cfg.receipt_ttl_seconds, 3600, "receipt TTL stays 1h");
    assert_eq!(
        cfg.session_retention_seconds, RETENTION,
        "default session retention is the new 30-day knob, not the receipt TTL"
    );
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(common::StaticKeyProvider::default()),
            Arc::new(common::StubQuoter::default()),
            Arc::new(Backend),
            Arc::new(Verifier),
            Arc::new(InMemoryReceiptStore::default()),
            cfg,
            time.clone(),
        )
        .unwrap(),
    );
    let app = build_router(service.clone());

    let post = |at: u64| {
        let service = service.clone();
        let app = app.clone();
        let time = time.clone();
        async move {
            time.0.store(at, Ordering::SeqCst);
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
            let receipt_id = response.headers()["x-receipt-id"]
                .to_str()
                .unwrap()
                .to_string();
            let receipt = service.get_receipt_by_receipt_id(&receipt_id).unwrap();
            let value = receipt.document_json().unwrap();
            let session_id = value["event_log"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["type"] == "upstream.verified")
                .unwrap()["session_id"]
                .as_str()
                .unwrap()
                .to_string();
            (receipt_id, session_id)
        }
    };

    let (receipt1, session) = post(t0).await;
    assert!(service.get_attested_session(&session).is_some());

    // Second citation within the validity window renews the deadline.
    let (receipt2, session2) = post(t0 + 3500).await;
    assert_eq!(session2, session, "same channel reuses the sealed session");

    // The first receipt has expired at its 1h TTL; the cited session outlives it.
    time.0.store(t0 + 3601, Ordering::SeqCst);
    assert!(
        service.get_receipt_by_receipt_id(&receipt1).is_none(),
        "first receipt expired at its own TTL"
    );
    assert!(service.get_receipt_by_receipt_id(&receipt2).is_some());
    assert!(
        service.get_attested_session(&session).is_some(),
        "session must outlive the receipts citing it (aci/1 §8)"
    );

    // Past the *first* deadline but within the renewed one.
    time.0.store(t0 + RETENTION, Ordering::SeqCst);
    assert!(
        service.get_attested_session(&session).is_some(),
        "renewed by the second citation; a single-citation session would lapse here"
    );

    // The renewed deadline is exact: at retention_until the record is gone.
    time.0.store(t0 + 3500 + RETENTION, Ordering::SeqCst);
    assert!(
        service.get_attested_session(&session).is_none(),
        "eviction at the renewed retention deadline"
    );
}
