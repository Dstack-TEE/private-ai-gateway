//! Runtime upstream configuration.
//!
//! The aggregator has exactly one upstream configuration file. Startup
//! loads it if present; an empty or missing file means "no upstreams
//! configured yet". The admin API replaces that same file and swaps the
//! in-memory backend/verifier state atomically.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures_util::future::join_all;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use serde::{Deserialize, Serialize};

use crate::aci::digest;
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{ChutesSessionStore, UpstreamBackend, UpstreamError};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

mod builders;
mod dynamic;
mod pull;
#[cfg(test)]
mod tests;
mod validation;

pub use pull::{PullRefreshOutcome, UpstreamConfigPuller, UpstreamPullConfig};
pub use validation::parse_config_text;

use builders::{build_chutes_provider_backend, build_state, config_digest};
use dynamic::{DynamicUpstreamBackend, DynamicUpstreamVerifier};
use validation::{
    read_config_file, session_refresh_seconds, snapshot_for, unique_upstream_models,
    validate_config, verification_refresh_seconds, verification_targets,
    verification_targets_for_refresh, write_config_file, UpstreamVerificationTarget,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    pub name: String,
    #[serde(default)]
    pub provider: UpstreamProvider,
    pub base_url: String,
    /// Per-upstream POST path the generic forwarder targets (e.g.
    /// `/v1/messages` for native Anthropic upstreams), appended to
    /// `base_url`. When omitted, chat-shaped surfaces
    /// (`/v1/chat/completions` and `/v1/messages`) target
    /// `/v1/chat/completions` (the OpenAI-compatible default) and other
    /// surfaces use the downstream path verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub models: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    /// Use HTTP Basic authentication with `bearer_token`. Defaults to Bearer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub basic_auth: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_subjects: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_image_digests: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_dstack_kms_root_public_keys: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pccs_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_cache_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_request_timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_refresh_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_refresh_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chutes_e2ee_api_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chutes_chute_ids: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chutes_e2ee_discovery_rounds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chutes_e2ee_discovery_interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PublicUpstreamConfig {
    pub name: String,
    pub provider: UpstreamProvider,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub models: BTreeMap<String, String>,
    pub bearer_token_configured: bool,
    pub basic_auth: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_subjects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_image_digests: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_dstack_kms_root_public_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pccs_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier_cache_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier_request_timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_refresh_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_refresh_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chutes_e2ee_api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chutes_chute_ids: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chutes_e2ee_discovery_rounds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chutes_e2ee_discovery_interval_seconds: Option<u64>,
}

