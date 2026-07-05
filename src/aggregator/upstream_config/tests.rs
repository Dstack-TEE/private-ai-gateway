use super::builders::build_verifier;
use super::dynamic::{DynamicUpstreamVerifier, EmptyUpstreamBackend};
use super::*;
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingVerifier {
    verifications: Arc<AtomicUsize>,
    invalidations: Arc<AtomicUsize>,
}

fn test_upstream_config(
    name: &str,
    provider: UpstreamProvider,
    public_model: &str,
    upstream_model: &str,
) -> UpstreamConfig {
    UpstreamConfig {
        name: name.to_string(),
        provider,
        base_url: format!("https://{name}.example"),
        path: None,
        models: BTreeMap::from([(public_model.to_string(), upstream_model.to_string())]),
        bearer_token: None,
        accepted_workload_ids: None,
        accepted_image_digests: None,
        accepted_dstack_kms_root_public_keys: None,
        pccs_url: None,
        verifier_cache_seconds: None,
        connect_timeout_seconds: None,
        read_timeout_seconds: None,
        verifier_request_timeout_seconds: None,
        verification_refresh_seconds: None,
        session_refresh_seconds: None,
        chutes_e2ee_api_base: None,
        chutes_chute_ids: None,
        chutes_e2ee_discovery_rounds: None,
        chutes_e2ee_discovery_interval_seconds: None,
    }
}

#[test]
fn router_provider_verifies_once_per_channel() {
    // A router (NEAR AI) with several models yields ONE verification target — the
    // shared gateway channel — so it seals one session per channel, not one per
    // model. A per-model provider keeps one target per model.
    let mut router = test_upstream_config("near-router", UpstreamProvider::NearAi, "pub-a", "up-a");
    router
        .models
        .insert("pub-b".to_string(), "up-b".to_string());
    assert_eq!(
        super::validation::verification_targets(std::slice::from_ref(&router)).len(),
        1,
        "router collapses its models to one channel target"
    );

    let mut per_model =
        test_upstream_config("phala", UpstreamProvider::PhalaDirect, "pub-a", "up-a");
    per_model
        .models
        .insert("pub-b".to_string(), "up-b".to_string());
    assert_eq!(
        super::validation::verification_targets(std::slice::from_ref(&per_model)).len(),
        2,
        "per-model provider verifies every model"
    );
}

#[test]
fn provider_attestation_scopes() {
    // NEAR AI (gateway TD) and Tinfoil (confidential-model-router) front many
    // models behind one verified channel, so they are per-router. Phala-direct
    // verifies a TEE per model; Chutes a key per instance; the rest default to
    // per-model. Only per-router drops the model from the channel identity.
    use AttestationScope::*;
    assert_eq!(UpstreamProvider::NearAi.attestation_scope(), PerRouter);
    assert_eq!(UpstreamProvider::Tinfoil.attestation_scope(), PerRouter);
    assert_eq!(UpstreamProvider::PhalaDirect.attestation_scope(), PerModel);
    assert_eq!(UpstreamProvider::Chutes.attestation_scope(), PerInstance);
    assert_eq!(
        UpstreamProvider::OpenAiCompatible.attestation_scope(),
        PerModel
    );
    assert_eq!(UpstreamProvider::AciService.attestation_scope(), PerModel);
    assert!(UpstreamProvider::NearAi.attestation_scope().is_per_router());
    assert!(!UpstreamProvider::Chutes.attestation_scope().is_per_router());
}

#[async_trait]
impl UpstreamVerifier for CountingVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifications.fetch_add(1, Ordering::SeqCst);
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "counting-verifier/v1".to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            ..Default::default()
        }
    }

    fn invalidate(&self, _request: &UpstreamVerificationRequest) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn parse_config_allows_same_public_model_on_distinct_route_ids() {
    let config = parse_config_text(
        r#"
            [
              {
                "name": "near-ai",
                "provider": "near-ai",
                "base_url": "https://near.example",
                "models": {"openai/gpt-oss-120b": "near-model"}
              },
              {
                "name": "secretai-107",
                "provider": "openai-compatible",
                "base_url": "https://secret.example",
                "models": {"openai/gpt-oss-120b": "secret-model"}
              }
            ]
            "#,
    )
    .expect("same public model can have multiple route ids");

    assert_eq!(config.len(), 2);
}

#[test]
fn parse_config_rejects_preverified_provider() {
    let err = parse_config_text(
        r#"
            [
              {
                "name": "fixture",
                "provider": "preverified",
                "base_url": "https://fixture.example",
                "models": {"public-model": "upstream-model"}
              }
            ]
            "#,
    )
    .expect_err("preverified must not be accepted as upstream config");

    assert!(err.to_string().contains("unknown variant"));
}

