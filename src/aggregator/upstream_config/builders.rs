//! Backend/verifier construction from upstream config.

use std::sync::Arc;
use std::time::Duration;

use super::dynamic::EmptyUpstreamBackend;
use super::validation::{validate_config, verification_refresh_seconds};
use super::{
    ConfiguredUpstreams, ProviderSessionRegistry, UpstreamConfig, UpstreamConfigError,
    UpstreamProvider, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use crate::aci::digest;
use crate::aci::upstream::{
    CacheSaltField, ChutesProviderBackend, ChutesSessionStore, ModelRoute, ModelRouterBackend,
    NearAiBackend, OpenAICompatibleBackend, PrivatemodeProviderBackend, PrivatemodeProxyDeployment,
    UpstreamBackend, ZeroGProviderBackend,
};
use crate::aci::verifier::{
    AciServiceUpstreamVerifier, AciServiceVerifierConfigError, AciServiceVerifierPolicy,
    ArmetAiProviderVerifier, ChutesProviderVerifier, NearAiProviderVerifier,
    PhalaDirectProviderVerifier, PreverifiedUpstreamVerifier, PrivatemodeProviderVerifier,
    RoutingUpstreamVerifier, SecretAiProviderVerifier, TinfoilProviderVerifier,
    ZeroGProviderVerifier,
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

pub(super) struct BuildError {
    field: &'static str,
    error: UpstreamConfigError,
}

impl BuildError {
    fn new(field: &'static str, error: impl ToString) -> Self {
        Self {
            field,
            error: UpstreamConfigError::InvalidConfig(error.to_string()),
        }
    }

    fn http_client(error: impl ToString) -> Self {
        Self::new("http_client", error)
    }

    fn aci_service(error: AciServiceVerifierConfigError) -> Self {
        use AciServiceVerifierConfigError::*;
        let field = match error {
            EmptyPolicy => "accepted_subjects|accepted_image_digests",
            EmptyKmsRootPolicy | InvalidKmsRootPublicKey(_) => {
                "accepted_dstack_kms_root_public_keys"
            }
            EmptyBaseUrl | InvalidBaseUrl(_) => "base_url",
            Client(_) => "http_client",
        };
        Self::new(field, error)
    }

    pub(super) fn into_parts(self) -> (&'static str, UpstreamConfigError) {
        (self.field, self.error)
    }
}

impl From<BuildError> for UpstreamConfigError {
    fn from(error: BuildError) -> Self {
        // Production keeps the same error details. The offline checker uses only the field.
        error.into_parts().1
    }
}

fn build_model_router(
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<ModelRouterBackend, UpstreamConfigError> {
    let mut router = ModelRouterBackend::new("model-router");
    for cfg in config {
        add_model_routes(&mut router, cfg, options, sessions)?;
    }
    Ok(router)
}

pub(super) fn add_model_routes(
    router: &mut ModelRouterBackend,
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<(), BuildError> {
    let backend = build_provider_backend(cfg, options, sessions)?;
    for (public_model, upstream_model) in &cfg.models {
        let route = ModelRoute::new(
            public_model.clone(),
            upstream_model.clone(),
            backend.clone(),
            format!("{}:{public_model}", cfg.name),
        )
        .map_err(|error| BuildError::new("models", error))?
        .with_path(cfg.path.clone())
        .with_streaming_usage(cfg.streaming_usage)
        .with_cache_salt_field(provider_cache_salt_field(cfg.provider))
        // NEAR's native JSON contains only text, without duration.
        .with_force_verbose_json(cfg.provider == UpstreamProvider::NearAi)
        .with_is_tee(Some(provider_is_tee(cfg.provider)));
        router
            .add_route(route)
            .map_err(|error| BuildError::new("models", error))?;
    }
    Ok(())
}

/// Cache controls are an explicit provider capability, not inferred from an
/// OpenAI-compatible wire format. Tags match `UpstreamProvider`'s serde names.
pub(super) fn provider_cache_salt_field(provider: UpstreamProvider) -> Option<CacheSaltField> {
    match provider {
        UpstreamProvider::Chutes => Some(CacheSaltField::CacheSalt("chutes")),
        UpstreamProvider::Privatemode => Some(CacheSaltField::CacheSalt("privatemode")),
        UpstreamProvider::Tinfoil => Some(CacheSaltField::UserCacheSecret("tinfoil")),
        UpstreamProvider::PhalaDirect => Some(CacheSaltField::CacheSalt("phala-direct")),
        UpstreamProvider::NearAi => Some(CacheSaltField::CacheSalt("near-ai")),
        UpstreamProvider::ArmetAi => Some(CacheSaltField::CacheSalt("armet-ai")),
        UpstreamProvider::ZeroGPrivateComputer => {
            Some(CacheSaltField::CacheSalt("0g-private-computer"))
        }
        UpstreamProvider::OpenAiCompatible
        | UpstreamProvider::Anthropic
        | UpstreamProvider::AciService
        | UpstreamProvider::SecretAi => None,
    }
}

/// Whether a provider route may serve `provider.aci_verified` requests. Plain
/// OpenAI-compatible cloud APIs have no provider attestation and are therefore
/// ineligible for that constraint.
pub(super) fn provider_is_tee(provider: UpstreamProvider) -> bool {
    match provider {
        UpstreamProvider::OpenAiCompatible | UpstreamProvider::Anthropic => false,
        UpstreamProvider::AciService
        | UpstreamProvider::Chutes
        | UpstreamProvider::Tinfoil
        | UpstreamProvider::NearAi
        | UpstreamProvider::SecretAi
        | UpstreamProvider::Privatemode
        | UpstreamProvider::PhalaDirect
        | UpstreamProvider::ArmetAi
        | UpstreamProvider::ZeroGPrivateComputer => true,
    }
}

pub(super) fn has_verifier_route(
    cfg: &UpstreamConfig,
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
) -> bool {
    provider_is_tee(cfg.provider)
        || match options.verifier_mode {
            UpstreamVerifierMode::None => false,
            UpstreamVerifierMode::Preverified => true,
            UpstreamVerifierMode::AciService => {
                !config.iter().any(|cfg| provider_is_tee(cfg.provider))
                    || cfg
                        .accepted_subjects
                        .as_ref()
                        .is_some_and(|ids| !ids.is_empty())
                    || cfg
                        .accepted_image_digests
                        .as_ref()
                        .is_some_and(|ids| !ids.is_empty())
            }
        }
}

fn build_provider_backend(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<Arc<dyn UpstreamBackend>, BuildError> {
    let connect_timeout_seconds = cfg
        .connect_timeout_seconds
        .unwrap_or(options.connect_timeout_seconds);
    let read_timeout_seconds = cfg
        .read_timeout_seconds
        .unwrap_or(options.read_timeout_seconds);
    match cfg.provider {
        UpstreamProvider::Chutes => {
            let session_store = sessions.chutes(&cfg.name).ok_or_else(|| {
                BuildError::new(
                    "session_store",
                    format!(
                        "missing Chutes provider session store for upstream {:?}",
                        cfg.name
                    ),
                )
            })?;
            Ok(Arc::new(
                build_chutes_provider_backend(cfg, options, session_store).map_err(|error| {
                    BuildError {
                        field: "http_client",
                        error,
                    }
                })?,
            ))
        }
        UpstreamProvider::Privatemode => {
            let backend = PrivatemodeProviderBackend::new_with_timeouts(
                privatemode_deployment(cfg, options)?,
                connect_timeout_seconds,
                read_timeout_seconds,
            )
            .map_err(BuildError::http_client)?
            .with_name(cfg.name.clone())
            .with_max_buffered_response_bytes(options.max_buffered_response_bytes);
            Ok(Arc::new(backend))
        }
        UpstreamProvider::ZeroGPrivateComputer => Ok(Arc::new(
            ZeroGProviderBackend::new_with_timeouts(
                cfg.base_url.clone(),
                cfg.bearer_token.clone().unwrap_or_default(),
                connect_timeout_seconds,
                read_timeout_seconds,
            )
            .map_err(BuildError::http_client)?
            .with_name(cfg.name.clone())
            .with_max_buffered_response_bytes(options.max_buffered_response_bytes),
        )),
        UpstreamProvider::OpenAiCompatible
        | UpstreamProvider::Anthropic
        | UpstreamProvider::AciService
        | UpstreamProvider::Tinfoil
        | UpstreamProvider::NearAi
        | UpstreamProvider::SecretAi
        | UpstreamProvider::PhalaDirect
        | UpstreamProvider::ArmetAi => {
            let mut backend = OpenAICompatibleBackend::new_with_timeouts(
                cfg.base_url.clone(),
                connect_timeout_seconds,
                read_timeout_seconds,
            )
            .map_err(BuildError::http_client)?
            .with_name(cfg.name.clone())
            .with_max_buffered_response_bytes(options.max_buffered_response_bytes);
            if cfg.provider == UpstreamProvider::Anthropic {
                backend = backend.with_anthropic_protocol();
                if let Some(token) = &cfg.bearer_token {
                    backend = backend.with_anthropic_api_key(token.clone());
                }
            } else if let Some(token) = &cfg.bearer_token {
                backend = backend.with_bearer_token(token.clone());
            }
            backend = backend.with_basic_auth(cfg.basic_auth);
            if cfg.provider == UpstreamProvider::NearAi && !cfg.basic_auth {
                if let Some(token) = cfg.bearer_token.as_ref().filter(|token| !token.is_empty()) {
                    return Ok(Arc::new(
                        NearAiBackend::new(backend, cfg.base_url.clone(), token.clone())
                            .map_err(BuildError::http_client)?,
                    ));
                }
            }
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
    .with_max_buffered_response_bytes(options.max_buffered_response_bytes)
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
    let mut router = RoutingUpstreamVerifier::new();
    let mut has_routes = false;
    for cfg in config {
        let Some(verifier) = build_provider_verifier(cfg, config, options, sessions)? else {
            continue;
        };
        router = router.add_route(
            cfg.name.clone(),
            cfg.base_url.trim_end_matches('/').to_string(),
            verifier,
        );
        has_routes = true;
    }
    Ok(has_routes.then(|| Arc::new(router) as Arc<dyn UpstreamVerifier>))
}

pub(super) fn build_provider_verifier(
    cfg: &UpstreamConfig,
    config: &[UpstreamConfig],
    options: &UpstreamRuntimeOptions,
    sessions: &ProviderSessionRegistry,
) -> Result<Option<Arc<dyn UpstreamVerifier>>, BuildError> {
    if !has_verifier_route(cfg, config, options) {
        return Ok(None);
    }
    let cache_seconds = cfg
        .verifier_cache_seconds
        .unwrap_or(options.verifier_cache_seconds);
    let request_timeout_seconds = cfg
        .verifier_request_timeout_seconds
        .unwrap_or(options.verifier_request_timeout_seconds);
    let verifier: Arc<dyn UpstreamVerifier> = match cfg.provider {
        UpstreamProvider::OpenAiCompatible | UpstreamProvider::Anthropic => {
            match options.verifier_mode {
                UpstreamVerifierMode::None => return Ok(None),
                UpstreamVerifierMode::Preverified => Arc::new(PreverifiedUpstreamVerifier::new(
                    "preverified/out-of-band/v1",
                )),
                UpstreamVerifierMode::AciService => build_aci_service_verifier(cfg, options)?,
            }
        }
        UpstreamProvider::AciService => build_aci_service_verifier(cfg, options)?,
        UpstreamProvider::Chutes => {
            let session_store = sessions.chutes(&cfg.name).ok_or_else(|| {
                BuildError::new(
                    "session_store",
                    format!(
                        "missing Chutes provider session store for upstream {:?}",
                        cfg.name
                    ),
                )
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
            Arc::new(verifier)
        }
        UpstreamProvider::Tinfoil => Arc::new(
            TinfoilProviderVerifier::new_with_cache(request_timeout_seconds, cache_seconds)
                .with_accepted_subjects(cfg.accepted_subjects.as_deref().unwrap_or_default()),
        ),
        UpstreamProvider::NearAi => {
            let mut verifier =
                NearAiProviderVerifier::new_with_cache(request_timeout_seconds, cache_seconds);
            if let Some(period) = verification_refresh_seconds(cfg, options) {
                verifier = verifier
                    .with_refresh_lead(Duration::from_secs(cache_seconds.saturating_sub(period)));
            }
            if let Some(token) = &cfg.bearer_token {
                verifier = verifier.with_api_key(token.clone());
            }
            Arc::new(verifier)
        }
        UpstreamProvider::SecretAi => {
            let accepted_subjects = cfg.accepted_subjects.as_deref().unwrap_or_default();
            if accepted_subjects.is_empty() {
                tracing::warn!(upstream = %cfg.name, "SecretAI has no accepted_subjects workload pin configured");
            }
            let verifier =
                SecretAiProviderVerifier::new_with_cache(request_timeout_seconds, cache_seconds)
                    .with_accepted_subjects(accepted_subjects.iter().cloned());
            Arc::new(verifier)
        }
        UpstreamProvider::Privatemode => Arc::new(
            PrivatemodeProviderVerifier::new(
                privatemode_deployment(cfg, options)?,
                cfg.connect_timeout_seconds
                    .unwrap_or(options.connect_timeout_seconds),
                request_timeout_seconds,
                cache_seconds,
            )
            .map_err(BuildError::http_client)?,
        ),
        UpstreamProvider::PhalaDirect => {
            let mut verifier =
                PhalaDirectProviderVerifier::new_with_cache(request_timeout_seconds, cache_seconds)
                    .with_accepted_subjects(cfg.accepted_subjects.clone().unwrap_or_default());
            if let Some(token) = &cfg.bearer_token {
                verifier = verifier.with_bearer_token(token.clone());
            }
            Arc::new(verifier)
        }
        // Validation guarantees the token, RPC, contract and subjects are present.
        UpstreamProvider::ZeroGPrivateComputer => Arc::new(
            ZeroGProviderVerifier::new(
                cfg.base_url.clone(),
                cfg.bearer_token.clone().unwrap_or_default(),
                cfg.zg_chain_rpc_url.clone().unwrap_or_default(),
                cfg.zg_serving_contract.clone().unwrap_or_default(),
                cfg.pccs_url.clone().or_else(|| options.pccs_url.clone()),
                request_timeout_seconds,
                cache_seconds,
            )
            .map_err(BuildError::http_client)?
            .with_accepted_subjects(cfg.accepted_subjects.clone().unwrap_or_default()),
        ),
        UpstreamProvider::ArmetAi => {
            let mut verifier =
                ArmetAiProviderVerifier::new_with_cache(request_timeout_seconds, cache_seconds)
                    .with_accepted_subjects(cfg.accepted_subjects.clone().unwrap_or_default());
            if let Some(token) = &cfg.bearer_token {
                verifier = verifier.with_bearer_token(token.clone());
            }
            Arc::new(verifier)
        }
    };
    Ok(Some(verifier))
}

/// The static, measured sidecar a Privatemode route may select. Mutable route
/// config names it by origin but cannot replace it.
fn privatemode_deployment(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
) -> Result<Arc<PrivatemodeProxyDeployment>, BuildError> {
    let deployment = options.privatemode_proxy.clone().ok_or_else(|| {
        BuildError::new(
            "privatemode_proxy",
            format!(
                "Privatemode upstream {:?} requires static privatemode_proxy gateway config",
                cfg.name
            ),
        )
    })?;
    if cfg.base_url.trim_end_matches('/') != deployment.base_url() {
        return Err(BuildError::new(
            "base_url",
            format!(
                "Privatemode upstream {:?} base_url {:?} does not match static proxy endpoint {:?}",
                cfg.name,
                cfg.base_url,
                deployment.base_url()
            ),
        ));
    }
    Ok(deployment)
}

fn build_aci_service_verifier(
    cfg: &UpstreamConfig,
    options: &UpstreamRuntimeOptions,
) -> Result<Arc<dyn UpstreamVerifier>, BuildError> {
    let policy = AciServiceVerifierPolicy::new(
        cfg.accepted_subjects
            .clone()
            .unwrap_or_else(|| options.accepted_subjects.clone()),
        cfg.accepted_image_digests
            .clone()
            .unwrap_or_else(|| options.accepted_image_digests.clone()),
        cfg.accepted_dstack_kms_root_public_keys
            .clone()
            .unwrap_or_else(|| options.accepted_dstack_kms_root_public_keys.clone()),
    )
    .map_err(BuildError::aci_service)?;
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
            .map_err(BuildError::aci_service)?
            .with_max_buffered_response_bytes(options.max_buffered_response_bytes),
        )),
        None => Ok(Arc::new(
            AciServiceUpstreamVerifier::with_default_pccs_and_timeouts(
                cfg.base_url.clone(),
                policy,
                cache_seconds,
                connect_timeout_seconds,
                request_timeout_seconds,
            )
            .map_err(BuildError::aci_service)?
            .with_max_buffered_response_bytes(options.max_buffered_response_bytes),
        )),
    }
}

pub(super) fn config_digest(config: &[UpstreamConfig]) -> Result<String, UpstreamConfigError> {
    // A local version stamp over the serialized config, not a protocol artifact.
    let bytes = serde_json::to_vec(config).map_err(|e| {
        UpstreamConfigError::InvalidConfig(format!("failed to serialize upstream config: {e}"))
    })?;
    Ok(digest::sha256_hex(&bytes))
}
