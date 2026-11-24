//! Upstream config file parsing/validation and verification-target derivation.

use std::collections::HashSet;
use std::path::Path;

use crate::aci::verifier::TinfoilProviderVerifier;

use super::{
    ConfiguredUpstreams, UpstreamConfig, UpstreamConfigError, UpstreamConfigSnapshot,
    UpstreamProvider, UpstreamRuntimeOptions,
};

const DEFAULT_UPSTREAM_SESSION_REFRESH_SECONDS: u64 = 45;

/// One configured model-endpoint to verify: the upstream's config `name`, the
/// configured upstream `model_id`, and the endpoint origin. Drives both the
/// verifier-cache refresh and the attested-session writes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct UpstreamVerificationTarget {
    pub(super) upstream_name: String,
    pub(super) model_id: String,
    pub(super) url_origin: Option<String>,
}

impl UpstreamVerificationTarget {
    pub(super) fn request(&self) -> crate::aggregator::service::UpstreamVerificationRequest {
        crate::aggregator::service::UpstreamVerificationRequest {
            upstream_name: self.upstream_name.clone(),
            url_origin: self.url_origin.clone(),
            model_id: self.model_id.clone(),
            forwarded_body_hash: crate::aci::digest::sha256_hex(b""),
            required: true,
        }
    }
}

