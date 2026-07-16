//! Backend/verifier construction from upstream config.

use std::sync::Arc;

use super::dynamic::EmptyUpstreamBackend;
use super::validation::validate_config;
use super::{
    ConfiguredUpstreams, ProviderSessionRegistry, UpstreamConfig, UpstreamConfigError,
    UpstreamProvider, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use crate::aci::canonical;
use crate::aci::upstream::{
    ChutesProviderBackend, ChutesSessionStore, ModelRoute, ModelRouterBackend,
    OpenAICompatibleBackend, UpstreamBackend,
};
use crate::aci::verifier::{
    AciServiceUpstreamVerifier, AciServiceVerifierPolicy, ChutesProviderVerifier,
    NearAiProviderVerifier, PhalaDirectProviderVerifier, PreverifiedUpstreamVerifier,
    RoutingUpstreamVerifier, TinfoilProviderVerifier,
};
use crate::aggregator::service::UpstreamVerifier;

pub(super) fn build_state(
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

/// Whether a provider route should participate in the default fail-closed
/// upstream verification path. Plain OpenAI-compatible cloud APIs are forwarded
/// with TLS endpoint binding only.
fn provider_is_tee(provider: UpstreamProvider) -> bool {
    match provider {
        UpstreamProvider::OpenAiCompatible | UpstreamProvider::Anthropic => false,
        UpstreamProvider::AciService
        | UpstreamProvider::Chutes
        | UpstreamProvider::Tinfoil
        | UpstreamProvider::NearAi
        | UpstreamProvider::PhalaDirect => true,
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
        | UpstreamProvider::Anthropic
        | UpstreamProvider::AciService
        | UpstreamProvider::Tinfoil
        | UpstreamProvider::NearAi
        | UpstreamProvider::PhalaDirect => {
            let mut backend = OpenAICompatibleBackend::new_with_timeouts(
                cfg.base_url.clone(),
                connect_timeout_seconds,
                read_timeout_seconds,
            )
            .map_err(|e| UpstreamConfigError::InvalidConfig(e.to_string()))?
            .with_name(cfg.name.clone());
            if let Some(token) = &cfg.bearer_token {
                backend = if cfg.provider == UpstreamProvider::Anthropic {
                    backend.with_anthropic_api_key(token.clone())
                } else {
                    backend.with_bearer_token(token.clone())
                };
            }
            backend = backend.with_basic_auth(cfg.basic_auth);
            Ok(Arc::new(backend))
        }
    }
}

pub(super) fn build_chutes_provider_backend(
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
    backend = backend.with_basic_auth(cfg.basic_auth);
    if let Some(base_url) = &cfg.chutes_e2ee_api_base {
        backend = backend.with_e2ee_api_base(base_url.clone());
    }
    if let Some(chute_ids) = &cfg.chutes_chute_ids {
        backend = backend.with_chute_ids(chute_ids.clone());
    }
    Ok(backend)
}

pub(super) fn build_verifier(
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
        UpstreamVerifierMode::AciService => {
            let mut router = RoutingUpstreamVerifier::new();
            for cfg in config {
                let verifier = build_aci_service_verifier(cfg, options)?;
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
    if !config.iter().any(|cfg| provider_is_tee(cfg.provider)) {
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
            UpstreamProvider::OpenAiCompatible | UpstreamProvider::Anthropic => {
                build_global_verifier_for_config(cfg, options)?
            }
            UpstreamProvider::AciService => Some(build_aci_service_verifier(cfg, options)?),
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
                verifier = verifier.with_basic_auth(cfg.basic_auth);
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
            UpstreamProvider::PhalaDirect => {
                let mut verifier = PhalaDirectProviderVerifier::new_with_cache(
                    request_timeout_seconds,
                    cache_seconds,
                );
                if let Some(token) = &cfg.bearer_token {
                    verifier = verifier.with_bearer_token(token.clone());
                }
                Some(Arc::new(verifier))
            }
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
        UpstreamVerifierMode::AciService => {
            let has_explicit_aci_policy = cfg
                .accepted_workload_ids
                .as_ref()
                .is_some_and(|ids| !ids.is_empty())
                || cfg
                    .accepted_image_digests
                    .as_ref()
                    .is_some_and(|digests| !digests.is_empty());
            if has_explicit_aci_policy {
                build_aci_service_verifier(cfg, options).map(Some)
            } else {
                Ok(None)
            }
        }
    }
}

fn build_aci_service_verifier(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
) -> Result<Arc<dyn UpstreamVerifier>, UpstreamConfigError> {
    let policy = AciServiceVerifierPolicy::new(
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
            AciServiceUpstreamVerifier::new_with_timeouts(
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
            AciServiceUpstreamVerifier::with_default_pccs_and_timeouts(
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
