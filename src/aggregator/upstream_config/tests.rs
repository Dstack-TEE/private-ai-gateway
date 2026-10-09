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
        crate::aci::verifier::PreverifiedUpstreamVerifier::new("counting-verifier/v1")
            .verify(request)
            .await
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        crate::aci::verifier::PreverifiedUpstreamVerifier::new("counting-verifier/v1")
            .cached(request)
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

#[derive(Default)]
struct RefreshObservations {
    starts: HashMap<(String, String), Vec<(Instant, bool)>>,
    replacements: Vec<(Instant, Instant)>,
    active: HashMap<String, usize>,
    peak: HashMap<String, usize>,
    global_peak: usize,
}

struct FakeVerifier {
    ttl: Duration,
    latency: Duration,
    service_cache: bool,
    failed_model: Option<(String, Duration)>,
    fail: std::sync::atomic::AtomicBool,
    cache: Mutex<HashMap<String, Instant>>,
    observations: Arc<Mutex<RefreshObservations>>,
}

impl FakeVerifier {
    fn new(ttl: u64, latency: u64) -> Self {
        Self {
            ttl: Duration::from_secs(ttl),
            latency: Duration::from_secs(latency),
            service_cache: false,
            failed_model: None,
            fail: std::sync::atomic::AtomicBool::new(false),
            cache: Mutex::new(HashMap::new()),
            observations: Arc::new(Mutex::new(RefreshObservations::default())),
        }
    }

    fn key(&self, request: &UpstreamVerificationRequest) -> String {
        if self.service_cache {
            String::new()
        } else {
            request.model_id.clone()
        }
    }

    fn event(
        request: &UpstreamVerificationRequest,
        result: VerificationResult,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name.clone(),
            model_id: request.model_id.clone(),
            url_origin: request.url_origin.clone(),
            result,
            ..Default::default()
        }
    }
}

struct ActiveRefresh<'a>(&'a FakeVerifier, String);
impl Drop for ActiveRefresh<'_> {
    fn drop(&mut self) {
        *self
            .0
            .observations
            .lock()
            .unwrap()
            .active
            .get_mut(&self.1)
            .unwrap() -= 1;
    }
}

#[async_trait]
impl UpstreamVerifier for FakeVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        if let Some(event) = self.cached(&request) {
            return event;
        }
        self.refresh(request).await
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        self.cache_remaining(request)
            .map(|_| Self::event(request, VerificationResult::Verified))
    }

    fn cache_remaining(&self, request: &UpstreamVerificationRequest) -> Option<Duration> {
        self.cache
            .lock()
            .unwrap()
            .get(&self.key(request))?
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let started = Instant::now();
        let cold = self.cache_remaining(&request).is_none();
        {
            let mut observations = self.observations.lock().unwrap();
            observations
                .starts
                .entry((request.upstream_name.clone(), request.model_id.clone()))
                .or_default()
                .push((started, cold));
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
            observations.global_peak = observations
                .global_peak
                .max(observations.active.values().sum());
        }
        let _active = ActiveRefresh(self, request.upstream_name.clone());
        let failed_model = self
            .failed_model
            .as_ref()
            .filter(|(model, _)| *model == request.model_id);
        tokio::time::sleep(failed_model.map_or(self.latency, |(_, latency)| *latency)).await;
        let result = if self.fail.load(Ordering::SeqCst) || failed_model.is_some() {
            VerificationResult::Failed
        } else {
            let previous_expiry = self
                .cache
                .lock()
                .unwrap()
                .insert(self.key(&request), started + self.ttl);
            if let Some(expiry) = previous_expiry {
                self.observations
                    .lock()
                    .unwrap()
                    .replacements
                    .push((Instant::now(), expiry));
            }
            VerificationResult::Verified
        };
        Self::event(&request, result)
    }
}

fn refresh_test_manager(
    config: Vec<UpstreamConfig>,
    verifier: Arc<dyn UpstreamVerifier>,
) -> Arc<UpstreamConfigManager> {
    let options = UpstreamRuntimeOptions {
        verifier_mode: UpstreamVerifierMode::None,
        accepted_subjects: Vec::new(),
        accepted_image_digests: Vec::new(),
        accepted_dstack_kms_root_public_keys: Vec::new(),
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 600,
        verifier_request_timeout_seconds: 60,
    };
    let mut state = build_state(&config, &options).unwrap();
    state.verifier = Some(verifier);
    Arc::new(UpstreamConfigManager {
        path: PathBuf::from("/unused/upstreams.json"),
        options,
        state: Arc::new(RwLock::new(Arc::new(state))),
        update_lock: Arc::new(Mutex::new(())),
        reload: watch::channel(()).0,
    })
}