#[test]
fn parse_config_rejects_attestation_report_base_url() {
    let err = parse_config_text(
        r#"
            [
              {
                "name": "aci",
                "provider": "aci-service",
                "base_url": "https://aci.example",
                "attestation_report_base_url": "http://aci.internal:8086",
                "models": {"public-model": "upstream-model"}
              }
            ]
            "#,
    )
    .expect_err("attestation report URL must not be configured separately from base_url");

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn global_aci_service_does_not_require_policy_for_plain_openai_compatible_upstreams() {
    let config = vec![
        test_upstream_config(
            "near-ai",
            UpstreamProvider::NearAi,
            "openai/gpt-oss-120b",
            "near-model",
        ),
        test_upstream_config(
            "secretai-107",
            UpstreamProvider::OpenAiCompatible,
            "openai/gpt-oss-120b",
            "secret-model",
        ),
    ];
    let options = UpstreamRuntimeOptions {
        verifier_mode: UpstreamVerifierMode::AciService,
        accepted_workload_ids: Vec::new(),
        accepted_image_digests: Vec::new(),
        accepted_dstack_kms_root_public_keys: Vec::new(),
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 600,
        verifier_request_timeout_seconds: 60,
    };

    let verifier = build_verifier(&config, &options, &ProviderSessionRegistry::default())
        .expect("plain OpenAI-compatible upstreams should not require ACI service policy");

    assert!(verifier.is_some());
}

#[tokio::test]
async fn dynamic_verifier_forwards_invalidation_to_current_verifier() {
    let verifications = Arc::new(AtomicUsize::new(0));
    let invalidations = Arc::new(AtomicUsize::new(0));
    let state = Arc::new(RwLock::new(Arc::new(ConfiguredUpstreams {
        config: Vec::new(),
        config_digest: "fixture".to_string(),
        backend: Arc::new(EmptyUpstreamBackend),
        verifier: Some(Arc::new(CountingVerifier {
            verifications,
            invalidations: invalidations.clone(),
        })),
        sessions: Arc::new(ProviderSessionRegistry::default()),
    })));
    let verifier = DynamicUpstreamVerifier { state };
    let request = UpstreamVerificationRequest {
        upstream_name: "provider-a".to_string(),
        url_origin: Some("https://provider-a.example".to_string()),
        model_id: "model-a".to_string(),
        forwarded_body_hash: "00".repeat(32),
        required: true,
    };

    verifier.invalidate(&request);

    assert_eq!(invalidations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn prewarm_verification_deduplicates_upstream_models() {
    let verifications = Arc::new(AtomicUsize::new(0));
    let invalidations = Arc::new(AtomicUsize::new(0));
    let config = vec![UpstreamConfig {
        name: "provider-a".to_string(),
        // Per-model provider (not a router): two public models sharing one
        // upstream model dedup to one target; a third yields a second.
        provider: UpstreamProvider::PhalaDirect,
        base_url: "https://provider-a.example/".to_string(),
        path: None,
        models: BTreeMap::from([
            ("public-a".to_string(), "upstream-a".to_string()),
            ("public-b".to_string(), "upstream-a".to_string()),
            ("public-c".to_string(), "upstream-c".to_string()),
        ]),
        bearer_token: None,
        accepted_workload_ids: None,
        accepted_image_digests: None,
        accepted_dstack_kms_root_public_keys: None,
        pccs_url: None,
        verifier_cache_seconds: None,
        connect_timeout_seconds: None,
        read_timeout_seconds: None,
        verifier_request_timeout_seconds: None,
        verification_refresh_seconds: None,
        session_refresh_seconds: None,
        chutes_e2ee_api_base: None,
        chutes_chute_ids: None,
        chutes_e2ee_discovery_rounds: None,
        chutes_e2ee_discovery_interval_seconds: None,
    }];
    let state = Arc::new(RwLock::new(Arc::new(ConfiguredUpstreams {
        config,
        config_digest: "fixture".to_string(),
        backend: Arc::new(EmptyUpstreamBackend),
        verifier: Some(Arc::new(CountingVerifier {
            verifications: verifications.clone(),
            invalidations,
        })),
        sessions: Arc::new(ProviderSessionRegistry::default()),
    })));
    let manager = UpstreamConfigManager {
        path: PathBuf::from("/tmp/upstreams.json"),
        options: UpstreamRuntimeOptions {
            verifier_mode: UpstreamVerifierMode::None,
            accepted_workload_ids: Vec::new(),
            accepted_image_digests: Vec::new(),
            accepted_dstack_kms_root_public_keys: Vec::new(),
            pccs_url: None,
            verifier_cache_seconds: 300,
            connect_timeout_seconds: 10,
            read_timeout_seconds: 600,
            verifier_request_timeout_seconds: 60,
        },
        state,
        session_sink: Arc::new(RwLock::new(None)),
    };

    let results = manager.prewarm_upstream_verification().await;

    assert_eq!(results.len(), 2);
    assert_eq!(verifications.load(Ordering::SeqCst), 2);
    assert_eq!(
        results[0].url_origin.as_deref(),
        Some("https://provider-a.example")
    );
}
