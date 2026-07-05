//! Gateway entrypoint.
//!
//! The runtime key provider and quoter are backed by dstack KMS and
//! the dstack TDX quote API through the Rust dstack SDK. There is no
//! ephemeral-key or stub-quote startup path.
//!
//! Configuration. `PRIVATE_AI_GATEWAY_CONFIG_PATH` points at the static gateway
//! JSON config. Gateway policy belongs in that file, not in environment
//! fallbacks. See `docs/configuration-reference.md` for the full config and env
//! reference.
//!
//! | Setting | Environment variable |
//! | --- | --- |
//! | Gateway config path | `PRIVATE_AI_GATEWAY_CONFIG_PATH` |
//!
//! Gateway-owned writable files are derived from `state_dir` in the static
//! config: the active upstream database `upstreams.json`, the attested-session
//! log `sessions.jsonl`, the managed keyset-epoch state `keyset-epoch.json`
//! (§4.2), and the issued keyset revocations `revocations.json` (§4.7).
//!
//! When the static config includes a `middleware` section, the gateway runs the
//! middleware: it consults the control plane at `middleware.control_url`
//! and calls the service directly. Without the section the gateway serves the
//! upstream directly.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use private_ai_gateway::aci::keys::{KeyProvider, Quoter};
use private_ai_gateway::aci::types::{
    KeysetEpoch, ServiceCapabilities, SourceProvenance, TlsSpki, WorkloadIdentity, WorkloadKeyset,
};
use private_ai_gateway::aci::upstream::{
    DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS, DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS,
};
use private_ai_gateway::aci::verifier::DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS;
use private_ai_gateway::aggregator::keyset_epoch::{self, DEFAULT_KEYSET_EPOCH_WINDOW_SECONDS};
use private_ai_gateway::aggregator::revocation_store::RevocationStore;
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, Clock, InMemoryReceiptStore, SystemClock, UpstreamVerifier,
};
use private_ai_gateway::aggregator::session_store::JsonlSessionStore;
use private_ai_gateway::aggregator::upstream_config::{
    parse_config_text, UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
};
use private_ai_gateway::dstack::{DstackAciProvider, DstackAciProviderConfig};
use private_ai_gateway::http::{build_router_with_admin, build_router_with_admin_and_middleware};
use private_ai_gateway::middleware::{Middleware, MiddlewareConfig};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use x509_parser::prelude::parse_x509_certificate;

