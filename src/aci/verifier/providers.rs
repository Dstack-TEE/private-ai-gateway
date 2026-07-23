//! Per-provider upstream verifiers that wrap the external bridge, plus the
//! origin/name routing verifier that dispatches to them.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures_util::StreamExt;

use super::external::ExternalProviderVerifier;
#[cfg(test)]
use super::external::ProviderVerifierConfigError;
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{ChutesSessionStore, PrivatemodeProxyDeployment, UpstreamError};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};
use crate::aggregator::upstream_config::UpstreamProvider;

const MAX_PRIVATEMODE_MODELS_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ChutesProviderVerifier {
    verifier: ExternalProviderVerifier,
}

impl ChutesProviderVerifier {
    pub fn new(timeout_seconds: u64) -> Self {
        Self::new_with_cache(timeout_seconds, 0)
    }

    pub fn new_with_cache(timeout_seconds: u64, cache_ttl_seconds: u64) -> Self {
        Self {
            verifier: ExternalProviderVerifier::private_inference(
                "chutes",
                UpstreamProvider::Chutes.attestation_scope(),
                timeout_seconds,
                cache_ttl_seconds,
            ),
        }
    }

    pub fn new_with_cache_and_session_store(
        timeout_seconds: u64,
        cache_ttl_seconds: u64,
        session_store: Arc<ChutesSessionStore>,
    ) -> Self {
        Self {
            verifier: ExternalProviderVerifier::private_inference(
                "chutes",
                UpstreamProvider::Chutes.attestation_scope(),
                timeout_seconds,
                cache_ttl_seconds,
            )
            .with_chutes_session_store(session_store),
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.verifier = self.verifier.with_option("chutes_api_key", api_key);
        self
    }

    pub fn with_basic_auth(mut self, enabled: bool) -> Self {
        self.verifier = self
            .verifier
            .with_option("chutes_basic_auth", enabled.to_string());
        self
    }

    pub fn with_e2ee_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.verifier = self.verifier.with_option("chutes_e2ee_api_base", api_base);
        self
    }

    pub fn with_chute_ids(mut self, chute_ids: BTreeMap<String, String>) -> Self {
        for (model_id, chute_id) in chute_ids {
            self.verifier = self
                .verifier
                .with_option(format!("chutes_chute_id:{model_id}"), chute_id);
        }
        self
    }

    pub fn with_discovery_rounds(mut self, rounds: u64) -> Self {
        self.verifier = self
            .verifier
            .with_option("chutes_e2ee_discovery_rounds", rounds.to_string());
        self
    }

    pub fn with_discovery_interval_seconds(mut self, seconds: u64) -> Self {
        self.verifier = self.verifier.with_option(
            "chutes_e2ee_discovery_interval_seconds",
            seconds.to_string(),
        );
        self
    }

    #[cfg(test)]
    pub(super) fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command(
                "chutes",
                UpstreamProvider::Chutes.attestation_scope(),
                command,
                timeout_seconds,
            )?,
        })
    }

    #[cfg(test)]
    pub(super) fn with_command_and_session_store(
        command: Vec<String>,
        timeout_seconds: u64,
        session_store: Arc<ChutesSessionStore>,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command(
                "chutes",
                UpstreamProvider::Chutes.attestation_scope(),
                command,
                timeout_seconds,
            )?
            .with_chutes_session_store(session_store),
        })
    }
}

#[async_trait]
impl UpstreamVerifier for ChutesProviderVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.verify(request).await
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.refresh(request).await
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        self.verifier.invalidate(request);
    }
}

#[derive(Debug, Clone)]
pub struct TinfoilProviderVerifier {
    verifier: ExternalProviderVerifier,
}

impl TinfoilProviderVerifier {
    pub fn new(timeout_seconds: u64) -> Self {
        Self::new_with_cache(timeout_seconds, 0)
    }

    pub fn new_with_cache(timeout_seconds: u64, cache_ttl_seconds: u64) -> Self {
        Self {
            verifier: ExternalProviderVerifier::private_inference(
                "tinfoil",
                UpstreamProvider::Tinfoil.attestation_scope(),
                timeout_seconds,
                cache_ttl_seconds,
            ),
        }
    }

    #[cfg(test)]
    pub(super) fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command(
                "tinfoil",
                UpstreamProvider::Tinfoil.attestation_scope(),
                command,
                timeout_seconds,
            )?,
        })
    }
}

