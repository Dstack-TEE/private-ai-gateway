//! Reusable building blocks for upstream verifiers.
//!
//! ACI §1.2 requires an aggregator to "verify upstreams inside attested
//! code before forwarding sensitive traffic" and record the result in
//! the receipt. The trait [`crate::aggregator::service::UpstreamVerifier`]
//! is the seam; this module provides two small concrete
//! implementations that are useful right now:
//!
//! * [`StaticUpstreamVerifier`] — returns a fixed
//!   [`crate::aci::receipt::UpstreamVerifiedEvent`]. Useful in tests
//!   and during bring-up when the deployment trusts a single hard-coded
//!   upstream and the verifier_id field is the only thing a relying
//!   party needs.
//! * [`PreverifiedUpstreamVerifier`] — returns a `verified` event whose
//!   fields are populated from the per-request
//!   `UpstreamVerificationRequest`. Suitable as a default for the
//!   "trusted environment, single upstream" case while real per-provider
//!   verifiers (Chutes, Tinfoil, NEAR AI, Phala dstack) are being
//!   written.
//!
//! Neither of these is a substitute for a real provider adapter that
//! fetches the upstream's evidence, applies that provider's verification
//! rules, and returns binding material the forwarding path can enforce.
//! Chutes, Tinfoil, NEAR AI, Phala dstack, and future providers can
//! expose different evidence and transport formats; the aggregator only
//! needs the common [`UpstreamVerifiedEvent`] result.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha384};
use sha3::Keccak256;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::aci::canonical::{sha256_hex, CanonicalError};
use crate::aci::identity;
use crate::aci::keys::verify_keyset_endorsement;
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::types::AttestationReport;
use crate::aci::upstream::{ChutesSessionStore, ChutesVerifiedDiscovery};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

pub const DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS: u64 = 10;
pub const DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS: u64 = 60;

/// Returns a caller-supplied event verbatim. The event's `required`
/// field is overwritten by the service to reflect the client's
/// effective verification mode (see `forward_chat_completion_request`),
/// so the caller's value there is advisory.
pub struct StaticUpstreamVerifier {
    event: UpstreamVerifiedEvent,
}

impl StaticUpstreamVerifier {
    pub fn new(event: UpstreamVerifiedEvent) -> Self {
        Self { event }
    }

    /// Convenience: build a `verified` event tagged with a fixed
    /// `verifier_id`.
    pub fn verified(verifier_id: impl Into<String>) -> Self {
        Self::new(UpstreamVerifiedEvent {
            vendor: String::new(),
            model_id: String::new(),
            url_origin: None,
            verifier_id: verifier_id.into(),
            result: VerificationResult::Verified,
            required: true,
            reason: None,
            evidence_digest: None,
            evidence_ref: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
        })
    }

    /// Convenience: build a `failed` event tagged with a fixed reason.
    pub fn failed(verifier_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(UpstreamVerifiedEvent {
            vendor: String::new(),
            model_id: String::new(),
            url_origin: None,
            verifier_id: verifier_id.into(),
            result: VerificationResult::Failed,
            required: true,
            reason: Some(reason.into()),
            evidence_digest: None,
            evidence_ref: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
        })
    }
}

#[async_trait]
impl UpstreamVerifier for StaticUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        // Populate the vendor / model_id / url_origin fields from the
        // request when the static event left them blank, so a static
        // configuration does not erase per-request context that helps
        // downstream verifiers.
        let mut event = self.event.clone();
        if event.vendor.is_empty() {
            event.vendor = request.upstream_name;
        }
        if event.model_id.is_empty() {
            event.model_id = request.model_id;
        }
        if event.url_origin.is_none() {
            event.url_origin = request.url_origin;
        }
        event
    }
}

/// Returns a `verified` event whose vendor / model_id / url_origin are
/// taken directly from the per-request [`UpstreamVerificationRequest`].
/// Useful as a placeholder verifier when the aggregator is in a
/// deployment where the upstream is already trusted out-of-band and the
/// only thing ACI needs is a deterministic `verifier_id` traceable to
/// the aggregator's source provenance.
pub struct PreverifiedUpstreamVerifier {
    verifier_id: String,
}

impl PreverifiedUpstreamVerifier {
    pub fn new(verifier_id: impl Into<String>) -> Self {
        Self {
            verifier_id: verifier_id.into(),
        }
    }
}

#[async_trait]
impl UpstreamVerifier for PreverifiedUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            vendor: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: self.verifier_id.clone(),
            result: VerificationResult::Verified,
            required: request.required,
            reason: None,
            evidence_digest: None,
            evidence_ref: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderVerifierConfigError {
    #[error("provider verifier command must not be empty")]
    EmptyCommand,
}

const PRIVATE_AI_VERIFIER_DIR_ENV: &str = "PRIVATE_AI_VERIFIER_DIR";

#[derive(Debug, Clone)]
struct ExternalProviderVerifier {
    provider: &'static str,
    command: Vec<String>,
    current_dir: Option<PathBuf>,
    env: Vec<(String, String)>,
    options: HashMap<String, String>,
    timeout_seconds: u64,
    cache_ttl_seconds: u64,
    cache: Arc<RwLock<HashMap<ExternalProviderVerifierCacheKey, CachedExternalProviderEvent>>>,
    verify_lock: Arc<tokio::sync::Mutex<()>>,
    chutes_session_store: Option<Arc<ChutesSessionStore>>,
}

