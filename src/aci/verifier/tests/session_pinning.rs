use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{counting_provider_script, ExternalProviderVerifier};
use crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM;
use crate::aci::keys::{KeyError, KeyProvider, Quote, Quoter, ALGO_ED25519};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::types::{KeyedPublicKey, TlsSpki};
use crate::aci::upstream::{
    ModelRoute, ModelRouterBackend, PreparedUpstreamRequest, UpstreamBackend, UpstreamError,
    UpstreamRequest, UpstreamResponse,
};
use crate::aci::verifier::{RoutingUpstreamVerifier, StaticUpstreamVerifier};
use crate::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore, UpstreamVerificationRequest,
    UpstreamVerifier,
};
use crate::aggregator::upstream_config::{
    AttestationScope, UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use crate::http::{build_router_with_admin, build_router_with_admin_and_middleware};
use crate::middleware::{Middleware, MiddlewareConfig, PrefixHashKey};

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        for name in ["upstreams.json", "counter"] {
            let _ = std::fs::remove_file(self.0.join(name));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

struct SessionKeys(SigningKey);

impl KeyProvider for SessionKeys {
    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: "receipt-fixture".to_string(),
            algo: ALGO_ED25519.to_string(),
            public_key_hex: hex::encode(self.0.verifying_key().as_bytes()),
        }]
    }

    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id != "receipt-fixture" {
            return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
        }
        Ok(self.0.sign(payload).to_bytes().to_vec())
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        let secret = x25519_dalek::StaticSecret::from([8; 32]);
        vec![KeyedPublicKey {
            key_id: "e2ee-fixture".to_string(),
            algo: E2EE_ALGO_X25519_AESGCM.to_string(),
            public_key_hex: hex::encode(x25519_dalek::PublicKey::from(&secret).as_bytes()),
        }]
    }

    fn tls_spkis(&self) -> Vec<TlsSpki> {
        Vec::new()
    }

    fn is_test_only(&self) -> bool {
        true
    }
}

struct UnusedQuoter;

#[async_trait]
impl Quoter for UnusedQuoter {
    async fn get_quote(&self, _report_data: [u8; 32]) -> Result<Quote, KeyError> {
        Err(KeyError::Quote(
            "session test does not request a quote".to_string(),
        ))
    }

    async fn get_quote_raw(&self, _report_data: [u8; 64]) -> Result<Quote, KeyError> {
        Err(KeyError::Quote(
            "session test does not request a quote".to_string(),
        ))
    }
}

struct ProcessVerifier(ExternalProviderVerifier);

#[async_trait]
impl UpstreamVerifier for ProcessVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.0.verify(request).await
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        self.0.cached(request)
    }
}