async fn advance(seconds: u64) {
    for _ in 0..seconds {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
}

fn start_refresh(
    manager: Arc<UpstreamConfigManager>,
    permits: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(manager.run_verification_refresh(Arc::new(Semaphore::new(permits))))
}

async fn stop_refresh(task: tokio::task::JoinHandle<()>) {
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::task::yield_now().await;
}

async fn run_refresh(
    config: Vec<UpstreamConfig>,
    verifier: Arc<dyn UpstreamVerifier>,
    seconds: u64,
) {
    let task = start_refresh(refresh_test_manager(config, verifier), 4);
    tokio::task::yield_now().await;
    advance(seconds).await;
    stop_refresh(task).await;
}

fn starts<'a>(o: &'a RefreshObservations, upstream: &str, model: &str) -> &'a [(Instant, bool)] {
    &o.starts[&(upstream.to_string(), model.to_string())]
}

#[tokio::test(start_paused = true)]
async fn eleven_sequential_targets_replace_caches_before_expiry() {
    let mut cfg = test_upstream_config(
        "provider",
        UpstreamProvider::PhalaDirect,
        "public-00",
        "model-00",
    );
    for i in 1..11 {
        cfg.models
            .insert(format!("public-{i:02}"), format!("model-{i:02}"));
    }
    cfg.models
        .insert("duplicate".to_string(), "model-00".to_string());
    let verifier = Arc::new(FakeVerifier::new(300, 24));
    run_refresh(vec![cfg], verifier.clone(), 1800).await;
    let observations = verifier.observations.lock().unwrap();
    assert_eq!(observations.starts.len(), 11);
    assert_eq!(observations.peak["provider"], 1);
    let mut total = 0;
    for starts in observations.starts.values() {
        assert!(starts.len() >= 6);
        assert!(starts[1..].iter().all(|(_, cold)| !cold));
        total += starts.len();
    }
    assert!(total <= 1800_u64.div_ceil(250) as usize * 11 + 11);
    assert!(!observations.replacements.is_empty());
    for (completed, previous_expiry) in &observations.replacements {
        assert!(
            completed < previous_expiry,
            "replacement completion relative to previous expiry: {:?}",
            completed.checked_duration_since(*previous_expiry)
        );
    }
    assert!(verifier
        .cache
        .lock()
        .unwrap()
        .values()
        .all(|expiry| Instant::now() < *expiry));
}

#[tokio::test(start_paused = true)]
async fn twenty_upstreams_share_four_background_permits_without_early_refreshes() {
    let observations = Arc::new(Mutex::new(RefreshObservations::default()));
    let mut routing = crate::aci::verifier::RoutingUpstreamVerifier::new();
    let mut config: Vec<_> = (0..20)
        .map(|i| {
            let cfg = test_upstream_config(
                &format!("provider-{i}"),
                UpstreamProvider::PhalaDirect,
                "public",
                "model",
            );
            let mut verifier = FakeVerifier::new(300, 25);
            verifier.observations = observations.clone();
            routing = std::mem::take(&mut routing).add_route(
                &cfg.name,
                &cfg.base_url,
                Arc::new(verifier),
            );
            cfg
        })
        .collect();
    let plain = test_upstream_config(
        "plain",
        UpstreamProvider::OpenAiCompatible,
        "public",
        "model",
    );
    let mut plain_verifier = FakeVerifier::new(300, 25);
    plain_verifier.observations = observations.clone();
    routing = routing.add_route(&plain.name, &plain.base_url, Arc::new(plain_verifier));
    config.push(plain);
    run_refresh(config, Arc::new(routing), 900).await;
    let observations = observations.lock().unwrap();
    assert_eq!(observations.starts.len(), 20);
    assert!(!observations
        .starts
        .keys()
        .any(|(upstream, _)| upstream == "plain"));
    assert_eq!(observations.global_peak, 4);
    assert!(observations.peak.values().all(|peak| *peak == 1));
    assert!(observations.active.values().all(|active| *active == 0));
    for starts in observations.starts.values() {
        assert!(starts.len() >= 3);
        assert!(starts
            .windows(2)
            .all(|pair| pair[1].0 - pair[0].0 >= Duration::from_secs(240)));
    }
}