#[async_trait]
impl UpstreamVerifier for TinfoilProviderVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.verify(request).await
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.refresh(request).await
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        self.verifier.invalidate(request);
    }
}

#[derive(Debug, Clone)]
pub struct NearAiProviderVerifier {
    verifier: ExternalProviderVerifier,
}

impl NearAiProviderVerifier {
    pub fn new(timeout_seconds: u64) -> Self {
        Self::new_with_cache(timeout_seconds, 0)
    }

    pub fn new_with_cache(timeout_seconds: u64, cache_ttl_seconds: u64) -> Self {
        Self {
            verifier: ExternalProviderVerifier::private_inference(
                "near-ai",
                UpstreamProvider::NearAi.attestation_scope(),
                timeout_seconds,
                cache_ttl_seconds,
            ),
        }
    }

    #[cfg(test)]
    pub(super) fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command(
                "near-ai",
                UpstreamProvider::NearAi.attestation_scope(),
                command,
                timeout_seconds,
            )?,
        })
    }
}

#[async_trait]
impl UpstreamVerifier for NearAiProviderVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.verify(request).await
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.refresh(request).await
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        self.verifier.invalidate(request);
    }
}

/// Verifier for one SecretAI SecretVM origin. The external bridge verifies the
/// CPU quote, NVIDIA evidence, SPKI and nonce report-data bindings, and the
/// exact measured workload. An operator workload allowlist is optional.
#[derive(Debug, Clone)]
pub struct SecretAiProviderVerifier {
    verifier: ExternalProviderVerifier,
}

impl SecretAiProviderVerifier {
    pub fn new(timeout_seconds: u64) -> Self {
        Self::new_with_cache(timeout_seconds, 0)
    }

    pub fn new_with_cache(timeout_seconds: u64, cache_ttl_seconds: u64) -> Self {
        Self {
            verifier: ExternalProviderVerifier::private_inference(
                "secret-ai",
                UpstreamProvider::SecretAi.attestation_scope(),
                timeout_seconds,
                cache_ttl_seconds,
            ),
        }
    }

    pub fn with_accepted_workload_ids(
        mut self,
        workload_ids: impl IntoIterator<Item = String>,
    ) -> Self {
        for workload_id in workload_ids {
            self.verifier = self.verifier.with_option(
                format!("secret_ai_accepted_workload_id:{workload_id}"),
                "true",
            );
        }
        self
    }

    #[cfg(test)]
    pub(super) fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command(
                "secret-ai",
                UpstreamProvider::SecretAi.attestation_scope(),
                command,
                timeout_seconds,
            )?,
        })
    }
}

#[async_trait]
impl UpstreamVerifier for SecretAiProviderVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.verify(request).await
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.refresh(request).await
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        self.verifier.invalidate(request);
    }
}

/// Verifier for the exact official Privatemode proxy deployment measured with
/// the gateway. A successful model-list request proves that the proxy completed
/// startup, where its static credential established the Contrast
/// inference-secret exchange, and that its current provider path is live.
#[derive(Debug, Clone)]
pub struct PrivatemodeProviderVerifier {
    deployment: Arc<PrivatemodeProxyDeployment>,
    client: reqwest::Client,
    cache_ttl_seconds: u64,
    cache: Arc<RwLock<Option<CachedPrivatemodeEvent>>>,
    verify_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone)]
struct CachedPrivatemodeEvent {
    expires_at: u64,
    event: UpstreamVerifiedEvent,
}

impl CachedPrivatemodeEvent {
    fn event_for(&self, request: &UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let mut event = self.event.clone();
        event.upstream_name = request.upstream_name.clone();
        event.model_id = request.model_id.clone();
        event.url_origin = request.url_origin.clone();
        event.required = request.required;
        event
    }
}

