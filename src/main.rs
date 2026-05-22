//! Gateway entrypoint.
//!
//! The runtime key provider and quoter are backed by dstack KMS and
//! the dstack TDX quote API through the Rust dstack SDK. There is no
//! ephemeral-key or stub-quote startup path.
//!
//! Configuration (env vars). Each setting uses the
//! `PRIVATE_AI_GATEWAY_*` prefix. The older `DSTACK_LLM_ROUTER_*`
//! names are still accepted as compatibility aliases; the
//! `PRIVATE_AI_GATEWAY_*` value wins when both are set.
//!
//! | Setting | Primary name | Compatibility alias |
//! | --- | --- | --- |
//! | Bind address | `PRIVATE_AI_GATEWAY_BIND` | `DSTACK_LLM_ROUTER_BIND` |
//! | Upstream config path | `PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH` | `DSTACK_LLM_ROUTER_UPSTREAM_CONFIG_PATH` |
//! | Initial upstream config seed path | `PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_SEED_PATH` | `DSTACK_LLM_ROUTER_UPSTREAM_CONFIG_SEED_PATH` |
//! | Admin API bearer token | `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` | `DSTACK_LLM_ROUTER_ADMIN_TOKEN` |
//! | Source-provenance repo URL | `PRIVATE_AI_GATEWAY_REPO_URL` | `DSTACK_LLM_ROUTER_REPO_URL` |
//! | Source-provenance commit | `PRIVATE_AI_GATEWAY_REPO_COMMIT` | `DSTACK_LLM_ROUTER_REPO_COMMIT` |
//! | Body retention seconds | `PRIVATE_AI_GATEWAY_BODY_RETENTION_SECONDS` | `DSTACK_LLM_ROUTER_BODY_RETENTION_SECONDS` |
//! | Receipt TTL seconds | `PRIVATE_AI_GATEWAY_RECEIPT_TTL_SECONDS` | `DSTACK_LLM_ROUTER_RECEIPT_TTL_SECONDS` |
//! | TLS certificate paths | `PRIVATE_AI_GATEWAY_TLS_CERT_PATHS` | `DSTACK_LLM_ROUTER_TLS_CERT_PATHS` |
//! | TLS SPKI SHA-256 list | `PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256` | `DSTACK_LLM_ROUTER_TLS_SPKI_SHA256` |
//! | Upstream verifier mode (`none`, `preverified`, `aci-dcap`) | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER` | `DSTACK_LLM_ROUTER_UPSTREAM_VERIFIER` |
//! | Accepted upstream workload IDs | `PRIVATE_AI_GATEWAY_UPSTREAM_ACCEPTED_WORKLOAD_IDS` | `DSTACK_LLM_ROUTER_UPSTREAM_ACCEPTED_WORKLOAD_IDS` |
//! | Accepted upstream image digests | `PRIVATE_AI_GATEWAY_UPSTREAM_ACCEPTED_IMAGE_DIGESTS` | `DSTACK_LLM_ROUTER_UPSTREAM_ACCEPTED_IMAGE_DIGESTS` |
//! | Accepted upstream dstack KMS root public keys | `PRIVATE_AI_GATEWAY_UPSTREAM_DSTACK_KMS_ROOT_PUBLIC_KEYS` | `DSTACK_LLM_ROUTER_UPSTREAM_DSTACK_KMS_ROOT_PUBLIC_KEYS` |
//! | Upstream verifier PCCS URL | `PRIVATE_AI_GATEWAY_UPSTREAM_PCCS_URL` | `DSTACK_LLM_ROUTER_UPSTREAM_PCCS_URL` |
//! | Upstream connect timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_CONNECT_TIMEOUT_SECONDS` | `DSTACK_LLM_ROUTER_UPSTREAM_CONNECT_TIMEOUT_SECONDS` |
//! | Upstream read timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_READ_TIMEOUT_SECONDS` | `DSTACK_LLM_ROUTER_UPSTREAM_READ_TIMEOUT_SECONDS` |
//! | Upstream verifier request timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS` | `DSTACK_LLM_ROUTER_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS` |
//! | dstack endpoint | `PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT` | `DSTACK_LLM_ROUTER_DSTACK_ENDPOINT` |
//! | Optional plaintext HTTP middleware URL | `PRIVATE_AI_GATEWAY_MIDDLEWARE_URL` | none |
//! | Internal backend bind address for middleware mode | `PRIVATE_AI_GATEWAY_BACKEND_BIND` | none |

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use private_ai_gateway::aci::keys::{KeyProvider, Quoter};
use private_ai_gateway::aci::types::{KeysetEpoch, ServiceCapabilities, SourceProvenance, TlsSpki};
use private_ai_gateway::aci::upstream::{
    DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS, DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS,
};
use private_ai_gateway::aci::verifier::DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS;
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, InMemoryReceiptStore, SystemClock, UpstreamVerifier,
};
use private_ai_gateway::aggregator::upstream_config::{
    parse_config_text, UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::dstack::{DstackAciProvider, DstackAciProviderConfig};
use private_ai_gateway::http::{
    build_internal_backend_router, build_router_with_admin,
    build_router_with_admin_and_http_middleware, GatewayRequestStore,
};
use sha2::{Digest, Sha256};
use x509_parser::prelude::parse_x509_certificate;

/// Read an env var, preferring the current name over the compatibility alias.
fn env_pref(current: &str, alias: &str) -> Option<String> {
    std::env::var(current)
        .ok()
        .or_else(|| std::env::var(alias).ok())
}

fn parse_seconds(setting: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|e| format!("invalid {setting} seconds {value:?}: {e}"))
}