impl ExternalProviderVerifier {
    fn private_inference(
        provider: &'static str,
        timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Self {
        let private_ai_dir = private_ai_verifier_dir();
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("private_ai_provider_verifier.py");
        let command = vec![
            "uv".to_string(),
            "run".to_string(),
            "python".to_string(),
            script.display().to_string(),
        ];
        Self {
            provider,
            command,
            current_dir: Some(private_ai_dir.clone()),
            env: vec![(
                PRIVATE_AI_VERIFIER_DIR_ENV.to_string(),
                private_ai_dir.display().to_string(),
            )],
            options: HashMap::new(),
            timeout_seconds,
            cache_ttl_seconds,
            cache: Arc::new(RwLock::new(HashMap::new())),
            verify_lock: Arc::new(tokio::sync::Mutex::new(())),
            chutes_session_store: None,
        }
    }

    #[cfg(test)]
    fn with_command(
        provider: &'static str,
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        if command.is_empty() {
            return Err(ProviderVerifierConfigError::EmptyCommand);
        }
        Ok(Self {
            provider,
            command,
            current_dir: None,
            env: Vec::new(),
            options: HashMap::new(),
            timeout_seconds,
            cache_ttl_seconds: 0,
            cache: Arc::new(RwLock::new(HashMap::new())),
            verify_lock: Arc::new(tokio::sync::Mutex::new(())),
            chutes_session_store: None,
        })
    }

    #[cfg(test)]
    fn with_command_and_cache(
        provider: &'static str,
        command: Vec<String>,
        timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        let mut verifier = Self::with_command(provider, command, timeout_seconds)?;
        verifier.cache_ttl_seconds = cache_ttl_seconds;
        Ok(verifier)
    }

    fn with_chutes_session_store(mut self, session_store: Arc<ChutesSessionStore>) -> Self {
        self.chutes_session_store = Some(session_store);
        self
    }

    fn with_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.insert(key.into(), value.into());
        self
    }

    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let cache_key = ExternalProviderVerifierCacheKey::from_request(&request);
        if let Some(event) = self.cached_event(&cache_key, &request) {
            return event;
        }
        let _verify_guard = self.verify_lock.lock().await;
        if let Some(event) = self.cached_event(&cache_key, &request) {
            return event;
        }
        self.verify_uncached(request, cache_key).await
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let cache_key = ExternalProviderVerifierCacheKey::from_request(&request);
        let _verify_guard = self.verify_lock.lock().await;
        self.verify_uncached(request, cache_key).await
    }

    async fn verify_uncached(
        &self,
        request: UpstreamVerificationRequest,
        cache_key: ExternalProviderVerifierCacheKey,
    ) -> UpstreamVerifiedEvent {
        let input = ExternalProviderVerifierInput {
            api_version: "aci.provider-verifier.request.v1",
            provider: self.provider,
            upstream_name: &request.upstream_name,
            url_origin: request.url_origin.as_deref(),
            model_id: &request.model_id,
            forwarded_body_hash: &request.forwarded_body_hash,
            required: request.required,
            timeout_seconds: self.timeout_seconds,
            provider_options: &self.options,
        };
        let input = match serde_json::to_vec(&input) {
            Ok(input) => input,
            Err(err) => {
                return self
                    .failed_event(request, format!("failed to encode verifier input: {err}"));
            }
        };
        let output = match self.run(input).await {
            Ok(output) => output,
            Err(err) => return self.failed_event(request, err),
        };
        let output: ExternalProviderVerifierOutput = match serde_json::from_slice(&output) {
            Ok(output) => output,
            Err(err) => {
                return self.failed_event(
                    request,
                    format!("provider verifier returned invalid JSON: {err}"),
                );
            }
        };
        match self.event_from_output(request.clone(), &output) {
            Ok(event) => {
                if event.result == VerificationResult::Verified {
                    if let Err(err) = self.record_provider_session(&output) {
                        return self.failed_event(request, err);
                    }
                }
                self.maybe_cache_event(cache_key, &event);
                event
            }
            Err(err) => self.failed_event(request, err),
        }
    }

    fn cached_event(
        &self,
        cache_key: &ExternalProviderVerifierCacheKey,
        request: &UpstreamVerificationRequest,
    ) -> Option<UpstreamVerifiedEvent> {
        if self.cache_ttl_seconds == 0 {
            return None;
        }
        let now = current_unix_secs();
        let cached = self
            .cache
            .read()
            .expect("external provider verifier cache poisoned")
            .get(cache_key)
            .cloned();
        match cached {
            Some(cached) if now < cached.expires_at => Some(cached.event_for(request)),
            Some(_) => {
                self.cache
                    .write()
                    .expect("external provider verifier cache poisoned")
                    .remove(cache_key);
                None
            }
            None => None,
        }
    }

    fn maybe_cache_event(
        &self,
        cache_key: ExternalProviderVerifierCacheKey,
        event: &UpstreamVerifiedEvent,
    ) {
        if self.cache_ttl_seconds == 0 || event.result != VerificationResult::Verified {
            return;
        }
        let cached = CachedExternalProviderEvent {
            expires_at: current_unix_secs().saturating_add(self.cache_ttl_seconds),
            event: event.clone(),
        };
        self.cache
            .write()
            .expect("external provider verifier cache poisoned")
            .insert(cache_key, cached);
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        self.cache
            .write()
            .expect("external provider verifier cache poisoned")
            .remove(&ExternalProviderVerifierCacheKey::from_request(request));
    }

    async fn run(&self, input: Vec<u8>) -> Result<Vec<u8>, String> {
        let Some((program, args)) = self.command.split_first() else {
            return Err("provider verifier command must not be empty".to_string());
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(current_dir) = &self.current_dir {
            command.current_dir(current_dir);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("failed to spawn provider verifier {program:?}: {e}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open provider verifier stdin".to_string())?;
        stdin
            .write_all(&input)
            .await
            .map_err(|e| format!("failed to write provider verifier stdin: {e}"))?;
        drop(stdin);

        let output = tokio::time::timeout(
            Duration::from_secs(self.timeout_seconds),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| {
            format!(
                "provider verifier timed out after {}s",
                self.timeout_seconds
            )
        })?
        .map_err(|e| format!("provider verifier process failed: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "provider verifier exited with status {}: {}",
                output.status,
                stderr.trim()
            ));
        }
        Ok(output.stdout)
    }

    fn event_from_output(
        &self,
        request: UpstreamVerificationRequest,
        output: &ExternalProviderVerifierOutput,
    ) -> Result<UpstreamVerifiedEvent, String> {
        let result = match output.result.as_str() {
            "verified" => VerificationResult::Verified,
            "failed" => VerificationResult::Failed,
            other => {
                return Err(format!(
                    "provider verifier returned invalid result {other:?}"
                ))
            }
        };
        let channel_bindings = parse_external_channel_bindings(output.channel_bindings.clone())?;
        if result == VerificationResult::Verified && channel_bindings.is_empty() {
            return Err(
                "provider verifier returned verified without an enforceable channel binding"
                    .to_string(),
            );
        }
        Ok(UpstreamVerifiedEvent {
            vendor: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: output
                .verifier_id
                .clone()
                .unwrap_or_else(|| format!("{}/external-verifier/v1", self.provider)),
            result,
            required: request.required,
            reason: output.reason.clone(),
            evidence_digest: output.evidence_digest.clone(),
            evidence_ref: output.evidence_ref.clone(),
            channel_bindings,
            provider_claims: output.provider_claims.clone(),
        })
    }

    fn record_provider_session(
        &self,
        output: &ExternalProviderVerifierOutput,
    ) -> Result<(), String> {
        let Some(chutes_session) = output.chutes_session.clone() else {
            return Ok(());
        };
        let Some(store) = &self.chutes_session_store else {
            return Ok(());
        };
        store
            .record_verified_discovery(chutes_session)
            .map(|_| ())
            .map_err(|e| format!("failed to record Chutes provider session: {e}"))
    }

    fn failed_event(
        &self,
        request: UpstreamVerificationRequest,
        reason: impl Into<String>,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            vendor: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: format!("{}/external-verifier/v1", self.provider),
            result: VerificationResult::Failed,
            required: request.required,
            reason: Some(reason.into()),
            evidence_digest: None,
            evidence_ref: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
        }
    }
}

fn private_ai_verifier_dir() -> PathBuf {
    if let Some(path) = std::env::var_os(PRIVATE_AI_VERIFIER_DIR_ENV) {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("aggregator crate must have a parent directory")
        .join("private-ai-verifier")
}

#[derive(Serialize)]
struct ExternalProviderVerifierInput<'a> {
    api_version: &'static str,
    provider: &'static str,
    upstream_name: &'a str,
    url_origin: Option<&'a str>,
    model_id: &'a str,
    forwarded_body_hash: &'a str,
    required: bool,
    timeout_seconds: u64,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    provider_options: &'a HashMap<String, String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ExternalProviderVerifierCacheKey {
    upstream_name: String,
    url_origin: Option<String>,
    model_id: String,
}

