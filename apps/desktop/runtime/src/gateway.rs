//! Sidecar lifecycle and the platform-neutral desktop view of the gateway.
//!
//! The ACI verifier sidecar listens on a private loopback port; the stable
//! local endpoint belongs to the in-process proxy. A session is only opened
//! for requests once the sidecar's verified identity and the catalog read
//! through it are both in, and they are published to the proxy together under
//! one generation; any loss of verification revokes the session and clears
//! the catalog at once. Platform clients subscribe to state changes and map
//! them to their own window and tray surfaces.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::contracts::{
    CatalogSummary, ConfidentialProfile, GatewayIdentity, GatewayState, LocalApiConfig,
    RequestActivity, SourceProvenance, StartGatewayConfig, UsageSummary, VerificationCheck,
};
use crate::usage::UsageStore;
use crate::{local_api, service_config};
use desktop_gateway::proxy::{ProxyEvent, ProxyState, Session};
use serde_json::{Map, Value};
use tokio::{
    runtime::Handle,
    sync::{mpsc::Receiver, watch},
};

const EVENT_SCHEMA_VERSION: u64 = 1;
const MAX_ACTIVITY: usize = 50;
const MAX_DIAGNOSTIC_BYTES: usize = 4_096;
const MAX_EVENT_BYTES: usize = 1_048_576;

pub struct GatewayManager {
    inner: Mutex<RuntimeState>,
    proxy: Arc<ProxyState>,
    usage: Arc<UsageStore>,
    launcher: Arc<dyn SidecarLauncher>,
    task_runtime: Handle,
    state_tx: watch::Sender<GatewayState>,
}

pub enum SidecarEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Error(String),
    Terminated,
}

pub trait SidecarChild: Send {
    fn kill(&mut self) -> Result<(), String>;
}

pub trait SidecarLauncher: Send + Sync {
    fn spawn(
        &self,
        args: Vec<String>,
    ) -> Result<(Receiver<SidecarEvent>, Box<dyn SidecarChild>), String>;
}

struct RuntimeState {
    child: Option<Box<dyn SidecarChild>>,
    /// Bumped on every start and stop; doubles as the proxy session generation.
    generation: u64,
    /// Bumped on every identity report and catalog refresh; only the newest
    /// epoch may publish a session, so a slow older read never wins.
    epoch: u64,
    stdout: Vec<u8>,
    diagnostic: VecDeque<u8>,
    /// Where the sidecar listens once ready; private to this process.
    sidecar_url: Option<String>,
    /// The sidecar reported a verified identity for this generation.
    identity_ready: bool,
    /// A settings verification may attest and discover models, but it never
    /// opens the local forwarding session to agents.
    verification_only: bool,
    /// Stable id for one protection run. Overview shows only this run while
    /// the Usage page reads every run from SQLite.
    session_id: String,
    /// Last published catalog, kept across sessions to report removed models.
    last_catalog: Option<CatalogSummary>,
    state: GatewayState,
}

impl GatewayManager {
    pub fn new(
        proxy: Arc<ProxyState>,
        usage: Arc<UsageStore>,
        launcher: Arc<dyn SidecarLauncher>,
        task_runtime: Handle,
        state: GatewayState,
    ) -> Self {
        let (state_tx, _) = watch::channel(state.clone());
        Self {
            inner: Mutex::new(RuntimeState {
                child: None,
                generation: 0,
                epoch: 0,
                stdout: Vec::new(),
                diagnostic: VecDeque::with_capacity(MAX_DIAGNOSTIC_BYTES),
                sidecar_url: None,
                identity_ready: false,
                verification_only: false,
                session_id: "unscoped".to_string(),
                last_catalog: None,
                state,
            }),
            proxy,
            usage,
            launcher,
            task_runtime,
            state_tx,
        }
    }

    pub fn snapshot(&self) -> Result<GatewayState, String> {
        Ok(self.lock()?.state.clone())
    }

    pub fn subscribe(&self) -> watch::Receiver<GatewayState> {
        self.state_tx.subscribe()
    }

    pub fn start(self: &Arc<Self>, config: StartGatewayConfig) -> Result<GatewayState, String> {
        self.start_inner(config, false, false)
    }

    pub fn begin_verification(
        self: &Arc<Self>,
        config: StartGatewayConfig,
        reset_catalog_history: bool,
    ) -> Result<GatewayState, String> {
        self.start_inner(config, true, reset_catalog_history)
    }