impl UpstreamConfig {
    pub fn redacted(&self) -> PublicUpstreamConfig {
        PublicUpstreamConfig {
            name: self.name.clone(),
            provider: self.provider,
            base_url: self.base_url.clone(),
            path: self.path.clone(),
            models: self.models.clone(),
            bearer_token_configured: self.bearer_token.is_some(),
            basic_auth: self.basic_auth,
            accepted_subjects: self.accepted_subjects.clone(),
            accepted_image_digests: self.accepted_image_digests.clone(),
            accepted_dstack_kms_root_public_keys: self.accepted_dstack_kms_root_public_keys.clone(),
            pccs_url: self.pccs_url.clone(),
            verifier_cache_seconds: self.verifier_cache_seconds,
            connect_timeout_seconds: self.connect_timeout_seconds,
            read_timeout_seconds: self.read_timeout_seconds,
            verifier_request_timeout_seconds: self.verifier_request_timeout_seconds,
            verification_refresh_seconds: self.verification_refresh_seconds,
            session_refresh_seconds: self.session_refresh_seconds,
            chutes_e2ee_api_base: self.chutes_e2ee_api_base.clone(),
            chutes_chute_ids: self.chutes_chute_ids.clone(),
            chutes_e2ee_discovery_rounds: self.chutes_e2ee_discovery_rounds,
            chutes_e2ee_discovery_interval_seconds: self.chutes_e2ee_discovery_interval_seconds,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum UpstreamProvider {
    #[default]
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// Native Anthropic API: authenticates with `x-api-key` plus the
    /// required `anthropic-version` header instead of a Bearer token.
    Anthropic,
    AciService,
    Chutes,
    Tinfoil,
    NearAi,
    SecretAi,
    PhalaDirect,
}

impl UpstreamProvider {
    /// The channel boundary this provider attests. Exhaustive so a new provider
    /// must choose its scope rather than inherit a default.
    pub(crate) fn attestation_scope(self) -> AttestationScope {
        match self {
            UpstreamProvider::NearAi | UpstreamProvider::Tinfoil | UpstreamProvider::SecretAi => {
                AttestationScope::PerRouter
            }
            UpstreamProvider::Chutes => AttestationScope::PerInstance,
            UpstreamProvider::PhalaDirect => AttestationScope::PerModel,
            // Plain cloud APIs (OpenAI-compatible, Anthropic) have no verifier
            // and ACI service uses its own, so for all of these this only tunes
            // prewarm probe granularity. Per-model is the safe default — it
            // never collapses channels. ACI's real scope is service-dependent
            // (router or model), resolved when ACI becomes a first-party router.
            UpstreamProvider::OpenAiCompatible
            | UpstreamProvider::Anthropic
            | UpstreamProvider::AciService => AttestationScope::PerModel,
        }
    }
}

/// The channel boundary a provider's attestation proves, and thus what identifies
/// its attested session: one shared channel for a router (model dropped from the
/// verifier cache key) or a distinct channel per model / per serving instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestationScope {
    PerRouter,
    PerModel,
    PerInstance,
}

impl AttestationScope {
    /// Routers share one channel across models, so verification is keyed on the
    /// channel alone (model dropped from the cache key).
    pub(crate) fn is_per_router(self) -> bool {
        matches!(self, AttestationScope::PerRouter)
    }

    /// Parse the scope token a provider verifier emits in its result.
    pub(crate) fn from_declared(token: &str) -> Option<Self> {
        match token {
            "router" => Some(Self::PerRouter),
            "model" => Some(Self::PerModel),
            "instance" => Some(Self::PerInstance),
            _ => None,
        }
    }

    /// The wire token for this scope (matches [`Self::from_declared`]).
    pub(crate) fn as_declared(self) -> &'static str {
        match self {
            Self::PerRouter => "router",
            Self::PerModel => "model",
            Self::PerInstance => "instance",
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpstreamVerifierMode {
    None,
    Preverified,
    AciService,
}

impl UpstreamVerifierMode {
    pub fn parse(value: &str) -> Result<Self, UpstreamConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "preverified" => Ok(Self::Preverified),
            "aci-service" => Ok(Self::AciService),
            other => Err(UpstreamConfigError::InvalidConfig(format!(
                "invalid upstream verifier mode {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamRuntimeOptions {
    pub verifier_mode: UpstreamVerifierMode,
    pub accepted_subjects: Vec<String>,
    pub accepted_image_digests: Vec<String>,
    pub accepted_dstack_kms_root_public_keys: Vec<String>,
    pub pccs_url: Option<String>,
    pub verifier_cache_seconds: u64,
    pub connect_timeout_seconds: u64,
    pub read_timeout_seconds: u64,
    pub verifier_request_timeout_seconds: u64,
}

/// Connection details for fetching a model's attestation report from its
/// upstream node. Produced by [`UpstreamConfigManager::attestation_upstream_target`].
#[derive(Debug, Clone)]
pub struct AttestationUpstreamTarget {
    pub upstream_name: String,
    pub provider: UpstreamProvider,
    /// Base URL with any trailing slash trimmed.
    pub base_url: String,
    /// The upstream's own model id (some providers need it as a query param).
    pub upstream_model_id: String,
    pub bearer_token: Option<String>,
    pub connect_timeout_seconds: u64,
    pub read_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamConfigSnapshot {
    pub config_path: String,
    pub config_digest: String,
    pub upstreams: Vec<PublicUpstreamConfig>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpstreamPrewarmResult {
    pub upstream_name: String,
    pub model_id: String,
    pub url_origin: Option<String>,
    pub verifier_id: String,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpstreamSessionRefreshResult {
    pub upstream_name: String,
    pub model_id: String,
    pub result: String,
    pub refreshed_nonces: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum UpstreamConfigError {
    #[error("failed to read upstream config {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to write upstream config {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid upstream config: {0}")]
    InvalidConfig(String),
}

struct ConfiguredUpstreams {
    config: Vec<UpstreamConfig>,
    config_digest: String,
    backend: Arc<dyn UpstreamBackend>,
    verifier: Option<Arc<dyn UpstreamVerifier>>,
    sessions: Arc<ProviderSessionRegistry>,
}

/// Sink that materializes verified upstream events into stored attested
/// sessions. Implemented by the service; the background verification loop calls
/// it after each verify/refresh so the session store is populated by the same
/// verification used for serving, without a separate refresh path.
pub trait UpstreamSessionSink: Send + Sync {
    fn record_session(&self, event: &UpstreamVerifiedEvent);
}

#[derive(Clone)]
pub struct UpstreamConfigManager {
    path: PathBuf,
    options: UpstreamRuntimeOptions,
    state: Arc<RwLock<Arc<ConfiguredUpstreams>>>,
    update_lock: Arc<Mutex<()>>,
    session_sink: Arc<RwLock<Option<Arc<dyn UpstreamSessionSink>>>>,
}

impl UpstreamConfigManager {
    pub fn load(
        path: impl Into<PathBuf>,
        options: UpstreamRuntimeOptions,
    ) -> Result<Self, UpstreamConfigError> {
        let path = path.into();
        let config = read_config_file(&path)?;
        let state = Arc::new(build_state(&config, &options)?);
        Ok(Self {
            path,
            options,
            state: Arc::new(RwLock::new(state)),
            update_lock: Arc::new(Mutex::new(())),
            session_sink: Arc::new(RwLock::new(None)),
        })
    }

    /// Attach the sink that the background verification writes attested sessions
    /// into. Set once after the service is built, before the lifecycle is
    /// spawned.
    pub fn set_session_sink(&self, sink: Arc<dyn UpstreamSessionSink>) {
        *self.session_sink.write().unwrap_or_else(|p| p.into_inner()) = Some(sink);
    }

    pub fn backend(&self) -> Arc<dyn UpstreamBackend> {
        Arc::new(DynamicUpstreamBackend {
            state: self.state.clone(),
        })
    }

    pub fn verifier(&self) -> Arc<dyn UpstreamVerifier> {
        Arc::new(DynamicUpstreamVerifier {
            state: self.state.clone(),
        })
    }

    /// Resolve `model` to the first configured upstream that serves it, with the
    /// connection details needed to fetch its attestation report. Carries the
    /// bearer token (so it is not derived from the redacted [`snapshot`]).
    /// Returns `None` when no configured upstream serves `model`.
    pub fn attestation_upstream_target(&self, model: &str) -> Option<AttestationUpstreamTarget> {
        let state = self.state.read().unwrap_or_else(|p| p.into_inner()).clone();
        let cfg = state.config.iter().find(|cfg| {
            cfg.models.contains_key(model) || cfg.models.values().any(|v| v == model)
        })?;
        // `model` is either a public alias (mapped to an upstream id) or already
        // an upstream model id.
        let upstream_model_id = cfg
            .models
            .get(model)
            .cloned()
            .unwrap_or_else(|| model.to_string());
        Some(AttestationUpstreamTarget {
            upstream_name: cfg.name.clone(),
            provider: cfg.provider,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            upstream_model_id,
            bearer_token: cfg.bearer_token.clone(),
            connect_timeout_seconds: cfg
                .connect_timeout_seconds
                .unwrap_or(self.options.connect_timeout_seconds),
            read_timeout_seconds: cfg
                .read_timeout_seconds
                .unwrap_or(self.options.read_timeout_seconds),
        })
    }

    pub fn snapshot(&self) -> UpstreamConfigSnapshot {
        let state = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .clone();
        snapshot_for(&self.path, &state)
    }

    pub fn replace(
        &self,
        config: Vec<UpstreamConfig>,
    ) -> Result<UpstreamConfigSnapshot, UpstreamConfigError> {
        self.replace_inner(config, false).map(Option::unwrap)
    }

    /// Validate and atomically install a config only when its digest differs.
    /// Pull polling uses this to avoid rewriting the secret-bearing state file
    /// on every unchanged response. The update lock also serializes an admin PUT
    /// racing a pull so an older build cannot overwrite a newer one after it.
    pub fn replace_if_changed(
        &self,
        config: Vec<UpstreamConfig>,
    ) -> Result<Option<UpstreamConfigSnapshot>, UpstreamConfigError> {
        self.replace_inner(config, true)
    }

    fn replace_inner(
        &self,
        config: Vec<UpstreamConfig>,
        skip_unchanged: bool,
    ) -> Result<Option<UpstreamConfigSnapshot>, UpstreamConfigError> {
        let _update = self
            .update_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        validate_config(&config)?;
        if skip_unchanged {
            // The digest depends only on the config, so compare before paying
            // for a full router/verifier build on every unchanged poll.
            let next_digest = config_digest(&config)?;
            let current = self
                .state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if current.config_digest == next_digest {
                return Ok(None);
            }
        }
        let next = Arc::new(build_state(&config, &self.options)?);
        write_config_file(&self.path, &config)?;
        *self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = next.clone();
        Ok(Some(snapshot_for(&self.path, &next)))
    }

    pub async fn prewarm_upstream_verification(&self) -> Vec<UpstreamPrewarmResult> {
        self.run_upstream_verification().await
    }

    pub fn verification_refresh_interval_seconds(&self) -> Option<u64> {
        let state = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .clone();
        state.verifier.as_ref()?;
        state
            .config
            .iter()
            .filter_map(|cfg| verification_refresh_seconds(cfg, &self.options))
            .min()
    }

    pub fn current_verification_requests(
        &self,
        model: Option<&str>,
        upstream_name: Option<&str>,
    ) -> Vec<UpstreamVerificationRequest> {
        let state = self.state.read().unwrap_or_else(|p| p.into_inner()).clone();
        verification_targets(&state.config)
            .into_iter()
            .filter(|target| upstream_name.is_none_or(|name| target.upstream_name == name))
            .filter(|target| {
                model.is_none_or(|model| {
                    state.config.iter().any(|cfg| {
                        cfg.name == target.upstream_name
                            && if cfg.provider.attestation_scope().is_per_router() {
                                cfg.models.contains_key(model)
                            } else {
                                cfg.models.get(model) == Some(&target.model_id)
                            }
                    })
                })
            })
            .map(|target| target.request())
            .collect()
    }

    pub async fn run_verification_refresh(self: Arc<Self>, log: fn(Vec<UpstreamPrewarmResult>)) {
        let mut scheduled_seconds = None;
        let mut next_due: BTreeMap<String, Instant> = BTreeMap::new();
        let mut in_flight: HashMap<String, JoinHandle<()>> = HashMap::new();
        let history = VerificationRefreshHistory::default();
        loop {
            let finished: Vec<_> = in_flight
                .iter()
                .filter(|(_, handle)| handle.is_finished())
                .map(|(name, _)| name.clone())
                .collect();
            for name in finished {
                if let Some(handle) = in_flight.remove(&name) {
                    if let Err(err) = handle.await {
                        tracing::error!(upstream = %name, error = %err, "upstream verification refresh task failed");
                    }
                }
            }
            let state = self.state.read().unwrap_or_else(|p| p.into_inner()).clone();
            let seconds = state.verifier.as_ref().and_then(|_| {
                state
                    .config
                    .iter()
                    .filter_map(|cfg| verification_refresh_seconds(cfg, &self.options))
                    .min()
            });
            let Some(seconds) = seconds else {
                scheduled_seconds = None;
                next_due.clear();
                history.set_targets(&[]);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };
            let targets = verification_targets_for_refresh(&state.config, &self.options);
            history.set_targets(&targets);
            let groups = group_verification_targets(targets);
            let period = Duration::from_secs(seconds);
            if scheduled_seconds != Some(seconds) || next_due.keys().ne(groups.keys()) {
                let now = Instant::now();
                next_due.retain(|name, _| groups.contains_key(name));
                for (index, name) in groups.keys().enumerate() {
                    let phase = period.mul_f64((index + 1) as f64 / groups.len() as f64);
                    let due = next_due.entry(name.clone()).or_insert(now + phase);
                    if scheduled_seconds != Some(seconds) {
                        *due = (*due).min(now + period);
                    }
                }
                scheduled_seconds = Some(seconds);
            }
            let Some(earliest) = next_due.values().copied().min() else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };
            let now = Instant::now();
            if earliest > now {
                tokio::time::sleep_until(earliest).await;
                continue;
            }
            let Some(verifier) = state.verifier.clone() else {
                continue;
            };
            let sink = self
                .session_sink
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            for (name, targets) in groups {
                let Some(due) = next_due.get_mut(&name) else {
                    continue;
                };
                if *due > now {
                    continue;
                }
                // Skip missed starts while preserving this group's phase.
                while *due <= now {
                    *due += period;
                }
                if in_flight
                    .get(&name)
                    .is_some_and(|handle| !handle.is_finished())
                {
                    continue;
                }
                if let Some(handle) = in_flight.remove(&name) {
                    if let Err(err) = handle.await {
                        tracing::error!(upstream = %name, error = %err, "upstream verification refresh task failed");
                    }
                }
                let history = history.clone();
                let verifier = verifier.clone();
                let sink = sink.clone();
                in_flight.insert(
                    name,
                    tokio::spawn(async move {
                        log(run_verification_group(verifier, targets, sink, Some(history)).await);
                    }),
                );
            }
        }
    }

    pub fn session_refresh_interval_seconds(&self) -> Option<u64> {
        let state = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .clone();
        state.verifier.as_ref()?;
        state
            .config
            .iter()
            .filter(|cfg| cfg.provider == UpstreamProvider::Chutes)
            .filter_map(session_refresh_seconds)
            .min()
    }

    async fn run_upstream_verification(&self) -> Vec<UpstreamPrewarmResult> {
        let (verifier, targets, sink) = {
            let state = self.state.read().unwrap_or_else(|p| p.into_inner()).clone();
            let Some(verifier) = state.verifier.clone() else {
                return Vec::new();
            };
            let targets = verification_targets(&state.config);
            let sink = self
                .session_sink
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            (verifier, targets, sink)
        };
        join_all(
            group_verification_targets(targets)
                .into_values()
                .map(|targets| {
                    run_verification_group(verifier.clone(), targets, sink.clone(), None)
                }),
        )
        .await
        .into_iter()
        .flatten()
        .collect()
    }

    pub async fn refresh_provider_sessions(&self) -> Vec<UpstreamSessionRefreshResult> {
        // Clone the `Arc` out of the lock (cheap) and read the config off it —
        // no need to deep-clone the whole config Vec each refresh tick.
        let state = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .clone();
        let Some(verifier) = state.verifier.clone() else {
            return Vec::new();
        };
        let sessions = state.sessions.clone();

        let mut results = Vec::new();
        for cfg in state
            .config
            .iter()
            .filter(|cfg| cfg.provider == UpstreamProvider::Chutes)
            .filter(|cfg| session_refresh_seconds(cfg).is_some())
        {
            let Some(session_store) = sessions.chutes(&cfg.name) else {
                continue;
            };
            let backend = match build_chutes_provider_backend(cfg, &self.options, session_store) {
                Ok(backend) => backend,
                Err(err) => {
                    for model_id in unique_upstream_models(cfg) {
                        results.push(UpstreamSessionRefreshResult {
                            upstream_name: cfg.name.clone(),
                            model_id,
                            result: "failed".to_string(),
                            refreshed_nonces: 0,
                            reason: Some(err.to_string()),
                        });
                    }
                    continue;
                }
            };
            let url_origin = Some(cfg.base_url.trim_end_matches('/').to_string());
            for model_id in unique_upstream_models(cfg) {
                let request = UpstreamVerificationRequest {
                    upstream_name: cfg.name.clone(),
                    url_origin: url_origin.clone(),
                    model_id: model_id.clone(),
                    forwarded_body_hash: digest::sha256_hex(b""),
                    required: true,
                };
                let event = verifier.verify(request.clone()).await;
                if event.result != VerificationResult::Verified {
                    results.push(UpstreamSessionRefreshResult {
                        upstream_name: cfg.name.clone(),
                        model_id,
                        result: "failed".to_string(),
                        refreshed_nonces: 0,
                        reason: event.reason,
                    });
                    continue;
                }
                match backend
                    .refresh_verified_sessions_for_model(&model_id, &event)
                    .await
                {
                    Ok(refreshed_nonces) => results.push(UpstreamSessionRefreshResult {
                        upstream_name: cfg.name.clone(),
                        model_id,
                        result: "refreshed".to_string(),
                        refreshed_nonces,
                        reason: None,
                    }),
                    Err(err) => {
                        if matches!(err, UpstreamError::ChannelBindingMismatch(_)) {
                            let refreshed_event = verifier.refresh(request).await;
                            if refreshed_event.result == VerificationResult::Verified {
                                results.push(UpstreamSessionRefreshResult {
                                    upstream_name: cfg.name.clone(),
                                    model_id,
                                    result: "refreshed_via_verifier".to_string(),
                                    refreshed_nonces: 0,
                                    reason: None,
                                });
                                continue;
                            }
                            results.push(UpstreamSessionRefreshResult {
                                upstream_name: cfg.name.clone(),
                                model_id,
                                result: "failed".to_string(),
                                refreshed_nonces: 0,
                                reason: refreshed_event.reason,
                            });
                            continue;
                        }
                        results.push(UpstreamSessionRefreshResult {
                            upstream_name: cfg.name.clone(),
                            model_id,
                            result: "failed".to_string(),
                            refreshed_nonces: 0,
                            reason: Some(err.to_string()),
                        });
                    }
                }
            }
        }
        results
    }
}

#[derive(Default)]
struct ProviderSessionRegistry {
    chutes: HashMap<String, Arc<ChutesSessionStore>>,
}

impl ProviderSessionRegistry {
    fn new(config: &[UpstreamConfig]) -> Self {
        let chutes = config
            .iter()
            .filter(|cfg| cfg.provider == UpstreamProvider::Chutes)
            .map(|cfg| (cfg.name.clone(), Arc::new(ChutesSessionStore::new())))
            .collect();
        Self { chutes }
    }

    fn chutes(&self, upstream_name: &str) -> Option<Arc<ChutesSessionStore>> {
        self.chutes.get(upstream_name).cloned()
    }
}

fn group_verification_targets(
    targets: Vec<UpstreamVerificationTarget>,
) -> BTreeMap<String, Vec<UpstreamVerificationTarget>> {
    let mut groups: BTreeMap<String, Vec<UpstreamVerificationTarget>> = BTreeMap::new();
    for target in targets {
        groups
            .entry(target.upstream_name.clone())
            .or_default()
            .push(target);
    }
    groups
}

#[derive(Clone, Default)]
struct VerificationRefreshHistory {
    last_success: Arc<Mutex<HashMap<UpstreamVerificationTarget, Option<Instant>>>>,
}

impl VerificationRefreshHistory {
    fn set_targets(&self, targets: &[UpstreamVerificationTarget]) {
        let mut last_success = self.last_success.lock().unwrap_or_else(|p| p.into_inner());
        last_success.retain(|target, _| targets.contains(target));
        for target in targets {
            last_success.entry(target.clone()).or_default();
        }
    }

    fn last_success(&self, target: &UpstreamVerificationTarget) -> Option<Instant> {
        self.last_success
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(target)
            .copied()
            .flatten()
    }

    fn record_success(&self, target: &UpstreamVerificationTarget, completed_at: Instant) {
        // A removed target's in-flight pass must not recreate its history.
        if let Some(last_success) = self
            .last_success
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(target)
        {
            *last_success = Some(completed_at);
        }
    }
}

async fn run_verification_group(
    verifier: Arc<dyn UpstreamVerifier>,
    targets: Vec<UpstreamVerificationTarget>,
    sink: Option<Arc<dyn UpstreamSessionSink>>,
    refresh: Option<VerificationRefreshHistory>,
) -> Vec<UpstreamPrewarmResult> {
    let mut results = Vec::with_capacity(targets.len());
    for target in targets {
        results.push(
            run_verification_target(verifier.as_ref(), target, sink.as_deref(), refresh.as_ref())
                .await,
        );
    }
    results
}

async fn run_verification_target(
    verifier: &dyn UpstreamVerifier,
    target: UpstreamVerificationTarget,
    sink: Option<&dyn UpstreamSessionSink>,
    refresh: Option<&VerificationRefreshHistory>,
) -> UpstreamPrewarmResult {
    let request = target.request();
    let event = if let Some(history) = refresh {
        if let Some(previous) = history.last_success(&target) {
            if verifier.cached(&request).is_none() {
                tracing::warn!(
                    upstream = %target.upstream_name,
                    model = %target.model_id,
                    seconds_since_last_success = previous.elapsed().as_secs_f64(),
                    "upstream verification cache was cold before refresh",
                );
            }
        }
        verifier.refresh(request).await
    } else {
        verifier.verify(request).await
    };
    if event.result == VerificationResult::Verified {
        if let Some(history) = refresh {
            history.record_success(&target, Instant::now());
        }
    }
    if let Some(sink) = sink {
        sink.record_session(&event);
    }
    UpstreamPrewarmResult {
        upstream_name: target.upstream_name,
        model_id: target.model_id,
        url_origin: target.url_origin,
        verifier_id: event.verifier_id,
        result: event.result.as_str().to_string(),
        reason: event.reason,
    }
}
