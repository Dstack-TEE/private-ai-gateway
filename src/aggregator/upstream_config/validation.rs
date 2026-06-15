//! Upstream config file parsing/validation and verification-target derivation.

use std::collections::HashSet;
use std::path::Path;

use super::{
    ConfiguredUpstreams, UpstreamConfig, UpstreamConfigError, UpstreamConfigSnapshot,
    UpstreamProvider, UpstreamRuntimeOptions,
};

const DEFAULT_UPSTREAM_SESSION_REFRESH_SECONDS: u64 = 45;

/// One configured model-endpoint to verify: the upstream's config `name`, the
/// configured upstream `model_id`, and the endpoint origin. Drives both the
/// verifier-cache prewarm and the attested-session writes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct UpstreamVerificationTarget {
    pub(super) upstream_name: String,
    pub(super) model_id: String,
    pub(super) url_origin: Option<String>,
}

pub(super) fn verification_targets(config: &[UpstreamConfig]) -> Vec<UpstreamVerificationTarget> {
    verification_targets_for_configs(config.iter())
}

pub(super) fn verification_targets_for_refresh(
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

pub(super) fn verification_refresh_seconds(
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

pub(super) fn session_refresh_seconds(cfg: &UpstreamConfig) -> Option<u64> {
    match cfg.session_refresh_seconds {
        Some(0) => None,
        Some(seconds) => Some(seconds),
        None => (cfg.provider == UpstreamProvider::Chutes)
            .then_some(DEFAULT_UPSTREAM_SESSION_REFRESH_SECONDS),
    }
}

pub(super) fn unique_upstream_models(cfg: &UpstreamConfig) -> Vec<String> {
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

pub(super) fn snapshot_for(path: &Path, state: &ConfiguredUpstreams) -> UpstreamConfigSnapshot {
    UpstreamConfigSnapshot {
        config_path: path.display().to_string(),
        config_digest: state.config_digest.clone(),
        upstreams: state.config.iter().map(UpstreamConfig::redacted).collect(),
    }
}

pub(super) fn read_config_file(path: &Path) -> Result<Vec<UpstreamConfig>, UpstreamConfigError> {
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

pub(super) fn validate_config(config: &[UpstreamConfig]) -> Result<(), UpstreamConfigError> {
    let mut names = HashSet::new();
    let mut route_ids = HashSet::new();
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
        if upstream.auth_passthrough && upstream.bearer_token.is_some() {
            return Err(UpstreamConfigError::InvalidConfig(format!(
                "upstream {:?} sets both auth_passthrough and bearer_token; \
                 a passthrough upstream must not configure a static token",
                upstream.name
            )));
        }
        if upstream.auth_passthrough
            && !super::builders::provider_supports_auth_passthrough(upstream.provider)
        {
            return Err(UpstreamConfigError::InvalidConfig(format!(
                "upstream {:?} sets auth_passthrough but provider {:?} does not \
                 support it (only OpenAI-compatible-forwarded providers do)",
                upstream.name, upstream.provider
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
            let route_id = format!("{}:{public_model}", upstream.name);
            if !route_ids.insert(route_id.clone()) {
                return Err(UpstreamConfigError::InvalidConfig(format!(
                    "route id {route_id:?} is duplicated"
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn write_config_file(
    path: &Path,
    config: &[UpstreamConfig],
) -> Result<(), UpstreamConfigError> {
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