    fn start_inner(
        self: &Arc<Self>,
        config: StartGatewayConfig,
        verification_only: bool,
        reset_catalog_history: bool,
    ) -> Result<GatewayState, String> {
        let config = service_config::resolve_runtime_config(config)?;
        let remote_url = config.remote_url.clone();

        let mut runtime = self.lock()?;
        if let Some(error) = &runtime.state.endpoint_error {
            return Err(format!("The local endpoint is unavailable: {error}"));
        }
        if runtime.child.is_some() {
            return Err("Gateway is already running".to_string());
        }

        let mut args = Vec::with_capacity(9);
        if config.require_production_os {
            args.push("--require-production-os".to_string());
        }
        args.extend([
            "serve".to_string(),
            remote_url.clone(),
            "--listen".to_string(),
            "127.0.0.1:0".to_string(),
            "--control".to_string(),
            "127.0.0.1:0".to_string(),
            "--json-events".to_string(),
            "--verify-receipts".to_string(),
        ]);

        let (receiver, child) = self.launcher.spawn(args)?;

        runtime.generation = runtime.generation.wrapping_add(1);
        let generation = runtime.generation;
        runtime.session_id = format!("{:016x}-{:016x}", now_secs(), generation);
        let session_id = runtime.session_id.clone();
        runtime.child = Some(child);
        runtime.stdout.clear();
        runtime.diagnostic.clear();
        runtime.sidecar_url = None;
        runtime.identity_ready = false;
        runtime.verification_only = verification_only;
        if reset_catalog_history {
            runtime.last_catalog = None;
        }
        runtime.state = GatewayState {
            status: "verifying".to_string(),
            configuration_verification: verification_only,
            progress: Some("Starting the verifier".to_string()),
            remote_url: Some(remote_url.clone()),
            session_id: Some(session_id.clone()),
            session_usage: UsageSummary::default(),
            config: StartGatewayConfig {
                remote_url,
                require_production_os: config.require_production_os,
            },
            catalog: None,
            activity: Vec::new(),
            ..Self::carried(&runtime.state)
        };
        let state = runtime.state.clone();
        drop(runtime);

        self.proxy.publish(Session {
            generation,
            epoch: 0,
            session_id: Some(session_id),
            ..Session::default()
        });
        self.publish(&state);
        spawn_event_reader(Arc::clone(self), generation, receiver);
        Ok(state)
    }