impl ExternalProviderVerifierCacheKey {
    fn from_request(request: &UpstreamVerificationRequest) -> Self {
        Self {
            upstream_name: request.upstream_name.clone(),
            url_origin: request.url_origin.clone(),
            model_id: request.model_id.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct CachedExternalProviderEvent {
    expires_at: u64,
    event: UpstreamVerifiedEvent,
}

impl CachedExternalProviderEvent {
    fn event_for(&self, request: &UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let mut event = self.event.clone();
        event.vendor = request.upstream_name.clone();
        event.model_id = request.model_id.clone();
        event.url_origin = request.url_origin.clone();
        event.required = request.required;
        event
    }
}

#[derive(Deserialize)]
struct ExternalProviderVerifierOutput {
    result: String,
    verifier_id: Option<String>,
    reason: Option<String>,
    evidence_digest: Option<String>,
    evidence_ref: Option<String>,
    #[serde(default)]
    channel_bindings: Vec<ExternalChannelBinding>,
    #[serde(default)]
    provider_claims: Option<serde_json::Value>,
    #[serde(default)]
    chutes_session: Option<ChutesVerifiedDiscovery>,
}

#[derive(Clone, Deserialize)]
struct ExternalChannelBinding {
    #[serde(rename = "type")]
    binding_type: String,
    origin: Option<String>,
    spki_sha256: Option<String>,
    certificate_sha256: Option<String>,
    provider: Option<String>,
    key_id: Option<String>,
    algorithm: Option<String>,
    public_key_sha256: Option<String>,
}

fn parse_external_channel_bindings(
    bindings: Vec<ExternalChannelBinding>,
) -> Result<Vec<ChannelBinding>, String> {
    let mut out = Vec::new();
    for binding in bindings {
        match binding.binding_type.as_str() {
            "tls_spki_sha256" => {
                let origin = binding.origin.ok_or_else(|| {
                    "tls_spki_sha256 channel binding is missing origin".to_string()
                })?;
                let spki_sha256 = binding.spki_sha256.ok_or_else(|| {
                    "tls_spki_sha256 channel binding is missing spki_sha256".to_string()
                })?;
                out.push(ChannelBinding::TlsSpkiSha256 {
                    origin,
                    spki_sha256: normalize_sha256_hex(&spki_sha256)?,
                });
            }
            "tls_certificate_sha256" => {
                let origin = binding.origin.ok_or_else(|| {
                    "tls_certificate_sha256 channel binding is missing origin".to_string()
                })?;
                let certificate_sha256 = binding.certificate_sha256.ok_or_else(|| {
                    "tls_certificate_sha256 channel binding is missing certificate_sha256"
                        .to_string()
                })?;
                out.push(ChannelBinding::TlsCertificateSha256 {
                    origin,
                    certificate_sha256: normalize_sha256_hex(&certificate_sha256)?,
                });
            }
            "e2ee_public_key_sha256" => {
                let provider = binding.provider.ok_or_else(|| {
                    "e2ee_public_key_sha256 channel binding is missing provider".to_string()
                })?;
                let algorithm = binding.algorithm.ok_or_else(|| {
                    "e2ee_public_key_sha256 channel binding is missing algorithm".to_string()
                })?;
                let public_key_sha256 = binding.public_key_sha256.ok_or_else(|| {
                    "e2ee_public_key_sha256 channel binding is missing public_key_sha256"
                        .to_string()
                })?;
                out.push(ChannelBinding::E2eePublicKeySha256 {
                    provider,
                    key_id: binding.key_id,
                    algorithm,
                    public_key_sha256: normalize_sha256_hex(&public_key_sha256)?,
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

fn normalize_sha256_hex(value: &str) -> Result<String, String> {
    decode_hex_32(value).map(hex::encode)
}

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
    fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command("chutes", command, timeout_seconds)?,
        })
    }

    #[cfg(test)]
    fn with_command_and_session_store(
        command: Vec<String>,
        timeout_seconds: u64,
        session_store: Arc<ChutesSessionStore>,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command("chutes", command, timeout_seconds)?
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
                timeout_seconds,
                cache_ttl_seconds,
            ),
        }
    }

    #[cfg(test)]
    fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command("tinfoil", command, timeout_seconds)?,
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
                timeout_seconds,
                cache_ttl_seconds,
            ),
        }
    }

