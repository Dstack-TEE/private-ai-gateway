//! External-process provider verifier bridge.
//!
//! [`ExternalProviderVerifier`] spawns the Python provider-verifier bridge
//! (`scripts/private_ai_provider_verifier.py`) and translates its JSON output
//! into [`UpstreamVerifiedEvent`]s. The per-provider wrappers in
//! [`super::providers`] configure and delegate to it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use aci_verify::decode_hex_32;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::time::Instant;

use super::CachedProviderEvent;
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{ChutesSessionStore, ChutesVerifiedDiscovery};
use crate::aggregator::service::UpstreamVerificationRequest;
use crate::aggregator::session::EvidenceRef;
use crate::aggregator::upstream_config::AttestationScope;

#[derive(Debug, thiserror::Error)]
pub enum ProviderVerifierConfigError {
    #[error("provider verifier command must not be empty")]
    EmptyCommand,
}

struct VerifierProcess {
    child: Option<Child>,
    #[cfg(unix)]
    group: libc::pid_t,
    #[cfg(test)]
    reaped: Option<tokio::sync::oneshot::Sender<()>>,
}

impl VerifierProcess {
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        let child = command.kill_on_drop(true).spawn()?;
        Ok(Self {
            #[cfg(unix)]
            group: child.id().expect("spawned verifier has a pid") as libc::pid_t,
            child: Some(child),
            #[cfg(test)]
            reaped: None,
        })
    }
}

