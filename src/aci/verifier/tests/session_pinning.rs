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
use crate::aci::verifier::RoutingUpstreamVerifier;
use crate::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore, UpstreamVerificationRequest,
    UpstreamVerifier,
};
use crate::aggregator::upstream_config::{
    AttestationScope, UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use crate::http::build_router_with_admin;

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

#[derive(Default)]
struct FixtureBackend(Mutex<Vec<String>>);

#[async_trait]
impl UpstreamBackend for FixtureBackend {
    fn name(&self) -> &str {
        "provider-upstream"
    }

    fn url_origin(&self) -> Option<&str> {
        Some("https://provider.example")
    }

    async fn forward(&self, request: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        self.0
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

#[tokio::test]
async fn external_router_listed_session_accepts_a_pin_for_another_public_model() {
    let path = std::env::temp_dir().join(format!("pag-router-session-test-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    let directory = TestDirectory(path);
    let counter = directory.0.join("counter");
    let config_path = directory.0.join("upstreams.json");
    std::fs::write(&config_path, serde_json::to_vec(&json!([{
        "name": "provider-upstream", "provider": "tinfoil", "base_url": "https://provider.example",
        "models": {"public-a": "provider-model", "public-b": "other-provider-model"},
    }])).unwrap()).unwrap();
    let manager = Arc::new(
        UpstreamConfigManager::load(
            &config_path,
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
        .unwrap(),
    );
    let verifier = Arc::new(ProcessVerifier(ExternalProviderVerifier::with_command_and_cache(
        "tinfoil", AttestationScope::PerRouter,
        counting_provider_script(&counter, "tinfoil", "tinfoil/session-test/v1", json!({
            "type": "tls_spki_sha256", "origin": "https://provider.example", "spki_sha256": "AA".repeat(32),
        })), 5, 300,
    ).unwrap()));
    let representative = manager.current_verification_requests(None, None).remove(0);
    assert_eq!(representative.model_id, "provider-model");
    assert_eq!(
        verifier.verify(representative).await.result,
        VerificationResult::Verified
    );
    let verifier = Arc::new(RoutingUpstreamVerifier::new().add_route(
        "provider-upstream",
        "https://provider.example",
        verifier,
    ));
    let backend = Arc::new(FixtureBackend::default());
    let mut router = ModelRouterBackend::new("fixture-router");
    for (public, upstream) in [
        ("public-a", "provider-model"),
        ("public-b", "other-provider-model"),
    ] {
        router
            .add_route(
                ModelRoute::new(
                    public,
                    upstream,
                    backend.clone(),
                    format!("provider-upstream:{public}"),
                )
                .unwrap()
                .with_is_tee(Some(true)),
            )
            .unwrap();
    }
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(SessionKeys(SigningKey::from_bytes(&[7; 32]))),
            Arc::new(UnusedQuoter),
            Arc::new(router),
            verifier,
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router_with_admin(service.clone(), manager, None);
    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/aci/sessions?model=public-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
    let id = listed["sessions"][0]["session_id"].as_str().unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "public-b", "messages": [], "provider": {"aci_session_ids": [id]},
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let receipt_id = response.headers()["x-receipt-id"].to_str().unwrap();
    let receipt = service
        .get_receipt_by_receipt_id(receipt_id)
        .unwrap()
        .document_json()
        .unwrap();
    let verified = receipt["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == "upstream.verified")
        .unwrap();
    assert_eq!(verified["session_id"], id);
    assert_eq!(verified["model_id"], "other-provider-model");
    assert_eq!(*backend.0.lock().unwrap(), ["other-provider-model"]);
    assert_eq!(
        std::fs::read_to_string(counter).unwrap(),
        "1",
        "the other model must reuse the real verifier cache"
    );
}
