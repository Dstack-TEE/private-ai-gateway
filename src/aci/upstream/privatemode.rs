//! Privatemode transport through an official proxy co-deployed with the gateway.
//!
//! The dstack Compose measurement binds the proxy image and its internal
//! network endpoint. The gateway independently verifies the exact Contrast
//! manifest and shared credential-secret bytes at startup, records their
//! digests in receipts, and sends plaintext only to that statically configured
//! service. The official proxy owns use of the credential, remote attestation,
//! secret exchange, and Privatemode body encryption.

use std::path::Path;
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
    #[error("Privatemode manifest path must be absolute")]
    RelativeManifestPath,
    #[error("failed to read Privatemode manifest {path}: {source}")]
    ReadManifest {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid Privatemode manifest SHA-256 digest: {0}")]
    InvalidManifestDigest(String),
    #[error("Privatemode manifest digest {actual} does not match configured digest {expected}")]
    ManifestDigestMismatch { actual: String, expected: String },
    #[error("invalid Privatemode manifest: {0}")]
    InvalidManifest(String),
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
    manifest: Vec<u8>,
    manifest_sha256: String,
    coordinator_policy_hash: String,
    credential_sha256: String,
    proxy_image_digest: String,
}

impl PrivatemodeProxyDeployment {
    pub fn new(
        base_url: impl Into<String>,
        manifest_path: impl AsRef<Path>,
        accepted_manifest_sha256: impl AsRef<str>,
        credential_path: impl AsRef<Path>,
        accepted_credential_sha256: impl AsRef<str>,
        proxy_image_digest: impl AsRef<str>,
    ) -> Result<Self, PrivatemodeDeploymentConfigError> {
        let base_url = normalize_origin(&base_url.into())?;

        let manifest_path = manifest_path.as_ref();
        if !manifest_path.is_absolute() {
            return Err(PrivatemodeDeploymentConfigError::RelativeManifestPath);
        }
        let manifest = std::fs::read(manifest_path).map_err(|source| {
            PrivatemodeDeploymentConfigError::ReadManifest {
                path: manifest_path.display().to_string(),
                source,
            }
        })?;
        let accepted_manifest_sha256 = normalize_sha256_hex(accepted_manifest_sha256.as_ref())
            .map_err(PrivatemodeDeploymentConfigError::InvalidManifestDigest)?;
        let manifest_sha256 = sha256_hex(&manifest);
        if manifest_sha256 != accepted_manifest_sha256 {
            return Err(PrivatemodeDeploymentConfigError::ManifestDigestMismatch {
                actual: manifest_sha256,
                expected: accepted_manifest_sha256,
            });
        }
        let coordinator_policy_hash = coordinator_policy_hash(&manifest)
            .map_err(PrivatemodeDeploymentConfigError::InvalidManifest)?;

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
            manifest,
            manifest_sha256: accepted_manifest_sha256,
            coordinator_policy_hash,
            credential_sha256,
            proxy_image_digest,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    pub fn manifest_evidence(&self) -> Value {
        serde_json::json!({
            "digest": format!("sha256:{}", self.manifest_sha256),
            "data": format!(
                "data:application/json;base64,{}",
                BASE64.encode(&self.manifest)
            ),
        })
    }

    pub fn coordinator_policy_hash(&self) -> &str {
        &self.coordinator_policy_hash
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

#[derive(Debug, thiserror::Error)]
pub enum PrivatemodeBackendConfigError {
    #[error("invalid Privatemode backend: {0}")]
    Backend(String),
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
    ) -> Result<Self, PrivatemodeBackendConfigError> {
        let client = deployment
            .forwarding_client(connect_timeout_seconds, read_timeout_seconds)
            .map_err(|err| PrivatemodeBackendConfigError::Backend(err.to_string()))?;
        let inner = OpenAICompatibleBackend::new_with_timeouts(
            deployment.base_url(),
            connect_timeout_seconds,
            read_timeout_seconds,
        )
        .map_err(|err| PrivatemodeBackendConfigError::Backend(err.to_string()))?
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
        let [ChannelBinding::ManifestImageSha256 {
            provider,
            manifest_sha256,
            coordinator_policy_hash,
            proxy_image_digest,
            credential_sha256,
        }] = event.channel_bindings.as_slice()
        else {
            return Err(binding_mismatch(
                "Privatemode verification must produce exactly one manifest_image_sha256 binding",
            ));
        };
        if provider != PROVIDER
            || manifest_sha256 != self.deployment.manifest_sha256()
            || coordinator_policy_hash != self.deployment.coordinator_policy_hash()
            || proxy_image_digest != self.deployment.proxy_image_digest()
            || credential_sha256.as_deref() != Some(self.deployment.credential_sha256())
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

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.inner.models().await
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

fn coordinator_policy_hash(manifest: &[u8]) -> Result<String, String> {
    let manifest: Value = serde_json::from_slice(manifest)
        .map_err(|err| format!("invalid Privatemode manifest JSON: {err}"))?;
    let policies = manifest
        .get("Policies")
        .and_then(Value::as_object)
        .ok_or_else(|| "Privatemode manifest is missing Policies".to_string())?;
    let coordinator_policies = policies
        .iter()
        .filter(|(_, policy)| policy.get("Role").and_then(Value::as_str) == Some("coordinator"))
        .map(|(hash, _)| normalize_sha256_hex(hash))
        .collect::<Result<Vec<_>, _>>()?;
    match coordinator_policies.as_slice() {
        [hash] => Ok(hash.clone()),
        policies => Err(format!(
            "Privatemode manifest must contain exactly one Coordinator policy, found {}",
            policies.len()
        )),
    }
}

fn binding_mismatch(message: impl Into<String>) -> UpstreamError {
    UpstreamError::ChannelBindingMismatch(message.into())
}

fn verification_required() -> UpstreamError {
    UpstreamError::ChannelBindingMismatch(
        "Privatemode forwarding requires an active co-deployed proxy binding".to_string(),
    )
}