const GIT_LAUNCHER_CONFIG_PATH: &str = "/etc/git-launcher/gateway.conf";

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn invalid_input(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GatewayConfigFile {
    bind: Option<String>,
    state_dir: Option<String>,
    upstream_config_seed_path: Option<String>,
    admin_token: Option<String>,
    /// Bounded keyset-epoch validity window in seconds (§4.7). Defaults to
    /// [`DEFAULT_KEYSET_EPOCH_WINDOW_SECONDS`] (~4 weeks).
    keyset_epoch_window_seconds: Option<u64>,
    tls: GatewayTlsConfig,
    dstack_endpoint: Option<String>,
    middleware: Option<MiddlewareConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GatewayTlsConfig {
    domain_certificates: Vec<TlsDomainCertificateEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsDomainCertificateEntry {
    domain: String,
    certificate_path: String,
}

fn load_gateway_config(path: &str) -> Result<GatewayConfigFile, String> {
    let path = Path::new(path);
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read gateway config {}: {e}", path.display()))?;
    let config: GatewayConfigFile = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse gateway config {}: {e}", path.display()))?;
    Ok(config)
}

fn resolve_state_dir(config_state_dir: Option<&str>) -> Result<PathBuf, String> {
    let state_dir = config_state_dir
        .unwrap_or("/var/lib/private-ai-gateway")
        .trim();
    if state_dir.is_empty() {
        return Err("gateway state_dir must not be empty".to_string());
    }
    Ok(PathBuf::from(state_dir))
}

fn upstream_config_path(state_dir: &Path) -> PathBuf {
    state_dir.join("upstreams.json")
}

fn session_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sessions.jsonl")
}

fn keyset_epoch_path(state_dir: &Path) -> PathBuf {
    state_dir.join("keyset-epoch.json")
}

fn revocations_path(state_dir: &Path) -> PathBuf {
    state_dir.join("revocations.json")
}

fn resolve_source_provenance() -> Result<SourceProvenance, String> {
    Ok(
        source_provenance_from_git_launcher_config(Path::new(GIT_LAUNCHER_CONFIG_PATH))?
            .unwrap_or_default(),
    )
}

fn source_provenance_from_git_launcher_config(
    path: &Path,
) -> Result<Option<SourceProvenance>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!(
                "failed to read git-launcher config {}: {err}",
                path.display()
            ));
        }
    };
    let mut repo_url = None;
    let mut repo_commit = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "REPO_URL" if !value.is_empty() => repo_url = Some(value.to_string()),
            "COMMIT_SHA" if !value.is_empty() => repo_commit = Some(value.to_string()),
            _ => {}
        }
    }
    let repo_url = repo_url
        .ok_or_else(|| format!("git-launcher config {} is missing REPO_URL", path.display()))?;
    let repo_commit = repo_commit.ok_or_else(|| {
        format!(
            "git-launcher config {} is missing COMMIT_SHA",
            path.display()
        )
    })?;
    validate_git_launcher_commit_sha(&repo_commit).map_err(|err| {
        format!(
            "git-launcher config {} has invalid COMMIT_SHA: {err}",
            path.display()
        )
    })?;
    Ok(Some(SourceProvenance {
        repo_url: Some(repo_url),
        repo_commit: Some(repo_commit),
        image_digest: None,
        image_provenance: None,
    }))
}

fn validate_git_launcher_commit_sha(value: &str) -> Result<(), String> {
    if (value.len() == 40 || value.len() == 64) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err("expected a full 40- or 64-character hexadecimal commit hash".to_string())
}

fn parse_domain_cert_entries(
    entries: &[TlsDomainCertificateEntry],
) -> Result<Vec<TlsSpki>, String> {
    let mut keys = Vec::new();
    let mut domains = std::collections::BTreeSet::new();
    for entry in entries {
        let domain = normalize_config_domain(&entry.domain)?;
        if !domains.insert(domain.clone()) {
            return Err(format!("TLS domain {domain:?} is duplicated"));
        }
        let certificate_path = entry.certificate_path.trim();
        if certificate_path.is_empty() {
            return Err(format!(
                "TLS domain certificate entry for {domain:?} has an empty certificate_path"
            ));
        }
        let mut key = tls_spki_from_cert_path(Path::new(certificate_path))?;
        key.domain = Some(domain);
        keys.push(key);
    }
    if keys.is_empty() {
        return Err("TLS domain config must contain at least one domain".to_string());
    }
    Ok(keys)
}

fn normalize_config_domain(raw: &str) -> Result<String, String> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.contains('/')
        || domain.contains(':')
        || domain.contains('=')
        || domain.contains(',')
        || domain.chars().any(char::is_whitespace)
    {
        return Err(format!("invalid TLS domain {raw:?}"));
    }
    Ok(domain)
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
        domain: None,
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

