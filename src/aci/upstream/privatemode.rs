//! Privatemode transport through an official proxy co-deployed with the gateway.
//!
//! The dstack Compose measurement binds the proxy image and its internal
//! network endpoint. The gateway independently verifies the shared credential
//! bytes at startup, records the measured proxy image in receipts, and sends
//! plaintext only to that statically configured service. The official proxy
//! owns dynamic manifest verification, credential use, secret exchange, and
//! Privatemode body encryption.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    OpenAICompatibleBackend, PreparedUpstreamRequest, UpstreamBackend, UpstreamError,
    UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};

const PROVIDER: &str = "privatemode";
const DEFAULT_ENCRYPTED_PATH: &str = "/v1/chat/completions";
const ENCRYPTED_PATHS: &[&str] = &[
    DEFAULT_ENCRYPTED_PATH,
    "/v1/completions",
    "/v1/embeddings",
    "/v1/messages",
];

#[derive(Debug, thiserror::Error)]
pub enum PrivatemodeDeploymentConfigError {
    #[error("Privatemode proxy base URL must be an HTTP(S) origin: {0}")]
    InvalidBaseUrl(String),
    #[error("Privatemode manifest log path must be absolute")]
    RelativeManifestLogPath,
    #[error("failed to read Privatemode manifest log {path}: {source}")]
    ReadManifestLog {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid Privatemode manifest log: {0}")]
    InvalidManifestLog(String),
    #[error("failed to read observed Privatemode manifest {path}: {source}")]
    ReadObservedManifest {
        path: String,
        source: std::io::Error,
    },
    #[error("Privatemode credential path must be absolute")]
    RelativeCredentialPath,
    #[error("failed to read Privatemode credential {path}: {source}")]
    ReadCredential {
        path: String,
        source: std::io::Error,
    },
    #[error("Privatemode credential must be non-empty UTF-8 without surrounding whitespace")]
    InvalidCredential,
    #[error("invalid Privatemode credential SHA-256 digest: {0}")]
    InvalidCredentialDigest(String),
    #[error("Privatemode credential digest {actual} does not match configured digest {expected}")]
    CredentialDigestMismatch { actual: String, expected: String },
    #[error("invalid Privatemode proxy OCI image digest: {0}")]
    InvalidImageDigest(String),
}

/// Static, measured deployment policy for one co-deployed Privatemode proxy.
#[derive(Debug)]
pub struct PrivatemodeProxyDeployment {
    base_url: String,
    manifest_log_path: PathBuf,
    credential_sha256: String,
    proxy_image_digest: String,
}

#[derive(Debug)]
pub(crate) struct ObservedPrivatemodeManifest {
    bytes: Vec<u8>,
    pub(crate) sha256: String,
    pub(crate) observed_at: String,
}

impl PrivatemodeProxyDeployment {
    pub fn new(
        base_url: impl Into<String>,
        manifest_log_path: impl AsRef<Path>,
        credential_path: impl AsRef<Path>,
        accepted_credential_sha256: impl AsRef<str>,
        proxy_image_digest: impl AsRef<str>,
    ) -> Result<Self, PrivatemodeDeploymentConfigError> {
        let base_url = normalize_origin(&base_url.into())?;

        let manifest_log_path = manifest_log_path.as_ref();
        if !manifest_log_path.is_absolute() {
            return Err(PrivatemodeDeploymentConfigError::RelativeManifestLogPath);
        }

        let credential_path = credential_path.as_ref();
        if !credential_path.is_absolute() {
            return Err(PrivatemodeDeploymentConfigError::RelativeCredentialPath);
        }
        let credential = std::fs::read(credential_path).map_err(|source| {
            PrivatemodeDeploymentConfigError::ReadCredential {
                path: credential_path.display().to_string(),
                source,
            }
        })?;
        let credential_text = std::str::from_utf8(&credential)
            .map_err(|_| PrivatemodeDeploymentConfigError::InvalidCredential)?;
        if credential_text.is_empty() || credential_text.trim() != credential_text {
            return Err(PrivatemodeDeploymentConfigError::InvalidCredential);
        }
        let credential_sha256 = normalize_sha256_hex(accepted_credential_sha256.as_ref())
            .map_err(PrivatemodeDeploymentConfigError::InvalidCredentialDigest)?;
        let actual_credential_sha256 = sha256_hex(&credential);
        if actual_credential_sha256 != credential_sha256 {
            return Err(PrivatemodeDeploymentConfigError::CredentialDigestMismatch {
                actual: actual_credential_sha256,
                expected: credential_sha256,
            });
        }
        let proxy_image_digest = format!(
            "sha256:{}",
            normalize_sha256_hex(proxy_image_digest.as_ref())
                .map_err(PrivatemodeDeploymentConfigError::InvalidImageDigest)?
        );

        Ok(Self {
            base_url,
            manifest_log_path: manifest_log_path.to_path_buf(),
            credential_sha256,
            proxy_image_digest,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn latest_observed_manifest(
        &self,
    ) -> Result<ObservedPrivatemodeManifest, PrivatemodeDeploymentConfigError> {
        let log = std::fs::read_to_string(&self.manifest_log_path).map_err(|source| {
            PrivatemodeDeploymentConfigError::ReadManifestLog {
                path: self.manifest_log_path.display().to_string(),
                source,
            }
        })?;
        let invalid_log = |message: &str| {
            PrivatemodeDeploymentConfigError::InvalidManifestLog(message.to_string())
        };
        let line = log
            .lines()
            .next_back()
            .ok_or_else(|| invalid_log("manifest log is empty"))?;
        let mut fields = line.split_ascii_whitespace();
        let (Some(observed_at), Some(logged_path), None) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid_log(
                "latest entry must contain a timestamp and manifest path",
            ));
        };
        let file_name = Path::new(logged_path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid_log("latest entry has an invalid manifest path"))?;
        let version = file_name
            .strip_suffix(".json")
            .ok_or_else(|| invalid_log("latest manifest filename must end in .json"))?;
        if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_log(
                "latest manifest filename must be a numeric version",
            ));
        }
        let manifest_path = self
            .manifest_log_path
            .parent()
            .expect("an absolute manifest log path has a parent")
            .join(file_name);
        let bytes = std::fs::read(&manifest_path).map_err(|source| {
            PrivatemodeDeploymentConfigError::ReadObservedManifest {
                path: manifest_path.display().to_string(),
                source,
            }
        })?;
        Ok(ObservedPrivatemodeManifest {
            sha256: sha256_hex(&bytes),
            observed_at: observed_at.to_string(),
            bytes,
        })
    }

