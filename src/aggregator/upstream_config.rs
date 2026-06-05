//! Runtime upstream configuration.
//!
//! The aggregator has exactly one upstream configuration file. Startup
//! loads it if present; an empty or missing file means "no upstreams
//! configured yet". The admin API replaces that same file and swaps the
//! in-memory backend/verifier state atomically.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::aci::canonical;
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{
    ChutesProviderBackend, ChutesSessionStore, ModelRoute, ModelRouterBackend,
    OpenAICompatibleBackend, PreparedUpstreamRequest, UpstreamBackend, UpstreamError,
    UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::verifier::{
    AciDcapUpstreamVerifier, AciDcapVerifierPolicy, ChutesProviderVerifier, NearAiProviderVerifier,
    PreverifiedUpstreamVerifier, RoutingUpstreamVerifier, TinfoilProviderVerifier,
};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    pub name: String,
    #[serde(default)]
    pub provider: UpstreamProvider,
    pub base_url: String,
    /// Per-upstream POST path the generic forwarder targets (e.g.
    /// `/v1/messages` for native Anthropic upstreams), appended to
    /// `base_url`. When omitted the downstream surface path is used
    /// verbatim, matching today's OpenAI-compatible behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub models: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_workload_ids: Option<Vec<String>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_workload_ids: Option<Vec<String>>,
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
            accepted_workload_ids: self.accepted_workload_ids.clone(),
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
    AciDcap,
    Chutes,
    Tinfoil,
    NearAi,
}

#[derive(Debug, Clone)]
pub enum UpstreamVerifierMode {
    None,
    Preverified,
    AciDcap,
}