    #[cfg(test)]
    fn with_command(
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        Ok(Self {
            verifier: ExternalProviderVerifier::with_command("near-ai", command, timeout_seconds)?,
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
            vendor: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "routing-upstream-verifier/v1".to_string(),
            result: VerificationResult::Failed,
            required: request.required,
            reason: Some("no verifier configured for selected upstream".to_string()),
            evidence_digest: None,
            evidence_ref: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
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
            vendor: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: "routing-upstream-verifier/v1".to_string(),
            result: VerificationResult::Failed,
            required: request.required,
            reason: Some("no verifier configured for selected upstream".to_string()),
            evidence_digest: None,
            evidence_ref: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
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

#[derive(Debug, Clone)]
pub struct ValidatedAciReport {
    pub workload_id: String,
    pub workload_keyset_digest: String,
    pub report_data: [u8; 32],
    pub evidence_digest: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AciReportValidationError {
    #[error("unsupported ACI api_version: {0}")]
    UnsupportedApiVersion(String),
    #[error("workload_id mismatch")]
    WorkloadIdMismatch,
    #[error("workload_keyset_digest mismatch")]
    WorkloadKeysetDigestMismatch,
    #[error("report_data mismatch")]
    ReportDataMismatch,
    #[error("invalid report_data hex: {0}")]
    InvalidReportDataHex(String),
    #[error("keyset_endorsement algo does not match workload identity algo")]
    KeysetEndorsementAlgoMismatch,
    #[error("invalid keyset_endorsement signature hex: {0}")]
    InvalidKeysetEndorsementHex(String),
    #[error("keyset_endorsement signature verification failed")]
    KeysetEndorsementInvalid,
    #[error("attestation report is not fresh at verifier time")]
    StaleReport,
    #[error("canonicalisation error: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Verify the ACI-level identity binding inside an attestation report.
///
/// This checks the workload id, keyset digest, nonce-bound report_data,
/// identity-key endorsement, and freshness. It deliberately does not
/// verify the vendor quote; provider adapters compose this with their
/// own hardware-verification step.
pub fn validate_aci_report_binding(
    report: &AttestationReport,
    nonce: Option<&str>,
    now_secs: u64,
    raw_report_body: Option<&[u8]>,
) -> Result<ValidatedAciReport, AciReportValidationError> {
    if report.api_version != "aci/1" {
        return Err(AciReportValidationError::UnsupportedApiVersion(
            report.api_version.clone(),
        ));
    }

    let computed_workload_id =
        identity::workload_id(&report.attestation.workload_keyset.workload_identity)?;
    if computed_workload_id != report.workload_id {
        return Err(AciReportValidationError::WorkloadIdMismatch);
    }

    let computed_keyset_digest =
        identity::workload_keyset_digest(&report.attestation.workload_keyset)?;
    if computed_keyset_digest != report.workload_keyset_digest {
        return Err(AciReportValidationError::WorkloadKeysetDigestMismatch);
    }

    let statement = identity::attestation_statement(
        &report.attestation.workload_keyset,
        nonce.map(str::to_string),
    )?;
    let expected_report_data = identity::report_data(&statement)?;
    let reported_report_data = decode_hex_32(&report.attestation.report_data_hex)
        .map_err(AciReportValidationError::InvalidReportDataHex)?;
    if reported_report_data != expected_report_data {
        return Err(AciReportValidationError::ReportDataMismatch);
    }

    let identity_key = &report
        .attestation
        .workload_keyset
        .workload_identity
        .public_key;
    if report.attestation.keyset_endorsement.algo != identity_key.algo {
        return Err(AciReportValidationError::KeysetEndorsementAlgoMismatch);
    }
    let endorsement_payload =
        identity::keyset_endorsement_payload(&report.attestation.workload_keyset)?;
    let endorsement_sig = decode_hex(&report.attestation.keyset_endorsement.value_hex)
        .map_err(AciReportValidationError::InvalidKeysetEndorsementHex)?;
    if !verify_keyset_endorsement(identity_key, &endorsement_payload, &endorsement_sig) {
        return Err(AciReportValidationError::KeysetEndorsementInvalid);
    }

    let freshness = &report.attestation.freshness;
    if now_secs < freshness.fetched_at || now_secs >= freshness.stale_after {
        return Err(AciReportValidationError::StaleReport);
    }

    Ok(ValidatedAciReport {
        workload_id: report.workload_id.clone(),
        workload_keyset_digest: report.workload_keyset_digest.clone(),
        report_data: expected_report_data,
        evidence_digest: raw_report_body.map(sha256_hex),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum AciDcapVerifierConfigError {
    #[error(
        "ACI DCAP upstream verifier requires at least one accepted workload id or image digest"
    )]
    EmptyPolicy,
    #[error(
        "ACI DCAP upstream verifier requires at least one accepted dstack KMS root public key"
    )]
    EmptyKmsRootPolicy,
    #[error("invalid dstack KMS root public key: {0}")]
    InvalidKmsRootPublicKey(String),
    #[error("upstream attestation report base URL is empty")]
    EmptyBaseUrl,
    #[error("failed to build verifier HTTP client: {0}")]
    Client(String),
}

#[derive(Debug, Clone)]
pub struct AciDcapVerifierPolicy {
    accepted_workload_ids: BTreeSet<String>,
    accepted_image_digests: BTreeSet<String>,
    accepted_kms_root_public_keys: BTreeSet<String>,
}

impl AciDcapVerifierPolicy {
    pub fn new(
        accepted_workload_ids: impl IntoIterator<Item = String>,
        accepted_image_digests: impl IntoIterator<Item = String>,
        accepted_kms_root_public_keys: impl IntoIterator<Item = String>,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        let accepted_workload_ids = accepted_workload_ids
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        let accepted_image_digests = accepted_image_digests
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        let accepted_kms_root_public_keys = accepted_kms_root_public_keys
            .into_iter()
            .map(|key| {
                compressed_k256_public_key_hex(&key)
                    .map_err(AciDcapVerifierConfigError::InvalidKmsRootPublicKey)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if accepted_workload_ids.is_empty() && accepted_image_digests.is_empty() {
            return Err(AciDcapVerifierConfigError::EmptyPolicy);
        }
        if accepted_kms_root_public_keys.is_empty() {
            return Err(AciDcapVerifierConfigError::EmptyKmsRootPolicy);
        }
        Ok(Self {
            accepted_workload_ids,
            accepted_image_digests,
            accepted_kms_root_public_keys,
        })
    }

    fn accepts(&self, report: &AttestationReport) -> bool {
        self.accepted_workload_ids.contains(&report.workload_id)
            || report
                .attestation
                .source_provenance
                .image_digest
                .as_ref()
                .is_some_and(|digest| self.accepted_image_digests.contains(digest))
    }
}

#[derive(Debug, thiserror::Error)]
enum AciDcapVerificationError {
    #[error("upstream attestation request failed: {0}")]
    Transport(String),
    #[error("upstream attestation returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("invalid upstream attestation JSON: {0}")]
    InvalidJson(String),
    #[error("ACI report binding failed: {0}")]
    AciBinding(#[from] AciReportValidationError),
    #[error("upstream attestation did not match verifier policy")]
    PolicyRejected,
    #[error("missing DCAP quote evidence")]
    MissingQuote,
    #[error("invalid DCAP quote hex: {0}")]
    InvalidQuoteHex(String),
    #[error("invalid quote_report_data hex: {0}")]
    InvalidQuoteReportDataHex(String),
    #[error("quote_report_data evidence does not match verified quote")]
    QuoteReportDataEvidenceMismatch,
    #[error("DCAP collateral fetch failed: {0}")]
    Collateral(String),
    #[error("DCAP quote verification failed: {0}")]
    QuoteVerification(String),
    #[error("upstream attestation verification timed out")]
    Timeout,
    #[error("attestation tee_type {reported:?} does not match verified quote type {verified:?}")]
    TeeTypeMismatch { reported: String, verified: String },
    #[error("verified quote report_data does not bind the ACI report_data")]
    QuoteReportDataMismatch,
    #[error("missing dstack event_log evidence")]
    MissingEventLog,
    #[error("invalid dstack event_log evidence: {0}")]
    InvalidEventLog(String),
    #[error("dstack event_log RTMR3 does not match verified quote")]
    EventLogRtmrMismatch,
    #[error("dstack app-id event missing from verified event log")]
    MissingAppId,
    #[error("missing dstack KMS key custody evidence")]
    MissingKeyCustody,
    #[error("unsupported key custody provider: {0}")]
    UnsupportedKeyCustodyProvider(String),
    #[error("invalid dstack KMS key custody evidence: {0}")]
    InvalidKeyCustody(String),
    #[error("missing dstack KMS identity key custody evidence")]
    MissingIdentityKeyCustody,
    #[error("dstack KMS identity key custody public key does not match workload identity")]
    IdentityKeyCustodyMismatch,
    #[error("dstack KMS identity signature chain verification failed: {0}")]
    KmsSignatureChain(String),
    #[error("dstack KMS root public key is not accepted by verifier policy")]
    KmsRootRejected,
    #[error("verified ACI/dstack upstream report did not publish a TLS SPKI binding")]
    MissingTlsSpkiBinding,
}

#[derive(Debug, Clone)]
struct CachedAciDcapVerification {
    expires_at: u64,
    vendor: String,
    evidence_digest: Option<String>,
    evidence_ref: Option<String>,
    channel_bindings: Vec<ChannelBinding>,
}

impl CachedAciDcapVerification {
    fn event_for(
        &self,
        request: UpstreamVerificationRequest,
        verifier_id: &str,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            vendor: self.vendor.clone(),
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: verifier_id.to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            reason: None,
            evidence_digest: self.evidence_digest.clone(),
            evidence_ref: self.evidence_ref.clone(),
            channel_bindings: self.channel_bindings.clone(),
            provider_claims: None,
        }
    }
}

/// Verifies an upstream ACI/dstack service by fetching its attestation
/// report, checking ACI identity/key binding against the configured
/// accepted identity, and verifying the embedded Intel DCAP quote with
/// `dcap-qvl`.
pub struct AciDcapUpstreamVerifier {
    client: reqwest::Client,
    report_base_url: String,
    pccs_url: String,
    policy: AciDcapVerifierPolicy,
    cache_ttl_seconds: u64,
    request_timeout_seconds: u64,
    cache: RwLock<Option<CachedAciDcapVerification>>,
    verifier_id: String,
}

impl AciDcapUpstreamVerifier {
    pub fn new(
        report_base_url: impl Into<String>,
        pccs_url: impl Into<String>,
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        Self::new_with_timeouts(
            report_base_url,
            pccs_url,
            policy,
            cache_ttl_seconds,
            DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS,
            DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS,
        )
    }

    pub fn new_with_timeouts(
        report_base_url: impl Into<String>,
        pccs_url: impl Into<String>,
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        let report_base_url = report_base_url.into();
        let report_base_url = report_base_url.trim().trim_end_matches('/').to_string();
        if report_base_url.is_empty() {
            return Err(AciDcapVerifierConfigError::EmptyBaseUrl);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .timeout(Duration::from_secs(request_timeout_seconds))
            .build()
            .map_err(|e| AciDcapVerifierConfigError::Client(e.to_string()))?;
        Ok(Self {
            client,
            report_base_url,
            pccs_url: pccs_url.into(),
            policy,
            cache_ttl_seconds,
            request_timeout_seconds,
            cache: RwLock::new(None),
            verifier_id: "aci-dcap/v1".to_string(),
        })
    }

    pub fn with_default_pccs(
        report_base_url: impl Into<String>,
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        Self::with_default_pccs_and_timeouts(
            report_base_url,
            policy,
            cache_ttl_seconds,
            DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS,
            DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS,
        )
    }

    pub fn with_default_pccs_and_timeouts(
        report_base_url: impl Into<String>,
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        Self::new_with_timeouts(
            report_base_url,
            dcap_qvl::PHALA_PCCS_URL.to_string(),
            policy,
            cache_ttl_seconds,
            connect_timeout_seconds,
            request_timeout_seconds,
        )
    }

    async fn verify_uncached(&self) -> Result<CachedAciDcapVerification, AciDcapVerificationError> {
        let nonce = random_nonce_hex();
        let evidence_ref = format!("{}/v1/attestation/report", self.report_base_url);
        let url = format!("{evidence_ref}?nonce={nonce}");
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| AciDcapVerificationError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| AciDcapVerificationError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(AciDcapVerificationError::HttpStatus {
                status,
                body: String::from_utf8_lossy(&body).to_string(),
            });
        }

        let report: AttestationReport = serde_json::from_slice(&body)
            .map_err(|e| AciDcapVerificationError::InvalidJson(e.to_string()))?;
        let verified_at = now_secs();
        let validated =
            validate_aci_report_binding(&report, Some(&nonce), verified_at, Some(&body))?;
        if !self.policy.accepts(&report) {
            return Err(AciDcapVerificationError::PolicyRejected);
        }
        self.verify_dcap_quote(&report, &validated, verified_at)
            .await?;
        let expires_at = verified_at
            .saturating_add(self.cache_ttl_seconds)
            .min(report.attestation.freshness.stale_after);
        let channel_bindings: Vec<ChannelBinding> = report
            .attestation
            .workload_keyset
            .tls_public_keys
            .iter()
            .map(|key| ChannelBinding::TlsSpkiSha256 {
                origin: self.report_base_url.clone(),
                spki_sha256: key.spki_sha256_hex.clone(),
            })
            .collect();
        if channel_bindings.is_empty() {
            return Err(AciDcapVerificationError::MissingTlsSpkiBinding);
        }

        Ok(CachedAciDcapVerification {
            expires_at,
            vendor: report.attestation.vendor,
            evidence_digest: validated.evidence_digest,
            evidence_ref: Some(evidence_ref),
            channel_bindings,
        })
    }

    async fn verify_dcap_quote(
        &self,
        report: &AttestationReport,
        validated: &ValidatedAciReport,
        now_secs: u64,
    ) -> Result<(), AciDcapVerificationError> {
        let quote_hex = report
            .attestation
            .evidence
            .get("quote")
            .and_then(Value::as_str)
            .ok_or(AciDcapVerificationError::MissingQuote)?;
        let raw_quote = decode_hex(quote_hex).map_err(AciDcapVerificationError::InvalidQuoteHex)?;

        let collateral = dcap_qvl::collateral::get_collateral(&self.pccs_url, &raw_quote)
            .await
            .map_err(|e| AciDcapVerificationError::Collateral(e.to_string()))?;
        let verified = dcap_qvl::verify::rustcrypto::verify(&raw_quote, &collateral, now_secs)
            .map_err(|e| AciDcapVerificationError::QuoteVerification(e.to_string()))?;

        let verified_tee_type = if verified.report.is_sgx() {
            "sgx"
        } else {
            "tdx"
        };
        if report.attestation.tee_type != verified_tee_type {
            return Err(AciDcapVerificationError::TeeTypeMismatch {
                reported: report.attestation.tee_type.clone(),
                verified: verified_tee_type.to_string(),
            });
        }

        let quote_report_data = dcap_report_data(&verified.report);
        if let Some(evidence_report_data_hex) = report
            .attestation
            .evidence
            .get("quote_report_data")
            .and_then(Value::as_str)
        {
            let evidence_report_data = decode_hex(evidence_report_data_hex)
                .map_err(AciDcapVerificationError::InvalidQuoteReportDataHex)?;
            if evidence_report_data.as_slice() != quote_report_data {
                return Err(AciDcapVerificationError::QuoteReportDataEvidenceMismatch);
            }
        }

        if quote_report_data != expected_dcap_report_data(validated.report_data).as_slice() {
            return Err(AciDcapVerificationError::QuoteReportDataMismatch);
        }
        let app_id =
            verify_dstack_event_log_and_app_id(&report.attestation.evidence, &verified.report)?;
        verify_dstack_kms_identity_custody(report, &app_id, &self.policy)?;
        Ok(())
    }
}

#[async_trait]
impl UpstreamVerifier for AciDcapUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let now_secs = now_secs();
        if let Some(cached) = self
            .cache
            .read()
            .expect("ACI DCAP verifier cache poisoned")
            .clone()
        {
            if now_secs < cached.expires_at {
                return cached.event_for(request, &self.verifier_id);
            }
        }

        match tokio::time::timeout(
            Duration::from_secs(self.request_timeout_seconds),
            self.verify_uncached(),
        )
        .await
        .map_err(|_| AciDcapVerificationError::Timeout)
        .and_then(|result| result)
        {
            Ok(verified) => {
                *self
                    .cache
                    .write()
                    .expect("ACI DCAP verifier cache poisoned") = Some(verified.clone());
                verified.event_for(request, &self.verifier_id)
            }
            Err(err) => UpstreamVerifiedEvent {
                vendor: request.upstream_name,
                model_id: request.model_id,
                url_origin: request.url_origin,
                verifier_id: self.verifier_id.clone(),
                result: VerificationResult::Failed,
                required: request.required,
                reason: Some(err.to_string()),
                evidence_digest: None,
                evidence_ref: Some(format!("{}/v1/attestation/report", self.report_base_url)),
                channel_bindings: Vec::new(),
                provider_claims: None,
            },
        }
    }
}

fn random_nonce_hex() -> String {
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .expect("system time is before UNIX_EPOCH")
}

fn expected_dcap_report_data(report_data: [u8; 32]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&report_data);
    out
}

fn dcap_report_data(report: &dcap_qvl::quote::Report) -> &[u8; 64] {
    match report {
        dcap_qvl::quote::Report::SgxEnclave(report) => &report.report_data,
        dcap_qvl::quote::Report::TD10(report) => &report.report_data,
        dcap_qvl::quote::Report::TD15(report) => &report.base.report_data,
    }
}

fn dcap_rtmr3(report: &dcap_qvl::quote::Report) -> Option<&[u8; 48]> {
    match report {
        dcap_qvl::quote::Report::TD10(report) => Some(&report.rt_mr3),
        dcap_qvl::quote::Report::TD15(report) => Some(&report.base.rt_mr3),
        dcap_qvl::quote::Report::SgxEnclave(_) => None,
    }
}

#[derive(Debug, Deserialize)]
struct DstackEventLog {
    imr: u32,
    digest: String,
    event: String,
    event_payload: String,
}

fn verify_dstack_event_log_and_app_id(
    evidence: &Value,
    report: &dcap_qvl::quote::Report,
) -> Result<Vec<u8>, AciDcapVerificationError> {
    let event_log = evidence
        .get("event_log")
        .and_then(Value::as_str)
        .ok_or(AciDcapVerificationError::MissingEventLog)?;
    let events = serde_json::from_str::<Vec<DstackEventLog>>(event_log)
        .map_err(|e| AciDcapVerificationError::InvalidEventLog(e.to_string()))?;
    let rtmr3 = replay_dstack_rtmr(&events, 3)?;
    let quote_rtmr3 = dcap_rtmr3(report).ok_or_else(|| {
        AciDcapVerificationError::InvalidEventLog(
            "dstack event log verification requires a TDX quote".to_string(),
        )
    })?;
    if rtmr3.as_slice() != quote_rtmr3 {
        return Err(AciDcapVerificationError::EventLogRtmrMismatch);
    }
    let app_id = events
        .iter()
        .take_while(|event| !(event.imr == 3 && event.event == "system-ready"))
        .find(|event| event.imr == 3 && event.event == "app-id")
        .ok_or(AciDcapVerificationError::MissingAppId)?;
    decode_hex(&app_id.event_payload).map_err(AciDcapVerificationError::InvalidEventLog)
}

fn replay_dstack_rtmr(
    events: &[DstackEventLog],
    imr: u32,
) -> Result<[u8; 48], AciDcapVerificationError> {
    let mut mr = vec![0u8; 48];
    for event in events.iter().filter(|event| event.imr == imr) {
        let mut digest =
            decode_hex(&event.digest).map_err(AciDcapVerificationError::InvalidEventLog)?;
        if digest.len() < 48 {
            digest.resize(48, 0);
        }
        mr.extend_from_slice(&digest);
        mr = Sha384::digest(&mr).to_vec();
    }
    mr.as_slice().try_into().map_err(|_| {
        AciDcapVerificationError::InvalidEventLog("replayed RTMR is not 48 bytes".to_string())
    })
}

fn verify_dstack_kms_identity_custody(
    report: &AttestationReport,
    app_id: &[u8],
    policy: &AciDcapVerifierPolicy,
) -> Result<(), AciDcapVerificationError> {
    let key_custody = report
        .attestation
        .evidence
        .get("key_custody")
        .ok_or(AciDcapVerificationError::MissingKeyCustody)?;
    let provider = key_custody
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody("missing provider".to_string())
        })?;
    if provider != "dstack-kms" {
        return Err(AciDcapVerificationError::UnsupportedKeyCustodyProvider(
            provider.to_string(),
        ));
    }
    let keys = key_custody
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| AciDcapVerificationError::InvalidKeyCustody("missing keys".to_string()))?;
    let identity = keys
        .iter()
        .find(|key| key.get("role").and_then(Value::as_str) == Some("identity"))
        .ok_or(AciDcapVerificationError::MissingIdentityKeyCustody)?;
    let public_key = identity
        .get("public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody missing public_key".to_string(),
            )
        })?;
    if public_key
        != report
            .attestation
            .workload_keyset
            .workload_identity
            .public_key
            .public_key_hex
    {
        return Err(AciDcapVerificationError::IdentityKeyCustodyMismatch);
    }
    let purpose = identity
        .get("purpose")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody missing purpose".to_string(),
            )
        })?;
    let signature_chain = identity
        .get("signature_chain")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody missing signature_chain".to_string(),
            )
        })?;
    if signature_chain.len() != 2 {
        return Err(AciDcapVerificationError::InvalidKeyCustody(format!(
            "identity key custody signature_chain must contain 2 signatures, got {}",
            signature_chain.len()
        )));
    }
    let purpose_signature = signature_chain[0]
        .as_str()
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody signature_chain[0] is not a string".to_string(),
            )
        })
        .and_then(|s| decode_hex(s).map_err(AciDcapVerificationError::InvalidKeyCustody))?;
    let app_signature = signature_chain[1]
        .as_str()
        .ok_or_else(|| {
            AciDcapVerificationError::InvalidKeyCustody(
                "identity key custody signature_chain[1] is not a string".to_string(),
            )
        })
        .and_then(|s| decode_hex(s).map_err(AciDcapVerificationError::InvalidKeyCustody))?;

    let identity_public_key_compressed = compressed_k256_public_key_hex(public_key)
        .map_err(AciDcapVerificationError::KmsSignatureChain)?;
    let purpose_message = format!("{purpose}:{identity_public_key_compressed}");
    let app_public_key = recover_k256_public_key(purpose_message.as_bytes(), &purpose_signature)
        .map_err(AciDcapVerificationError::KmsSignatureChain)?;
    let app_public_key_compressed = app_public_key.to_sec1_bytes();
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id,
        &app_public_key_compressed,
    ]
    .concat();
    let root_public_key = recover_k256_public_key(&root_message, &app_signature)
        .map_err(AciDcapVerificationError::KmsSignatureChain)?;
    let root_public_key_compressed = hex::encode(root_public_key.to_sec1_bytes());
    if !policy
        .accepted_kms_root_public_keys
        .contains(&root_public_key_compressed)
    {
        return Err(AciDcapVerificationError::KmsRootRejected);
    }
    Ok(())
}