    pub(crate) fn manifest_evidence(&self, manifest: &ObservedPrivatemodeManifest) -> Value {
        serde_json::json!({
            "type": "privatemode_manifest_observation",
            "observation": "latest_proxy_fetch_log",
            "bound_to_active_secret": false,
            "observed_at": manifest.observed_at,
            "digest": format!("sha256:{}", manifest.sha256),
            "data": format!(
                "data:application/json;base64,{}",
                BASE64.encode(&manifest.bytes)
            ),
        })
    }

    pub fn proxy_image_digest(&self) -> &str {
        &self.proxy_image_digest
    }

    pub fn credential_sha256(&self) -> &str {
        &self.credential_sha256
    }

    pub(crate) fn forwarding_client(
        &self,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<reqwest::Client, UpstreamError> {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .read_timeout(Duration::from_secs(read_timeout_seconds))
            .build()
            .map_err(|err| UpstreamError::Transport(err.to_string()))
    }

    pub(crate) fn readiness_client(
        &self,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
    ) -> Result<reqwest::Client, UpstreamError> {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .timeout(Duration::from_secs(request_timeout_seconds))
            .build()
            .map_err(|err| UpstreamError::Transport(err.to_string()))
    }
}

pub struct PrivatemodeProviderBackend {
    inner: OpenAICompatibleBackend,
    deployment: Arc<PrivatemodeProxyDeployment>,
}

impl PrivatemodeProviderBackend {
    pub fn new_with_timeouts(
        deployment: Arc<PrivatemodeProxyDeployment>,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        let client = deployment.forwarding_client(connect_timeout_seconds, read_timeout_seconds)?;
        let inner = OpenAICompatibleBackend::new_with_timeouts(
            deployment.base_url(),
            connect_timeout_seconds,
            read_timeout_seconds,
        )?
        .with_client(client);
        Ok(Self { inner, deployment })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.with_name(name);
        self
    }

