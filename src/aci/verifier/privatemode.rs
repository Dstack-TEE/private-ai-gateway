//! Verifier for the official Privatemode proxy co-deployed with the gateway.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures_util::StreamExt;

use super::{current_unix_secs, CachedProviderEvent};
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{readiness_client, PrivatemodeProxyDeployment, UpstreamError};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

const MAX_MODELS_RESPONSE_BYTES: usize = 1024 * 1024;

/// Verifier for the exact official Privatemode proxy deployment measured with
/// the gateway. The proxy completes its initial Contrast verification and
/// secret exchange before serving, so the model-list probe corroborates startup
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
        let observed = match self.probe().await {
            Ok(()) => self
                .deployment
                .latest_observed_manifest()
                .map_err(|err| err.to_string()),
            Err(err) => Err(err.to_string()),
        };
        let manifest = match observed {
            Ok(manifest) => manifest,
            Err(reason) => {
                event.reason = Some(reason);
                return event;
            }
        };
        event.result = VerificationResult::Verified;
        event.evidence = Some(manifest.evidence());
        event.channel_bindings = vec![self.deployment.channel_binding()];
        event.provider_claims = Some(serde_json::json!({
            "trust_boundary": "attested-compose-privatemode-proxy",
            "attestation_scope": "contrast-attested-e2ee-secret",
            "request_encryption": "privatemode-oae",
            "success_response_authentication": "privatemode-oae",
            "inference_secret_policy": "latest-per-attempt-fail-closed",
            "manifest_mode": "dynamic",
            "observed_manifest_sha256": manifest.sha256,
            "manifest_observed_at": manifest.observed_at,
            "manifest_observation": "latest-proxy-fetch-log",
            "manifest_bound_to_active_secret": false,
        }));
        event
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
                "Privatemode proxy readiness probe returned {status}"
            )));
        }
        let too_large = || {
            UpstreamError::Transport(format!(
                "Privatemode proxy readiness response exceeds {MAX_MODELS_RESPONSE_BYTES} bytes"
            ))
        };
        if response
            .content_length()
            .is_some_and(|length| length > MAX_MODELS_RESPONSE_BYTES as u64)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| {
                UpstreamError::Transport(format!("Privatemode proxy readiness body failed: {err}"))
            })?;
            if body.len() + chunk.len() > MAX_MODELS_RESPONSE_BYTES {
                return Err(too_large());
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