impl PrivatemodeProviderVerifier {
    pub fn new(
        deployment: Arc<PrivatemodeProxyDeployment>,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        let client =
            deployment.readiness_client(connect_timeout_seconds, request_timeout_seconds)?;
        Ok(Self {
            deployment,
            client,
            cache_ttl_seconds,
            cache: Arc::new(RwLock::new(None)),
            verify_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    async fn verify_deployment(
        &self,
        request: UpstreamVerificationRequest,
    ) -> UpstreamVerifiedEvent {
        if let Err(err) = self.probe().await {
            return UpstreamVerifiedEvent {
                upstream_name: request.upstream_name,
                provider_type: Some("privatemode".to_string()),
                model_id: request.model_id,
                url_origin: Some(self.deployment.base_url().to_string()),
                verifier_id: "privatemode-proxy/co-deployed-contrast/v1".to_string(),
                result: VerificationResult::Failed,
                required: request.required,
                reason: Some(err.to_string()),
                ..Default::default()
            };
        }
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            provider_type: Some("privatemode".to_string()),
            model_id: request.model_id,
            url_origin: Some(self.deployment.base_url().to_string()),
            verifier_id: "privatemode-proxy/co-deployed-contrast/v1".to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            evidence: Some(self.deployment.manifest_evidence()),
            channel_bindings: vec![ChannelBinding::ManifestImageSha256 {
                provider: "privatemode".to_string(),
                manifest_sha256: self.deployment.manifest_sha256().to_string(),
                coordinator_policy_hash: self.deployment.coordinator_policy_hash().to_string(),
                proxy_image_digest: self.deployment.proxy_image_digest().to_string(),
                credential_sha256: Some(self.deployment.credential_sha256().to_string()),
            }],
            provider_claims: Some(serde_json::json!({
                "trust_boundary": "attested-compose-privatemode-proxy",
                "proxy_image_digest": self.deployment.proxy_image_digest(),
                "manifest_sha256": self.deployment.manifest_sha256(),
                "coordinator_policy_hash": self.deployment.coordinator_policy_hash(),
                "credential_sha256": self.deployment.credential_sha256(),
                "request_encryption": "privatemode-oae",
            })),
            reason: None,
        }
    }

    async fn probe(&self) -> Result<(), UpstreamError> {
        let response = self
            .client
            .get(format!("{}/v1/models", self.deployment.base_url()))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|err| {
                UpstreamError::Transport(format!("Privatemode proxy probe failed: {err}"))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(UpstreamError::Transport(format!(
                "Privatemode proxy attestation probe returned {status}"
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PRIVATEMODE_MODELS_RESPONSE_BYTES as u64)
        {
            return Err(UpstreamError::Transport(format!(
                "Privatemode proxy readiness response exceeds {MAX_PRIVATEMODE_MODELS_RESPONSE_BYTES} bytes"
            )));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| {
                UpstreamError::Transport(format!("Privatemode proxy readiness body failed: {err}"))
            })?;
            let next_len = body.len().checked_add(chunk.len()).ok_or_else(|| {
                UpstreamError::Transport(
                    "Privatemode proxy readiness response length overflowed".to_string(),
                )
            })?;
            if next_len > MAX_PRIVATEMODE_MODELS_RESPONSE_BYTES {
                return Err(UpstreamError::Transport(format!(
                    "Privatemode proxy readiness response exceeds {MAX_PRIVATEMODE_MODELS_RESPONSE_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let payload: serde_json::Value = serde_json::from_slice(&body).map_err(|err| {
            UpstreamError::Transport(format!(
                "Privatemode proxy readiness returned invalid JSON: {err}"
            ))
        })?;
        if !payload.get("data").is_some_and(serde_json::Value::is_array) {
            return Err(UpstreamError::Transport(
                "Privatemode proxy readiness returned an invalid model list".to_string(),
            ));
        }
        Ok(())
    }

    fn cached_event(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        if self.cache_ttl_seconds == 0 {
            return None;
        }
        let cached = self
            .cache
            .read()
            .expect("Privatemode verifier cache poisoned")
            .clone();
        match cached {
            Some(cached) if super::current_unix_secs() < cached.expires_at => {
                Some(cached.event_for(request))
            }
            // Leave an expired entry in place until the serialized verify or
            // refresh replaces it. Clearing it after dropping the read lock
            // could erase a fresh event written concurrently by refresh.
            Some(_) => None,
            None => None,
        }
    }

    fn maybe_cache(&self, event: &UpstreamVerifiedEvent) {
        if self.cache_ttl_seconds == 0 || event.result != VerificationResult::Verified {
            return;
        }
        *self
            .cache
            .write()
            .expect("Privatemode verifier cache poisoned") = Some(CachedPrivatemodeEvent {
            expires_at: super::current_unix_secs().saturating_add(self.cache_ttl_seconds),
            event: event.clone(),
        });
    }
}

#[async_trait]
impl UpstreamVerifier for PrivatemodeProviderVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        if let Some(event) = self.cached_event(&request) {
            return event;
        }
        let _verify_guard = self.verify_lock.lock().await;
        if let Some(event) = self.cached_event(&request) {
            return event;
        }
        let event = self.verify_deployment(request).await;
        self.maybe_cache(&event);
        event
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let _verify_guard = self.verify_lock.lock().await;
        let event = self.verify_deployment(request).await;
        self.maybe_cache(&event);
        event
    }

    fn invalidate(&self, _request: &UpstreamVerificationRequest) {
        *self
            .cache
            .write()
            .expect("Privatemode verifier cache poisoned") = None;
    }
}

/// Verifier for `PhalaDirect` upstreams: a Phala dstack-vllm-proxy attestation
/// endpoint reached directly (per model). The external bridge fetches the
/// `version=2` attestation report, verifies the dstack TDX quote, GPU evidence,
/// and the report_data binding (signing address + nonce + custom-domain TLS
/// SPKI), and returns a `tls_spki_sha256` channel binding the
/// [`OpenAICompatibleBackend`] pins on the forward connection.
///
/// (Named "direct" because it is expected to be superseded by an ACI-compatible
/// server; see [`AciServiceUpstreamVerifier`].)
#[derive(Debug, Clone)]
pub struct PhalaDirectProviderVerifier {
    verifier: ExternalProviderVerifier,
}

impl PhalaDirectProviderVerifier {
    pub fn new(timeout_seconds: u64) -> Self {
        Self::new_with_cache(timeout_seconds, 0)
    }

    pub fn new_with_cache(timeout_seconds: u64, cache_ttl_seconds: u64) -> Self {
        Self {
            verifier: ExternalProviderVerifier::private_inference(
                "phala-direct",
                UpstreamProvider::PhalaDirect.attestation_scope(),
                timeout_seconds,
                cache_ttl_seconds,
            ),
        }
    }

    /// Bearer token sent on the attestation report request (the proxy's
    /// `/v1/attestation/report` endpoint requires authorization).
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.verifier = self
            .verifier
            .with_option("phala_direct_bearer_token", token);
        self
    }

    #[cfg(test)]
    pub(super) fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command(
                "phala-direct",
                UpstreamProvider::PhalaDirect.attestation_scope(),
                command,
                timeout_seconds,
            )?,
        })
    }
}

#[async_trait]
impl UpstreamVerifier for PhalaDirectProviderVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.verify(request).await
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.verifier.refresh(request).await
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        self.verifier.invalidate(request);
    }
}

