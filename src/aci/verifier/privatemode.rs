//! Verifier for the official Privatemode proxy co-deployed with the gateway.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

mod evidence;
use tokio::time::Instant;

use super::CachedProviderEvent;
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{readiness_client, PrivatemodeProxyDeployment, UpstreamError};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

/// Verifier for the exact official Privatemode proxy deployment measured with
/// the gateway. The proxy completes its initial Contrast verification and
/// secret exchange before serving, so its readiness probe corroborates startup
/// and liveness. By default, dynamic manifest history is an observation only.
/// An opt-in proxy extension binds each response to per-secret evidence.
#[derive(Debug, Clone)]
pub struct PrivatemodeProviderVerifier {
    deployment: Arc<PrivatemodeProxyDeployment>,
    client: reqwest::Client,
    evidence_timeout: Duration,
    cache_ttl_seconds: u64,
    cache: Arc<RwLock<Option<CachedProviderEvent>>>,
    verify_lock: Arc<tokio::sync::Mutex<()>>,
    evidence_cache: Arc<tokio::sync::Mutex<VecDeque<Value>>>,
    last_bound: Arc<RwLock<Option<UpstreamVerifiedEvent>>>,
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
            evidence_timeout: Duration::from_secs(request_timeout_seconds.min(10)),
            cache_ttl_seconds,
            cache: Arc::new(RwLock::new(None)),
            verify_lock: Arc::new(tokio::sync::Mutex::new(())),
            evidence_cache: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
            last_bound: Arc::new(RwLock::new(None)),
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
        if self.deployment.active_secret_evidence() {
            if let Some(bound) = self
                .last_bound
                .read()
                .expect("Privatemode bound event poisoned")
                .as_ref()
            {
                event.evidence = bound.evidence.clone();
                event.provider_claims = bound.provider_claims.clone();
            } else {
                event.provider_claims = Some(serde_json::json!({
                    "active_secret_evidence": true,
                    "manifest_bound_to_active_secret": false,
                }));
            }
        } else {
            let manifest = match self.deployment.latest_observed_manifest() {
                Ok(manifest) => manifest,
                Err(err) => {
                    event.reason = Some(err.to_string());
                    return event;
                }
            };
            event.evidence = Some(manifest.evidence());
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
        }
        event.result = VerificationResult::Verified;
        event.channel_bindings = vec![self.deployment.channel_binding()];
        event
    }

    async fn secret_evidence(&self, secret_id: &str) -> Result<Value, String> {
        let id = evidence::url_id(secret_id)?;
        if let Some(bundle) = self
            .evidence_cache
            .lock()
            .await
            .iter()
            .find(|bundle| bundle["secret_id"] == secret_id)
        {
            return Ok(bundle.clone());
        }
        // Fetch outside the cache lock so one slow ID cannot block other IDs.
        let mut response = self
            .client
            .get(format!(
                "{}/privatemode/v1/secret-evidence/{id}",
                self.deployment.base_url()
            ))
            .timeout(self.evidence_timeout)
            .send()
            .await
            .map_err(|_| "Privatemode active-secret evidence fetch failed".to_string())?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(format!(
                "Privatemode active-secret evidence returned {}",
                response.status()
            ));
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::trim)
            != Some("application/json")
        {
            return Err("Privatemode active-secret evidence must be application/json".to_string());
        }
        if response
            .content_length()
            .is_some_and(|length| length > evidence::MAX_BODY_BYTES as u64)
        {
            return Err("Privatemode active-secret evidence exceeds size limit".to_string());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Privatemode active-secret evidence read failed".to_string())?
        {
            if chunk.len() > evidence::MAX_BODY_BYTES.saturating_sub(body.len()) {
                return Err("Privatemode active-secret evidence exceeds size limit".to_string());
            }
            body.extend_from_slice(&chunk);
        }
        let bundle = evidence::validate(&body, secret_id, &id)?;
        let mut cache = self.evidence_cache.lock().await;
        // Concurrent misses may have fetched the same immutable bundle.
        if let Some(cached) = cache.iter().find(|bundle| bundle["secret_id"] == secret_id) {
            return Ok(cached.clone());
        }
        if cache.len() == 8 {
            cache.pop_front();
        }
        cache.push_back(bundle.clone());
        Ok(bundle)
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

    fn cached_entry(&self) -> Option<CachedProviderEvent> {
        if self.cache_ttl_seconds == 0 {
            return None;
        }
        self.cache
            .read()
            .expect("Privatemode verifier cache poisoned")
            .clone()
            .filter(|cached| Instant::now() < cached.expires_at)
    }

    async fn verify_and_cache(
        &self,
        request: UpstreamVerificationRequest,
    ) -> UpstreamVerifiedEvent {
        let started = Instant::now();
        let event = self.verify_deployment(request).await;
        // A failed refresh keeps the still-valid entry.
        if self.cache_ttl_seconds > 0 && event.result == VerificationResult::Verified {
            *self
                .cache
                .write()
                .expect("Privatemode verifier cache poisoned") = Some(CachedProviderEvent {
                expires_at: started + Duration::from_secs(self.cache_ttl_seconds),
                event: event.clone(),
            });
        }
        event
    }
}

#[async_trait]
impl UpstreamVerifier for PrivatemodeProviderVerifier {
    async fn verify_response(
        &self,
        _request: UpstreamVerificationRequest,
        mut event: UpstreamVerifiedEvent,
        headers: &HashMap<String, String>,
    ) -> Result<Option<UpstreamVerifiedEvent>, String> {
        if !self.deployment.active_secret_evidence() {
            return Ok(None);
        }
        let secret_id = headers
            .get("privatemode-secret-id")
            .ok_or("Privatemode response is missing Privatemode-Secret-ID")?;
        let bundle = tokio::time::timeout(self.evidence_timeout, self.secret_evidence(secret_id))
            .await
            .map_err(|_| "Privatemode active-secret evidence deadline exceeded".to_string())??;
        event.provider_claims = Some(serde_json::json!({
            "trust_boundary": "attested-compose-privatemode-proxy",
            "attestation_scope": "contrast-attested-e2ee-secret",
            "request_encryption": "privatemode-oae",
            "success_response_authentication": "privatemode-oae",
            "inference_secret_policy": "latest-per-attempt-fail-closed",
            "active_secret_evidence": true,
            "manifest_bound_to_active_secret": true,
            "active_manifest_sha256": bundle["manifest_sha256"],
            "secret_id": secret_id,
        }));
        event.evidence = Some(bundle);
        *self
            .last_bound
            .write()
            .expect("Privatemode bound event poisoned") = Some(event.clone());
        if let Some(cached) = self
            .cache
            .write()
            .expect("Privatemode verifier cache poisoned")
            .as_mut()
        {
            cached.event = event.clone();
        }
        Ok(Some(event))
    }

    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        if let Some(event) = self.cached(&request) {
            return event;
        }
        let _verify_guard = self.verify_lock.lock().await;
        if let Some(event) = self.cached(&request) {
            return event;
        }
        self.verify_and_cache(request).await
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        self.cached_entry().map(|cached| cached.event_for(request))
    }

    fn cache_remaining(&self, _request: &UpstreamVerificationRequest) -> Option<Duration> {
        self.cached_entry()?
            .expires_at
            .checked_duration_since(Instant::now())
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let _verify_guard = self.verify_lock.lock().await;
        self.verify_and_cache(request).await
    }

    fn invalidate(&self, _request: &UpstreamVerificationRequest) {
        *self
            .cache
            .write()
            .expect("Privatemode verifier cache poisoned") = None;
    }
}
