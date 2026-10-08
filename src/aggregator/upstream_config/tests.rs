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
        basic_auth: false,
        accepted_subjects: None,
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
    // NEAR AI, Tinfoil, and SecretAI front many models behind one verified
    // channel, so they are per-router. Phala-direct verifies a TEE per model;
    // Chutes a key per instance; the rest default to per-model. Only per-router
    // drops the model from the channel identity.
    use AttestationScope::*;
    assert_eq!(UpstreamProvider::NearAi.attestation_scope(), PerRouter);
    assert_eq!(UpstreamProvider::Tinfoil.attestation_scope(), PerRouter);
    assert_eq!(UpstreamProvider::SecretAi.attestation_scope(), PerRouter);
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

#[test]
fn parse_secret_ai_allows_an_unpinned_workload() {
    let config = parse_config_text(
        r#"
            [{
              "name": "secret-ai",
              "provider": "secret-ai",
              "base_url": "https://secret.example:21434",
              "models": {"public-model": "upstream-model"}
            }]
            "#,
    )
    .expect("SecretAI should measure an unpinned workload by default");

    assert_eq!(config[0].provider, UpstreamProvider::SecretAi);
    assert_eq!(config[0].accepted_subjects, None);
}

#[test]
fn parse_secret_ai_rejects_invalid_origins() {
    for base_url in [
        "http://secret.example",
        "https://user@secret.example",
        "https://secret.example/evidence",
        "https://secret.example?target=other",
        "https://secret.example#fragment",
    ] {
        let err = parse_config_text(&format!(
            r#"[{{
              "name": "secret-ai",
              "provider": "secret-ai",
              "base_url": "{base_url}",
              "models": {{"public-model": "upstream-model"}}
            }}]"#
        ))
        .expect_err("SecretAI must reject an origin that the verifier cannot use");
        assert!(
            err.to_string().contains("requires a root HTTPS base_url"),
            "{base_url:?}: {err}"
        );
    }
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
        accepted_subjects: Vec::new(),
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
        basic_auth: false,
        accepted_subjects: None,
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
            accepted_subjects: Vec::new(),
            accepted_image_digests: Vec::new(),
            accepted_dstack_kms_root_public_keys: Vec::new(),
            pccs_url: None,
            verifier_cache_seconds: 300,
            connect_timeout_seconds: 10,
            read_timeout_seconds: 600,
            verifier_request_timeout_seconds: 60,
        },
        state,
        update_lock: Arc::new(Mutex::new(())),
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

#[derive(Default)]
struct RefreshObservations {
    completions: HashMap<(String, String), Vec<Instant>>,
    active: HashMap<String, usize>,
    peak: HashMap<String, usize>,
}

struct SleepingVerifier {
    latencies: HashMap<String, u64>,
    sleep_on_verify: bool,
    observations: Mutex<RefreshObservations>,
}

#[async_trait]
impl UpstreamVerifier for SleepingVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        if self.sleep_on_verify {
            return self.refresh(request).await;
        }
        self.observations
            .lock()
            .unwrap()
            .completions
            .entry((request.upstream_name.clone(), request.model_id.clone()))
            .or_default()
            .push(Instant::now());
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "sleeping-verifier/v1".to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            ..Default::default()
        }
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        {
            let mut observations = self.observations.lock().unwrap();
            let active = observations
                .active
                .entry(request.upstream_name.clone())
                .or_default();
            *active += 1;
            let active = *active;
            let peak = observations
                .peak
                .entry(request.upstream_name.clone())
                .or_default();
            *peak = (*peak).max(active);
        }
        tokio::time::sleep(Duration::from_secs(self.latencies[&request.upstream_name])).await;
        {
            let mut observations = self.observations.lock().unwrap();
            *observations.active.get_mut(&request.upstream_name).unwrap() -= 1;
            observations
                .completions
                .entry((request.upstream_name.clone(), request.model_id.clone()))
                .or_default()
                .push(Instant::now());
        }
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "sleeping-verifier/v1".to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            ..Default::default()
        }
    }
}