impl Drop for VerifierProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        // The group belongs to this spawn; SIGKILL also stops bridge subprocesses.
        if unsafe { libc::killpg(self.group, libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(%error, "failed to kill provider verifier process group");
            }
        }
        if let Some(mut child) = self.child.take() {
            #[cfg(not(unix))]
            if let Err(error) = child.start_kill() {
                tracing::warn!(%error, "failed to kill provider verifier");
            }
            #[cfg(test)]
            let reaped = self.reaped.take();
            tokio::spawn(async move {
                if let Err(error) = child.wait().await {
                    tracing::warn!(%error, "failed to reap provider verifier");
                }
                #[cfg(test)]
                if let Some(reaped) = reaped {
                    let _ = reaped.send(());
                }
            });
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ExternalProviderVerifier {
    provider: &'static str,
    /// The channel boundary this provider attests (channel-keying + scope seam).
    scope: AttestationScope,
    command: Vec<String>,
    current_dir: Option<PathBuf>,
    env: Vec<(String, String)>,
    options: HashMap<String, String>,
    timeout_seconds: u64,
    cache_ttl_seconds: u64,
    cache: Arc<RwLock<HashMap<ExternalProviderVerifierCacheKey, CachedProviderEvent>>>,
    verify_lock: Arc<tokio::sync::Mutex<()>>,
    chutes_session_store: Option<Arc<ChutesSessionStore>>,
    #[cfg(test)]
    reaped: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl ExternalProviderVerifier {
    pub(super) fn private_inference(
        provider: &'static str,
        scope: AttestationScope,
        timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let script = manifest_dir
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
            scope,
            command,
            // Run `uv run` in the gateway project so the bridge uses the gateway's
            // own uv environment and the vendored `scripts/confidential_verifier`
            // package — no sibling private-ai-verifier checkout required. An
            // external verifier checkout can still be selected by setting
            // PRIVATE_AI_VERIFIER_DIR in the gateway process environment, which the
            // spawned bridge inherits.
            current_dir: Some(manifest_dir),
            env: Vec::new(),
            options: HashMap::new(),
            timeout_seconds,
            cache_ttl_seconds,
            cache: Arc::new(RwLock::new(HashMap::new())),
            verify_lock: Arc::new(tokio::sync::Mutex::new(())),
            chutes_session_store: None,
            #[cfg(test)]
            reaped: Default::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_command(
        provider: &'static str,
        scope: AttestationScope,
        command: Vec<String>,
        timeout_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        if command.is_empty() {
            return Err(ProviderVerifierConfigError::EmptyCommand);
        }
        Ok(Self {
            provider,
            scope,
            command,
            current_dir: None,
            env: Vec::new(),
            options: HashMap::new(),
            timeout_seconds,
            cache_ttl_seconds: 0,
            cache: Arc::new(RwLock::new(HashMap::new())),
            verify_lock: Arc::new(tokio::sync::Mutex::new(())),
            chutes_session_store: None,
            #[cfg(test)]
            reaped: Default::default(),
        })
    }

    #[cfg(test)]
    pub(super) fn with_command_and_cache(
        provider: &'static str,
        scope: AttestationScope,
        command: Vec<String>,
        timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Result<Self, ProviderVerifierConfigError> {
        let mut verifier = Self::with_command(provider, scope, command, timeout_seconds)?;
        verifier.cache_ttl_seconds = cache_ttl_seconds;
        Ok(verifier)
    }

    pub(super) fn with_chutes_session_store(
        mut self,
        session_store: Arc<ChutesSessionStore>,
    ) -> Self {
        self.chutes_session_store = Some(session_store);
        self
    }

    pub(super) fn with_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.insert(key.into(), value.into());
        self
    }

    /// Cache / channel-identity key. A per-router provider shares one verified
    /// channel across all its models, so the model is dropped from the key.
    fn cache_key(&self, request: &UpstreamVerificationRequest) -> ExternalProviderVerifierCacheKey {
        let mut key = ExternalProviderVerifierCacheKey::from_request(request);
        if self.scope.is_per_router() {
            key.model_id = String::new();
        }
        key
    }

    pub(super) async fn verify(
        &self,
        request: UpstreamVerificationRequest,
    ) -> UpstreamVerifiedEvent {
        if let Some(event) = self.cached(&request) {
            return event;
        }
        let _verify_guard = self.verify_lock.lock().await;
        if let Some(event) = self.cached(&request) {
            return event;
        }
        self.verify_uncached(request).await
    }

    pub(super) fn cached(
        &self,
        request: &UpstreamVerificationRequest,
    ) -> Option<UpstreamVerifiedEvent> {
        let cache_key = self.cache_key(request);
        if self.cache_ttl_seconds == 0 {
            return None;
        }
        let now = Instant::now();
        let cached = self
            .cache
            .read()
            .expect("external provider verifier cache poisoned")
            .get(&cache_key)
            .cloned();
        match cached {
            Some(cached) if now < cached.expires_at => Some(cached.event_for(request)),
            Some(_) => {
                self.cache
                    .write()
                    .expect("external provider verifier cache poisoned")
                    .remove(&cache_key);
                None
            }
            None => None,
        }
    }

    pub(super) fn cache_remaining(
        &self,
        request: &UpstreamVerificationRequest,
    ) -> Option<Duration> {
        let cache = self
            .cache
            .read()
            .expect("external provider verifier cache poisoned");
        cache
            .get(&self.cache_key(request))?
            .expires_at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
    }

    pub(super) async fn refresh(
        &self,
        request: UpstreamVerificationRequest,
    ) -> UpstreamVerifiedEvent {
        let _verify_guard = self.verify_lock.lock().await;
        self.verify_uncached(request).await
    }

    async fn verify_uncached(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let cache_key = self.cache_key(&request);
        let started = Instant::now();
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
                self.maybe_cache_event(cache_key, &event, started);
                event
            }
            Err(err) => self.failed_event(request, err),
        }
    }

    fn maybe_cache_event(
        &self,
        cache_key: ExternalProviderVerifierCacheKey,
        event: &UpstreamVerifiedEvent,
        started: Instant,
    ) {
        if self.cache_ttl_seconds == 0 || event.result != VerificationResult::Verified {
            return;
        }
        let cached = CachedProviderEvent {
            expires_at: started + Duration::from_secs(self.cache_ttl_seconds),
            event: event.clone(),
        };
        self.cache
            .write()
            .expect("external provider verifier cache poisoned")
            .insert(cache_key, cached);
    }

    pub(super) fn invalidate(&self, request: &UpstreamVerificationRequest) {
        // Must use the same key as `verify`/`refresh` (channel-keyed for routers),
        // otherwise invalidation would target a different entry than the one
        // cached and a router's verification could never be flushed.
        self.cache
            .write()
            .expect("external provider verifier cache poisoned")
            .remove(&self.cache_key(request));
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
            .stderr(Stdio::piped());
        if let Some(current_dir) = &self.current_dir {
            command.current_dir(current_dir);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let mut process = VerifierProcess::spawn(&mut command)
            .map_err(|e| format!("failed to spawn provider verifier {program:?}: {e}"))?;
        #[cfg(test)]
        {
            process.reaped = self.reaped.lock().unwrap().take();
        }
        let child = process.child.as_mut().expect("spawned verifier child");
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open provider verifier stdin".to_string())?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to open provider verifier stdout".to_string())?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| "failed to open provider verifier stderr".to_string())?;
        let output = tokio::time::timeout(Duration::from_secs(self.timeout_seconds), async {
            stdin.write_all(&input).await?;
            drop(stdin);
            let mut out = Vec::new();
            let mut err = Vec::new();
            let (status, _, _) = tokio::try_join!(
                child.wait(),
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err),
            )?;
            Ok::<_, std::io::Error>(std::process::Output {
                status,
                stdout: out,
                stderr: err,
            })
        })
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
        let mut evidence = output.evidence.clone();
        if result == VerificationResult::Verified {
            self.enforce_attested_scope(output.attested_scope.as_deref())?;
            evidence = Some(canonical_verified_evidence(evidence)?);
        }
        Ok(UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            provider_type: Some(self.provider.to_string()),
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: output
                .verifier_id
                .clone()
                .unwrap_or_else(|| format!("{}/external-verifier/v1", self.provider)),
            result,
            required: request.required,
            reason: output.reason.clone(),
            evidence,
            channel_bindings,
            provider_claims: output.provider_claims.clone(),
        })
    }

    /// Fail-closed scope seam: a verified result must declare the scope its
    /// provider attests; a per-router provider that returns model-scoped evidence,
    /// or none, is rejected.
    fn enforce_attested_scope(&self, declared: Option<&str>) -> Result<(), String> {
        let expected = self.scope;
        match declared {
            Some(token) => match AttestationScope::from_declared(token) {
                Some(scope) if scope == expected => Ok(()),
                Some(scope) => Err(format!(
                    "provider verifier attested {} scope but {} is a per-{} provider",
                    scope.as_declared(),
                    self.provider,
                    expected.as_declared(),
                )),
                None => Err(format!(
                    "provider verifier returned an unrecognized attestation scope {token:?}"
                )),
            },
            None if expected.is_per_router() => Err(format!(
                "router provider {} did not declare its attestation scope; refusing to \
                 seal a channel session from undeclared evidence",
                self.provider,
            )),
            None => Ok(()),
        }
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
            upstream_name: request.upstream_name,
            provider_type: Some(self.provider.to_string()),
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: format!("{}/external-verifier/v1", self.provider),
            result: VerificationResult::Failed,
            required: request.required,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
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

#[derive(Deserialize)]
struct ExternalProviderVerifierOutput {
    result: String,
    verifier_id: Option<String>,
    reason: Option<String>,
    evidence: Option<Value>,
    #[serde(default)]
    channel_bindings: Vec<ExternalChannelBinding>,
    #[serde(default)]
    provider_claims: Option<serde_json::Value>,
    /// The channel boundary the verifier attests ("router" / "model" /
    /// "instance"), enforced fail-closed against the provider's expected scope.
    #[serde(default)]
    attested_scope: Option<String>,
    #[serde(default)]
    chutes_session: Option<ChutesVerifiedDiscovery>,
}

#[derive(Clone, Deserialize)]
struct ExternalChannelBinding {
    #[serde(rename = "type")]
    binding_type: String,
    origin: Option<String>,
    spki_sha256: Option<String>,
    provider: Option<String>,
    key_id: Option<String>,
    algorithm: Option<String>,
    public_key_sha256: Option<String>,
}

/// A verified result must carry evidence whose data hashes to its digest
/// (§8.2). The data is re-encoded so a stored session rebuilds byte for byte.
fn canonical_verified_evidence(evidence: Option<Value>) -> Result<Value, String> {
    let mut evidence = evidence.ok_or("provider verifier returned verified without evidence")?;
    let (content_type, bytes) = EvidenceRef::from_value(&evidence).decode().ok_or(
        "provider verifier returned verified with evidence whose data does not hash to its digest",
    )?;
    evidence["data"] = EvidenceRef::from_bytes(&content_type, &bytes)
        .data_uri
        .into();
    Ok(evidence)
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

#[cfg(all(test, target_os = "linux"))]
mod process_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::fd::{AsRawFd, FromRawFd};

    #[tokio::test(start_paused = true)]
    async fn abort_and_timeout_kill_and_reap_verifier_process_tree() {
        const HELPER_ENV: &str = "ACI_VERIFIER_PROCESS_TEST_HELPER";
        if std::env::var_os(HELPER_ENV).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .env(HELPER_ENV, "1")
                .args([
                    "--exact",
                    "aci::verifier::external::process_tests::abort_and_timeout_kill_and_reap_verifier_process_tree",
                    "--nocapture",
                ])
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        // Only this isolated test process adopts killed grandchildren, so it can
        // reap them without relying on init's timing or changing other tests.
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
            0
        );
        for timeout in [false, true] {
            // The inherited pipe confirms both processes exist before advancing time.
            let mut fds = [0; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
            let writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
            let script = format!(
                r#"sleep 60 & printf '%s %s\n' "$$" "$!" > /proc/self/fd/{}; wait"#,
                writer.as_raw_fd()
            );
            let read_pids = || {
                drop(writer);
                let mut ready = String::new();
                BufReader::new(reader).read_line(&mut ready).unwrap();
                let pids: Vec<libc::pid_t> = ready
                    .split_whitespace()
                    .map(|pid| pid.parse().unwrap())
                    .collect();
                assert_eq!(pids.len(), 2);
                assert!(pids
                    .iter()
                    .all(|pid| unsafe { libc::getpgid(*pid) } == pids[0]));
                pids
            };
            let (reaped, waited) = tokio::sync::oneshot::channel();
            let pids = if timeout {
                let verifier = ExternalProviderVerifier::with_command(
                    "tinfoil",
                    AttestationScope::PerRouter,
                    vec!["sh".to_string(), "-c".to_string(), script],
                    1,
                )
                .unwrap();
                *verifier.reaped.lock().unwrap() = Some(reaped);
                let mut run = Box::pin(verifier.run(Vec::new()));
                assert!(futures_util::poll!(&mut run).is_pending());
                let pids = read_pids();
                tokio::time::advance(Duration::from_secs(1)).await;
                assert_eq!(
                    run.await.unwrap_err(),
                    "provider verifier timed out after 1s"
                );
                pids
            } else {
                let mut command = Command::new("sh");
                command.args(["-c", &script]);
                let mut process = VerifierProcess::spawn(&mut command).unwrap();
                process.reaped = Some(reaped);
                let pids = read_pids();
                let task = tokio::spawn(async move {
                    process.child.as_mut().unwrap().wait().await.unwrap();
                });
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                pids
            };
            waited.await.unwrap();
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pids[1], &mut status, 0) }, pids[1]);
            assert!(libc::WIFSIGNALED(status));
            assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
            // Both the leader and its grandchild are gone, not just the direct child.
            assert_eq!(unsafe { libc::killpg(pids[0], 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
}