fn parse_positive_seconds(setting: &str, value: &str) -> Result<u64, String> {
    let seconds = parse_seconds(setting, value)?;
    if seconds == 0 {
        return Err(format!("{setting} seconds must be greater than zero"));
    }
    Ok(seconds)
}

fn parse_body_retention_seconds(value: &str) -> Result<u64, String> {
    parse_seconds("body retention", value)
}

fn parse_comma_list(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

fn invalid_input(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn validate_http_url(setting: &str, value: String) -> Result<String, std::io::Error> {
    let url = reqwest::Url::parse(&value)
        .map_err(|e| invalid_input(format!("invalid {setting} URL {value:?}: {e}")))?;
    match url.scheme() {
        "http" | "https" => Ok(value),
        other => Err(invalid_input(format!(
            "invalid {setting} URL {value:?}: unsupported scheme {other:?}"
        ))),
    }
}

fn parse_tls_spki_list(value: &str) -> Result<Vec<TlsSpki>, String> {
    let mut keys = Vec::new();
    for raw in value.split(',') {
        let item = raw.trim();
        if item.len() != 64 || !item.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            return Err(format!(
                "invalid TLS SPKI SHA-256 digest {item:?}: expected 64 hex characters"
            ));
        }
        keys.push(TlsSpki {
            spki_sha256_hex: item.to_ascii_lowercase(),
        });
    }
    if keys.is_empty() {
        return Err("TLS SPKI SHA-256 list must not be empty".to_string());
    }
    Ok(keys)
}

fn parse_tls_cert_paths(value: &str) -> Result<Vec<TlsSpki>, String> {
    let mut keys = Vec::new();
    for raw in value.split(',') {
        let path = raw.trim();
        if path.is_empty() {
            return Err("TLS certificate path list contains an empty path".to_string());
        }
        keys.push(tls_spki_from_cert_path(Path::new(path))?);
    }
    if keys.is_empty() {
        return Err("TLS certificate path list must not be empty".to_string());
    }
    Ok(keys)
}

fn tls_spki_from_cert_path(path: &Path) -> Result<TlsSpki, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("failed to read TLS certificate {}: {e}", path.display()))?;
    let der = leaf_certificate_der(&bytes)
        .map_err(|e| format!("failed to parse TLS certificate {}: {e}", path.display()))?;
    let (_, cert) = parse_x509_certificate(&der)
        .map_err(|e| format!("failed to parse X.509 certificate {}: {e}", path.display()))?;
    let digest = Sha256::digest(cert.public_key().raw);
    Ok(TlsSpki {
        spki_sha256_hex: hex::encode(digest),
    })
}

