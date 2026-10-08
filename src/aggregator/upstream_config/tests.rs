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
    starts: HashMap<(String, String), Vec<Instant>>,
    completions: HashMap<(String, String), Vec<Instant>>,
    active: HashMap<String, usize>,
    peak: HashMap<String, usize>,
}

#[derive(Clone, Default)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct SleepingVerifier {
    latencies: HashMap<String, u64>,
    sleep_on_verify: bool,
    observations: Mutex<RefreshObservations>,
}

impl SleepingVerifier {
    fn event_for(&self, request: &UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name.clone(),
            model_id: request.model_id.clone(),
            url_origin: request.url_origin.clone(),
            verifier_id: "sleeping-verifier/v1".to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            ..Default::default()
        }
    }
}

#[async_trait]
impl UpstreamVerifier for SleepingVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        if let Some(event) = self.cached(&request) {
            return event;
        }
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
        self.event_for(&request)
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        let observations = self.observations.lock().unwrap();
        let completed_at = observations
            .completions
            .get(&(request.upstream_name.clone(), request.model_id.clone()))?
            .last()?;
        (completed_at.elapsed() < Duration::from_secs(300)).then(|| self.event_for(request))
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        {
            let mut observations = self.observations.lock().unwrap();
            observations
                .starts
                .entry((request.upstream_name.clone(), request.model_id.clone()))
                .or_default()
                .push(Instant::now());
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
        self.event_for(&request)
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

fn replace_refresh_test_config(manager: &UpstreamConfigManager, config: Vec<UpstreamConfig>) {
    let state = manager.state.read().unwrap().clone();
    *manager.state.write().unwrap() = Arc::new(ConfiguredUpstreams {
        config,
        config_digest: "fixture".to_string(),
        backend: state.backend.clone(),
        verifier: state.verifier.clone(),
        sessions: state.sessions.clone(),
    });
}

async fn advance_refresh_time(seconds: u64) {
    for _ in 0..seconds {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
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
        advance_refresh_time(1).await;
    }
    stop_refresh_test(refresh, verifier).await;
}

async fn stop_refresh_test(refresh: JoinHandle<()>, verifier: &SleepingVerifier) {
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
        advance_refresh_time(1).await;
    }
}

fn refresh_load_bound(first_due: Duration, period: Duration, window: Duration) -> usize {
    std::iter::successors(Some(first_due), |due| Some(*due + period))
        .take_while(|due| *due <= window)
        .count()
        + 1
}