fn refresh_test_manager(
    config: Vec<UpstreamConfig>,
    verifier: Arc<dyn UpstreamVerifier>,
) -> Arc<UpstreamConfigManager> {
    Arc::new(UpstreamConfigManager {
        path: PathBuf::from("/unused/upstreams.json"),
        options: UpstreamRuntimeOptions {
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
        state: Arc::new(RwLock::new(Arc::new(ConfiguredUpstreams {
            config,
            config_digest: "fixture".to_string(),
            backend: Arc::new(EmptyUpstreamBackend),
            verifier: Some(verifier),
            sessions: Arc::new(ProviderSessionRegistry::default()),
        }))),
        update_lock: Arc::new(Mutex::new(())),
        session_sink: Arc::new(RwLock::new(None)),
    })
}

async fn drive_refresh_test(
    manager: Arc<UpstreamConfigManager>,
    verifier: &SleepingVerifier,
    duration: u64,
) {
    let started = Instant::now();
    manager.prewarm_upstream_verification().await;
    let refresh = tokio::spawn(manager.run_verification_refresh(|_| {}));
    tokio::task::yield_now().await;
    while started.elapsed() < Duration::from_secs(duration) {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    // Drain only the passes already started, leaving no detached test tasks.
    while verifier
        .observations
        .lock()
        .unwrap()
        .active
        .values()
        .any(|count| *count > 0)
    {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn tick_refresh_keeps_six_slow_upstreams_warm_without_extra_load() {
    let config: Vec<_> = (0..6)
        .map(|i| {
            test_upstream_config(
                &format!("provider-{i}"),
                UpstreamProvider::PhalaDirect,
                "public",
                "model",
            )
        })
        .collect();
    let verifier = Arc::new(SleepingVerifier {
        latencies: config.iter().map(|cfg| (cfg.name.clone(), 50)).collect(),
        sleep_on_verify: false,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let manager = refresh_test_manager(config, verifier.clone());
    drive_refresh_test(manager, &verifier, 1800).await;
    let observations = verifier.observations.lock().unwrap();
    assert_eq!(observations.completions.len(), 6);
    for times in observations.completions.values() {
        assert!(times.len() >= 7);
        assert!(times
            .windows(2)
            .all(|pair| pair[1] - pair[0] < Duration::from_secs(300)));
        assert!(
            times.len() <= 1800 / 240 + 1,
            "no refresh load beyond tick cadence"
        );
    }
    assert!(observations.peak.values().all(|peak| *peak == 1));
}

#[tokio::test(start_paused = true)]
async fn slow_upstream_pass_is_isolated_and_never_overlaps_itself() {
    let mut slow =
        test_upstream_config("slow", UpstreamProvider::PhalaDirect, "public-a", "model-a");
    // Two sequential targets make the group's pass outlast a tick.
    slow.models
        .insert("public-b".to_string(), "model-b".to_string());
    let healthy = test_upstream_config("healthy", UpstreamProvider::PhalaDirect, "public", "model");
    let verifier = Arc::new(SleepingVerifier {
        latencies: HashMap::from([("slow".to_string(), 200), ("healthy".to_string(), 1)]),
        sleep_on_verify: false,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let manager = refresh_test_manager(vec![slow, healthy], verifier.clone());
    drive_refresh_test(manager, &verifier, 1800).await;
    let observations = verifier.observations.lock().unwrap();
    let healthy_times = &observations.completions[&("healthy".to_string(), "model".to_string())];
    assert!(healthy_times
        .windows(2)
        .all(|pair| pair[1] - pair[0] < Duration::from_secs(300)));
    assert!(healthy_times.len() >= 5);
    assert_eq!(observations.peak["slow"], 1);
    for times in observations.completions.values() {
        assert!(times.len() <= 1800 / 240 + 1);
    }
}

#[test]
fn default_verification_interval_uses_effective_request_timeout() {
    let mut config =
        test_upstream_config("provider", UpstreamProvider::PhalaDirect, "public", "model");
    let manager = refresh_test_manager(
        vec![config.clone()],
        Arc::new(CountingVerifier {
            verifications: Arc::new(AtomicUsize::new(0)),
            invalidations: Arc::new(AtomicUsize::new(0)),
        }),
    );
    assert_eq!(manager.verification_refresh_interval_seconds(), Some(240));
    config.verifier_request_timeout_seconds = Some(90);
    assert_eq!(
        verification_refresh_seconds(&config, &manager.options),
        Some(210)
    );
    config.verifier_cache_seconds = Some(80);
    assert_eq!(
        verification_refresh_seconds(&config, &manager.options),
        Some(1)
    );
    config.verification_refresh_seconds = Some(0);
    assert_eq!(
        verification_refresh_seconds(&config, &manager.options),
        None
    );
}

#[test]
fn current_requests_filter_per_model_and_router_channels() {
    let mut per_model = test_upstream_config(
        "per-model",
        UpstreamProvider::PhalaDirect,
        "public-a",
        "up-a",
    );
    per_model
        .models
        .insert("public-b".to_string(), "up-b".to_string());
    let mut router = test_upstream_config("router", UpstreamProvider::NearAi, "public-a", "up-a");
    router
        .models
        .insert("public-b".to_string(), "up-b".to_string());
    let manager = refresh_test_manager(
        vec![per_model, router],
        Arc::new(CountingVerifier {
            verifications: Arc::new(AtomicUsize::new(0)),
            invalidations: Arc::new(AtomicUsize::new(0)),
        }),
    );
    assert_eq!(manager.current_verification_requests(None, None).len(), 3);
    for model in ["public-b", "up-b"] {
        let requests = manager.current_verification_requests(Some(model), None);
        assert_eq!(requests.len(), 2);
        let per_model = requests
            .iter()
            .find(|request| request.upstream_name == "per-model")
            .unwrap();
        assert_eq!(per_model.model_id, "up-b");
        assert_eq!(per_model.forwarded_body_hash, digest::sha256_hex(b""));
        assert!(per_model.required);
        assert_eq!(
            manager
                .current_verification_requests(Some(model), Some("per-model"))
                .len(),
            1
        );
    }
    assert!(manager
        .current_verification_requests(Some("unknown"), None)
        .is_empty());
}

#[tokio::test(start_paused = true)]
async fn prewarm_groups_run_concurrently_with_sequential_targets() {
    let mut first = test_upstream_config(
        "first",
        UpstreamProvider::PhalaDirect,
        "public-a",
        "model-a",
    );
    first
        .models
        .insert("public-b".to_string(), "model-b".to_string());
    let second = test_upstream_config("second", UpstreamProvider::PhalaDirect, "public", "model");
    let verifier = Arc::new(SleepingVerifier {
        latencies: HashMap::from([("first".to_string(), 50), ("second".to_string(), 50)]),
        sleep_on_verify: true,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let manager = refresh_test_manager(vec![first, second], verifier.clone());
    let started = Instant::now();
    assert_eq!(manager.prewarm_upstream_verification().await.len(), 3);
    assert_eq!(started.elapsed(), Duration::from_secs(100));
    let observations = verifier.observations.lock().unwrap();
    assert_eq!(observations.peak["first"], 1);
    assert_eq!(observations.peak["second"], 1);
    assert_eq!(
        observations.completions[&("second".to_string(), "model".to_string())][0] - started,
        Duration::from_secs(50)
    );
}