#[tokio::test(start_paused = true)]
async fn hot_reload_aborts_old_tasks_and_refreshes_new_verifiers_immediately() {
    let mut cfg =
        test_upstream_config("provider", UpstreamProvider::PhalaDirect, "public", "model");
    let old = Arc::new(FakeVerifier::new(300, 25));
    let manager = refresh_test_manager(vec![cfg.clone()], old.clone());
    let task = start_refresh(manager.clone(), 4);
    tokio::task::yield_now().await;
    advance(245).await; // An old verification is in flight.
    let changed_at = Instant::now();
    cfg.verifier_cache_seconds = Some(60);
    cfg.verifier_request_timeout_seconds = Some(10);
    let new = Arc::new(FakeVerifier::new(60, 1));
    let mut state = build_state(&[cfg], &manager.options).unwrap();
    state.verifier = Some(new.clone());
    manager.install_state(Arc::new(state));
    tokio::task::yield_now().await;
    advance(201).await;
    stop_refresh(task).await;
    let old = old.observations.lock().unwrap();
    assert_eq!(starts(&old, "provider", "model").len(), 2);
    assert_eq!(old.active["provider"], 0);
    let new = new.observations.lock().unwrap();
    let starts = starts(&new, "provider", "model");
    assert_eq!(starts[0].0, changed_at);
    assert_eq!(starts.len(), 5);
    assert!(starts
        .windows(2)
        .all(|pair| pair[1].0 - pair[0].0 == Duration::from_secs(50)));
}

#[tokio::test(start_paused = true)]
async fn failed_refresh_retries_at_expiry_then_once_per_period() {
    let cfg = test_upstream_config("provider", UpstreamProvider::PhalaDirect, "public", "model");
    let verifier = Arc::new(FakeVerifier::new(300, 0));
    let request = verification_targets(std::slice::from_ref(&cfg))[0].request();
    let epoch = Instant::now();
    let task = start_refresh(refresh_test_manager(vec![cfg], verifier.clone()), 4);
    tokio::task::yield_now().await;
    advance(1).await;
    verifier.fail.store(true, Ordering::SeqCst);
    advance(239).await;
    assert!(verifier.cached(&request).is_some());
    advance(59).await;
    assert!(verifier.cached(&request).is_some());
    advance(1).await;
    assert!(verifier.cached(&request).is_none());
    advance(481).await;
    stop_refresh(task).await;
    let observations = verifier.observations.lock().unwrap();
    assert_eq!(
        starts(&observations, "provider", "model")
            .iter()
            .map(|(time, _)| (*time - epoch).as_secs())
            .collect::<Vec<_>>(),
        [0, 240, 300, 540, 780]
    );
}

#[tokio::test(start_paused = true)]
async fn slow_failed_target_does_not_starve_a_healthy_sibling() {
    let mut cfg = test_upstream_config("provider", UpstreamProvider::PhalaDirect, "a", "a");
    cfg.models.insert("b".to_string(), "b".to_string());
    let mut verifier = FakeVerifier::new(300, 1);
    verifier.failed_model = Some(("a".to_string(), Duration::from_secs(240)));
    let verifier = Arc::new(verifier);
    let epoch = Instant::now();
    run_refresh(vec![cfg], verifier.clone(), 1800).await;
    let observations = verifier.observations.lock().unwrap();
    let healthy = starts(&observations, "provider", "b");
    assert!(healthy.len() >= 2);
    assert!(healthy
        .windows(2)
        .all(|pair| pair[1].0 - pair[0].0 < Duration::from_secs(300)));
    assert!(starts(&observations, "provider", "a").len() >= 3);
    assert!(
        healthy[1..].iter().all(|(_, cold)| !cold),
        "healthy starts (seconds, cold): {:?}",
        healthy
            .iter()
            .map(|(time, cold)| ((*time - epoch).as_secs(), *cold))
            .collect::<Vec<_>>()
    );
}

#[tokio::test(start_paused = true)]
async fn aci_service_models_share_one_verification_per_period() {
    let mut cfg = test_upstream_config("provider", UpstreamProvider::AciService, "a", "x");
    cfg.accepted_subjects = Some(vec!["test-subject".to_string()]);
    let root = k256::ecdsa::SigningKey::from_slice(&[1; 32]).unwrap();
    cfg.accepted_dstack_kms_root_public_keys = Some(vec![hex::encode(
        root.verifying_key().to_encoded_point(false).as_bytes(),
    )]);
    cfg.models.extend([
        ("b".to_string(), "y".to_string()),
        ("c".to_string(), "z".to_string()),
    ]);
    let mut verifier = FakeVerifier::new(300, 1);
    verifier.service_cache = true;
    let verifier = Arc::new(verifier);
    run_refresh(vec![cfg], verifier.clone(), 721).await;
    let observations = verifier.observations.lock().unwrap();
    let starts: Vec<_> = observations.starts.values().flatten().collect();
    assert_eq!(starts.len(), 4);
    assert_eq!(observations.peak["provider"], 1);
}