#[tokio::test(start_paused = true)]
async fn tick_refresh_keeps_six_slow_upstreams_warm_without_extra_load() {
    let writer = LogWriter::default();
    let output = writer.0.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
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
    let period = Duration::from_secs(manager.verification_refresh_interval_seconds().unwrap());
    let ttl = Duration::from_secs(manager.options.verifier_cache_seconds);
    let started = Instant::now();
    drive_refresh_test(manager, &verifier, 1800).await;
    let observations = verifier.observations.lock().unwrap();
    assert_eq!(observations.completions.len(), 6);
    for i in 0..6 {
        let key = (format!("provider-{i}"), "model".to_string());
        let starts = &observations.starts[&key];
        let times = &observations.completions[&key];
        let first_due = period * (i + 1) / 6;
        assert_eq!(starts[0] - started, first_due);
        assert!(starts[0] - started <= period);
        assert!(
            times[1] - times[0] < ttl,
            "first replacement must precede prewarm expiry"
        );
        assert!(times.len() >= 7);
        assert!(times.windows(2).all(|pair| pair[1] - pair[0] < ttl));
        assert!(starts.windows(2).all(|pair| pair[1] - pair[0] == period));
        assert!(
            times.len() <= refresh_load_bound(first_due, period, Duration::from_secs(1800)),
            "no refresh load beyond tick cadence"
        );
    }
    assert!(observations.peak.values().all(|peak| *peak == 1));
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains("cache was cold before refresh"), "{logs}");
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
    let period = Duration::from_secs(manager.verification_refresh_interval_seconds().unwrap());
    drive_refresh_test(manager, &verifier, 1800).await;
    let observations = verifier.observations.lock().unwrap();
    let healthy_times = &observations.completions[&("healthy".to_string(), "model".to_string())];
    assert!(healthy_times
        .windows(2)
        .all(|pair| pair[1] - pair[0] < Duration::from_secs(300)));
    assert!(healthy_times.len() >= 5);
    assert_eq!(observations.peak["slow"], 1);
    for (upstream, first_due) in [("healthy", period / 2), ("slow", period)] {
        let bound = refresh_load_bound(first_due, period, Duration::from_secs(1800));
        for ((name, _), times) in &observations.completions {
            if name == upstream {
                assert!(times.len() <= bound);
            }
        }
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
    for (model, expected) in [("public-b", "up-b"), ("public-a", "up-a")] {
        let requests = manager.current_verification_requests(Some(model), None);
        assert_eq!(requests.len(), 2);
        let per_model = requests
            .iter()
            .find(|request| request.upstream_name == "per-model")
            .unwrap();
        assert_eq!(per_model.model_id, expected);
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
        .current_verification_requests(Some("up-b"), None)
        .is_empty());
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

#[tokio::test(start_paused = true)]
async fn refresh_groups_have_distinct_phases_and_keep_their_period() {
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
        latencies: config.iter().map(|cfg| (cfg.name.clone(), 1)).collect(),
        sleep_on_verify: false,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let manager = refresh_test_manager(config, verifier.clone());
    let epoch = Instant::now();
    drive_refresh_test(manager, &verifier, 1800).await;
    let observations = verifier.observations.lock().unwrap();
    let mut all_starts = std::collections::HashSet::new();
    for i in 0..6 {
        let starts = &observations.starts[&(format!("provider-{i}"), "model".to_string())];
        assert_eq!(starts[0] - epoch, Duration::from_secs((i + 1) * 240 / 6));
        assert!(starts.len() >= 6);
        assert!(starts
            .windows(2)
            .all(|pair| pair[1] - pair[0] == Duration::from_secs(240)));
        for started in starts {
            assert!(
                all_starts.insert(*started),
                "upstream groups must not burst together"
            );
        }
    }
}

#[test]
fn current_requests_resolve_aliases_before_upstream_model_ids() {
    let mut cfg = test_upstream_config("provider", UpstreamProvider::PhalaDirect, "a", "x");
    cfg.models.insert("x".to_string(), "y".to_string());
    let manager = refresh_test_manager(
        vec![cfg],
        Arc::new(CountingVerifier {
            verifications: Arc::new(AtomicUsize::new(0)),
            invalidations: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let requests = manager.current_verification_requests(Some("x"), None);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model_id, "y");
    assert!(manager
        .current_verification_requests(Some("y"), None)
        .is_empty());
}

#[tokio::test(start_paused = true)]
async fn refresh_warns_only_when_a_previously_refreshed_targets_cache_is_cold() {
    struct ExpiringVerifier(Mutex<HashMap<String, (Instant, UpstreamVerifiedEvent)>>);
    #[async_trait]
    impl UpstreamVerifier for ExpiringVerifier {
        async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
            if let Some(event) = self.cached(&request) {
                return event;
            }
            self.refresh(request).await
        }

        fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
            self.0
                .lock()
                .unwrap()
                .get(&request.model_id)
                .filter(|(expires_at, _)| Instant::now() < *expires_at)
                .map(|(_, event)| event.clone())
        }

        async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let ttl = if request.model_id == "model-a" {
                600
            } else {
                100
            };
            let event = UpstreamVerifiedEvent {
                upstream_name: request.upstream_name,
                model_id: request.model_id,
                result: VerificationResult::Verified,
                ..Default::default()
            };
            self.0.lock().unwrap().insert(
                event.model_id.clone(),
                (Instant::now() + Duration::from_secs(ttl), event.clone()),
            );
            event
        }
    }
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = LogWriter(output.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let mut cfg = test_upstream_config(
        "provider",
        UpstreamProvider::PhalaDirect,
        "public-a",
        "model-a",
    );
    cfg.models
        .insert("public-b".to_string(), "model-b".to_string());
    let targets = verification_targets(&[cfg]);
    let verifier = Arc::new(ExpiringVerifier(Mutex::new(HashMap::new())));
    let history = VerificationRefreshHistory::default();
    history.set_targets(&targets);
    run_verification_group(
        verifier.clone(),
        targets.clone(),
        None,
        Some(history.clone()),
    )
    .await;
    assert!(
        output.lock().unwrap().is_empty(),
        "first refresh has no prior success"
    );
    tokio::time::advance(Duration::from_secs(240)).await;
    assert!(verifier.cached(&targets[0].request()).is_some());
    assert!(verifier.cached(&targets[1].request()).is_none());
    run_verification_group(verifier.clone(), targets.clone(), None, Some(history)).await;
    assert!(verifier.cached(&targets[1].request()).is_some());
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert_eq!(logs.lines().count(), 1);
    assert!(logs.contains("upstream=provider"), "{logs}");
    assert!(logs.contains("model=model-b"), "{logs}");
    assert!(logs.contains("seconds_since_last_success=241"), "{logs}");
    assert!(logs.contains("cache was cold before refresh"), "{logs}");
}

#[tokio::test(start_paused = true)]
async fn removed_targets_do_not_restore_history_when_their_inflight_refresh_finishes() {
    let cfg = test_upstream_config("provider", UpstreamProvider::PhalaDirect, "public", "model");
    let targets = verification_targets(&[cfg]);
    let verifier = Arc::new(SleepingVerifier {
        latencies: HashMap::from([("provider".to_string(), 50)]),
        sleep_on_verify: false,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let history = VerificationRefreshHistory::default();
    history.set_targets(&targets);
    let pass = tokio::spawn(run_verification_group(
        verifier.clone(),
        targets,
        None,
        Some(history.clone()),
    ));
    tokio::task::yield_now().await;
    assert_eq!(verifier.observations.lock().unwrap().active["provider"], 1);
    history.set_targets(&[]);
    assert_eq!(pass.await.unwrap()[0].result, "verified");
    assert!(history.last_success.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn config_changes_preserve_due_times_without_restarting_inflight_passes() {
    let mut slow = test_upstream_config(
        "provider-0",
        UpstreamProvider::PhalaDirect,
        "public",
        "model",
    );
    let healthy = test_upstream_config(
        "provider-1",
        UpstreamProvider::PhalaDirect,
        "public",
        "model",
    );
    let verifier = Arc::new(SleepingVerifier {
        latencies: HashMap::from([
            ("provider-0".to_string(), 500),
            ("provider-1".to_string(), 1),
        ]),
        sleep_on_verify: false,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let manager = refresh_test_manager(vec![slow.clone()], verifier.clone());
    let epoch = Instant::now();
    let refresh = tokio::spawn(manager.clone().run_verification_refresh(|_| {}));
    tokio::task::yield_now().await;
    advance_refresh_time(300).await;
    replace_refresh_test_config(&manager, vec![slow.clone(), healthy]);
    advance_refresh_time(600).await;
    slow.verification_refresh_seconds = Some(60);
    replace_refresh_test_config(&manager, vec![slow]);
    advance_refresh_time(700).await;
    stop_refresh_test(refresh, &verifier).await;
    let observations = verifier.observations.lock().unwrap();
    let slow = &observations.starts[&("provider-0".to_string(), "model".to_string())];
    assert_eq!(
        slow.iter()
            .map(|time| (*time - epoch).as_secs())
            .collect::<Vec<_>>(),
        [240, 960, 1500]
    );
    let healthy = &observations.starts[&("provider-1".to_string(), "model".to_string())];
    assert_eq!(
        healthy
            .iter()
            .map(|time| (*time - epoch).as_secs())
            .collect::<Vec<_>>(),
        [720]
    );
    assert_eq!(observations.peak["provider-0"], 1);
}

#[tokio::test(start_paused = true)]
async fn adding_an_upstream_preserves_existing_groups_next_refreshes() {
    let config: Vec<_> = (0..3)
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
        latencies: config.iter().map(|cfg| (cfg.name.clone(), 1)).collect(),
        sleep_on_verify: false,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let manager =
        refresh_test_manager(vec![config[0].clone(), config[2].clone()], verifier.clone());
    let epoch = Instant::now();
    let refresh = tokio::spawn(manager.clone().run_verification_refresh(|_| {}));
    tokio::task::yield_now().await;
    advance_refresh_time(350).await;
    replace_refresh_test_config(&manager, config);
    advance_refresh_time(400).await;
    stop_refresh_test(refresh, &verifier).await;
    let observations = verifier.observations.lock().unwrap();
    for (name, expected) in [
        ("provider-0", vec![120, 360, 600]),
        ("provider-1", vec![520]),
        ("provider-2", vec![240, 480, 720]),
    ] {
        let starts = &observations.starts[&(name.to_string(), "model".to_string())];
        assert_eq!(
            starts
                .iter()
                .map(|time| (*time - epoch).as_secs())
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[tokio::test(start_paused = true)]
async fn reducing_the_interval_pulls_in_existing_due_times() {
    let mut config: Vec<_> = (0..2)
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
        latencies: config.iter().map(|cfg| (cfg.name.clone(), 1)).collect(),
        sleep_on_verify: false,
        observations: Mutex::new(RefreshObservations::default()),
    });
    let manager = refresh_test_manager(config.clone(), verifier.clone());
    let epoch = Instant::now();
    let refresh = tokio::spawn(manager.clone().run_verification_refresh(|_| {}));
    tokio::task::yield_now().await;
    advance_refresh_time(110).await;
    for cfg in &mut config {
        cfg.verification_refresh_seconds = Some(60);
    }
    replace_refresh_test_config(&manager, config);
    advance_refresh_time(390).await;
    stop_refresh_test(refresh, &verifier).await;
    let observations = verifier.observations.lock().unwrap();
    for (name, first) in [("provider-0", 120), ("provider-1", 180)] {
        let starts = &observations.starts[&(name.to_string(), "model".to_string())];
        assert_eq!(starts[0] - epoch, Duration::from_secs(first));
        assert!(starts.len() >= 6);
        assert!(starts
            .windows(2)
            .all(|pair| pair[1] - pair[0] == Duration::from_secs(60)));
    }
}