impl UpstreamVerifierMode {
    pub fn parse(value: &str) -> Result<Self, UpstreamConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "preverified" => Ok(Self::Preverified),
            "aci-dcap" => Ok(Self::AciDcap),
            other => Err(UpstreamConfigError::InvalidConfig(format!(
                "invalid upstream verifier mode {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamRuntimeOptions {
    pub verifier_mode: UpstreamVerifierMode,
    pub accepted_workload_ids: Vec<String>,
    pub accepted_image_digests: Vec<String>,
    pub accepted_dstack_kms_root_public_keys: Vec<String>,
    pub pccs_url: Option<String>,
    pub verifier_cache_seconds: u64,
    pub connect_timeout_seconds: u64,
    pub read_timeout_seconds: u64,
    pub verifier_request_timeout_seconds: u64,
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

const DEFAULT_UPSTREAM_SESSION_REFRESH_SECONDS: u64 = 45;

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

#[derive(Clone)]
pub struct UpstreamConfigManager {
    path: PathBuf,
    options: UpstreamRuntimeOptions,
    state: Arc<RwLock<Arc<ConfiguredUpstreams>>>,
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
        })
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
        validate_config(&config)?;
        let next = Arc::new(build_state(&config, &self.options)?);
        write_config_file(&self.path, &config)?;
        *self
            .state
            .write()
            .expect("upstream config manager state poisoned") = next.clone();
        Ok(snapshot_for(&self.path, &next))
    }

    pub async fn prewarm_upstream_verification(&self) -> Vec<UpstreamPrewarmResult> {
        self.run_upstream_verification(false).await
    }

    pub async fn refresh_upstream_verification(&self) -> Vec<UpstreamPrewarmResult> {
        self.run_upstream_verification(true).await
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

    async fn run_upstream_verification(&self, refresh: bool) -> Vec<UpstreamPrewarmResult> {
        let (verifier, targets) = {
            let state = self
                .state
                .read()
                .expect("upstream config manager state poisoned")
                .clone();
            let Some(verifier) = state.verifier.clone() else {
                return Vec::new();
            };
            let targets = if refresh {
                verification_targets_for_refresh(&state.config, &self.options)
            } else {
                verification_targets(&state.config)
            };
            (verifier, targets)
        };

        let mut results = Vec::with_capacity(targets.len());
        for target in targets {
            let request = UpstreamVerificationRequest {
                upstream_name: target.upstream_name.clone(),
                url_origin: target.url_origin.clone(),
                model_id: target.model_id.clone(),
                forwarded_body_hash: canonical::sha256_hex(b""),
                required: true,
            };
            let event = if refresh {
                verifier.refresh(request).await
            } else {
                verifier.verify(request).await
            };
            results.push(UpstreamPrewarmResult {
                upstream_name: target.upstream_name,
                model_id: target.model_id,
                url_origin: target.url_origin,
                verifier_id: event.verifier_id,
                result: event.result.as_str().to_string(),
                reason: event.reason,
            });
        }
        results
    }

    pub async fn refresh_provider_sessions(&self) -> Vec<UpstreamSessionRefreshResult> {
        let (config, verifier, sessions) = {
            let state = self
                .state
                .read()
                .expect("upstream config manager state poisoned")
                .clone();
            let Some(verifier) = state.verifier.clone() else {
                return Vec::new();
            };
            (state.config.clone(), verifier, state.sessions.clone())
        };

        let mut results = Vec::new();
        for cfg in config
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
                    forwarded_body_hash: canonical::sha256_hex(b""),
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct UpstreamVerificationTarget {
    upstream_name: String,
    model_id: String,
    url_origin: Option<String>,
}

fn verification_targets(config: &[UpstreamConfig]) -> Vec<UpstreamVerificationTarget> {
    verification_targets_for_configs(config.iter())
}

fn verification_targets_for_refresh(
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
) -> Vec<UpstreamVerificationTarget> {
    verification_targets_for_configs(
        config
            .iter()
            .filter(|cfg| verification_refresh_seconds(cfg, options).is_some()),
    )
}

fn verification_targets_for_configs<'a>(
    configs: impl Iterator<Item = &'a UpstreamConfig>,
) -> Vec<UpstreamVerificationTarget> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for cfg in configs {
        let url_origin = Some(cfg.base_url.trim_end_matches('/').to_string());
        for model_id in cfg.models.values() {
            let target = UpstreamVerificationTarget {
                upstream_name: cfg.name.clone(),
                model_id: model_id.clone(),
                url_origin: url_origin.clone(),
            };
            if seen.insert(target.clone()) {
                targets.push(target);
            }
        }
    }
    targets
}

fn verification_refresh_seconds(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
) -> Option<u64> {
    match cfg.verification_refresh_seconds {
        Some(0) => None,
        Some(seconds) => Some(seconds),
        None => {
            let cache_seconds = cfg
                .verifier_cache_seconds
                .unwrap_or(options.verifier_cache_seconds);
            Some(cache_seconds.saturating_sub(60).max(1))
        }
    }
}

fn session_refresh_seconds(cfg: &UpstreamConfig) -> Option<u64> {
    match cfg.session_refresh_seconds {
        Some(0) => None,
        Some(seconds) => Some(seconds),
        None => (cfg.provider == UpstreamProvider::Chutes)
            .then_some(DEFAULT_UPSTREAM_SESSION_REFRESH_SECONDS),
    }
}

fn unique_upstream_models(cfg: &UpstreamConfig) -> Vec<String> {
    let mut seen = HashSet::new();
    cfg.models
        .values()
        .filter(|model_id| seen.insert((*model_id).clone()))
        .cloned()
        .collect()
}

fn looks_like_uuid(value: &str) -> bool {
    value.len() == 36
        && value.split('-').count() == 5
        && value.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

fn snapshot_for(path: &Path, state: &ConfiguredUpstreams) -> UpstreamConfigSnapshot {
    UpstreamConfigSnapshot {
        config_path: path.display().to_string(),
        config_digest: state.config_digest.clone(),
        upstreams: state.config.iter().map(UpstreamConfig::redacted).collect(),
    }
}

fn read_config_file(path: &Path) -> Result<Vec<UpstreamConfig>, UpstreamConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(UpstreamConfigError::Read {
                path: path.display().to_string(),
                source: err,
            });
        }
    };
    parse_config_text(&text)
}