fn resolve_tls_public_keys(config: &GatewayTlsConfig) -> Result<Option<Vec<TlsSpki>>, String> {
    if !config.domain_certificates.is_empty() {
        return parse_domain_cert_entries(&config.domain_certificates).map(Some);
    }
    Ok(None)
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

    let gateway_config_path = env_non_empty("PRIVATE_AI_GATEWAY_CONFIG_PATH").ok_or_else(|| {
        invalid_input("PRIVATE_AI_GATEWAY_CONFIG_PATH must point to the static gateway config file")
    })?;
    let gateway_config = load_gateway_config(&gateway_config_path)?;

    let bind = gateway_config
        .bind
        .clone()
        .unwrap_or_else(|| "127.0.0.1:8086".to_string());
    let state_dir = resolve_state_dir(gateway_config.state_dir.as_deref())?;
    std::fs::create_dir_all(&state_dir).map_err(|err| {
        invalid_input(format!(
            "failed to create gateway state directory {}: {err}",
            state_dir.display()
        ))
    })?;
    let upstream_config_path = upstream_config_path(&state_dir);
    let session_log_path = session_log_path(&state_dir);
    let upstream_config_seed_path = gateway_config.upstream_config_seed_path.clone();
    let admin_token = gateway_config.admin_token.clone();
    let source_provenance = resolve_source_provenance()?;
    let tls_public_keys = resolve_tls_public_keys(&gateway_config.tls)?;
    let dstack_endpoint = gateway_config.dstack_endpoint.clone();
    let middleware_config = gateway_config.middleware.clone();

    let provider = Arc::new(
        DstackAciProvider::new(dstack_endpoint, DstackAciProviderConfig::default()).await?,
    );
    let keys: Arc<dyn KeyProvider> = provider.clone();
    let quoter: Arc<dyn Quoter> = provider;
    seed_upstream_config_if_empty(&upstream_config_path, upstream_config_seed_path.as_deref())?;
    let upstream_config = Arc::new(UpstreamConfigManager::load(
        upstream_config_path.clone(),
        UpstreamRuntimeOptions {
            verifier_mode: UpstreamVerifierMode::None,
            accepted_workload_ids: Vec::new(),
            accepted_image_digests: Vec::new(),
            accepted_dstack_kms_root_public_keys: Vec::new(),
            pccs_url: None,
            verifier_cache_seconds: 300,
            connect_timeout_seconds: DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECONDS,
            read_timeout_seconds: DEFAULT_UPSTREAM_READ_TIMEOUT_SECONDS,
            verifier_request_timeout_seconds: DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS,
        },
    )?);
    let upstream = upstream_config.backend();
    let receipt_store = Arc::new(InMemoryReceiptStore::default());
    let upstream_verifier: Arc<dyn UpstreamVerifier> = upstream_config.verifier();

    // Issued keyset revocations persist in the state dir: a service that
    // repudiated a keyset must not silently resume serving it after a restart.
    let revocation_store = Arc::new(
        RevocationStore::open(revocations_path(&state_dir)).map_err(|err| {
            invalid_input(format!("failed to open gateway revocation store: {err}"))
        })?,
    );

    // Resolve the managed keyset epoch before assembling the keyset. The service
    // builds the same keyset from these parts plus `config.keyset_epoch`, so the
    // epoch and digest agree.
    let keyset_epoch_window_seconds = gateway_config
        .keyset_epoch_window_seconds
        .unwrap_or(DEFAULT_KEYSET_EPOCH_WINDOW_SECONDS);
    if keyset_epoch_window_seconds == 0 {
        return Err(invalid_input("keyset_epoch_window_seconds must be greater than zero").into());
    }
    let identity_subject: Option<String> = None;
    let keyset_identity = WorkloadIdentity {
        public_key: keys.identity_public_key(),
        subject: identity_subject.clone(),
    };
    let keyset_receipt_keys = keys.receipt_keys();
    let keyset_e2ee_keys = keys.e2ee_keys();
    let keyset_tls_public_keys = tls_public_keys.clone().unwrap_or_else(|| keys.tls_spkis());
    let make_keyset = |epoch: KeysetEpoch| WorkloadKeyset {
        workload_identity: keyset_identity.clone(),
        keyset_epoch: epoch,
        receipt_signing_keys: keyset_receipt_keys.clone(),
        e2ee_public_keys: keyset_e2ee_keys.clone(),
        tls_public_keys: keyset_tls_public_keys.clone(),
    };
    let keyset_epoch = keyset_epoch::resolve_launcher_epoch(
        &keyset_epoch_path(&state_dir),
        make_keyset,
        &revocation_store,
        SystemClock.now_secs(),
        keyset_epoch_window_seconds,
    )
    .map_err(|err| invalid_input(format!("failed to resolve managed keyset epoch: {err}")))?;
    tracing::info!(
        version = keyset_epoch.version,
        not_after = keyset_epoch.not_after,
        "resolved managed keyset epoch"
    );

    let config = AciServiceConfig {
        vendor: "private-ai-gateway-dev".to_string(),
        tee_type: "tdx".to_string(),
        source_provenance,
        keyset_epoch,
        identity_subject,
        service_capabilities: ServiceCapabilities {
            supported_e2ee_versions: vec!["2".to_string()],
        },
        freshness_seconds: 3600,
        receipt_ttl_seconds: 3600,
        upstream_required_default: true,
        allow_test_keys: false,
        tls_public_keys,
    };

    let service_inner = AciService::new_with_upstream_verifier(
        keys,
        quoter,
        upstream,
        upstream_verifier,
        receipt_store,
        config,
        Arc::new(SystemClock),
    )?;
    let session_store = Arc::new(JsonlSessionStore::open(&session_log_path).map_err(|err| {
        invalid_input(format!(
            "failed to open gateway session log {}: {err}",
            session_log_path.display()
        ))
    })?);
    tracing::info!(session_log = %session_log_path.display(), "persisting attested sessions to JSONL log");
    // Reclaim the duplicate/expired lines accumulated while the process was down
    // before serving, then keep the file bounded on a periodic cadence.
    let kept = compact_session_log(session_store.clone())
        .await
        .map_err(|err| {
            invalid_input(format!(
                "failed to compact gateway session log {} on startup: {err}",
                session_log_path.display()
            ))
        })?;
    tracing::info!(
        session_log = %session_log_path.display(),
        kept,
        "compacted attested-session log on startup"
    );
    spawn_session_log_compaction(session_store.clone(), session_log_path.clone());
    let service_inner = service_inner
        .with_session_store(session_store)
        .with_revocation_store(revocation_store);
    let service = Arc::new(service_inner);
    // The background upstream verification keeps the attested-session store fresh
    // on the same cadence it re-attests for serving, so `/v1/aci/sessions`
    // (preflight) is populated before any traffic. (The completion path also
    // writes the session it served; writes are idempotent + content-addressed.)
    // Attach the sink before spawning the lifecycle so the boot prewarm populates
    // the store.
    upstream_config.set_session_sink(service.clone());
    spawn_upstream_lifecycle(upstream_config.clone());

    let app = if let Some(middleware_config) = middleware_config {
        let middleware = Arc::new(Middleware::new(&middleware_config).map_err(invalid_input)?);
        tracing::info!(
            control_url = %middleware_config.control_url,
            "private-ai-gateway middleware enabled"
        );
        build_router_with_admin_and_middleware(service, upstream_config, admin_token, middleware)
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

/// How often the background task rewrites the attested-session log from its live
/// index. The live set is tiny (one entry per channel), so an hourly rewrite
/// keeps the file bounded without churning it.
const SESSION_LOG_COMPACTION_INTERVAL_SECONDS: u64 = 3600;

fn spawn_session_log_compaction(store: Arc<JsonlSessionStore>, session_log_path: PathBuf) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(SESSION_LOG_COMPACTION_INTERVAL_SECONDS)).await;
            match compact_session_log(store.clone()).await {
                Ok(kept) => tracing::info!(
                    session_log = %session_log_path.display(),
                    kept,
                    "compacted attested-session log"
                ),
                Err(err) => tracing::warn!(
                    session_log = %session_log_path.display(),
                    error = %err,
                    "attested-session log compaction failed"
                ),
            }
        }
    });
}