    pub async fn wait_for_verification(&self, session_id: &str) -> Result<GatewayState, String> {
        let mut states = self.state_tx.subscribe();
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let state = states.borrow().clone();
                if state.session_id.as_deref() != Some(session_id) {
                    return Err("Configuration verification was superseded".to_string());
                }
                match state.status.as_str() {
                    "verified" => return Ok(state),
                    "blocked" | "error" => {
                        return Err(state
                            .error
                            .unwrap_or_else(|| "Configuration verification failed".to_string()));
                    }
                    "stopped" => return Err("Configuration verification was cancelled".to_string()),
                    _ => {}
                }
                states
                    .changed()
                    .await
                    .map_err(|_| "Configuration verification stopped unexpectedly".to_string())?;
            }
        })
        .await
        .map_err(|_| "Configuration verification timed out".to_string())?
    }

    pub fn restore_snapshot(&self, state: GatewayState) {
        let Ok(mut runtime) = self.lock() else {
            return;
        };
        runtime.session_id = state
            .session_id
            .clone()
            .unwrap_or_else(|| "unscoped".to_string());
        runtime.last_catalog = state.catalog.clone();
        runtime.state = state;
        let state = runtime.state.clone();
        drop(runtime);
        self.publish(&state);
    }

    /// Stop the sidecar in any state, including while verifying. Requests
    /// already forwarded fail with the sidecar; no new request is accepted.
    pub fn stop(&self) -> Result<GatewayState, String> {
        let mut runtime = self.lock()?;
        let child = runtime.child.take();
        runtime.generation = runtime.generation.wrapping_add(1);
        let generation = runtime.generation;
        runtime.stdout.clear();
        runtime.diagnostic.clear();
        runtime.sidecar_url = None;
        runtime.identity_ready = false;
        runtime.verification_only = false;
        runtime.state = Self::carried(&runtime.state);
        let state = runtime.state.clone();
        let epoch = runtime.epoch;
        let session_id = runtime.session_id.clone();
        drop(runtime);

        self.proxy.publish(Session {
            generation,
            epoch,
            session_id: Some(session_id),
            ..Session::default()
        });
        if let Some(mut child) = child {
            child
                .kill()
                .map_err(|error| format!("Cannot stop ACI executable: {error}"))?;
        }
        self.publish(&state);
        Ok(state)
    }

    /// What survives a stop or restart of the sidecar: settings, key status,
    /// the local endpoint, and recent activity. The catalog does not: it
    /// belongs to a verified session.
    fn carried(previous: &GatewayState) -> GatewayState {
        GatewayState {
            config: previous.config.clone(),
            profiles: previous.profiles.clone(),
            active_profile_id: previous.active_profile_id.clone(),
            local_api: previous.local_api.clone(),
            api_key_saved: previous.api_key_saved,
            proxy_url: previous.proxy_url.clone(),
            endpoint_error: previous.endpoint_error.clone(),
            activity: previous.activity.clone(),
            session_id: previous.session_id.clone(),
            session_usage: previous.session_usage.clone(),
            usage_revision: previous.usage_revision,
            catalog: previous.catalog.clone(),
            ..GatewayState::default()
        }
    }

    /// Tray switch: stop when a sidecar is running, otherwise start with the
    /// last configuration. Failures surface in the state instead of a result.
    pub fn toggle(self: &Arc<Self>) {
        let (running, config) = match self.lock() {
            Ok(runtime) => (runtime.child.is_some(), runtime.state.config.clone()),
            Err(_) => return,
        };
        let result = if running {
            self.stop()
        } else {
            self.start(config)
        };
        if let Err(message) = result {
            self.report_error(message);
        }
    }

    pub fn set_api_key_saved(&self, saved: bool) {
        self.update(|state| state.api_key_saved = saved);
    }

    pub fn set_profile_credential_saved(&self, profile_id: &str, saved: bool) {
        self.update(|state| {
            if let Some(profile) = state
                .profiles
                .iter_mut()
                .find(|profile| profile.id == profile_id)
            {
                profile.credential_saved = Some(saved);
            }
            if state.active_profile_id == profile_id {
                state.api_key_saved = saved;
            }
        });
    }

    pub fn set_service_configuration(
        &self,
        config: StartGatewayConfig,
        profiles: Vec<ConfidentialProfile>,
        active_profile_id: String,
        api_key_saved: bool,
        retain_catalog: bool,
    ) {
        let Ok(mut runtime) = self.lock() else {
            return;
        };
        runtime.state.config = config;
        runtime.state.profiles = profiles;
        runtime.state.active_profile_id = active_profile_id;
        runtime.state.api_key_saved = api_key_saved;
        runtime.state.remote_url = None;
        runtime.state.identity = None;
        runtime.state.checks.clear();
        runtime.state.error = None;
        if retain_catalog {
            runtime.last_catalog = runtime.state.catalog.clone();
        } else {
            runtime.last_catalog = None;
            runtime.state.catalog = None;
        }
        let state = runtime.state.clone();
        drop(runtime);
        self.publish(&state);
    }

    /// Record whether the stable local endpoint is bound. A failure blocks
    /// starting and connecting until the listener is successfully rebound.
    pub fn set_endpoint(&self, config: LocalApiConfig, bound: Result<String, String>) {
        self.update(|state| match bound {
            Ok(endpoint) => {
                state.local_api = config;
                state.proxy_url = Some(endpoint);
                state.endpoint_error = None;
            }
            Err(error) => {
                state.local_api = config;
                state.proxy_url = None;
                state.endpoint_error = Some(error);
            }
        });
    }

    pub fn local_api(&self) -> Result<local_api::ResolvedLocalApi, String> {
        local_api::resolve(self.lock()?.state.local_api.clone())
    }

    pub fn is_running(&self) -> Result<bool, String> {
        Ok(self.lock()?.child.is_some())
    }

    pub fn report_error(&self, message: String) {
        self.update(|state| state.error = Some(message));
    }

    pub fn clear_session_usage(&self) {
        self.update(|state| {
            state.activity.clear();
            state.session_usage = UsageSummary::default();
            state.usage_revision = state.usage_revision.wrapping_add(1);
        });
    }

    /// A request the local proxy answered itself, before any receipt.
    pub fn record_proxy_event(&self, event: ProxyEvent) {
        if event.path == "/v1/models" {
            return;
        }
        let activity = RequestActivity {
            id: event.request_id,
            session_id: event.session_id,
            method: event.method,
            path: event.path,
            model: event.model,
            status: event.status,
            streamed: event.streamed,
            receipt_id: event.receipt_id,
            verified: event.verified,
            detail: event.detail,
            at: event.at,
            agent: event.agent,
            locally_constrained: event.locally_constrained,
            rewritten: event.rewritten,
            left_device: event.left_device,
            input_tokens: event.input_tokens,
            output_tokens: event.output_tokens,
            cache_read_tokens: event.cache_read_tokens,
            cache_write_tokens: event.cache_write_tokens,
            cost_usd: event.cost_usd,
        };
        let summary = self
            .usage
            .upsert(&activity)
            .and_then(|()| self.usage.session_summary(&activity.session_id));
        self.update(|state| {
            merge_activity(state, activity.clone());
            state.usage_revision = state.usage_revision.wrapping_add(1);
            match summary {
                Ok(summary)
                    if state.session_id.as_deref() == Some(activity.session_id.as_str()) =>
                {
                    state.session_usage = summary;
                }
                Err(error) => state.error = Some(error),
                _ => {}
            }
        });
    }

    /// Re-read the catalog under a new epoch and republish the session. A
    /// result from an older epoch or generation is dropped.
    pub async fn refresh_catalog(self: &Arc<Self>) -> Result<GatewayState, String> {
        let (generation, epoch) = {
            let mut runtime = self.lock()?;
            if !runtime.identity_ready {
                return Err("Start the gateway and wait for verification first".to_string());
            }
            runtime.epoch += 1;
            let epoch = runtime.epoch;
            // Publishing the new epoch invalidates any read still in flight;
            // the session stays verified with its current catalog meanwhile.
            let current = self.proxy.session();
            self.proxy.publish(Session { epoch, ..current });
            (runtime.generation, epoch)
        };
        self.load_catalog(generation, epoch).await?;
        self.snapshot()
    }

    async fn load_catalog(self: &Arc<Self>, generation: u64, epoch: u64) -> Result<(), String> {
        let result = self.proxy.fetch_catalog(generation, epoch).await;
        let mut runtime = self.lock()?;
        if runtime.generation != generation || runtime.epoch != epoch || !runtime.identity_ready {
            return Ok(());
        }
        let Some(sidecar_url) = runtime.sidecar_url.clone() else {
            return Ok(());
        };
        let outcome = match result {
            Ok(catalog) => {
                let summary = CatalogSummary::from_catalog(&catalog, runtime.last_catalog.as_ref());
                runtime.last_catalog = Some(summary.clone());
                runtime.state.catalog = Some(summary);
                runtime.state.status = "verified".to_string();
                runtime.state.progress = None;
                runtime.state.error = None;
                if !runtime.verification_only {
                    runtime.state.protected_since.get_or_insert_with(now_secs);
                    self.proxy.publish(Session {
                        generation,
                        epoch,
                        session_id: Some(runtime.session_id.clone()),
                        base_url: Some(sidecar_url),
                        verified: true,
                        catalog: Some(catalog),
                    });
                }
                Ok(())
            }
            Err(error) => {
                let message = format!("Cannot read the verified model list: {error}");
                if runtime.state.status == "verified" {
                    // A failed manual refresh keeps the session it had.
                    runtime.state.error = Some(message.clone());
                    Err(message)
                } else {
                    // The first catalog read failed: never leave a running
                    // sidecar behind a stopped-looking UI. Terminate it and
                    // land in a plain, retryable error state.
                    drop(runtime);
                    let _ = self.fail(generation, message.clone());
                    return Err(message);
                }
            }
        };
        let state = runtime.state.clone();
        drop(runtime);
        self.publish(&state);
        outcome
    }

    fn update(&self, change: impl FnOnce(&mut GatewayState)) {
        let Ok(mut runtime) = self.lock() else {
            return;
        };
        change(&mut runtime.state);
        let state = runtime.state.clone();
        drop(runtime);
        self.publish(&state);
    }

    fn publish(&self, state: &GatewayState) {
        let _ = self.state_tx.send(state.clone());
    }

    fn handle_stdout(self: &Arc<Self>, generation: u64, bytes: &[u8]) -> Result<(), String> {
        let lines = {
            let mut runtime = self.lock()?;
            if runtime.generation != generation {
                return Ok(());
            }
            if runtime.stdout.len().saturating_add(bytes.len()) > MAX_EVENT_BYTES {
                drop(runtime);
                self.fail(generation, "ACI emitted an oversized event".to_string())?;
                return Ok(());
            }
            runtime.stdout.extend_from_slice(bytes);

            let mut lines = Vec::new();
            while let Some(position) = runtime.stdout.iter().position(|byte| *byte == b'\n') {
                let line = runtime.stdout.drain(..=position).collect::<Vec<_>>();
                lines.push(line);
            }
            lines
        };

        for line in lines {
            let line = String::from_utf8(line)
                .map_err(|_| "ACI emitted non-UTF-8 event data".to_string())?;
            let line = line.trim();
            if !line.is_empty() {
                self.handle_line(generation, line)?;
            }
        }
        Ok(())
    }

    fn handle_line(self: &Arc<Self>, generation: u64, line: &str) -> Result<(), String> {
        let event: Value = serde_json::from_str(line)
            .map_err(|_| "ACI emitted invalid JSON event data".to_string())?;
        let object = event
            .as_object()
            .ok_or_else(|| "ACI emitted an invalid event".to_string())?;
        if object.get("schema_version").and_then(Value::as_u64) != Some(EVENT_SCHEMA_VERSION) {
            return Err("ACI emitted an unknown event schema".to_string());
        }

        let event_type = required_string(object, "type")?;
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }

        let mut load_catalog = false;
        let mut persist = None;
        match event_type.as_str() {
            // Identity in (or rotated): a new epoch; the session stays closed
            // until the catalog read through this identity is in too.
            "ready" | "identity_updated" => {
                apply_identity_event(&mut runtime.state, object)?;
                if event_type == "ready" {
                    runtime.state.remote_url = Some(required_string(object, "remote_url")?);
                    runtime.sidecar_url = Some(required_string(object, "proxy_url")?);
                }
                runtime.identity_ready = true;
                runtime.epoch += 1;
                runtime.state.status = "verifying".to_string();
                runtime.state.progress = Some("Reading the verified model list".to_string());
                runtime.state.catalog = None;
                load_catalog = true;
            }
            "request_complete" => {
                persist = Some(apply_request_event(&mut runtime.state, object)?);
            }
            // Verification lost: one atomic barrier. The epoch moves so a
            // read still in flight can neither publish nor clear this error,
            // and the identity must be reported again before anything opens.
            "blocked" => {
                runtime.epoch += 1;
                runtime.identity_ready = false;
                runtime.state.status = "blocked".to_string();
                runtime.state.progress = None;
                runtime.state.catalog = None;
                runtime.state.error = Some(
                    optional_string(object, "reason")
                        .unwrap_or_else(|| "ACI blocked forwarding".to_string()),
                );
            }
            "fatal" => {
                runtime.epoch += 1;
                runtime.identity_ready = false;
                runtime.state.status = "error".to_string();
                runtime.state.progress = None;
                runtime.state.catalog = None;
                runtime.state.error = Some(
                    optional_string(object, "message").unwrap_or_else(|| "ACI failed".to_string()),
                );
            }
            _ => return Ok(()),
        }

        let mut state = runtime.state.clone();
        let epoch = runtime.epoch;
        if state.status != "verified" {
            // Any state other than verified revokes the session at once.
            self.proxy.publish(Session {
                generation,
                epoch,
                session_id: Some(runtime.session_id.clone()),
                base_url: runtime.sidecar_url.clone(),
                ..Session::default()
            });
        }
        drop(runtime);
        if let Some(activity) = persist {
            let summary = self
                .usage
                .upsert(&activity)
                .and_then(|()| self.usage.session_summary(&activity.session_id));
            let mut runtime = self.lock()?;
            if runtime.generation == generation {
                runtime.state.usage_revision = runtime.state.usage_revision.wrapping_add(1);
                match summary {
                    Ok(summary)
                        if runtime.state.session_id.as_deref()
                            == Some(activity.session_id.as_str()) =>
                    {
                        runtime.state.session_usage = summary;
                    }
                    Err(error) => runtime.state.error = Some(error),
                    _ => {}
                }
                state = runtime.state.clone();
            }
        }
        self.publish(&state);
        if load_catalog {
            let manager = Arc::clone(self);
            self.task_runtime.spawn(async move {
                let _ = manager.load_catalog(generation, epoch).await;
            });
        }
        Ok(())
    }

    fn append_diagnostic(&self, generation: u64, bytes: &[u8]) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        for byte in bytes {
            if runtime.diagnostic.len() == MAX_DIAGNOSTIC_BYTES {
                runtime.diagnostic.pop_front();
            }
            runtime.diagnostic.push_back(*byte);
        }
        Ok(())
    }

    fn terminated(&self, generation: u64) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        runtime.child = None;
        runtime.epoch += 1;
        runtime.identity_ready = false;
        runtime.verification_only = false;
        runtime.state.configuration_verification = false;
        runtime.state.catalog = None;
        runtime.state.progress = None;
        if runtime.state.status != "error" {
            let diagnostic =
                String::from_utf8_lossy(&runtime.diagnostic.iter().copied().collect::<Vec<_>>())
                    .trim()
                    .to_string();
            runtime.state.status = "error".to_string();
            runtime.state.error = Some(if diagnostic.is_empty() {
                "ACI stopped unexpectedly".to_string()
            } else {
                format!("ACI stopped unexpectedly: {diagnostic}")
            });
        }
        let state = runtime.state.clone();
        let epoch = runtime.epoch;
        let session_id = runtime.session_id.clone();
        drop(runtime);
        self.proxy.publish(Session {
            generation,
            epoch,
            session_id: Some(session_id),
            ..Session::default()
        });
        self.publish(&state);
        Ok(())
    }

    fn fail(&self, generation: u64, message: String) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        let child = runtime.child.take();
        runtime.epoch += 1;
        runtime.identity_ready = false;
        runtime.verification_only = false;
        runtime.state.configuration_verification = false;
        runtime.state.status = "error".to_string();
        runtime.state.progress = None;
        runtime.state.catalog = None;
        runtime.state.error = Some(message);
        let state = runtime.state.clone();
        let epoch = runtime.epoch;
        let session_id = runtime.session_id.clone();
        drop(runtime);
        self.proxy.publish(Session {
            generation,
            epoch,
            session_id: Some(session_id),
            ..Session::default()
        });
        if let Some(mut child) = child {
            let _ = child.kill();
        }
        self.publish(&state);
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, RuntimeState>, String> {
        self.inner
            .lock()
            .map_err(|_| "Gateway runtime state is unavailable".to_string())
    }
}