pub fn parse_config_text(text: &str) -> Result<Vec<UpstreamConfig>, UpstreamConfigError> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let config: Vec<UpstreamConfig> = serde_json::from_str(text).map_err(|e| {
        UpstreamConfigError::InvalidConfig(format!("invalid upstream config JSON: {e}"))
    })?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &[UpstreamConfig]) -> Result<(), UpstreamConfigError> {
    let mut names = HashSet::new();
    let mut public_models = HashMap::new();
    for upstream in config {
        if upstream.name.trim().is_empty() {
            return Err(UpstreamConfigError::InvalidConfig(
                "upstream name must not be empty".to_string(),
            ));
        }
        if !names.insert(upstream.name.as_str()) {
            return Err(UpstreamConfigError::InvalidConfig(format!(
                "upstream name {:?} is duplicated",
                upstream.name
            )));
        }
        if upstream.base_url.trim().is_empty() {
            return Err(UpstreamConfigError::InvalidConfig(format!(
                "upstream {:?} base_url must not be empty",
                upstream.name
            )));
        }
        if upstream.models.is_empty() {
            return Err(UpstreamConfigError::InvalidConfig(format!(
                "upstream {:?} must route at least one public model",
                upstream.name
            )));
        }
        for (field, value) in [
            ("connect_timeout_seconds", upstream.connect_timeout_seconds),
            ("read_timeout_seconds", upstream.read_timeout_seconds),
            (
                "verifier_request_timeout_seconds",
                upstream.verifier_request_timeout_seconds,
            ),
            ("verifier_cache_seconds", upstream.verifier_cache_seconds),
        ] {
            if value == Some(0) {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "upstream {:?} {field} must be greater than zero",
                    upstream.name
                )));
            }
        }
        if let Some(rounds) = upstream.chutes_e2ee_discovery_rounds {
            if rounds == 0 || rounds > 10 {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "upstream {:?} chutes_e2ee_discovery_rounds must be between 1 and 10",
                    upstream.name
                )));
            }
        }
        if let Some(base) = upstream.chutes_e2ee_api_base.as_ref() {
            if base.trim().is_empty() {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "upstream {:?} chutes_e2ee_api_base must not be empty",
                    upstream.name
                )));
            }
        }
        if let Some(chute_ids) = upstream.chutes_chute_ids.as_ref() {
            if chute_ids.is_empty() {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "upstream {:?} chutes_chute_ids must not be empty when configured",
                    upstream.name
                )));
            }
            let configured_upstream_models = upstream
                .models
                .values()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            for (model_id, chute_id) in chute_ids {
                if model_id.trim().is_empty() {
                    return Err(UpstreamConfigError::InvalidConfig(format!(
                        "upstream {:?} chutes_chute_ids has an empty model id",
                        upstream.name
                    )));
                }
                if !configured_upstream_models.contains(model_id.as_str()) {
                    return Err(UpstreamConfigError::InvalidConfig(format!(
                        "upstream {:?} chutes_chute_ids key {model_id:?} is not one of its upstream model ids",
                        upstream.name
                    )));
                }
                if !looks_like_uuid(chute_id) {
                    return Err(UpstreamConfigError::InvalidConfig(format!(
                        "upstream {:?} chutes_chute_ids[{model_id:?}] must be a chute_id UUID",
                        upstream.name
                    )));
                }
            }
        }
        if upstream.provider != UpstreamProvider::Chutes
            && (upstream.chutes_e2ee_api_base.is_some()
                || upstream.chutes_chute_ids.is_some()
                || upstream.chutes_e2ee_discovery_rounds.is_some()
                || upstream.chutes_e2ee_discovery_interval_seconds.is_some())
        {
            return Err(UpstreamConfigError::InvalidConfig(format!(
                "upstream {:?} has Chutes E2EE fields but provider is not chutes",
                upstream.name
            )));
        }
        for (public_model, upstream_model) in &upstream.models {
            if public_model.trim().is_empty() {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "upstream {:?} has an empty public model id",
                    upstream.name
                )));
            }
            if upstream_model.trim().is_empty() {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "upstream {:?} route {:?} has an empty upstream model id",
                    upstream.name, public_model
                )));
            }
            if let Some(previous) = public_models.insert(public_model, &upstream.name) {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "public model id {public_model:?} is routed by both {previous:?} and {:?}",
                    upstream.name
                )));
            }
        }
    }
    Ok(())
}

