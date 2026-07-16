//! Per-provider upstream verifiers that wrap the external bridge, plus the
//! origin/name routing verifier that dispatches to them.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;

use super::external::ExternalProviderVerifier;
#[cfg(test)]
use super::external::ProviderVerifierConfigError;
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::ChutesSessionStore;
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};
use crate::aggregator::upstream_config::UpstreamProvider;

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

/// Verifier for `PhalaDirect` upstreams: a Phala dstack-vllm-proxy attestation
/// endpoint reached directly (per model). The external bridge fetches the
/// `version=2` attestation report, verifies the dstack TDX quote, GPU evidence,
/// and the report_data binding (signing address + nonce + custom-domain TLS
/// SPKI), and returns a `tls_spki_sha256` channel binding the
/// [`OpenAICompatibleBackend`] pins on the forward connection.
///
/// (Named "direct" because it is expected to be superseded by an ACI-compatible
/// server; see [`AciDcapUpstreamVerifier`].)
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

pub struct RoutingUpstreamVerifier {
    by_origin: HashMap<String, Arc<dyn UpstreamVerifier>>,
    by_name: HashMap<String, Arc<dyn UpstreamVerifier>>,
}

impl RoutingUpstreamVerifier {
    pub fn new() -> Self {
        Self {
            by_origin: HashMap::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn add_origin(
        mut self,
        origin: impl Into<String>,
        verifier: Arc<dyn UpstreamVerifier>,
    ) -> Self {
        self.by_origin.insert(origin.into(), verifier);
        self
    }

    pub fn add_name(
        mut self,
        name: impl Into<String>,
        verifier: Arc<dyn UpstreamVerifier>,
    ) -> Self {
        self.by_name.insert(name.into(), verifier);
        self
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
        if let Some(origin) = request.url_origin.as_ref() {
            if let Some(verifier) = self.by_origin.get(origin) {
                return verifier.verify(request).await;
            }
        }
        if let Some(verifier) = self.by_name.get(&request.upstream_name) {
            return verifier.verify(request).await;
        }
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "routing-upstream-verifier/v1".to_string(),
            result: VerificationResult::Failed,
            required: request.required,
            reason: Some("no verifier configured for selected upstream".to_string()),
            ..Default::default()
        }
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        if let Some(origin) = request.url_origin.as_ref() {
            if let Some(verifier) = self.by_origin.get(origin) {
                return verifier.refresh(request).await;
            }
        }
        if let Some(verifier) = self.by_name.get(&request.upstream_name) {
            return verifier.refresh(request).await;
        }
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "routing-upstream-verifier/v1".to_string(),
            result: VerificationResult::Failed,
            required: request.required,
            reason: Some("no verifier configured for selected upstream".to_string()),
            ..Default::default()
        }
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        if let Some(origin) = request.url_origin.as_ref() {
            if let Some(verifier) = self.by_origin.get(origin) {
                verifier.invalidate(request);
                return;
            }
        }
        if let Some(verifier) = self.by_name.get(&request.upstream_name) {
            verifier.invalidate(request);
        }
    }
}