fn recover_k256_public_key(message: &[u8], signature: &[u8]) -> Result<K256VerifyingKey, String> {
    if signature.len() != 65 {
        return Err(format!(
            "recoverable secp256k1 signature must be 65 bytes, got {}",
            signature.len()
        ));
    }
    let mut recovery_byte = signature[64];
    if (27..=30).contains(&recovery_byte) {
        recovery_byte -= 27;
    }
    let recid = RecoveryId::from_byte(recovery_byte)
        .ok_or_else(|| format!("invalid recovery id: {}", signature[64]))?;
    let sig = K256Signature::from_slice(&signature[..64])
        .map_err(|e| format!("invalid secp256k1 signature: {e}"))?;
    let digest = Keccak256::new_with_prefix(message);
    K256VerifyingKey::recover_from_digest(digest, &sig, recid)
        .map_err(|e| format!("secp256k1 public key recovery failed: {e}"))
}

fn compressed_k256_public_key_hex(public_key_hex: &str) -> Result<String, String> {
    let public_key = decode_hex(public_key_hex)?;
    let point = EncodedPoint::from_bytes(public_key)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    let key = K256VerifyingKey::from_encoded_point(&point)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    Ok(hex::encode(key.to_sec1_bytes()))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|e| e.to_string())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = decode_hex(value)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 32 bytes, got {}", bytes.len()))
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use k256::ecdsa::SigningKey;
    use serde_json::json;

    use super::*;
    use crate::aci::keys::ALGO_ECDSA_SECP256K1;
    use crate::aci::types::{
        AttestationEnvelope, Freshness, KeysetEndorsement, KeysetEpoch, PublicKeyMaterial,
        ServiceCapabilities, SourceProvenance, WorkloadIdentity, WorkloadKeyset,
    };

    fn signing_key(byte: u8) -> SigningKey {
        SigningKey::from_slice(&[byte; 32]).unwrap()
    }

    fn public_key_uncompressed_hex(key: &SigningKey) -> String {
        hex::encode(key.verifying_key().to_encoded_point(false).as_bytes())
    }

    fn public_key_compressed_hex(key: &SigningKey) -> String {
        hex::encode(key.verifying_key().to_sec1_bytes())
    }

    fn sign_recoverable(key: &SigningKey, message: &[u8]) -> String {
        let digest = Keccak256::new_with_prefix(message);
        let (signature, recid) = key.sign_digest_recoverable(digest).unwrap();
        let mut out = signature.to_vec();
        out.push(recid.to_byte());
        hex::encode(out)
    }

    fn custody_report(identity: &SigningKey, signature_chain: Vec<String>) -> AttestationReport {
        let identity_public_key = public_key_uncompressed_hex(identity);
        AttestationReport {
            api_version: "aci/1".to_string(),
            workload_id: "test-workload".to_string(),
            workload_keyset_digest: "test-keyset".to_string(),
            attestation: AttestationEnvelope {
                vendor: "test".to_string(),
                tee_type: "tdx".to_string(),
                workload_keyset: WorkloadKeyset {
                    workload_identity: WorkloadIdentity {
                        public_key: PublicKeyMaterial {
                            algo: ALGO_ECDSA_SECP256K1.to_string(),
                            public_key_hex: identity_public_key.clone(),
                        },
                        subject: None,
                    },
                    keyset_epoch: KeysetEpoch {
                        version: 1,
                        not_after: u64::MAX,
                    },
                    receipt_signing_keys: Vec::new(),
                    e2ee_public_keys: Vec::new(),
                    tls_public_keys: Vec::new(),
                },
                report_data_hex: String::new(),
                keyset_endorsement: KeysetEndorsement {
                    algo: ALGO_ECDSA_SECP256K1.to_string(),
                    value_hex: String::new(),
                },
                source_provenance: SourceProvenance::default(),
                freshness: Freshness {
                    fetched_at: 0,
                    stale_after: u64::MAX,
                },
                evidence: json!({
                    "key_custody": {
                        "provider": "dstack-kms",
                        "keys": [{
                            "role": "identity",
                            "path": "aci/identity/v1",
                            "purpose": "aci.identity.v1",
                            "algo": ALGO_ECDSA_SECP256K1,
                            "public_key": identity_public_key,
                            "signature_chain": signature_chain,
                        }]
                    }
                }),
            },
            service_capabilities: ServiceCapabilities::default(),
        }
    }

    fn provider_script(provider: &str, verifier_id: &str, binding: Value) -> Vec<String> {
        let output = json!({
            "result": "verified",
            "verifier_id": verifier_id,
            "evidence_digest": format!("sha256:{}", "11".repeat(32)),
            "evidence_ref": format!("{provider}://evidence/provider-model"),
            "channel_bindings": [binding],
            "provider_claims": {
                "fixture_provider": provider,
                "model_evidence_present": true,
            },
        })
        .to_string();
        let script = format!(
            r#"payload="$(cat)"
case "$payload" in
  *'"provider":"{provider}"'*'"model_id":"provider-model"'*) printf '%s' '{output}' ;;
  *) printf '%s' '{{"result":"failed","reason":"unexpected verifier input"}}' ;;