    fn enforce_proxy_binding(&self, event: &UpstreamVerifiedEvent) -> Result<(), UpstreamError> {
        if event.result != VerificationResult::Verified {
            return Err(binding_mismatch(
                "Privatemode forwarding requires a verified event",
            ));
        }
        if event.provider_type.as_deref() != Some(PROVIDER) {
            return Err(binding_mismatch(format!(
                "verification provider {:?} is not {PROVIDER:?}",
                event.provider_type
            )));
        }
        if event.url_origin.as_deref() != self.url_origin() {
            return Err(binding_mismatch(format!(
                "verified proxy origin {:?} does not match co-deployed service {:?}",
                event.url_origin,
                self.url_origin()
            )));
        }
        let [ChannelBinding::ProxyImageSha256 {
            provider,
            proxy_image_digest,
            credential_sha256,
        }] = event.channel_bindings.as_slice()
        else {
            return Err(binding_mismatch(
                "Privatemode verification must produce exactly one proxy_image_sha256 binding",
            ));
        };
        if provider != PROVIDER
            || proxy_image_digest != self.deployment.proxy_image_digest()
            || credential_sha256 != self.deployment.credential_sha256()
        {
            return Err(binding_mismatch(
                "Privatemode event does not match the measured proxy deployment",
            ));
        }
        Ok(())
    }

    fn enforce_encrypted_path(&self, req: &PreparedUpstreamRequest) -> Result<(), UpstreamError> {
        let path = req
            .request
            .path
            .as_deref()
            .unwrap_or(DEFAULT_ENCRYPTED_PATH);
        if ENCRYPTED_PATHS.contains(&path) {
            return Ok(());
        }
        Err(UpstreamError::Routing(format!(
            "Privatemode refuses path {path:?}: the pinned proxy does not encrypt that handler"
        )))
    }
}

#[async_trait]
impl UpstreamBackend for PrivatemodeProviderBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn url_origin(&self) -> Option<&str> {
        self.inner.url_origin()
    }

    fn preserves_chat_surface_path(&self) -> bool {
        true
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        self.inner.prepare(req)
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(verification_required())
    }

    async fn forward_prepared(
        &self,
        _req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        Err(verification_required())
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.enforce_encrypted_path(&req)?;
        self.enforce_proxy_binding(event)?;
        self.inner.forward_prepared(req).await
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(verification_required())
    }

    async fn forward_stream_prepared(
        &self,
        _req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(verification_required())
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.enforce_encrypted_path(&req)?;
        self.enforce_proxy_binding(event)?;
        self.inner.forward_stream_prepared(req).await
    }
}

fn normalize_origin(value: &str) -> Result<String, PrivatemodeDeploymentConfigError> {
    let mut url = reqwest::Url::parse(value.trim())
        .map_err(|err| PrivatemodeDeploymentConfigError::InvalidBaseUrl(err.to_string()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(PrivatemodeDeploymentConfigError::InvalidBaseUrl(
            "expected scheme, host, and optional port only".to_string(),
        ));
    }
    url.set_path("");
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn normalize_sha256_hex(value: &str) -> Result<String, String> {
    let value = value.trim().strip_prefix("sha256:").unwrap_or(value.trim());
    let bytes = hex::decode(value).map_err(|err| err.to_string())?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    Ok(hex::encode(bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn binding_mismatch(message: impl Into<String>) -> UpstreamError {
    UpstreamError::ChannelBindingMismatch(message.into())
}

fn verification_required() -> UpstreamError {
    UpstreamError::ChannelBindingMismatch(
        "Privatemode forwarding requires an active co-deployed proxy binding".to_string(),
    )
}