struct RoutedUpstreamVerifier {
    origin: String,
    verifier: Arc<dyn UpstreamVerifier>,
}

pub struct RoutingUpstreamVerifier {
    by_name: HashMap<String, RoutedUpstreamVerifier>,
}

impl RoutingUpstreamVerifier {
    pub fn new() -> Self {
        Self {
            by_name: HashMap::new(),
        }
    }

    pub fn add_route(
        mut self,
        name: impl Into<String>,
        origin: impl Into<String>,
        verifier: Arc<dyn UpstreamVerifier>,
    ) -> Self {
        self.by_name.insert(
            name.into(),
            RoutedUpstreamVerifier {
                origin: origin.into(),
                verifier,
            },
        );
        self
    }

    fn verifier_for(
        &self,
        request: &UpstreamVerificationRequest,
    ) -> Result<&Arc<dyn UpstreamVerifier>, String> {
        let route = self.by_name.get(&request.upstream_name).ok_or_else(|| {
            format!(
                "no verifier configured for selected upstream {:?}",
                request.upstream_name
            )
        })?;
        let requested_origin = request.url_origin.as_deref().ok_or_else(|| {
            format!(
                "selected upstream {:?} is missing its URL origin",
                request.upstream_name
            )
        })?;
        if requested_origin != route.origin {
            return Err(format!(
                "selected upstream {:?} requested origin {:?}, expected {:?}",
                request.upstream_name, requested_origin, route.origin
            ));
        }
        Ok(&route.verifier)
    }

    fn failed_event(request: UpstreamVerificationRequest, reason: String) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "routing-upstream-verifier/v1".to_string(),
            result: VerificationResult::Failed,
            required: request.required,
            reason: Some(reason),
            ..Default::default()
        }
    }
}

impl Default for RoutingUpstreamVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UpstreamVerifier for RoutingUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        match self.verifier_for(&request) {
            Ok(verifier) => verifier.verify(request).await,
            Err(reason) => Self::failed_event(request, reason),
        }
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        match self.verifier_for(&request) {
            Ok(verifier) => verifier.refresh(request).await,
            Err(reason) => Self::failed_event(request, reason),
        }
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        if let Ok(verifier) = self.verifier_for(request) {
            verifier.invalidate(request);
        }
    }
}