esac"#
        );
        vec!["/bin/sh".to_string(), "-c".to_string(), script]
    }

    fn counting_provider_script(
        counter_path: &std::path::Path,
        provider: &str,
        verifier_id: &str,
        binding: Value,
    ) -> Vec<String> {
        let output = json!({
            "result": "verified",
            "verifier_id": verifier_id,
            "evidence_digest": format!("sha256:{}", "11".repeat(32)),
            "evidence_ref": format!("{provider}://evidence/provider-model"),
            "channel_bindings": [binding],
        })
        .to_string();
        let script = format!(
            r#"payload="$(cat)"
case "$payload" in
  *'"provider":"{provider}"'*'"model_id":"provider-model"'*)
    count="$(cat "$1" 2>/dev/null || printf '0')"
    count="$((count + 1))"
    printf '%s' "$count" > "$1"
    printf '%s' '{output}'
    ;;
  *) printf '%s' '{{"result":"failed","reason":"unexpected verifier input"}}' ;;
esac"#
        );
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            script,
            "provider-cache-test".to_string(),
            counter_path.display().to_string(),
        ]
    }

    async fn assert_provider_script_verifier(
        verifier: &dyn UpstreamVerifier,
        provider: &str,
        verifier_id: &str,
        expected_binding: ChannelBinding,
    ) {
        let event = verifier
            .verify(UpstreamVerificationRequest {
                upstream_name: "provider-upstream".to_string(),
                url_origin: Some("https://provider.example".to_string()),
                model_id: "provider-model".to_string(),
                forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
                required: true,
            })
            .await;

        assert_eq!(event.result, VerificationResult::Verified);
        assert_eq!(event.verifier_id, verifier_id);
        assert_eq!(event.channel_bindings, vec![expected_binding]);
        assert_eq!(
            event.provider_claims,
            Some(json!({
                "fixture_provider": provider,
                "model_evidence_present": true,
            }))
        );
    }

    #[tokio::test]
    async fn chutes_provider_verifier_runs_provider_owned_external_verifier() {
        let verifier = ChutesProviderVerifier::with_command(
            provider_script(
                "chutes",
                "chutes/external-test/v1",
                json!({
                    "type": "e2ee_public_key_sha256",
                    "provider": "chutes",
                    "key_id": "instance-a",
                    "algorithm": "chutes-ml-kem-768",
                    "public_key_sha256": "AA".repeat(32),
                }),
            ),
            5,
        )
        .unwrap();
        assert_provider_script_verifier(
            &verifier,
            "chutes",
            "chutes/external-test/v1",
            ChannelBinding::E2eePublicKeySha256 {
                provider: "chutes".to_string(),
                key_id: Some("instance-a".to_string()),
                algorithm: "chutes-ml-kem-768".to_string(),
                public_key_sha256: "aa".repeat(32),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn chutes_provider_verifier_records_provider_session_material() {
        let session_store = Arc::new(ChutesSessionStore::new());
        let output = json!({
            "result": "verified",
            "verifier_id": "chutes/external-test/v1",
            "evidence_digest": format!("sha256:{}", "11".repeat(32)),
            "evidence_ref": "chutes://evidence/provider-model",
            "channel_bindings": [{
                "type": "e2ee_public_key_sha256",
                "provider": "chutes",
                "key_id": "instance-a",
                "algorithm": "chutes-ml-kem-768",
                "public_key_sha256": "AA".repeat(32),
            }],
            "chutes_session": {
                "chute_id": "chute-a",
                "nonce_expires_in": 55,
                "instances": [{
                    "instance_id": "instance-a",
                    "e2e_pubkey": "fixture-pubkey",
                    "public_key_sha256": "AA".repeat(32),
                    "nonces": ["nonce-a", "nonce-b"],
                }]
            }
        })
        .to_string();
        let script = format!("cat >/dev/null; printf '%s' '{output}'");
        let verifier = ChutesProviderVerifier::with_command_and_session_store(
            vec!["/bin/sh".to_string(), "-c".to_string(), script],
            5,
            session_store.clone(),
        )
        .unwrap();
        let event = verifier
            .verify(UpstreamVerificationRequest {
                upstream_name: "provider-upstream".to_string(),
                url_origin: Some("https://provider.example".to_string()),
                model_id: "provider-model".to_string(),
                forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
                required: true,
            })
            .await;

        assert_eq!(event.result, VerificationResult::Verified);
        assert_eq!(session_store.pooled_nonce_count("chute-a"), 2);
    }

    #[tokio::test]
    async fn tinfoil_provider_verifier_runs_provider_owned_external_verifier() {
        let verifier = TinfoilProviderVerifier::with_command(
            provider_script(
                "tinfoil",
                "tinfoil/external-test/v1",
                json!({
                    "type": "tls_spki_sha256",
                    "origin": "https://provider.example",
                    "spki_sha256": "AA".repeat(32),
                }),
            ),
            5,
        )
        .unwrap();
        assert_provider_script_verifier(
            &verifier,
            "tinfoil",
            "tinfoil/external-test/v1",
            ChannelBinding::TlsSpkiSha256 {
                origin: "https://provider.example".to_string(),
                spki_sha256: "aa".repeat(32),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn near_ai_provider_verifier_runs_provider_owned_external_verifier() {
        let verifier = NearAiProviderVerifier::with_command(
            provider_script(
                "near-ai",
                "near-ai/external-test/v1",
                json!({
                    "type": "tls_spki_sha256",
                    "origin": "https://provider.example",
                    "spki_sha256": "AA".repeat(32),
                }),
            ),
            5,
        )
        .unwrap();
        assert_provider_script_verifier(
            &verifier,
            "near-ai",
            "near-ai/external-test/v1",
            ChannelBinding::TlsSpkiSha256 {
                origin: "https://provider.example".to_string(),
                spki_sha256: "aa".repeat(32),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn provider_external_verifier_rejects_verified_without_binding() {
        let verifier = ChutesProviderVerifier::with_command(
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat >/dev/null; printf '%s' '{\"result\":\"verified\",\"verifier_id\":\"bad/v1\"}'"
                    .to_string(),
            ],
            5,
        )
        .unwrap();
        let event = verifier
            .verify(UpstreamVerificationRequest {
                upstream_name: "provider-upstream".to_string(),
                url_origin: Some("https://provider.example".to_string()),
                model_id: "provider-model".to_string(),
                forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
                required: true,
            })
            .await;

        assert_eq!(event.result, VerificationResult::Failed);
        assert!(event
            .reason
            .unwrap()
            .contains("without an enforceable channel binding"));
    }

    #[tokio::test]
    async fn external_provider_verifier_caches_verified_bindings() {
        let counter_path = std::env::temp_dir().join(format!(
            "private-ai-gateway-provider-cache-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&counter_path);
        let verifier = ExternalProviderVerifier::with_command_and_cache(
            "tinfoil",
            counting_provider_script(
                &counter_path,
                "tinfoil",
                "tinfoil/external-test/v1",
                json!({
                    "type": "tls_spki_sha256",
                    "origin": "https://provider.example",
                    "spki_sha256": "AA".repeat(32),
                }),
            ),
            5,
            300,
        )
        .unwrap();
        let request = UpstreamVerificationRequest {
            upstream_name: "provider-upstream".to_string(),
            url_origin: Some("https://provider.example".to_string()),
            model_id: "provider-model".to_string(),
            forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
            required: true,
        };
        let first = verifier.verify(request.clone()).await;
        let second_request = UpstreamVerificationRequest {
            forwarded_body_hash: format!("sha256:{}", "33".repeat(32)),
            required: false,
            ..request
        };
        let second = verifier.verify(second_request.clone()).await;

        assert_eq!(first.result, VerificationResult::Verified);
        assert_eq!(second.result, VerificationResult::Verified);
        assert!(!second.required);
        assert_eq!(
            std::fs::read_to_string(&counter_path).unwrap(),
            "1",
            "cached provider verifier should not run the external verifier twice"
        );

        verifier.invalidate(&second_request);
        let third = verifier.verify(second_request).await;
        assert_eq!(third.result, VerificationResult::Verified);
        assert_eq!(
            std::fs::read_to_string(&counter_path).unwrap(),
            "2",
            "invalidating the provider verifier cache should force a fresh external verifier run"
        );
        let _ = std::fs::remove_file(counter_path);
    }

    #[tokio::test]
    async fn external_provider_refresh_keeps_existing_cache_on_failure() {
        let counter_path = std::env::temp_dir().join(format!(
            "private-ai-gateway-provider-refresh-cache-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&counter_path);
        let output = json!({
            "result": "verified",
            "verifier_id": "tinfoil/external-test/v1",
            "evidence_digest": format!("sha256:{}", "11".repeat(32)),
            "evidence_ref": "tinfoil://evidence/provider-model",
            "channel_bindings": [{
                "type": "tls_spki_sha256",
                "origin": "https://provider.example",
                "spki_sha256": "AA".repeat(32),
            }],
        })
        .to_string();
        let script = format!(
            r#"cat >/dev/null
count="$(cat "$1" 2>/dev/null || printf '0')"
count="$((count + 1))"
printf '%s' "$count" > "$1"
if [ "$count" -eq 1 ]; then
  printf '%s' '{output}'
else
  printf '%s\n' 'refresh failed' >&2
  exit 42
fi"#
        );
        let verifier = ExternalProviderVerifier::with_command_and_cache(
            "tinfoil",
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                script,
                "provider-refresh-cache-test".to_string(),
                counter_path.display().to_string(),
            ],
            5,
            300,
        )
        .unwrap();
        let request = UpstreamVerificationRequest {
            upstream_name: "provider-upstream".to_string(),
            url_origin: Some("https://provider.example".to_string()),
            model_id: "provider-model".to_string(),
            forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
            required: true,
        };

        let first = verifier.verify(request.clone()).await;
        let refresh = verifier.refresh(request.clone()).await;
        let after_failed_refresh = verifier.verify(request).await;

        assert_eq!(first.result, VerificationResult::Verified);
        assert_eq!(refresh.result, VerificationResult::Failed);
        assert_eq!(after_failed_refresh.result, VerificationResult::Verified);
        assert_eq!(
            std::fs::read_to_string(&counter_path).unwrap(),
            "2",
            "failed refresh must not remove the previous verified cache entry"
        );
        let _ = std::fs::remove_file(counter_path);
    }

    #[test]
    fn cached_aci_dcap_verification_preserves_channel_bindings() {
        let cached = CachedAciDcapVerification {
            expires_at: 10,
            vendor: "gpu-a".to_string(),
            evidence_digest: Some(format!("sha256:{}", "11".repeat(32))),
            evidence_ref: Some("https://gpu-a.example/v1/attestation/report".to_string()),
            channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
                origin: "https://gpu-a.example".to_string(),
                spki_sha256: "aa".repeat(32),
            }],
        };
        let event = cached.event_for(
            UpstreamVerificationRequest {
                upstream_name: "ignored".to_string(),
                url_origin: Some("https://gpu-a.example".to_string()),
                model_id: "model-a".to_string(),
                forwarded_body_hash: format!("sha256:{}", "22".repeat(32)),
                required: true,
            },
            "aci-dcap/v1",
        );

        assert_eq!(event.result, VerificationResult::Verified);
        assert_eq!(event.channel_bindings, cached.channel_bindings);
    }

    #[test]
    fn verifies_dstack_kms_identity_key_custody_chain() {
        let root = signing_key(1);
        let app = signing_key(2);
        let identity = signing_key(3);
        let app_id = [0xab; 20];

        let purpose_message = format!("aci.identity.v1:{}", public_key_compressed_hex(&identity));
        let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
        let root_message = [
            b"dstack-kms-issued".as_slice(),
            b":",
            app_id.as_slice(),
            &app.verifying_key().to_sec1_bytes(),
        ]
        .concat();
        let app_signature = sign_recoverable(&root, &root_message);
        let report = custody_report(&identity, vec![purpose_signature, app_signature]);
        let policy = AciDcapVerifierPolicy::new(
            vec![report.workload_id.clone()],
            Vec::new(),
            vec![public_key_uncompressed_hex(&root)],
        )
        .unwrap();

        verify_dstack_kms_identity_custody(&report, &app_id, &policy).unwrap();
    }

    #[test]
    fn rejects_dstack_kms_identity_key_custody_under_unaccepted_root() {
        let root = signing_key(1);
        let other_root = signing_key(4);
        let app = signing_key(2);
        let identity = signing_key(3);
        let app_id = [0xab; 20];

        let purpose_message = format!("aci.identity.v1:{}", public_key_compressed_hex(&identity));
        let purpose_signature = sign_recoverable(&app, purpose_message.as_bytes());
        let root_message = [
            b"dstack-kms-issued".as_slice(),
            b":",
            app_id.as_slice(),
            &app.verifying_key().to_sec1_bytes(),
        ]
        .concat();
        let app_signature = sign_recoverable(&root, &root_message);
        let report = custody_report(&identity, vec![purpose_signature, app_signature]);
        let policy = AciDcapVerifierPolicy::new(
            vec![report.workload_id.clone()],
            Vec::new(),
            vec![public_key_uncompressed_hex(&other_root)],
        )
        .unwrap();

        let err = verify_dstack_kms_identity_custody(&report, &app_id, &policy)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "dstack KMS root public key is not accepted by verifier policy"
        );
    }
}
