//! Verifier for the official Privatemode proxy co-deployed with the gateway.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use super::{current_unix_secs, CachedProviderEvent};
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{readiness_client, PrivatemodeProxyDeployment, UpstreamError};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

/// Verifier for the exact official Privatemode proxy deployment measured with
/// the gateway. The proxy completes its initial Contrast verification and
/// secret exchange before serving, so its readiness probe corroborates startup
/// and liveness. Dynamic manifest history is reported as an observation only:
/// v1.48 does not bind a logged manifest to the secret used for a request.
#[derive(Debug, Clone)]
pub struct PrivatemodeProviderVerifier {
    deployment: Arc<PrivatemodeProxyDeployment>,
    client: reqwest::Client,
    cache_ttl_seconds: u64,
    cache: Arc<RwLock<Option<CachedProviderEvent>>>,
    verify_lock: Arc<tokio::sync::Mutex<()>>,
}

impl PrivatemodeProviderVerifier {
    pub fn new(
        deployment: Arc<PrivatemodeProxyDeployment>,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        let client = readiness_client(connect_timeout_seconds, request_timeout_seconds)?;
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
        let mut event = UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            provider_type: Some("privatemode".to_string()),
            model_id: request.model_id,
            url_origin: Some(self.deployment.base_url().to_string()),
            verifier_id: "privatemode-proxy/co-deployed-contrast/v1".to_string(),
            result: VerificationResult::Failed,
            required: request.required,
            ..Default::default()
        };
        if let Err(err) = self.probe().await {
            event.reason = Some(err.to_string());
            return event;
        }
        event.result = VerificationResult::Verified;
        event.channel_bindings = vec![self.deployment.channel_binding()];
        let mut claims = serde_json::json!({
            "trust_boundary": "attested-compose-privatemode-proxy",
            "attestation_scope": "contrast-attested-e2ee-secret",
            "request_encryption": "privatemode-oae",
            "success_response_authentication": "privatemode-oae",
            "inference_secret_policy": "latest-per-attempt-fail-closed",
            "manifest_mode": "dynamic",
        });
        // The manifest observation is supplemental: it is not bound to the
        // active secret, so a missing or unreadable one never blocks serving.
        match self.deployment.latest_observed_manifest() {
            Ok(manifest) => {
                event.evidence = Some(manifest.evidence());
                claims["observed_manifest_sha256"] = manifest.sha256.into();
                claims["manifest_observed_at"] = manifest.observed_at.into();
                claims["manifest_observation"] = "latest-proxy-fetch-log".into();
                claims["manifest_bound_to_active_secret"] = false.into();
            }
            Err(err) => {
                tracing::warn!(error = %err, "Privatemode manifest observation unavailable")
            }
        }
        event.provider_claims = Some(claims);
        event
    }

    /// The proxy's `/readyz` answers only after it starts listening, which it
    /// does only after the initial Contrast verification and secret exchange
    /// for its API key succeed.
    async fn probe(&self) -> Result<(), UpstreamError> {
        let response = self
            .client
            .get(format!("{}/readyz", self.deployment.base_url()))
            .send()
            .await
            .map_err(|err| {
                UpstreamError::Transport(format!("Privatemode proxy probe failed: {err}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(UpstreamError::Transport(format!(
                "Privatemode proxy readiness probe returned {status}"
            )));
        }
        Ok(())
    }

    fn cached_event(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        if self.cache_ttl_seconds == 0 {
            return None;
        }
        // An expired entry stays until the serialized verify or refresh
        // replaces it; clearing it here could erase a concurrent refresh.
        self.cache
            .read()
            .expect("Privatemode verifier cache poisoned")
            .as_ref()
            .filter(|cached| current_unix_secs() < cached.expires_at)
            .map(|cached| cached.event_for(request))
    }

    fn maybe_cache(&self, event: &UpstreamVerifiedEvent) {
        if self.cache_ttl_seconds == 0 || event.result != VerificationResult::Verified {
            return;
        }
        *self
            .cache
            .write()
            .expect("Privatemode verifier cache poisoned") = Some(CachedProviderEvent {
            expires_at: current_unix_secs().saturating_add(self.cache_ttl_seconds),
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