fn spawn_event_reader(
    manager: Arc<GatewayManager>,
    generation: u64,
    mut receiver: Receiver<SidecarEvent>,
) {
    let task_runtime = manager.task_runtime.clone();
    task_runtime.spawn(async move {
        while let Some(event) = receiver.recv().await {
            let result = match event {
                SidecarEvent::Stdout(bytes) => manager.handle_stdout(generation, &bytes),
                SidecarEvent::Stderr(bytes) => manager.append_diagnostic(generation, &bytes),
                SidecarEvent::Error(error) => {
                    manager.fail(generation, format!("ACI process error: {error}"))
                }
                SidecarEvent::Terminated => manager.terminated(generation),
            };
            if let Err(error) = result {
                let _ = manager.fail(generation, error);
            }
        }
    });
}

/// Record the sidecar's identity and checks; the status is decided by the
/// caller once the catalog is in.
fn apply_identity_event(
    state: &mut GatewayState,
    event: &Map<String, Value>,
) -> Result<(), String> {
    state.identity = Some(parse_identity(event)?);
    state.checks = parse_checks(event.get("verification"));
    state.error = None;
    Ok(())
}

fn parse_identity(event: &Map<String, Value>) -> Result<GatewayIdentity, String> {
    let source = event.get("source_provenance").and_then(Value::as_object);
    let capabilities = event.get("service_capabilities").and_then(Value::as_object);

    Ok(GatewayIdentity {
        tee_type: required_string(event, "tee_type")?,
        trust_level: required_string(event, "trust_level")?,
        keyset_digest: required_string(event, "keyset_digest")?,
        keyset_not_after: event
            .get("keyset_not_after")
            .and_then(Value::as_u64)
            .ok_or_else(|| "ACI emitted an invalid identity event".to_string())?,
        tls_spki: optional_string(event, "tls_spki"),
        source: SourceProvenance {
            repo_url: source.and_then(|value| optional_string(value, "repo_url")),
            repo_commit: source.and_then(|value| optional_string(value, "repo_commit")),
            image_digest: source.and_then(|value| optional_string(value, "image_digest")),
        },
        serving: capabilities
            .and_then(|value| optional_string(value, "serving"))
            .unwrap_or_else(|| "aggregator".to_string()),
        supported_e2ee_versions: capabilities
            .and_then(|value| value.get("supported_e2ee_versions"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_checks(value: Option<&Value>) -> Vec<VerificationCheck> {
    value
        .and_then(Value::as_object)
        .and_then(|verification| verification.get("checks"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let item = item.as_object()?;
            let status = optional_string(item, "status")?;
            if !matches!(status.as_str(), "pass" | "fail" | "skip" | "info") {
                return None;
            }
            Some(VerificationCheck {
                id: optional_string(item, "id")?,
                section: optional_string(item, "section")?,
                title: optional_string(item, "title")?,
                status,
                detail: optional_string(item, "detail").unwrap_or_default(),
            })
        })
        .collect()
}

fn apply_request_event(
    state: &mut GatewayState,
    event: &Map<String, Value>,
) -> Result<RequestActivity, String> {
    let status = event
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| "ACI emitted an invalid request event".to_string())?;
    let receipt_id = optional_string(event, "receipt_id");
    let (request_id, session_id, agent) = parse_request_tag(
        optional_string(event, "tag").as_deref(),
        receipt_id.as_deref(),
    );
    let activity = RequestActivity {
        id: request_id,
        session_id,
        method: required_string(event, "method")?,
        path: required_string(event, "path")?,
        model: None,
        status,
        streamed: event
            .get("streamed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        receipt_id: receipt_id.clone(),
        verified: event.get("verified").and_then(Value::as_bool),
        detail: optional_string(event, "detail").unwrap_or_default(),
        at: now_secs(),
        agent,
        locally_constrained: event.get("locally_constrained").and_then(Value::as_bool),
        rewritten: event.get("rewritten").and_then(Value::as_bool),
        left_device: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    };
    merge_activity(state, activity.clone());
    Ok(activity)
}

fn merge_activity(state: &mut GatewayState, mut incoming: RequestActivity) {
    if incoming.path == "/v1/models" {
        return;
    }
    if let Some(existing) = state
        .activity
        .iter_mut()
        .find(|item| item.id == incoming.id)
    {
        incoming.at = existing.at.min(incoming.at);
        incoming.agent = incoming.agent.or_else(|| existing.agent.clone());
        incoming.model = incoming.model.or_else(|| existing.model.clone());
        incoming.receipt_id = incoming.receipt_id.or_else(|| existing.receipt_id.clone());
        incoming.verified = incoming.verified.or(existing.verified);
        if incoming.detail.is_empty() {
            incoming.detail = existing.detail.clone();
        }
        incoming.locally_constrained = incoming
            .locally_constrained
            .or(existing.locally_constrained);
        incoming.rewritten = incoming.rewritten.or(existing.rewritten);
        incoming.left_device |= existing.left_device;
        incoming.input_tokens = incoming.input_tokens.or(existing.input_tokens);
        incoming.output_tokens = incoming.output_tokens.or(existing.output_tokens);
        incoming.cache_read_tokens = incoming.cache_read_tokens.or(existing.cache_read_tokens);
        incoming.cache_write_tokens = incoming.cache_write_tokens.or(existing.cache_write_tokens);
        incoming.cost_usd = incoming.cost_usd.or(existing.cost_usd);
        *existing = incoming;
    } else {
        state.activity.insert(0, incoming);
    }
    state.activity.sort_by(|left, right| right.at.cmp(&left.at));
    state.activity.truncate(MAX_ACTIVITY);
}

fn parse_request_tag(
    tag: Option<&str>,
    receipt_id: Option<&str>,
) -> (String, String, Option<String>) {
    if let Some(tag) = tag {
        let mut parts = tag.splitn(4, ':');
        if parts.next() == Some("pag") {
            if let (Some(request), Some(session), Some(agent)) =
                (parts.next(), parts.next(), parts.next())
            {
                if !request.is_empty() && !session.is_empty() && !agent.is_empty() {
                    return (
                        request.to_string(),
                        session.to_string(),
                        Some(agent.to_string()),
                    );
                }
            }
        }
        return (
            receipt_id.unwrap_or(tag).to_string(),
            "legacy".to_string(),
            Some(tag.to_string()),
        );
    }
    (
        receipt_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("legacy-{:016x}", now_secs())),
        "legacy".to_string(),
        None,
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    optional_string(object, key).ok_or_else(|| format!("ACI event is missing {key}"))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_preserves_usage_but_not_the_protection_clock() {
        let mut state = GatewayState {
            protected_since: Some(123),
            ..GatewayState::default()
        };
        state.session_usage.requests = 7;
        let stopped = GatewayManager::carried(&state);
        assert_eq!(stopped.protected_since, None);
        assert_eq!(stopped.session_usage.requests, 7);
    }
    use serde_json::json;

    #[test]
    fn identity_alone_does_not_verify_and_requests_are_attributed() {
        let identity = json!({
            "type": "ready",
            "schema_version": 1,
            "remote_url": "https://tee.redpill.ai",
            "proxy_url": "http://127.0.0.1:53211",
            "tee_type": "tdx",
            "trust_level": "hardware_verified",
            "keyset_digest": "sha256:keyset",
            "keyset_not_after": 2_000_000_000,
            "source_provenance": { "repo_commit": "abc123" },
            "verification": { "checks": [{
                "id": "id-1", "section": "9.1(1)", "title": "Hardware quote",
                "status": "pass", "detail": "TDX quote verified"
            }]}
        });
        let mut state = GatewayState::default();
        apply_identity_event(&mut state, identity.as_object().unwrap()).unwrap();
        assert_eq!(
            state.status, "stopped",
            "status is decided once the catalog is in"
        );
        assert!(state.identity.is_some());

        let request = json!({
            "method": "POST", "path": "/v1/messages", "status": 200, "streamed": true,
            "receipt_id": "rcpt-1", "verified": null,
            "detail": "receipt rcpt-1 recorded", "tag": "pag:req-1:session-1:claude-code"
        });
        apply_request_event(&mut state, request.as_object().unwrap()).unwrap();
        let verdict = json!({
            "method": "POST", "path": "/v1/messages", "status": 200, "streamed": true,
            "receipt_id": "rcpt-1", "verified": true, "rewritten": true,
            "locally_constrained": true,
            "detail": "receipt verified", "tag": "pag:req-1:session-1:claude-code"
        });
        apply_request_event(&mut state, verdict.as_object().unwrap()).unwrap();

        assert_eq!(state.activity.len(), 1);
        let item = &state.activity[0];
        assert_eq!(item.id, "req-1");
        assert_eq!(item.session_id, "session-1");
        assert_eq!(item.agent.as_deref(), Some("claude-code"));
        assert_eq!(item.verified, Some(true));
        assert_eq!(item.rewritten, Some(true));
        assert_eq!(item.locally_constrained, Some(true));
    }

    #[test]
    fn proxy_receipt_and_usage_events_merge_into_one_complete_activity() {
        let mut state = GatewayState::default();
        merge_activity(
            &mut state,
            RequestActivity {
                id: "req-merge".to_string(),
                session_id: "session-merge".to_string(),
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                model: Some("openai/gpt-oss-20b".to_string()),
                status: 200,
                streamed: true,
                receipt_id: Some("rcpt-merge".to_string()),
                verified: None,
                detail: "Awaiting receipt verification".to_string(),
                at: 100,
                agent: Some("claude-code".to_string()),
                locally_constrained: None,
                rewritten: None,
                left_device: true,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost_usd: None,
            },
        );

        let verdict = json!({
            "method": "POST", "path": "/v1/messages", "status": 200,
            "streamed": true, "receipt_id": "rcpt-merge", "verified": true,
            "locally_constrained": true, "rewritten": true,
            "detail": "receipt verified",
            "tag": "pag:req-merge:session-merge:claude-code"
        });
        apply_request_event(&mut state, verdict.as_object().unwrap()).unwrap();

        merge_activity(
            &mut state,
            RequestActivity {
                id: "req-merge".to_string(),
                session_id: "session-merge".to_string(),
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                model: Some("openai/gpt-oss-20b".to_string()),
                status: 200,
                streamed: true,
                receipt_id: None,
                verified: None,
                detail: String::new(),
                at: 102,
                agent: Some("claude-code".to_string()),
                locally_constrained: None,
                rewritten: None,
                left_device: true,
                input_tokens: Some(1_024),
                output_tokens: Some(256),
                cache_read_tokens: Some(512),
                cache_write_tokens: Some(64),
                cost_usd: Some(0.0042),
            },
        );

        assert_eq!(state.activity.len(), 1);
        let item = &state.activity[0];
        assert_eq!(item.id, "req-merge");
        assert_eq!(item.at, 100);
        assert_eq!(item.receipt_id.as_deref(), Some("rcpt-merge"));
        assert_eq!(item.verified, Some(true));
        assert_eq!(item.locally_constrained, Some(true));
        assert_eq!(item.rewritten, Some(true));
        assert_eq!(item.input_tokens, Some(1_024));
        assert_eq!(item.output_tokens, Some(256));
        assert_eq!(item.cache_read_tokens, Some(512));
        assert_eq!(item.cache_write_tokens, Some(64));
        assert_eq!(item.cost_usd, Some(0.0042));
    }
}