fn leaf_certificate_der(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = Cursor::new(bytes);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid PEM certificate: {e}"))?;
    if let Some(cert) = certs.first() {
        return Ok(cert.as_ref().to_vec());
    }
    Ok(bytes.to_vec())
}

fn resolve_tls_public_keys(
    cert_paths: Option<&str>,
    explicit_spkis: Option<&str>,
) -> Result<Option<Vec<TlsSpki>>, String> {
    match (cert_paths, explicit_spkis) {
        (Some(_), Some(_)) => Err(
            "set either PRIVATE_AI_GATEWAY_TLS_CERT_PATHS or PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256, not both"
                .to_string(),
        ),
        (Some(paths), None) => parse_tls_cert_paths(paths).map(Some),
        (None, Some(spkis)) => parse_tls_spki_list(spkis).map(Some),
        (None, None) => Ok(None),
    }
}

fn seed_upstream_config_if_empty(
    target_path: &Path,
    seed_path: Option<&str>,
) -> Result<(), std::io::Error> {
    let Some(seed_path) = seed_path else {
        return Ok(());
    };
    let seed_path = Path::new(seed_path);
    let target_has_config = match std::fs::read_to_string(target_path) {
        Ok(text) => !text.trim().is_empty(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(std::io::Error::new(
                err.kind(),
                format!(
                    "failed to read upstream config {} before applying seed: {err}",
                    target_path.display()
                ),
            ));
        }
    };
    if target_has_config {
        tracing::info!(
            target = %target_path.display(),
            seed = %seed_path.display(),
            "upstream config already exists; seed config not applied"
        );
        return Ok(());
    }

    let seed_text = std::fs::read_to_string(seed_path).map_err(|err| {
        std::io::Error::new(
            err.kind(),
            format!(
                "failed to read upstream config seed {}: {err}",
                seed_path.display()
            ),
        )
    })?;
    parse_config_text(&seed_text).map_err(|err| {
        invalid_input(format!(
            "invalid upstream config seed {}: {err}",
            seed_path.display()
        ))
    })?;
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target_path, seed_text)?;
    tracing::info!(
        target = %target_path.display(),
        seed = %seed_path.display(),
        "seeded initial upstream config"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind = env_pref("PRIVATE_AI_GATEWAY_BIND", "DSTACK_LLM_ROUTER_BIND")
        .unwrap_or_else(|| "127.0.0.1:8086".to_string());
    let upstream_config_path = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH",
        "DSTACK_LLM_ROUTER_UPSTREAM_CONFIG_PATH",
    )
    .unwrap_or_else(|| "/var/lib/private-ai-gateway/upstreams.json".to_string());
    let upstream_config_seed_path = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_SEED_PATH",
        "DSTACK_LLM_ROUTER_UPSTREAM_CONFIG_SEED_PATH",
    );
    let admin_token = env_pref(
        "PRIVATE_AI_GATEWAY_ADMIN_TOKEN",
        "DSTACK_LLM_ROUTER_ADMIN_TOKEN",
    );
    let repo_url = env_pref("PRIVATE_AI_GATEWAY_REPO_URL", "DSTACK_LLM_ROUTER_REPO_URL");
    let repo_commit = env_pref(
        "PRIVATE_AI_GATEWAY_REPO_COMMIT",
        "DSTACK_LLM_ROUTER_REPO_COMMIT",
    );
    let body_retention_seconds = env_pref(
        "PRIVATE_AI_GATEWAY_BODY_RETENTION_SECONDS",
        "DSTACK_LLM_ROUTER_BODY_RETENTION_SECONDS",
    )
    .as_deref()
    .map(parse_body_retention_seconds)
    .transpose()?
    .unwrap_or(0);
    let receipt_ttl_seconds = env_pref(
        "PRIVATE_AI_GATEWAY_RECEIPT_TTL_SECONDS",
        "DSTACK_LLM_ROUTER_RECEIPT_TTL_SECONDS",
    )
    .as_deref()
    .map(|value| parse_seconds("receipt TTL", value))
    .transpose()?
    .unwrap_or(3600);
    let tls_cert_paths = env_pref(
        "PRIVATE_AI_GATEWAY_TLS_CERT_PATHS",
        "DSTACK_LLM_ROUTER_TLS_CERT_PATHS",
    );
    let tls_spkis = env_pref(
        "PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256",
        "DSTACK_LLM_ROUTER_TLS_SPKI_SHA256",
    );
    let tls_public_keys = resolve_tls_public_keys(tls_cert_paths.as_deref(), tls_spkis.as_deref())?;
    let upstream_verifier_mode = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER",
        "DSTACK_LLM_ROUTER_UPSTREAM_VERIFIER",
    )
    .unwrap_or_else(|| "none".to_string());
    let upstream_verifier_mode = UpstreamVerifierMode::parse(&upstream_verifier_mode)
        .map_err(|e| invalid_input(e.to_string()))?;
    let upstream_verifier_cache_seconds = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_CACHE_SECONDS",
        "DSTACK_LLM_ROUTER_UPSTREAM_VERIFIER_CACHE_SECONDS",
    )
    .as_deref()
    .map(|value| parse_positive_seconds("upstream verifier cache", value))
    .transpose()?
    .unwrap_or(300);
    let upstream_connect_timeout_seconds = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_CONNECT_TIMEOUT_SECONDS",
        "DSTACK_LLM_ROUTER_UPSTREAM_CONNECT_TIMEOUT_SECONDS",
    )
    .as_deref()
    .map(|value| parse_positive_seconds("upstream connect timeout", value))
    .transpose()?
    .unwrap_or(DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS);
    let upstream_read_timeout_seconds = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_READ_TIMEOUT_SECONDS",
        "DSTACK_LLM_ROUTER_UPSTREAM_READ_TIMEOUT_SECONDS",
    )
    .as_deref()
    .map(|value| parse_positive_seconds("upstream read timeout", value))
    .transpose()?
    .unwrap_or(DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS);
    let upstream_verifier_request_timeout_seconds = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS",
        "DSTACK_LLM_ROUTER_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS",
    )
    .as_deref()
    .map(|value| parse_positive_seconds("upstream verifier request timeout", value))
    .transpose()?
    .unwrap_or(DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS);
    let upstream_accepted_workload_ids = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_ACCEPTED_WORKLOAD_IDS",
        "DSTACK_LLM_ROUTER_UPSTREAM_ACCEPTED_WORKLOAD_IDS",
    );
    let upstream_accepted_image_digests = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_ACCEPTED_IMAGE_DIGESTS",
        "DSTACK_LLM_ROUTER_UPSTREAM_ACCEPTED_IMAGE_DIGESTS",
    );
    let upstream_dstack_kms_root_public_keys = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_DSTACK_KMS_ROOT_PUBLIC_KEYS",
        "DSTACK_LLM_ROUTER_UPSTREAM_DSTACK_KMS_ROOT_PUBLIC_KEYS",
    );
    let upstream_pccs_url = env_pref(
        "PRIVATE_AI_GATEWAY_UPSTREAM_PCCS_URL",
        "DSTACK_LLM_ROUTER_UPSTREAM_PCCS_URL",
    );
    let dstack_endpoint = env_pref(
        "PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT",
        "DSTACK_LLM_ROUTER_DSTACK_ENDPOINT",
    )
    .or_else(|| {
        env_pref(
            "PRIVATE_AI_GATEWAY_DSTACK_QUOTER_URL",
            "DSTACK_LLM_ROUTER_DSTACK_QUOTER_URL",
        )
    });
    let middleware_url = std::env::var("PRIVATE_AI_GATEWAY_MIDDLEWARE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| validate_http_url("PRIVATE_AI_GATEWAY_MIDDLEWARE_URL", value))
        .transpose()?;
    let backend_bind = std::env::var("PRIVATE_AI_GATEWAY_BACKEND_BIND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1:19091".to_string());

    let provider = Arc::new(
        DstackAciProvider::new(dstack_endpoint, DstackAciProviderConfig::default()).await?,
    );
    let keys: Arc<dyn KeyProvider> = provider.clone();
    let quoter: Arc<dyn Quoter> = provider;
    seed_upstream_config_if_empty(
        Path::new(&upstream_config_path),
        upstream_config_seed_path.as_deref(),
    )?;
    let upstream_config = Arc::new(UpstreamConfigManager::load(
        upstream_config_path,
        UpstreamRuntimeOptions {
            verifier_mode: upstream_verifier_mode.clone(),
            accepted_workload_ids: parse_comma_list(upstream_accepted_workload_ids.as_deref()),
            accepted_image_digests: parse_comma_list(upstream_accepted_image_digests.as_deref()),
            accepted_dstack_kms_root_public_keys: parse_comma_list(
                upstream_dstack_kms_root_public_keys.as_deref(),
            ),
            pccs_url: upstream_pccs_url,
            verifier_cache_seconds: upstream_verifier_cache_seconds,
            connect_timeout_seconds: upstream_connect_timeout_seconds,
            read_timeout_seconds: upstream_read_timeout_seconds,
            verifier_request_timeout_seconds: upstream_verifier_request_timeout_seconds,
        },
    )?);
    spawn_upstream_lifecycle(upstream_config.clone());
    let upstream = upstream_config.backend();
    let receipt_store = Arc::new(InMemoryReceiptStore::default());
    let upstream_verifier: Arc<dyn UpstreamVerifier> = upstream_config.verifier();

    let config = AciServiceConfig {
        vendor: "private-ai-gateway-dev".to_string(),
        tee_type: "tdx".to_string(),
        source_provenance: SourceProvenance {
            repo_url,
            repo_commit,
            image_digest: None,
            image_provenance: None,
        },
        keyset_epoch: KeysetEpoch {
            version: 1,
            not_after: u64::MAX,
        },
        identity_subject: None,
        service_capabilities: ServiceCapabilities {
            supported_e2ee_versions: vec!["2".to_string()],
            body_retention_seconds,
        },
        freshness_seconds: 3600,
        receipt_ttl_seconds,
        upstream_required_default: true,
        allow_test_keys: false,
        tls_public_keys,
    };

    let service = Arc::new(AciService::new_with_upstream_verifier(
        keys,
        quoter,
        upstream,
        upstream_verifier,
        receipt_store,
        config,
        Arc::new(SystemClock),
    )?);

    let app = if let Some(middleware_url) = middleware_url {
        let request_store = GatewayRequestStore::default();
        let backend_app = build_internal_backend_router(service.clone(), request_store.clone());
        let backend_listener = tokio::net::TcpListener::bind(&backend_bind).await?;
        tracing::info!(
            %backend_bind,
            middleware_url = %middleware_url,
            "private-ai-gateway internal backend listening"
        );
        tokio::spawn(async move {
            if let Err(err) = axum::serve(backend_listener, backend_app).await {
                tracing::error!(error = %err, "private-ai-gateway internal backend stopped");
            }
        });
        build_router_with_admin_and_http_middleware(
            service,
            upstream_config,
            admin_token,
            request_store,
            middleware_url,
        )
    } else {
        build_router_with_admin(service, upstream_config, admin_token)
    };

    tracing::info!(%bind, "private-ai-gateway listening");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn spawn_upstream_lifecycle(upstream_config: Arc<UpstreamConfigManager>) {
    let prewarm_config = upstream_config.clone();
    tokio::spawn(async move {
        let results = prewarm_config.prewarm_upstream_verification().await;
        log_prewarm_results(results);
    });

    let verification_config = upstream_config.clone();
    tokio::spawn(async move {
        loop {
            let Some(seconds) = verification_config.verification_refresh_interval_seconds() else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            let results = verification_config.refresh_upstream_verification().await;
            log_prewarm_results(results);
        }
    });

    let session_config = upstream_config;
    tokio::spawn(async move {
        loop {
            let Some(seconds) = session_config.session_refresh_interval_seconds() else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            let results = session_config.refresh_provider_sessions().await;
            for result in results {
                match result.reason {
                    Some(reason) => tracing::warn!(
                        upstream = %result.upstream_name,
                        model = %result.model_id,
                        result = %result.result,
                        refreshed_nonces = result.refreshed_nonces,
                        reason = %reason,
                        "upstream provider session refresh finished"
                    ),
                    None => tracing::info!(
                        upstream = %result.upstream_name,
                        model = %result.model_id,
                        result = %result.result,
                        refreshed_nonces = result.refreshed_nonces,
                        "upstream provider session refresh finished"
                    ),
                }
            }
        }
    });
}

fn log_prewarm_results(
    results: Vec<private_ai_gateway::aggregator::upstream_config::UpstreamPrewarmResult>,
) {
    for result in results {
        match result.reason {
            Some(reason) => tracing::warn!(
                upstream = %result.upstream_name,
                model = %result.model_id,
                origin = ?result.url_origin,
                verifier = %result.verifier_id,
                result = %result.result,
                reason = %reason,
                "upstream verification prewarm finished"
            ),
            None => tracing::info!(
                upstream = %result.upstream_name,
                model = %result.model_id,
                origin = ?result.url_origin,
                verifier = %result.verifier_id,
                result = %result.result,
                "upstream verification prewarm finished"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use private_ai_gateway::aggregator::upstream_config::{parse_config_text, UpstreamProvider};

    use super::{
        parse_body_retention_seconds, parse_positive_seconds, parse_tls_cert_paths,
        parse_tls_spki_list, resolve_tls_public_keys, seed_upstream_config_if_empty,
        validate_http_url,
    };

    const TEST_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDEzCCAfugAwIBAgIURSrXHU8qZulH+2txkz9ZX8PE2rUwDQYJKoZIhvcNAQEL
BQAwGTEXMBUGA1UEAwwOdGlwLXRlc3QubG9jYWwwHhcNMjYwNTE0MDA1OTM5WhcN
MjYwNTE1MDA1OTM5WjAZMRcwFQYDVQQDDA50aXAtdGVzdC5sb2NhbDCCASIwDQYJ
KoZIhvcNAQEBBQADggEPADCCAQoCggEBAI3UiI+obpuYMBYkyASSEh1ZAqEu7IU8
qnmQ5qfHaKMIBzpjAfxvOheXS+GaD+BPNDYSTH0gpFP1yA3FDO102YVetpc7nWQz
NMc1KU3XdBRAnkyMsHxDKsrcKPxtq63kWEjHosFaqIy+TazYHu92ipj39Wl4a7x1
eXASjBTKqhDlV4cnyLzXhw6d1wu/haRK2F06xfb9E3YD/dT7nRE7pDXq8HHidLCm
AwhRVwvpva+IaG1SfbInNEr336fFdNnz3Ku+8iIKPLU5STNF9Uh4jKNOgFgiUCM1
05fqVg5BkY/sj1XKIGyOo8f91P/TxJxUwOzjyqQnVgtwkH/TiHA61SsCAwEAAaNT
MFEwHQYDVR0OBBYEFHRvjDiOr8T9EutZ2o0yl2Ld0NypMB8GA1UdIwQYMBaAFHRv
jDiOr8T9EutZ2o0yl2Ld0NypMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQEL
BQADggEBAFUxaxsNlvobJSV8CzPfYuwyM2w6gz5WArB8u1iZy3ScdzeQUu7JDVh/
cF7WlABDhuz++CEzjLszdAOP5mHJgYHEHHie+NqWrhgrT+rhskhoIK+mtb5ZKrgm
iizx/oNcBA9Zv9/STHzG8M4QpbGH5aRUwXiFUNHrckD9h89+s71sk6B18CxnEp2Y
H9j+YJx37yIZZeYPMXl/5K6NPIH1z3TfNL9AxaZASO2KMT7Y8y2bUp+HGW6MpqCP
5P+TqdVfn/HjL1eTdxIPH6HGK4cL0CO5D333Jhvv8zv1hmr6TRdoLbMiQVJ1jmDC
kBH1U3IsAJyU8UbZqzFEUGG7Ro3vdOQ=
-----END CERTIFICATE-----
"#;

    fn write_temp_cert() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "private-ai-gateway-test-cert-{}-{:?}.pem",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, TEST_CERT_PEM).unwrap();
        path
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "private-ai-gateway-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn parses_body_retention_seconds() {
        assert_eq!(parse_body_retention_seconds("0").unwrap(), 0);
        assert_eq!(parse_body_retention_seconds("86400").unwrap(), 86400);
        assert!(parse_body_retention_seconds("-1").is_err());
        assert!(parse_body_retention_seconds("1.5").is_err());
    }

    #[test]
    fn positive_timeout_seconds_rejects_zero() {
        assert_eq!(
            parse_positive_seconds("upstream connect timeout", "1").unwrap(),
            1
        );
        assert!(parse_positive_seconds("upstream connect timeout", "0").is_err());
    }

    #[test]
    fn parses_tls_spki_list_as_lowercase_hex_digests() {
        let first = "AA".repeat(32);
        let second = "bb".repeat(32);
        let parsed = parse_tls_spki_list(&format!("{first},{second}")).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].spki_sha256_hex, "aa".repeat(32));
        assert_eq!(parsed[1].spki_sha256_hex, "bb".repeat(32));
    }

    #[test]
    fn rejects_invalid_tls_spki_list() {
        assert!(parse_tls_spki_list("").is_err());
        assert!(parse_tls_spki_list("aa").is_err());
        assert!(parse_tls_spki_list(&format!("{},zz", "aa".repeat(32))).is_err());
    }

    #[test]
    fn parses_multi_upstream_model_routes() {
        let configs = parse_config_text(
            r#"[
                {
                    "name": "gpu-a",
                    "base_url": "https://gpu-a.example",
                    "models": {
                        "public-a": "upstream-a"
                    },
                    "accepted_workload_ids": ["aci:workload:a"],
                    "accepted_dstack_kms_root_public_keys": ["02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
                }
            ]"#,
        )
        .unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].provider, UpstreamProvider::OpenAiCompatible);
        assert_eq!(configs[0].models["public-a"], "upstream-a");
    }

    #[test]
    fn parses_provider_owned_upstream_adapter() {
        let configs = parse_config_text(
            r#"[
                {
                    "name": "chutes-a",
                    "provider": "chutes",
                    "base_url": "https://llm.chutes.example",
                    "models": {
                        "public-a": "upstream-a"
                    },
                    "bearer_token": "fixture-token",
                    "verification_refresh_seconds": 240,
                    "session_refresh_seconds": 45,
                    "chutes_e2ee_api_base": "https://api.chutes.example",
                    "chutes_chute_ids": {
                        "upstream-a": "2ff25e81-4586-5ec8-b892-3a6f342693d7"
                    },
                    "chutes_e2ee_discovery_rounds": 3,
                    "chutes_e2ee_discovery_interval_seconds": 1
                }
            ]"#,
        )
        .unwrap();
        assert_eq!(configs[0].provider, UpstreamProvider::Chutes);
        assert_eq!(configs[0].bearer_token.as_deref(), Some("fixture-token"));
        assert_eq!(configs[0].verification_refresh_seconds, Some(240));
        assert_eq!(configs[0].session_refresh_seconds, Some(45));
        assert_eq!(
            configs[0].chutes_e2ee_api_base.as_deref(),
            Some("https://api.chutes.example")
        );
        assert_eq!(
            configs[0]
                .chutes_chute_ids
                .as_ref()
                .unwrap()
                .get("upstream-a")
                .map(String::as_str),
            Some("2ff25e81-4586-5ec8-b892-3a6f342693d7")
        );
        assert_eq!(configs[0].chutes_e2ee_discovery_rounds, Some(3));
        assert_eq!(configs[0].chutes_e2ee_discovery_interval_seconds, Some(1));
    }

    #[test]
    fn rejects_chutes_chute_id_for_unconfigured_upstream_model() {
        let err = parse_config_text(
            r#"[
                {
                    "name": "chutes-a",
                    "provider": "chutes",
                    "base_url": "https://llm.chutes.example",
                    "models": {
                        "public-a": "upstream-a"
                    },
                    "chutes_chute_ids": {
                        "other-model": "2ff25e81-4586-5ec8-b892-3a6f342693d7"
                    }
                }
            ]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("is not one of its upstream model ids"));
    }

    #[test]
    fn rejects_chutes_discovery_rounds_outside_supported_range() {
        let err = parse_config_text(
            r#"[
                {
                    "name": "chutes-a",
                    "provider": "chutes",
                    "base_url": "https://llm.chutes.example",
                    "models": {
                        "public-a": "upstream-a"
                    },
                    "chutes_e2ee_discovery_rounds": 0
                }
            ]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("chutes_e2ee_discovery_rounds must be between 1 and 10"));
    }

    #[test]
    fn empty_upstream_config_is_allowed() {
        assert!(parse_config_text("").unwrap().is_empty());
        assert!(parse_config_text("[]").unwrap().is_empty());
    }

    #[test]
    fn seeds_upstream_config_when_target_missing() {
        let target = temp_path("target");
        let seed = temp_path("seed");
        std::fs::write(
            &seed,
            r#"[{"name":"gpu-a","base_url":"https://gpu-a.example","models":{"public-a":"upstream-a"}}]"#,
        )
        .unwrap();

        seed_upstream_config_if_empty(&target, Some(seed.to_str().unwrap())).unwrap();

        let seeded = std::fs::read_to_string(&target).unwrap();
        assert!(seeded.contains("\"public-a\""));
        let _ = std::fs::remove_file(target);
        let _ = std::fs::remove_file(seed);
    }

    #[test]
    fn seed_does_not_overwrite_existing_upstream_config() {
        let target = temp_path("target-existing");
        let seed = temp_path("seed-existing");
        std::fs::write(
            &target,
            r#"[{"name":"kept","base_url":"https://kept.example","models":{"kept":"kept"}}]"#,
        )
        .unwrap();
        std::fs::write(
            &seed,
            r#"[{"name":"seed","base_url":"https://seed.example","models":{"seed":"seed"}}]"#,
        )
        .unwrap();

        seed_upstream_config_if_empty(&target, Some(seed.to_str().unwrap())).unwrap();

        let kept = std::fs::read_to_string(&target).unwrap();
        assert!(kept.contains("\"kept\""));
        assert!(!kept.contains("\"seed\""));
        let _ = std::fs::remove_file(target);
        let _ = std::fs::remove_file(seed);
    }

    #[test]
    fn seed_rejects_invalid_upstream_config() {
        let target = temp_path("target-invalid-seed");
        let seed = temp_path("seed-invalid");
        std::fs::write(&seed, r#"[{"name":"","base_url":"","models":{}}]"#).unwrap();

        let err = seed_upstream_config_if_empty(&target, Some(seed.to_str().unwrap()))
            .expect_err("invalid seed must fail startup");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!target.exists());
        let _ = std::fs::remove_file(seed);
    }

    #[test]
    fn rejects_duplicate_upstream_names() {
        let err = parse_config_text(
            r#"[
                {"name":"gpu-a","base_url":"https://a.example","models":{"a":"a"}},
                {"name":"gpu-a","base_url":"https://b.example","models":{"b":"b"}}
            ]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("upstream name \"gpu-a\" is duplicated"));
    }

    #[test]
    fn parses_tls_cert_paths_as_spki_digests() {
        let path = write_temp_cert();
        let parsed = parse_tls_cert_paths(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].spki_sha256_hex,
            "c6686007081874ef8a5e8f95b7620e16c0ff0c65235ff8efcf9350cd9c5cf9dd"
        );
    }

    #[test]
    fn tls_cert_paths_and_explicit_spkis_are_mutually_exclusive() {
        let explicit = "aa".repeat(32);
        assert!(resolve_tls_public_keys(Some("/cert.pem"), Some(&explicit)).is_err());
    }

    #[test]
    fn middleware_url_must_be_http_or_https() {
        assert!(validate_http_url("middleware", "http://127.0.0.1:19090".to_string()).is_ok());
        assert!(validate_http_url("middleware", "https://middleware.local".to_string()).is_ok());
        assert!(validate_http_url("middleware", "127.0.0.1:19090".to_string()).is_err());
        assert!(validate_http_url("middleware", "unix:/tmp/backend.sock".to_string()).is_err());
    }
}