struct FixtureBackend(&'static str, Mutex<Vec<String>>);

#[async_trait]
impl UpstreamBackend for FixtureBackend {
    fn name(&self) -> &str {
        self.0
    }

    fn url_origin(&self) -> Option<&str> {
        Some("https://provider.example")
    }

    async fn forward(&self, request: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        self.1
            .lock()
            .unwrap()
            .push(body["model"].as_str().unwrap().to_string());
        Ok(UpstreamResponse {
            status_code: 200,
            body: br#"{"id":"chat-router-cache","choices":[]}"#.to_vec(),
            headers: HashMap::new(),
            served_instance_id: None,
        })
    }

    async fn forward_verified_prepared(
        &self,
        request: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        assert_eq!(
            event.channel_bindings,
            vec![ChannelBinding::TlsSpkiSha256 {
                origin: "https://provider.example".to_string(),
                spki_sha256: "aa".repeat(32),
            }]
        );
        self.forward(request.request).await
    }
}

fn manager(config: Value) -> (TestDirectory, Arc<UpstreamConfigManager>) {
    let directory = TestDirectory(
        std::env::temp_dir().join(format!("pag-route-list-test-{}", std::process::id())),
    );
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("upstreams.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let manager = UpstreamConfigManager::load(
        &path,
        UpstreamRuntimeOptions {
            verifier_mode: UpstreamVerifierMode::None,
            accepted_subjects: Vec::new(),
            accepted_image_digests: Vec::new(),
            accepted_dstack_kms_root_public_keys: Vec::new(),
            pccs_url: None,
            verifier_cache_seconds: 300,
            connect_timeout_seconds: 10,
            read_timeout_seconds: 600,
            verifier_request_timeout_seconds: 60,
        },
    )
    .unwrap();
    (directory, Arc::new(manager))
}

fn service(backend: ModelRouterBackend, verifier: RoutingUpstreamVerifier) -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(SessionKeys(SigningKey::from_bytes(&[7; 32]))),
            Arc::new(UnusedQuoter),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

async fn list(app: axum::Router, query: &str) -> Vec<Value> {
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/aci/sessions?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    body["sessions"].as_array().unwrap().clone()
}

async fn pin(app: axum::Router, service: &AciService, model: &str, id: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": model, "messages": [], "provider": {"aci_session_ids": [id]},
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let receipt = service
        .get_receipt_by_receipt_id(response.headers()["x-receipt-id"].to_str().unwrap())
        .unwrap()
        .document_json()
        .unwrap();
    let verified = receipt["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == "upstream.verified")
        .unwrap()
        .clone();
    assert_eq!(verified["session_id"], id);
    to_bytes(response.into_body(), usize::MAX).await.unwrap();
    verified
}

#[tokio::test]
async fn session_listing_matches_pin_gate_across_routes() {
    let (directory, manager) = manager(json!([
        {"name":"a", "provider":"tinfoil", "base_url":"https://provider.example", "models":{"a":"provider-model", "shared":"x"}},
        {"name":"b", "provider":"tinfoil", "base_url":"https://provider.example", "models":{"provider-model":"y", "shared":"z"}},
    ]));
    let counter = directory.0.join("counter");
    let process = Arc::new(ProcessVerifier(ExternalProviderVerifier::with_command_and_cache(
        "tinfoil", AttestationScope::PerRouter,
        counting_provider_script(&counter, "tinfoil", "tinfoil/session-test/v1", json!({
            "type": "tls_spki_sha256", "origin": "https://provider.example", "spki_sha256": "AA".repeat(32),
        })), 5, 300,
    ).unwrap()));
    let representative = manager
        .current_verification_requests(Some("a"), None, false)
        .remove(0);
    assert_eq!(representative.model_id, "provider-model");
    assert_eq!(
        process.verify(representative).await.result,
        VerificationResult::Verified
    );
    let verifier = RoutingUpstreamVerifier::new()
        .add_route("a", "https://provider.example", process)
        .add_route(
            "b",
            "https://provider.example",
            Arc::new(StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
                result: VerificationResult::Verified,
                channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
                    origin: "https://provider.example".to_string(),
                    spki_sha256: "aa".repeat(32),
                }],
                ..Default::default()
            })),
        );
    let a = Arc::new(FixtureBackend("a", Mutex::new(Vec::new())));
    let b = Arc::new(FixtureBackend("b", Mutex::new(Vec::new())));
    let mut backend = ModelRouterBackend::new("fixture");
    for (name, routes, upstream) in [
        ("a", [("a", "provider-model"), ("shared", "x")], a.clone()),
        ("b", [("provider-model", "y"), ("shared", "z")], b.clone()),
    ] {
        for (public, model) in routes {
            backend
                .add_route(
                    ModelRoute::new(public, model, upstream.clone(), format!("{name}:{public}"))
                        .unwrap()
                        .with_is_tee(Some(true)),
                )
                .unwrap();
        }
    }
    let service = service(backend, verifier);
    let plain = build_router_with_admin(service.clone(), manager.clone(), None);
    let listed = list(plain.clone(), "model=a").await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["upstream_name"], "a");
    let id = listed[0]["session_id"].as_str().unwrap();
    let verified = pin(plain.clone(), &service, "shared", id).await;
    assert_eq!(verified["model_id"], "x");
    assert_eq!(*a.1.lock().unwrap(), ["x"]);
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap(),
        "1",
        "cross-model pin must reuse the real verifier cache"
    );
    let alias = list(plain.clone(), "model=provider-model").await;
    assert_eq!(alias.len(), 1);
    assert_eq!(
        alias[0]["upstream_name"], "b",
        "upstream IDs cannot shadow public aliases"
    );
    pin(
        plain.clone(),
        &service,
        "provider-model",
        alias[0]["session_id"].as_str().unwrap(),
    )
    .await;
    assert!(list(plain.clone(), "model=unknown").await.is_empty());
    assert!(list(plain, "model=shared&upstream_name=b").await.is_empty());
    a.1.lock().unwrap().clear();
    b.1.lock().unwrap().clear();
    let control = axum::Router::new().route("/consult/pre", axum::routing::post(|| async { axum::Json(json!({
        "allow":true, "candidates":[{"routeId":"a:shared","format":"openai"},{"routeId":"b:shared","format":"openai"}]
    })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, control).await.unwrap();
    });
    let middleware = Arc::new(
        Middleware::new(
            PrefixHashKey::new([7; 32]),
            &MiddlewareConfig {
                control_url: format!("http://{address}"),
                control_token: None,
                control_timeout_ms: None,
                control_post_timeout_ms: None,
                sse_keepalive_ms: None,
                send_request_features: Some(false),
                tee_only_domains: Vec::new(),
            },
        )
        .unwrap(),
    );
    let app = build_router_with_admin_and_middleware(service.clone(), manager, None, middleware);
    let listed = list(app.clone(), "model=shared").await;
    assert_eq!(listed.len(), 2);
    let id = listed
        .iter()
        .find(|session| session["upstream_name"] == "b")
        .unwrap()["session_id"]
        .as_str()
        .unwrap();
    pin(app, &service, "shared", id).await;
    assert!(
        a.1.lock().unwrap().is_empty(),
        "pin-mismatched A must be skipped"
    );
    assert_eq!(*b.1.lock().unwrap(), ["z"]);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