pub(super) fn verification_targets(config: &[UpstreamConfig]) -> Vec<UpstreamVerificationTarget> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for cfg in config {
        let url_origin = Some(cfg.base_url.trim_end_matches('/').to_string());
        // NEAR AI validates the selected model even though its evidence is
        // router-scoped. Routers with shared verification use one model.
        let model_ids: Vec<&String> = if cfg.provider.shares_verification_across_models() {
            cfg.models.values().take(1).collect()
        } else {
            cfg.models.values().collect()
        };
        for model_id in model_ids {
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
        None => Some(
            cfg.verifier_cache_seconds
                .unwrap_or(options.verifier_cache_seconds)
                .saturating_sub(
                    cfg.verifier_request_timeout_seconds
                        .unwrap_or(options.verifier_request_timeout_seconds),
                )
                .max(1),
        ),
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

fn valid_root_https_origin(value: &str) -> bool {
    let Ok(origin) = reqwest::Url::parse(value) else {
        return false;
    };
    origin.scheme() == "https"
        && origin.host_str().is_some()
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
}

/// `tdx-measurement:sha256:<64 lowercase hex>`, the pin format the armet-ai
/// bridge derives from the verified MRTD, RTMR0-3 and TD launch configuration.
pub(super) fn valid_tdx_measurement_subject(value: &str) -> bool {
    value
        .strip_prefix("tdx-measurement:sha256:")
        .is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// `tdx-boot-chain:sha256:<64 lowercase hex>`: SHA-256 over a dstack OS
/// image's MRTD ‖ RTMR1 ‖ RTMR2, which a 0G provider's quote must match.
fn valid_tdx_boot_chain_subject(value: &str) -> bool {
    value
        .strip_prefix("tdx-boot-chain:sha256:")
        .is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
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
    validate_config_with(config, |_, _, error| Err(error))
}

pub(super) fn validate_config_with(
    config: &[UpstreamConfig],
    mut report: impl FnMut(
        &UpstreamConfig,
        &'static str,
        UpstreamConfigError,
    ) -> Result<(), UpstreamConfigError>,
) -> Result<(), UpstreamConfigError> {
    let mut names = HashSet::new();
    let mut route_ids = HashSet::new();
    for upstream in config {
        let mut invalid =
            |field, message| report(upstream, field, UpstreamConfigError::InvalidConfig(message));
        if upstream.name.trim().is_empty() {
            invalid("name", "upstream name must not be empty".to_string())?;
        }
        if !names.insert(upstream.name.as_str()) {
            invalid(
                "name",
                format!("upstream name {:?} is duplicated", upstream.name),
            )?;
        }
        if upstream.base_url.trim().is_empty() {
            invalid(
                "base_url",
                format!("upstream {:?} base_url must not be empty", upstream.name),
            )?;
        }
        if upstream
            .path
            .as_deref()
            .is_some_and(|path| path.trim_start_matches('/') == "v1/audio/transcriptions")
            && matches!(
                upstream.provider,
                UpstreamProvider::Chutes
                    | UpstreamProvider::ZeroGPrivateComputer
                    | UpstreamProvider::Anthropic
            )
        {
            invalid(
                "path",
                "this provider does not support audio transcriptions".into(),
            )?;
        }
        if upstream.streaming_usage.is_some()
            && upstream
                .path
                .as_deref()
                .is_some_and(|path| !crate::aci::upstream::supports_streaming_usage(path))
        {
            invalid(
                "streaming_usage",
                format!(
                    "upstream {:?} streaming_usage requires an OpenAI chat/completions path",
                    upstream.name
                ),
            )?;
        }
        if upstream.models.is_empty() {
            invalid(
                "models",
                format!(
                    "upstream {:?} must route at least one public model",
                    upstream.name
                ),
            )?;
        }
        if upstream.provider == UpstreamProvider::SecretAi
            && !valid_root_https_origin(&upstream.base_url)
        {
            invalid("base_url", format!(
                "upstream {:?} provider secret-ai requires a root HTTPS base_url without userinfo, query, or fragment",
                upstream.name
            ))?;
        }
        if upstream.provider == UpstreamProvider::Tinfoil {
            let subjects = upstream.accepted_subjects.as_deref().unwrap_or_default();
            for pin in subjects.iter().filter_map(|subject| {
                subject.strip_prefix(TinfoilProviderVerifier::RELEASE_SUBJECT_PREFIX)
            }) {
                if !pin.strip_prefix("sha256:").is_some_and(|hex| {
                    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                }) {
                    invalid("accepted_subjects", format!(
                        "upstream {:?} provider tinfoil accepted_subjects release pins must be tinfoil-release:sha256:<64 lowercase hex>",
                        upstream.name
                    ))?;
                }
            }
        }
        if upstream.provider == UpstreamProvider::ArmetAi {
            if !valid_root_https_origin(&upstream.base_url) {
                invalid("base_url", format!(
                    "upstream {:?} provider armet-ai requires a root HTTPS base_url without userinfo, query, or fragment",
                    upstream.name
                ))?;
            }
            let subjects = upstream.accepted_subjects.as_deref().unwrap_or_default();
            if subjects.is_empty() {
                invalid("accepted_subjects", format!(
                    "upstream {:?} provider armet-ai requires at least one accepted_subjects measurement pin",
                    upstream.name
                ))?;
            }
            if let Some(bad) = subjects.iter().find(|s| !valid_tdx_measurement_subject(s)) {
                invalid("accepted_subjects", format!(
                    "upstream {:?} provider armet-ai accepted_subjects entry {bad:?} must be tdx-measurement:sha256:<64 lowercase hex>",
                    upstream.name
                ))?;
            }
        }
        if upstream.provider == UpstreamProvider::PhalaDirect {
            let subjects = upstream.accepted_subjects.as_deref().unwrap_or_default();
            if let Some(bad) = subjects.iter().find(|subject| {
                subject.starts_with("dstack-compose:")
                    && !subject
                        .strip_prefix("dstack-compose:sha256:")
                        .is_some_and(|hex| {
                            hex.len() == 64
                                && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                        })
            }) {
                invalid("accepted_subjects", format!(
                    "upstream {:?} provider phala-direct accepted_subjects entry {bad:?} must be dstack-compose:sha256:<64 lowercase hex>",
                    upstream.name
                ))?;
            }
        }
        if upstream.basic_auth {
            if upstream.bearer_token.is_none() {
                invalid(
                    "bearer_token",
                    format!(
                        "upstream {:?} basic_auth requires bearer_token",
                        upstream.name
                    ),
                )?;
            }
            if !matches!(
                upstream.provider,
                UpstreamProvider::OpenAiCompatible | UpstreamProvider::Chutes
            ) {
                invalid("basic_auth", format!(
                    "upstream {:?} basic_auth is supported only for openai-compatible or chutes providers",
                    upstream.name
                ))?;
            }
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
                invalid(
                    field,
                    format!(
                        "upstream {:?} {field} must be greater than zero",
                        upstream.name
                    ),
                )?;
            }
        }
        if let Some(rounds) = upstream.chutes_e2ee_discovery_rounds {
            if rounds == 0 || rounds > 10 {
                invalid(
                    "chutes_e2ee_discovery_rounds",
                    format!(
                        "upstream {:?} chutes_e2ee_discovery_rounds must be between 1 and 10",
                        upstream.name
                    ),
                )?;
            }
        }
        if let Some(base) = upstream.chutes_e2ee_api_base.as_ref() {
            if base.trim().is_empty() {
                invalid(
                    "chutes_e2ee_api_base",
                    format!(
                        "upstream {:?} chutes_e2ee_api_base must not be empty",
                        upstream.name
                    ),
                )?;
            }
        }
        if let Some(chute_ids) = upstream.chutes_chute_ids.as_ref() {
            if chute_ids.is_empty() {
                invalid(
                    "chutes_chute_ids",
                    format!(
                        "upstream {:?} chutes_chute_ids must not be empty when configured",
                        upstream.name
                    ),
                )?;
            }
            let configured_upstream_models = upstream
                .models
                .values()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            for (model_id, chute_id) in chute_ids {
                if model_id.trim().is_empty() {
                    invalid(
                        "chutes_chute_ids",
                        format!(
                            "upstream {:?} chutes_chute_ids has an empty model id",
                            upstream.name
                        ),
                    )?;
                }
                if !configured_upstream_models.contains(model_id.as_str()) {
                    invalid("chutes_chute_ids", format!(
                        "upstream {:?} chutes_chute_ids key {model_id:?} is not one of its upstream model ids",
                        upstream.name
                    ))?;
                }
                if !looks_like_uuid(chute_id) {
                    invalid(
                        "chutes_chute_ids",
                        format!(
                            "upstream {:?} chutes_chute_ids[{model_id:?}] must be a chute_id UUID",
                            upstream.name
                        ),
                    )?;
                }
            }
        }
        if upstream.provider != UpstreamProvider::Chutes {
            for (field, is_set) in [
                (
                    "chutes_e2ee_api_base",
                    upstream.chutes_e2ee_api_base.is_some(),
                ),
                ("chutes_chute_ids", upstream.chutes_chute_ids.is_some()),
                (
                    "chutes_e2ee_discovery_rounds",
                    upstream.chutes_e2ee_discovery_rounds.is_some(),
                ),
                (
                    "chutes_e2ee_discovery_interval_seconds",
                    upstream.chutes_e2ee_discovery_interval_seconds.is_some(),
                ),
            ] {
                if is_set {
                    invalid(
                        field,
                        format!(
                            "upstream {:?} has Chutes E2EE fields but provider is not chutes",
                            upstream.name
                        ),
                    )?;
                }
            }
        }
        // The private trust tier and the TargetSeparated rule are code, not config.
        if upstream.provider == UpstreamProvider::ZeroGPrivateComputer {
            let rpc_is_https = upstream
                .zg_chain_rpc_url
                .as_deref()
                .and_then(|url| reqwest::Url::parse(url).ok())
                .is_some_and(|url| url.scheme() == "https");
            let contract_is_address = upstream.zg_serving_contract.as_deref().is_some_and(|v| {
                v.len() == 42 && v.starts_with("0x") && hex::decode(&v[2..]).is_ok()
            });
            let subjects = upstream.accepted_subjects.as_deref().unwrap_or_default();
            for (field, valid) in [
                ("base_url", valid_root_https_origin(&upstream.base_url)),
                (
                    "accepted_subjects",
                    !subjects.is_empty()
                        && subjects.iter().all(|s| valid_tdx_boot_chain_subject(s)),
                ),
                (
                    "bearer_token",
                    upstream
                        .bearer_token
                        .as_deref()
                        .is_some_and(|token| !token.trim().is_empty()),
                ),
                ("path", upstream.path.is_none()),
                ("zg_chain_rpc_url", rpc_is_https),
                ("zg_serving_contract", contract_is_address),
            ] {
                if !valid {
                    invalid(field, format!(
                        "upstream {:?} provider 0g-private-computer requires the 0G router as a root HTTPS base_url, bearer_token, an HTTPS zg_chain_rpc_url, zg_serving_contract (0x + 40 hex) and accepted_subjects pins (tdx-boot-chain:sha256:<64 lowercase hex>), and forbids path",
                        upstream.name
                    ))?;
                }
            }
        } else {
            for (field, is_set) in [
                ("zg_chain_rpc_url", upstream.zg_chain_rpc_url.is_some()),
                (
                    "zg_serving_contract",
                    upstream.zg_serving_contract.is_some(),
                ),
            ] {
                if is_set {
                    invalid(
                        field,
                        format!(
                            "upstream {:?} has 0G fields but provider is not 0g-private-computer",
                            upstream.name
                        ),
                    )?;
                }
            }
        }
        if upstream.provider == UpstreamProvider::Privatemode && upstream.bearer_token.is_some() {
            invalid("bearer_token", format!(
                "upstream {:?} provider privatemode forbids bearer_token; the measured proxy reads its credential from the shared Compose secret",
                upstream.name
            ))?;
        }
        if upstream.provider == UpstreamProvider::Privatemode && upstream.path.is_some() {
            invalid("path", format!(
                "upstream {:?} provider privatemode forbids path; the measured proxy backend pins encrypted handler paths",
                upstream.name
            ))?;
        }
        // The native Anthropic API only serves /v1/messages; without an
        // explicit path the router falls back to /v1/chat/completions and
        // every request 404s with no config-time signal.
        if upstream.provider == UpstreamProvider::Anthropic
            && upstream.path.as_deref().is_none_or(str::is_empty)
        {
            invalid(
                "path",
                format!(
                    "upstream {:?} provider anthropic requires path (e.g. \"/v1/messages\")",
                    upstream.name
                ),
            )?;
        }
        for (public_model, upstream_model) in &upstream.models {
            if public_model.trim().is_empty() {
                invalid(
                    "models",
                    format!("upstream {:?} has an empty public model id", upstream.name),
                )?;
            }
            if upstream_model.trim().is_empty() {
                invalid(
                    "models",
                    format!(
                        "upstream {:?} route {:?} has an empty upstream model id",
                        upstream.name, public_model
                    ),
                )?;
            }
            let route_id = format!("{}:{public_model}", upstream.name);
            if !route_ids.insert(route_id.clone()) {
                invalid("models", format!("route id {route_id:?} is duplicated"))?;
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