#[tokio::test(start_paused = true)]
async fn minimum_start_interval_survives_slow_or_short_lived_verification() {
    for (cache, ttl, latency, period) in [
        (300, 300, 25, 10),
        (300, 30, 0, 240),
        (30, 30, 40, 30),
        (300, 0, 0, 240),
    ] {
        let mut cfg =
            test_upstream_config("provider", UpstreamProvider::PhalaDirect, "public", "model");
        cfg.verifier_cache_seconds = Some(cache);
        cfg.verification_refresh_seconds = Some(period);
        let verifier = Arc::new(FakeVerifier::new(ttl, latency));
        run_refresh(vec![cfg], verifier.clone(), 1800).await;
        let observations = verifier.observations.lock().unwrap();
        let starts = starts(&observations, "provider", "model");
        assert!(starts.len() >= 3, "expired successes must remain scheduled");
        assert!(starts.len() <= 1800 / period as usize + 1);
        assert!(starts
            .windows(2)
            .all(|pair| pair[1].0 - pair[0].0 >= Duration::from_secs(period)));
        if ttl == 0 {
            assert_eq!(starts.len(), 1800 / period as usize + 1);
            assert!(starts.iter().all(|(_, cold)| *cold));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn refresh_configuration_controls_actual_start_times() {
    for (timeout, refresh, period) in [(None, None, 240), (Some(90), None, 210), (None, Some(0), 0)]
    {
        let mut cfg =
            test_upstream_config("provider", UpstreamProvider::PhalaDirect, "public", "model");
        cfg.verifier_request_timeout_seconds = timeout;
        cfg.verification_refresh_seconds = refresh;
        let verifier = Arc::new(FakeVerifier::new(300, 0));
        run_refresh(vec![cfg], verifier.clone(), 600).await;
        let observations = verifier.observations.lock().unwrap();
        if period == 0 {
            assert!(observations.starts.is_empty());
        } else {
            let starts = starts(&observations, "provider", "model");
            assert_eq!(starts.len(), 600 / period as usize + 1);
            assert!(starts
                .windows(2)
                .all(|pair| pair[1].0 - pair[0].0 == Duration::from_secs(period)));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn permit_wait_reselects_the_most_urgent_sibling() {
    let mut cfg = test_upstream_config("provider", UpstreamProvider::PhalaDirect, "a", "a");
    cfg.models.insert("b".to_string(), "b".to_string());
    let verifier = Arc::new(FakeVerifier::new(300, 25));
    let permits = Arc::new(Semaphore::new(1));
    let manager = refresh_test_manager(vec![cfg], verifier.clone());
    let task = tokio::spawn(manager.run_verification_refresh(permits.clone()));
    tokio::task::yield_now().await;
    advance(50).await; // Both targets have completed their initial attempt.
    let held = permits.acquire_owned().await.unwrap();
    advance(215).await; // A is queued; B now also meets its minimum start interval.
    verifier
        .cache
        .lock()
        .unwrap()
        .insert("b".to_string(), Instant::now() + Duration::from_secs(10));
    drop(held);
    advance(1).await;
    stop_refresh(task).await;
    let observations = verifier.observations.lock().unwrap();
    assert_eq!(starts(&observations, "provider", "a").len(), 1);
    assert_eq!(starts(&observations, "provider", "b").len(), 2);
}

#[tokio::test(start_paused = true)]
async fn cache_ttl_at_or_below_timeout_has_bounded_failed_retries() {
    for ttl in [30, 60] {
        let mut cfg =
            test_upstream_config("provider", UpstreamProvider::PhalaDirect, "public", "model");
        cfg.verifier_cache_seconds = Some(ttl);
        let verifier = Arc::new(FakeVerifier::new(ttl, 0));
        verifier.fail.store(true, Ordering::SeqCst);
        run_refresh(vec![cfg], verifier.clone(), 10).await;
        let observations = verifier.observations.lock().unwrap();
        let starts = starts(&observations, "provider", "model");
        assert!(starts.len() <= 11);
        assert!(starts
            .windows(2)
            .all(|pair| pair[1].0 - pair[0].0 >= Duration::from_secs(1)));
    }
}

#[test]
fn current_requests_exclude_plain_routes_even_with_a_global_verifier() {
    let plain = test_upstream_config("plain", UpstreamProvider::OpenAiCompatible, "shared", "m");
    let tee = test_upstream_config("tee", UpstreamProvider::PhalaDirect, "shared", "m");
    let verifier = Arc::new(crate::aci::verifier::StaticUpstreamVerifier::new(
        UpstreamVerifiedEvent {
            result: VerificationResult::Verified,
            ..Default::default()
        },
    ));
    assert!(verifier
        .cached(&verification_targets(std::slice::from_ref(&plain))[0].request())
        .is_some());
    let manager = refresh_test_manager(vec![plain, tee], verifier);
    for all_routes in [false, true] {
        let requests = manager.current_verification_requests(None, None, all_routes);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].upstream_name, "tee");
    }
    assert!(manager
        .current_verification_requests(Some("shared"), None, false)
        .is_empty());
    assert_eq!(
        manager
            .current_verification_requests(Some("shared"), None, true)
            .len(),
        1
    );
}