async fn compact_session_log(store: Arc<JsonlSessionStore>) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || store.compact(SystemClock.now_secs()))
        .await
        .map_err(|err| format!("compaction task failed: {err}"))?
        .map_err(|err| err.to_string())
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
        keyset_epoch_path, load_gateway_config, resolve_state_dir, resolve_tls_public_keys,
        revocations_path, seed_upstream_config_if_empty, session_log_path,
        source_provenance_from_git_launcher_config, upstream_config_path,
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
    fn gateway_config_parses_state_dir() {
        let config_path = temp_path("gateway-config-state-dir");
        std::fs::write(
            &config_path,
            r#"{
                "state_dir": "/gateway/state"
            }"#,
        )
        .unwrap();

        let config = load_gateway_config(config_path.to_str().unwrap()).unwrap();

        assert_eq!(config.state_dir.as_deref(), Some("/gateway/state"));
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn gateway_config_parses_middleware_section() {
        let config_path = temp_path("gateway-config-middleware");
        std::fs::write(
            &config_path,
            r#"{
                "middleware": {
                    "control_url": "https://control.example",
                    "control_token": "secret"
                }
            }"#,
        )
        .unwrap();

        let config = load_gateway_config(config_path.to_str().unwrap()).unwrap();

        let middleware = config.middleware.expect("middleware section must parse");
        assert_eq!(middleware.control_url, "https://control.example");
        assert_eq!(middleware.control_token.as_deref(), Some("secret"));
        // Optional timeouts default to None and fall back inside the client.
        assert_eq!(middleware.control_timeout_ms, None);
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn gateway_config_rejects_removed_fields() {
        for (name, body) in [
            ("receipt-ttl", r#"{"receipt_ttl_seconds": 3600}"#),
            (
                "upstream-verifier",
                r#"{"upstream_verifier": {"mode": "none"}}"#,
            ),
            (
                "source-provenance",
                r#"{"source_provenance": {"repo_url": "https://example.com/repo", "repo_commit": "deadbeef"}}"#,
            ),
            (
                "tls-certificate-paths",
                r#"{"tls": {"certificate_paths": ["/cert.pem"]}}"#,
            ),
        ] {
            let config_path = temp_path(name);
            std::fs::write(&config_path, body).unwrap();

            let err = load_gateway_config(config_path.to_str().unwrap())
                .expect_err("removed gateway config field must be rejected");

            assert!(
                err.contains("unknown field"),
                "unexpected parse error for {name}: {err}"
            );
            let _ = std::fs::remove_file(config_path);
        }
    }

    #[test]
    fn source_provenance_parses_git_launcher_pin() {
        let config_path = temp_path("git-launcher-config");
        std::fs::write(
            &config_path,
            r#"
                # fetched by git-launcher before entrypoint.sh runs
                REPO_URL=https://github.com/Dstack-TEE/private-ai-gateway.git
                COMMIT_SHA=0123456789abcdef0123456789abcdef01234567
                WORK_DIR=/var/lib/git-launcher/private-ai-gateway
            "#,
        )
        .unwrap();

        let provenance = source_provenance_from_git_launcher_config(&config_path)
            .unwrap()
            .unwrap();

        assert_eq!(
            provenance.repo_url.as_deref(),
            Some("https://github.com/Dstack-TEE/private-ai-gateway.git")
        );
        assert_eq!(
            provenance.repo_commit.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(provenance.image_digest.is_none());
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn source_provenance_rejects_incomplete_git_launcher_pin() {
        let config_path = temp_path("git-launcher-config-missing-commit");
        std::fs::write(
            &config_path,
            "REPO_URL=https://github.com/Dstack-TEE/private-ai-gateway.git\n",
        )
        .unwrap();

        let err = source_provenance_from_git_launcher_config(&config_path)
            .expect_err("incomplete git-launcher config must fail startup");

        assert!(err.contains("COMMIT_SHA"));
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn source_provenance_rejects_non_full_git_launcher_commit_pin() {
        let config_path = temp_path("git-launcher-config-short-commit");
        std::fs::write(
            &config_path,
            r#"
                REPO_URL=https://github.com/Dstack-TEE/private-ai-gateway.git
                COMMIT_SHA=main
            "#,
        )
        .unwrap();

        let err = source_provenance_from_git_launcher_config(&config_path)
            .expect_err("git-launcher config must pin a full commit hash");

        assert!(err.contains("full 40- or 64-character hexadecimal commit hash"));
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn source_provenance_is_unknown_when_git_launcher_pin_is_absent() {
        let config_path = temp_path("git-launcher-config-missing");
        let _ = std::fs::remove_file(&config_path);

        let provenance = source_provenance_from_git_launcher_config(&config_path).unwrap();

        assert!(provenance.is_none());
    }

    #[test]
    fn gateway_state_paths_are_derived_from_state_dir() {
        let state_dir = std::path::PathBuf::from("/var/lib/private-ai-gateway");

        assert_eq!(
            resolve_state_dir(Some("/var/lib/private-ai-gateway")).unwrap(),
            state_dir
        );
        assert_eq!(
            upstream_config_path(&state_dir),
            state_dir.join("upstreams.json")
        );
        assert_eq!(
            session_log_path(&state_dir),
            state_dir.join("sessions.jsonl")
        );
        assert_eq!(
            keyset_epoch_path(&state_dir),
            state_dir.join("keyset-epoch.json")
        );
        assert_eq!(
            revocations_path(&state_dir),
            state_dir.join("revocations.json")
        );
        assert!(resolve_state_dir(Some("  ")).is_err());
    }

    #[test]
    fn gateway_config_parses_keyset_epoch_window() {
        let config_path = temp_path("gateway-config-keyset-window");
        std::fs::write(
            &config_path,
            r#"{ "keyset_epoch_window_seconds": 1209600 }"#,
        )
        .unwrap();

        let config = load_gateway_config(config_path.to_str().unwrap()).unwrap();

        assert_eq!(config.keyset_epoch_window_seconds, Some(1_209_600));
        let _ = std::fs::remove_file(config_path);
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
    fn gateway_config_domain_certificates_parse_as_spki_digests() {
        let api_cert = write_temp_cert();
        let chat_cert = write_temp_cert();
        let config_path = temp_path("gateway-config-domain-certificates");
        std::fs::write(
            &config_path,
            format!(
                r#"{{
                    "tls": {{
                        "domain_certificates": [
                            {{"domain":"Api.Example.COM", "certificate_path":"{}"}},
                            {{"domain":"chat.example.com.", "certificate_path":"{}"}}
                        ]
                    }}
                }}"#,
                api_cert.display(),
                chat_cert.display()
            ),
        )
        .unwrap();

        let config = load_gateway_config(config_path.to_str().unwrap()).unwrap();
        let parsed = resolve_tls_public_keys(&config.tls).unwrap().unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].domain.as_deref(), Some("api.example.com"));
        assert_eq!(parsed[1].domain.as_deref(), Some("chat.example.com"));
        assert_eq!(
            parsed[0].spki_sha256_hex,
            "c6686007081874ef8a5e8f95b7620e16c0ff0c65235ff8efcf9350cd9c5cf9dd"
        );
        let _ = std::fs::remove_file(api_cert);
        let _ = std::fs::remove_file(chat_cert);
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn rejects_duplicate_domain_tls_config_entries() {
        let cert = write_temp_cert();
        let config_path = temp_path("gateway-config-domain-certificates-duplicate");
        std::fs::write(
            &config_path,
            format!(
                r#"{{
                    "tls": {{
                        "domain_certificates": [
                            {{"domain":"api.example.com", "certificate_path":"{}"}},
                            {{"domain":"API.example.com", "certificate_path":"{}"}}
                        ]
                    }}
                }}"#,
                cert.display(),
                cert.display()
            ),
        )
        .unwrap();

        let config = load_gateway_config(config_path.to_str().unwrap()).unwrap();
        let err = resolve_tls_public_keys(&config.tls).unwrap_err();

        assert!(err.contains("duplicated"));
        let _ = std::fs::remove_file(cert);
        let _ = std::fs::remove_file(config_path);
    }
}