fn write_config_file(path: &Path, config: &[UpstreamConfig]) -> Result<(), UpstreamConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| UpstreamConfigError::Write {
            path: parent.display().to_string(),
            source: e,
        })?;
    }
    let body = serde_json::to_vec_pretty(config).map_err(|e| {
        UpstreamConfigError::InvalidConfig(format!("failed to serialize upstream config: {e}"))
    })?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("json")
    ));
    std::fs::write(&tmp, body).map_err(|e| UpstreamConfigError::Write {
        path: tmp.display().to_string(),
        source: e,
    })?;
    std::fs::rename(&tmp, path).map_err(|e| UpstreamConfigError::Write {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(())
}

fn build_state(
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
) -> Result<ConfiguredUpstreams, UpstreamConfigError> {
    validate_config(config)?;
    let sessions = Arc::new(ProviderSessionRegistry::new(config));
    let backend: Arc<dyn UpstreamBackend> = if config.is_empty() {
        Arc::new(EmptyUpstreamBackend)
    } else {
        Arc::new(build_model_router(config, options, sessions.as_ref())?)
    };
    let verifier = build_verifier(config, options, sessions.as_ref())?;
    Ok(ConfiguredUpstreams {
        config: config.to_vec(),
        config_digest: config_digest(config)?,
        backend,
        verifier,
        sessions,
    })
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

fn build_model_router(
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<ModelRouterBackend, UpstreamConfigError> {
    let mut router = ModelRouterBackend::new("model-router");
    for cfg in config {
        let backend = build_provider_backend(cfg, options, sessions)?;
        for (public_model, upstream_model) in &cfg.models {
            router
                .add_route(
                    ModelRoute::new(
                        public_model.clone(),
                        upstream_model.clone(),
                        backend.clone(),
                        format!("{}:{public_model}", cfg.name),
                    )
                    .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?
                    .with_path(cfg.path.clone())
                    .with_is_tee(Some(provider_is_tee(cfg.provider))),
                )
                .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?;
        }
    }
    Ok(router)
}

/// Whether a provider performs hardware attestation (TEE). Non-TEE
/// providers (plain OpenAI-compatible cloud APIs) are forwarded with
/// TLS endpoint binding only and never fail closed for lack of evidence.
fn provider_is_tee(provider: UpstreamProvider) -> bool {
    match provider {
        UpstreamProvider::OpenAiCompatible => false,
        UpstreamProvider::AciDcap
        | UpstreamProvider::Chutes
        | UpstreamProvider::Tinfoil
        | UpstreamProvider::NearAi => true,
    }
}

fn build_provider_backend(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<Arc<dyn UpstreamBackend>, UpstreamConfigError> {
    let connect_timeout_seconds = cfg
        .connect_timeout_seconds
        .unwrap_or(options.connect_timeout_seconds);
    let read_timeout_seconds = cfg
        .read_timeout_seconds
        .unwrap_or(options.read_timeout_seconds);
    match cfg.provider {
        UpstreamProvider::Chutes => {
            let session_store = sessions.chutes(&cfg.name).ok_or_else(|| {
                UpstreamConfigError::InvalidConfig(format!(
                    "missing Chutes provider session store for upstream {:?}",
                    cfg.name
                ))
            })?;
            Ok(Arc::new(build_chutes_provider_backend(
                cfg,
                options,
                session_store,
            )?))
        }
        UpstreamProvider::OpenAiCompatible
        | UpstreamProvider::AciDcap
        | UpstreamProvider::Tinfoil
        | UpstreamProvider::NearAi => {
            let mut backend = OpenAICompatibleBackend::new_with_timeouts(
                cfg.base_url.clone(),
                connect_timeout_seconds,
                read_timeout_seconds,
            )
            .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?
            .with_name(cfg.name.clone());
            if let Some(token) = &cfg.bearer_token {
                backend = backend.with_bearer_token(token.clone());
            }
            Ok(Arc::new(backend))
        }
    }
}

fn build_chutes_provider_backend(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
    session_store: Arc<ChutesSessionStore>,
) -> Result<ChutesProviderBackend, UpstreamConfigError> {
    let connect_timeout_seconds = cfg
        .connect_timeout_seconds
        .unwrap_or(options.connect_timeout_seconds);
    let read_timeout_seconds = cfg
        .read_timeout_seconds
        .unwrap_or(options.read_timeout_seconds);
    let mut backend = ChutesProviderBackend::new_with_timeouts(
        cfg.base_url.clone(),
        connect_timeout_seconds,
        read_timeout_seconds,
    )
    .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?
    .with_name(cfg.name.clone())
    .with_session_store(session_store);
    if let Some(token) = &cfg.bearer_token {
        backend = backend.with_bearer_token(token.clone());
    }
    if let Some(base_url) = &cfg.chutes_e2ee_api_base {
        backend = backend.with_e2ee_api_base(base_url.clone());
    }
    if let Some(chute_ids) = &cfg.chutes_chute_ids {
        backend = backend.with_chute_ids(chute_ids.clone());
    }
    Ok(backend)
}

fn build_verifier(
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<Option<Arc<dyn UpstreamVerifier>>, UpstreamConfigError> {
    if let Some(provider_verifier) = build_provider_verifier(config, options, sessions)? {
        return Ok(Some(provider_verifier));
    }
    match options.verifier_mode {
        UpstreamVerifierMode::None => Ok(None),
        UpstreamVerifierMode::Preverified => Ok(Some(Arc::new(PreverifiedUpstreamVerifier::new(
            "preverified/out-of-band/v1",
        )))),
        UpstreamVerifierMode::AciDcap => {
            let mut router = RoutingUpstreamVerifier::new();
            for cfg in config {
                let verifier = build_aci_dcap_verifier(cfg, options)?;
                router = router
                    .add_origin(
                        cfg.base_url.trim_end_matches('/').to_string(),
                        verifier.clone(),
                    )
                    .add_name(cfg.name.clone(), verifier);
            }
            Ok(Some(Arc::new(router)))
        }
    }
}

fn build_provider_verifier(
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<Option<Arc<dyn UpstreamVerifier>>, UpstreamConfigError> {
    if !config
        .iter()
        .any(|cfg| cfg.provider != UpstreamProvider::OpenAiCompatible)
    {
        return Ok(None);
    }
    let mut router = RoutingUpstreamVerifier::new();
    for cfg in config {
        let cache_seconds = cfg
            .verifier_cache_seconds
            .unwrap_or(options.verifier_cache_seconds);
        let request_timeout_seconds = cfg
            .verifier_request_timeout_seconds
            .unwrap_or(options.verifier_request_timeout_seconds);
        let verifier: Option<Arc<dyn UpstreamVerifier>> = match cfg.provider {
            UpstreamProvider::OpenAiCompatible => build_global_verifier_for_config(cfg, options)?,
            UpstreamProvider::AciDcap => Some(build_aci_dcap_verifier(cfg, options)?),
            UpstreamProvider::Chutes => {
                let session_store = sessions.chutes(&cfg.name).ok_or_else(|| {
                    UpstreamConfigError::InvalidConfig(format!(
                        "missing Chutes provider session store for upstream {:?}",
                        cfg.name
                    ))
                })?;
                let mut verifier = ChutesProviderVerifier::new_with_cache_and_session_store(
                    request_timeout_seconds,
                    cache_seconds,
                    session_store,
                );
                if let Some(token) = &cfg.bearer_token {
                    verifier = verifier.with_api_key(token.clone());
                }
                if let Some(base_url) = &cfg.chutes_e2ee_api_base {
                    verifier = verifier.with_e2ee_api_base(base_url.clone());
                }
                if let Some(chute_ids) = &cfg.chutes_chute_ids {
                    verifier = verifier.with_chute_ids(chute_ids.clone());
                }
                if let Some(rounds) = cfg.chutes_e2ee_discovery_rounds {
                    verifier = verifier.with_discovery_rounds(rounds);
                }
                if let Some(interval) = cfg.chutes_e2ee_discovery_interval_seconds {
                    verifier = verifier.with_discovery_interval_seconds(interval);
                }
                Some(Arc::new(verifier))
            }
            UpstreamProvider::Tinfoil => Some(Arc::new(TinfoilProviderVerifier::new_with_cache(
                request_timeout_seconds,
                cache_seconds,
            ))),
            UpstreamProvider::NearAi => Some(Arc::new(NearAiProviderVerifier::new_with_cache(
                request_timeout_seconds,
                cache_seconds,
            ))),
        };
        if let Some(verifier) = verifier {
            router = router
                .add_origin(
                    cfg.base_url.trim_end_matches('/').to_string(),
                    verifier.clone(),
                )
                .add_name(cfg.name.clone(), verifier);
        }
    }
    Ok(Some(Arc::new(router)))
}

fn build_global_verifier_for_config(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
) -> Result<Option<Arc<dyn UpstreamVerifier>>, UpstreamConfigError> {
    match options.verifier_mode {
        UpstreamVerifierMode::None => Ok(None),
        UpstreamVerifierMode::Preverified => Ok(Some(Arc::new(PreverifiedUpstreamVerifier::new(
            "preverified/out-of-band/v1",
        )))),
        UpstreamVerifierMode::AciDcap => build_aci_dcap_verifier(cfg, options).map(Some),
    }
}

fn build_aci_dcap_verifier(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
) -> Result<Arc<dyn UpstreamVerifier>, UpstreamConfigError> {
    let policy = AciDcapVerifierPolicy::new(
        cfg.accepted_workload_ids
            .clone()
            .unwrap_or_else(|| options.accepted_workload_ids.clone()),
        cfg.accepted_image_digests
            .clone()
            .unwrap_or_else(|| options.accepted_image_digests.clone()),
        cfg.accepted_dstack_kms_root_public_keys
            .clone()
            .unwrap_or_else(|| options.accepted_dstack_kms_root_public_keys.clone()),
    )
    .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?;
    let cache_seconds = cfg
        .verifier_cache_seconds
        .unwrap_or(options.verifier_cache_seconds);
    let connect_timeout_seconds = cfg
        .connect_timeout_seconds
        .unwrap_or(options.connect_timeout_seconds);
    let request_timeout_seconds = cfg
        .verifier_request_timeout_seconds
        .unwrap_or(options.verifier_request_timeout_seconds);
    let pccs_url = cfg.pccs_url.clone().or_else(|| options.pccs_url.clone());
    match pccs_url {
        Some(pccs_url) => Ok(Arc::new(
            AciDcapUpstreamVerifier::new_with_timeouts(
                cfg.base_url.clone(),
                pccs_url,
                policy,
                cache_seconds,
                connect_timeout_seconds,
                request_timeout_seconds,
            )
            .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?,
        )),
        None => Ok(Arc::new(
            AciDcapUpstreamVerifier::with_default_pccs_and_timeouts(
                cfg.base_url.clone(),
                policy,
                cache_seconds,
                connect_timeout_seconds,
                request_timeout_seconds,
            )
            .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?,
        )),
    }
}

fn config_digest(config: &[UpstreamConfig]) -> Result<String, UpstreamConfigError> {
    let value = serde_json::to_value(config).map_err(|e| {
        UpstreamConfigError::InvalidConfig(format!("failed to serialize upstream config: {e}"))
    })?;
    canonical::jcs_sha256_hex(&value).map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))
}

struct EmptyUpstreamBackend;

#[async_trait]
impl UpstreamBackend for EmptyUpstreamBackend {
    fn name(&self) -> &str {
        "unconfigured"
    }

    fn url_origin(&self) -> Option<&str> {
        None
    }

    fn prepare(&self, _req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        Err(UpstreamError::Routing(
            "no upstreams configured".to_string(),
        ))
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Routing(
            "no upstreams configured".to_string(),
        ))
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(UpstreamError::Routing(
            "no upstreams configured".to_string(),
        ))
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Ok(UpstreamResponse {
            status_code: 200,
            body: serde_json::to_vec(&json!({"object": "list", "data": []}))
                .map_err(|e| UpstreamError::Routing(e.to_string()))?,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
        })
    }
}

struct DynamicUpstreamBackend {
    state: Arc<RwLock<Arc<ConfiguredUpstreams>>>,
}

impl DynamicUpstreamBackend {
    fn backend(&self) -> Arc<dyn UpstreamBackend> {
        self.state
            .read()
            .expect("upstream config manager state poisoned")
            .backend
            .clone()
    }
}

#[async_trait]
impl UpstreamBackend for DynamicUpstreamBackend {
    fn name(&self) -> &str {
        "dynamic-upstream-config"
    }

    fn url_origin(&self) -> Option<&str> {
        None
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        self.backend().prepare(req)
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().forward(req).await
    }

    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().forward_prepared(req).await
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().forward_verified_prepared(req, event).await
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().models().await
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.backend().forward_stream(req).await
    }

    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.backend().forward_stream_prepared(req).await
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.backend()
            .forward_stream_verified_prepared(req, event)
            .await
    }
}

struct DynamicUpstreamVerifier {
    state: Arc<RwLock<Arc<ConfiguredUpstreams>>>,
}

#[async_trait]
impl UpstreamVerifier for DynamicUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let verifier = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .verifier
            .clone();
        match verifier {
            Some(verifier) => verifier.verify(request).await,
            None => UpstreamVerifiedEvent {
                vendor: request.upstream_name,
                model_id: request.model_id,
                url_origin: request.url_origin,
                verifier_id: "none".to_string(),
                result: VerificationResult::Failed,
                required: request.required,
                reason: Some("no upstream verifier configured".to_string()),
                evidence: None,
                channel_bindings: Vec::new(),
                provider_claims: None,
            },
        }
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let verifier = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .verifier
            .clone();
        match verifier {
            Some(verifier) => verifier.refresh(request).await,
            None => UpstreamVerifiedEvent {
                vendor: request.upstream_name,
                model_id: request.model_id,
                url_origin: request.url_origin,
                verifier_id: "none".to_string(),
                result: VerificationResult::Failed,
                required: request.required,
                reason: Some("no upstream verifier configured".to_string()),
                evidence: None,
                channel_bindings: Vec::new(),
                provider_claims: None,
            },
        }
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        let verifier = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .verifier
            .clone();
        if let Some(verifier) = verifier {
            verifier.invalidate(request);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingVerifier {
        verifications: Arc<AtomicUsize>,
        invalidations: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl UpstreamVerifier for CountingVerifier {
        async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
            self.verifications.fetch_add(1, Ordering::SeqCst);
            UpstreamVerifiedEvent {
                vendor: request.upstream_name,
                model_id: request.model_id,
                url_origin: request.url_origin,
                verifier_id: "counting-verifier/v1".to_string(),
                result: VerificationResult::Verified,
                required: request.required,
                reason: None,
                evidence: None,
                channel_bindings: Vec::new(),
                provider_claims: None,
            }
        }

        fn invalidate(&self, _request: &UpstreamVerificationRequest) {
            self.invalidations.fetch_add(1, Ordering::SeqCst);
        }
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
            provider: UpstreamProvider::Tinfoil,
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
        };

        let results = manager.prewarm_upstream_verification().await;

        assert_eq!(results.len(), 2);
        assert_eq!(verifications.load(Ordering::SeqCst), 2);
        assert_eq!(
            results[0].url_origin.as_deref(),
            Some("https://provider-a.example")
        );
    }
}
