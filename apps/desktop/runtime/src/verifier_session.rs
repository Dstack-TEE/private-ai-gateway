//! The verifier session state machine: verifier lifecycle and the
//! platform-neutral protection state (`AppState`) clients observe.
//!
//! The stable local endpoint and verifier both run in the service process. A
//! session is only opened for requests once the verifier's identity and the
//! catalog read through it are both in, and they are published together under
//! one generation; any loss of verification revokes the session and clears
//! the catalog at once. Platform clients subscribe to state changes and map
//! them to their own window and tray surfaces.

mod events;
use events::*;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use crate::endpoint_inventory::InventoryUpdater;
use crate::usage::UsageStore;
use aci_protocol::types::ServiceCapabilities;
use agent_bridge::catalog::{Catalog, EndpointInventory};
use agent_bridge::proxy::{ProxyEvent, ProxyState, Session};
use desktop_core::contracts::{
    AppState, CatalogSummary, ConfidentialProfile, ListenConfig, ModelSummary, RequestActivity,
    ServiceIdentity, SourceProvenance, StartConfig, UsageSummary, VerificationCheck,
};
use desktop_core::listen::ResolvedListen;
use desktop_core::local_api;
use desktop_core::now_secs;
use desktop_core::service_config;
use serde_json::{Map, Value};
use tokio::{runtime::Handle, sync::watch};

const MAX_ACTIVITY: usize = 50;

pub struct SessionManager {
    inventory: Option<Arc<InventoryUpdater>>,
    inner: Mutex<RuntimeState>,
    proxy: Arc<ProxyState>,
    usage: Arc<UsageStore>,
    launcher: Arc<dyn VerifierLauncher>,
    task_runtime: Handle,
    state_tx: watch::Sender<AppState>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct IdentityEvent {
    pub trust_level: String,
    pub tee_type: String,
    pub keyset_digest: String,
    pub keyset_not_after: u64,
    pub tls_spki: Option<String>,
    pub source_provenance: IdentitySourceProvenance,
    pub service_capabilities: ServiceCapabilities,
    pub verification: Value,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct IdentitySourceProvenance {
    pub repo_url: Option<String>,
    pub repo_commit: Option<String>,
    pub image_digest: Option<String>,
}

#[derive(Clone)]
pub enum VerifierEvent {
    Ready {
        identity: IdentityEvent,
        remote_url: String,
        service: Arc<dyn agent_bridge::proxy::VerifiedService>,
    },
    IdentityUpdated {
        identity: IdentityEvent,
    },
    Blocked {
        code: Option<String>,
        reason: String,
    },
    Fatal {
        message: String,
    },
    Terminated {
        error: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct VerifierConfig {
    pub remote_url: String,
    pub require_production_os: bool,
}

pub type VerifierEventSink = Arc<dyn Fn(VerifierEvent) + Send + Sync>;

pub trait VerifierTask: Send {
    fn stop(&mut self) -> Result<(), String>;
}

pub trait VerifierLauncher: Send + Sync {
    fn spawn(
        &self,
        config: VerifierConfig,
        events: VerifierEventSink,
        requests: tokio::sync::mpsc::Sender<ProxyEvent>,
    ) -> Result<Box<dyn VerifierTask>, String>;
}

struct RuntimeState {
    task: Option<Box<dyn VerifierTask>>,
    /// Bumped on every start and stop; doubles as the proxy session generation.
    generation: u64,
    /// Bumped on identity changes, independently of catalog refreshes.
    epoch: u64,
    catalog_read: u64,
    catalog: Option<Catalog>,
    service: Option<Arc<dyn agent_bridge::proxy::VerifiedService>>,
    /// The verifier reported a verified identity for this generation.
    identity_ready: bool,
    /// A settings verification may attest and discover models, but it never
    /// opens the local forwarding session to agents.
    verification_only: bool,
    /// Stable id for one protection run. Overview shows only this run while
    /// the Usage page reads every run from SQLite.
    session_id: String,
    /// Last published catalog, kept across sessions to report removed models.
    last_catalog: Option<CatalogSummary>,
    state: AppState,
}

impl SessionManager {
    pub(crate) fn with_endpoint_inventory(mut self, inventory: InventoryUpdater) -> Self {
        self.inventory = Some(Arc::new(inventory));
        self
    }
    pub fn new(
        proxy: Arc<ProxyState>,
        usage: Arc<UsageStore>,
        launcher: Arc<dyn VerifierLauncher>,
        task_runtime: Handle,
        mut state: AppState,
    ) -> Self {
        match usage.active_session() {
            Ok(Some((id, started_at))) => {
                match usage.session_summary(&id) {
                    Ok(summary) => state.session_usage = summary,
                    Err(error) => state.error = Some(error),
                }
                state.session_id = Some(id);
                state.protected_since = Some(started_at);
                state.session_active = true;
                state.reconnecting = true;
                if state.error.is_none() {
                    state.error = Some("The previous protection session was interrupted. Protection will resume after verification.".into());
                }
            }
            Err(error) => state.error = Some(error),
            Ok(None) => {}
        }
        let session_id = state
            .session_id
            .clone()
            .unwrap_or_else(|| "unscoped".into());
        let (state_tx, _) = watch::channel(state.clone());
        Self {
            inner: Mutex::new(RuntimeState {
                task: None,
                generation: 0,
                epoch: 0,
                catalog_read: 0,
                catalog: None,
                service: None,
                identity_ready: false,
                verification_only: false,
                session_id,
                last_catalog: None,
                state,
            }),
            proxy,
            inventory: None,
            usage,
            launcher,
            task_runtime,
            state_tx,
        }
    }

    pub fn snapshot(&self) -> Result<AppState, String> {
        Ok(self.lock()?.state.clone())
    }

    pub fn subscribe(&self) -> watch::Receiver<AppState> {
        self.state_tx.subscribe()
    }

    pub fn start(self: &Arc<Self>, config: StartConfig) -> Result<AppState, String> {
        self.start_inner(config, false, false)
    }

    pub fn begin_verification(
        self: &Arc<Self>,
        config: StartConfig,
        reset_catalog_history: bool,
    ) -> Result<AppState, String> {
        self.start_inner(config, true, reset_catalog_history)
    }

    fn start_inner(
        self: &Arc<Self>,
        config: StartConfig,
        verification_only: bool,
        reset_catalog_history: bool,
    ) -> Result<AppState, String> {
        let config = service_config::resolve_runtime_config(config)?;
        let remote_url = config.remote_url.clone();

        let mut runtime = self.lock()?;
        if let Some(error) = &runtime.state.endpoint_error {
            return Err(format!("The local endpoint is unavailable: {error}"));
        }
        if runtime.task.is_some() {
            return Err("Gateway is already running".to_string());
        }

        runtime.generation = runtime.generation.wrapping_add(1);
        let generation = runtime.generation;
        let continuing = runtime.state.session_active && runtime.state.session_id.is_some();
        if !continuing {
            runtime.session_id = uuid::Uuid::new_v4().to_string();
        }
        let session_id = runtime.session_id.clone();
        let started_at = if continuing {
            runtime.state.protected_since
        } else {
            None
        }
        .or_else(|| (!verification_only).then(now_secs));
        runtime.service = None;
        runtime.identity_ready = false;
        runtime.verification_only = verification_only;
        if reset_catalog_history {
            runtime.last_catalog = None;
        }
        runtime.state = AppState {
            status: "verifying".to_string(),
            configuration_verification: verification_only,
            progress: Some("Starting the verifier".to_string()),
            remote_url: Some(remote_url.clone()),
            session_id: Some(session_id.clone()),
            reconnecting: continuing && !verification_only,
            session_active: continuing || !verification_only,
            protected_since: started_at,
            session_usage: if continuing {
                runtime.state.session_usage.clone()
            } else {
                UsageSummary::default()
            },
            config: StartConfig {
                remote_url,
                require_production_os: config.require_production_os,
            },
            catalog: None,
            activity: if continuing {
                runtime.state.activity.clone()
            } else {
                Vec::new()
            },
            ..Self::carried(&runtime.state)
        };
        let state = runtime.state.clone();
        drop(runtime);

        self.proxy.publish(Session {
            generation,
            epoch: 0,
            session_id: Some(session_id.clone()),
            ..Session::default()
        });
        self.publish();
        let weak = Arc::downgrade(self);
        let events: VerifierEventSink = Arc::new(move |event| {
            if let Some(manager) = weak.upgrade() {
                if let Err(error) = manager.handle_event(generation, event) {
                    let _ = manager.fail(generation, error);
                }
            }
        });
        let task = self.launcher.spawn(
            VerifierConfig {
                remote_url: config.remote_url.clone(),
                require_production_os: config.require_production_os,
            },
            events,
            self.proxy.event_sender(),
        );
        let mut task = match task {
            Ok(task) => task,
            Err(error) => {
                let _ = self.fail(generation, error.clone());
                return Err(error);
            }
        };
        if !verification_only {
            if let Err(error) = self
                .usage
                .save_active_session(&session_id, started_at.unwrap_or_else(now_secs))
            {
                let _ = task.stop();
                let _ = self.fail(generation, error.clone());
                return Err(error);
            }
        }
        {
            let mut runtime = self.lock()?;
            if runtime.generation != generation {
                let _ = task.stop();
                return Err("Gateway start was superseded".to_string());
            }
            if matches!(runtime.state.status.as_str(), "error" | "blocked") {
                let _ = task.stop();
            } else {
                runtime.task = Some(task);
            }
        }
        let manager = Arc::clone(self);
        self.task_runtime.spawn(async move {
            let budget = Duration::from_secs(if verification_only { 45 } else { 120 });
            if let Err(error) = manager.wait_for_verification(&session_id, budget).await {
                let _ = manager.fail_if(generation, Some("verifying"), error);
            }
        });
        Ok(state)
    }

    pub async fn wait_for_verification(
        &self,
        session_id: &str,
        budget: Duration,
    ) -> Result<AppState, String> {
        let mut states = self.state_tx.subscribe();
        tokio::time::timeout(budget, async {
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
        .map_err(|_| "Service verification timed out".to_string())?
    }

    pub fn restore_snapshot(&self, mut state: AppState) {
        let Ok(mut runtime) = self.lock() else {
            return;
        };
        runtime.session_id = state
            .session_id
            .clone()
            .unwrap_or_else(|| "unscoped".to_string());
        runtime.last_catalog = state.catalog.clone();
        state.wake_monitor_available = runtime.state.wake_monitor_available;
        runtime.state = state;
        drop(runtime);
        self.publish();
    }

    /// Stop the verifier task in any state, including while verifying. Requests
    /// already forwarded are revoked; no new request is accepted.
    pub fn stop(&self) -> Result<AppState, String> {
        self.stop_with_reconnect(false)
    }

    pub fn stop_with_reconnect(&self, reconnecting: bool) -> Result<AppState, String> {
        let mut runtime = self.lock()?;
        let task = runtime.task.take();
        runtime.generation = runtime.generation.wrapping_add(1);
        let generation = runtime.generation;
        runtime.service = None;
        runtime.identity_ready = false;
        runtime.verification_only = false;
        let protected_since = runtime.state.protected_since;
        runtime.state = Self::carried(&runtime.state);
        runtime.state.reconnecting = reconnecting;
        runtime.state.session_active = reconnecting && runtime.state.session_active;
        if reconnecting {
            runtime.state.protected_since = protected_since;
        }
        let state = runtime.state.clone();
        let epoch = runtime.epoch;
        let session_id = runtime.session_id.clone();
        drop(runtime);

        self.proxy.publish(Session {
            generation,
            epoch,
            session_id: reconnecting.then_some(session_id),
            ..Session::default()
        });
        let session_result = if reconnecting {
            Ok(())
        } else {
            self.usage.end_session()
        };
        if let Some(mut task) = task {
            task.stop()
                .map_err(|error| format!("Cannot stop verifier task: {error}"))?;
        }
        self.publish();
        session_result?;
        Ok(state)
    }

    /// What survives a stop or restart of the verifier: settings, key status,
    /// the local endpoint, and recent activity. The catalog does not: it
    /// belongs to a verified session.
    fn carried(previous: &AppState) -> AppState {
        AppState {
            wake_monitor_available: previous.wake_monitor_available,
            config: previous.config.clone(),
            client_key_revision: previous.client_key_revision,
            client_key_available: previous.client_key_available,
            profiles: previous.profiles.clone(),
            active_profile_id: previous.active_profile_id.clone(),
            local_api: previous.local_api.clone(),
            api_key_saved: previous.api_key_saved,
            proxy_url: previous.proxy_url.clone(),
            endpoint_error: previous.endpoint_error.clone(),
            activity: previous.activity.clone(),
            session_id: previous.session_id.clone(),
            session_active: previous.session_active,
            session_usage: previous.session_usage.clone(),
            usage_revision: previous.usage_revision,
            catalog: previous.catalog.clone(),
            web_ui: previous.web_ui.clone(),
            ..AppState::default()
        }
    }

    pub fn set_api_key_saved(&self, saved: bool) {
        self.update(|state| state.api_key_saved = saved);
    }

    pub fn client_key_changed(&self, available: bool) {
        self.update(|state| {
            state.client_key_revision = state.client_key_revision.saturating_add(1);
            state.client_key_available = Some(available);
        });
    }

    pub fn set_wake_monitor_available(&self, available: bool) -> bool {
        let Ok(mut runtime) = self.lock() else {
            return false;
        };
        if runtime.state.wake_monitor_available == Some(available) {
            return false;
        }
        runtime.state.wake_monitor_available = Some(available);
        drop(runtime);
        self.publish();
        true
    }

    pub fn set_profile_credential_saved(&self, profile_id: &str, saved: bool) {
        self.update(|state| {
            if let Some(profile) = state
                .profiles
                .iter_mut()
                .find(|profile| profile.id == profile_id)
            {
                profile.credential_saved = saved;
            }
            if state.active_profile_id == profile_id {
                state.api_key_saved = saved;
            }
        });
    }

    pub fn set_service_configuration(
        &self,
        config: StartConfig,
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
        drop(runtime);
        self.publish();
    }

    pub fn update_profile_list(
        &self,
        profiles: Vec<ConfidentialProfile>,
        active_profile_id: String,
        config: StartConfig,
    ) {
        self.update(|state| {
            state.profiles = profiles;
            state.active_profile_id = active_profile_id;
            state.config = config;
        });
    }

    /// Record whether the stable local endpoint is bound. A failure blocks
    /// starting and connecting until the listener is successfully rebound.
    pub fn set_endpoint(&self, config: ListenConfig, bound: Result<String, String>) {
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

    pub fn set_web_ui(&self, status: desktop_core::contracts::WebUiStatus) {
        self.update(|state| state.web_ui = status);
    }

    pub fn local_api(&self) -> Result<ResolvedListen, String> {
        local_api::resolve(self.lock()?.state.local_api.clone())
    }

    pub fn is_running(&self) -> Result<bool, String> {
        Ok(self.lock()?.task.is_some())
    }

    pub fn report_error(&self, message: String) {
        self.update(|state| state.error = Some(message));
    }

    pub fn cancel_reconnection(&self) {
        self.update(|state| state.reconnecting = false);
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
        let Ok(mut runtime) = self.lock() else {
            return;
        };
        // Receipt audits finish asynchronously. Never let a verdict from a
        // stopped verifier generation mutate the current session's activity.
        if event.generation != runtime.generation {
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
            local_policy_applied: event.local_policy_applied,
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
        {
            let state = &mut runtime.state;
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
        }
        let state = runtime.state.clone();
        drop(runtime);
        self.state_tx.send_replace(state);
    }

    /// Refresh discovery without revoking the current verified session.
    pub async fn refresh_catalog(self: &Arc<Self>) -> Result<AppState, String> {
        let (generation, epoch) = {
            let runtime = self.lock()?;
            if !runtime.identity_ready {
                return Err("Start the gateway and wait for verification first".to_string());
            }
            (runtime.generation, runtime.epoch)
        };
        self.load_catalog(generation, epoch).await?;
        self.snapshot()
    }

    async fn load_catalog(self: &Arc<Self>, generation: u64, epoch: u64) -> Result<(), String> {
        let read = {
            let mut runtime = self.lock()?;
            if runtime.generation != generation || runtime.epoch != epoch || !runtime.identity_ready
            {
                return Ok(());
            }
            runtime.catalog_read += 1;
            runtime.catalog_read
        };
        let result = self.proxy.fetch_catalog(generation, epoch).await;
        let mut runtime = self.lock()?;
        if runtime.generation != generation
            || runtime.epoch != epoch
            || !runtime.identity_ready
            || runtime.catalog_read != read
        {
            return Ok(());
        }
        if runtime.service.is_none() {
            return Ok(());
        }
        let result = result.and_then(|mut catalog| {
            if let Some(endpoint) = runtime.state.remote_url.as_deref() {
                let inventory = match &self.inventory {
                    Some(updater) => updater.current(),
                    None => EndpointInventory::bundled()?,
                };
                catalog.apply_endpoint_inventory(endpoint, &inventory)?;
            }
            Ok(catalog)
        });
        let outcome = match result {
            Ok(catalog) => {
                runtime.state.status = "verified".to_string();
                runtime.state.reconnecting = false;
                runtime.state.progress = None;
                runtime.state.error = None;
                self.publish_catalog(&mut runtime, catalog);
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
                    // verifier behind a stopped-looking UI. Terminate it and
                    // land in a plain, retryable error state.
                    drop(runtime);
                    let _ = self.fail(generation, message.clone());
                    return Err(message);
                }
            }
        };
        drop(runtime);
        self.publish();
        if outcome.is_ok() {
            self.refresh_inventory(generation, epoch);
        }
        outcome
    }

    fn publish_catalog(&self, runtime: &mut RuntimeState, catalog: Catalog) {
        let summary = catalog_summary(&catalog, runtime.last_catalog.as_ref());
        runtime.last_catalog = Some(summary.clone());
        runtime.state.catalog = Some(summary);
        runtime.catalog = Some(catalog.clone());
        if !runtime.verification_only {
            runtime.state.protected_since.get_or_insert_with(now_secs);
            self.proxy.publish(Session {
                generation: runtime.generation,
                epoch: runtime.epoch,
                session_id: Some(runtime.session_id.clone()),
                service: runtime.service.clone(),
                verified: true,
                catalog: Some(catalog),
            });
        }
    }

    fn refresh_inventory(self: &Arc<Self>, generation: u64, epoch: u64) {
        let Some(updater) = self.inventory.clone() else {
            return;
        };
        let eligible = self.lock().is_ok_and(|runtime| {
            runtime
                .state
                .remote_url
                .as_deref()
                .is_some_and(Catalog::has_endpoint_inventory)
        });
        if !eligible {
            return;
        }
        let manager = Arc::downgrade(self);
        self.task_runtime.spawn(async move {
            updater.refresh().await;
            let Some(manager) = manager.upgrade() else {
                return;
            };
            if let Err(error) = manager.apply_inventory(generation, epoch) {
                desktop_core::diagnostic!("Cannot apply model endpoint inventory: {error}");
            }
        });
    }

    fn apply_inventory(&self, generation: u64, epoch: u64) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation
            || runtime.epoch != epoch
            || !runtime.identity_ready
            || runtime.state.status != "verified"
        {
            return Ok(());
        }
        let (Some(mut catalog), Some(endpoint)) =
            (runtime.catalog.clone(), runtime.state.remote_url.as_deref())
        else {
            return Ok(());
        };
        let Some(updater) = &self.inventory else {
            return Ok(());
        };
        let previous = catalog.revision.clone();
        catalog.apply_endpoint_inventory(endpoint, &updater.current())?;
        if catalog.revision == previous {
            return Ok(());
        }
        self.publish_catalog(&mut runtime, catalog);
        drop(runtime);
        self.publish();
        Ok(())
    }

    fn update(&self, change: impl FnOnce(&mut AppState)) {
        let Ok(mut runtime) = self.lock() else {
            return;
        };
        change(&mut runtime.state);
        drop(runtime);
        self.publish();
    }

    fn publish(&self) {
        if let Ok(runtime) = self.lock() {
            self.state_tx.send_replace(runtime.state.clone());
        }
    }

    fn terminated(&self, generation: u64, error: Option<String>) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        runtime.task = None;
        runtime.service = None;
        runtime.epoch += 1;
        runtime.identity_ready = false;
        runtime.verification_only = false;
        runtime.state.catalog = None;
        runtime.state.progress = None;
        if !matches!(runtime.state.status.as_str(), "error" | "blocked") {
            runtime.state.status = "error".to_string();
            runtime.state.error = Some(if let Some(error) = error {
                format!("Verifier task stopped unexpectedly: {error}")
            } else {
                "Verifier stopped unexpectedly".to_string()
            });
        }
        runtime.state.reconnecting = crate::recovery::connection_intended(&runtime.state);
        let epoch = runtime.epoch;
        let session_id = runtime.session_id.clone();
        drop(runtime);
        self.proxy.publish(Session {
            generation,
            epoch,
            session_id: Some(session_id),
            ..Session::default()
        });
        self.publish();
        Ok(())
    }

    fn fail(&self, generation: u64, message: String) -> Result<(), String> {
        self.fail_if(generation, None, message)
    }

    fn fail_if(
        &self,
        generation: u64,
        status: Option<&str>,
        message: String,
    ) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation
            || status.is_some_and(|status| runtime.state.status != status)
        {
            return Ok(());
        }
        let task = runtime.task.take();
        runtime.service = None;
        runtime.epoch += 1;
        runtime.identity_ready = false;
        runtime.verification_only = false;
        if runtime.state.status != "blocked" {
            runtime.state.status = "error".to_string();
        }
        runtime.state.reconnecting = crate::recovery::connection_intended(&runtime.state);
        runtime.state.progress = None;
        runtime.state.catalog = None;
        runtime.state.error = Some(message);
        let epoch = runtime.epoch;
        let session_id = runtime.session_id.clone();
        drop(runtime);
        self.proxy.publish(Session {
            generation,
            epoch,
            session_id: Some(session_id),
            ..Session::default()
        });
        if let Some(mut task) = task {
            let _ = task.stop();
        }
        self.publish();
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, RuntimeState>, String> {
        self.inner
            .lock()
            .map_err(|_| "Gateway runtime state is unavailable".to_string())
    }
}

/// Summarizes a verified catalog for clients, carrying forward removed ids.
fn catalog_summary(catalog: &Catalog, previous: Option<&CatalogSummary>) -> CatalogSummary {
    let models: Vec<ModelSummary> = catalog
        .models
        .iter()
        .map(|model| ModelSummary {
            id: model.id().to_string(),
            name: model.display_name().to_string(),
            supported_endpoints: model.supported_surfaces.as_ref().map(|surfaces| {
                surfaces
                    .iter()
                    .map(|surface| surface.path().to_string())
                    .collect()
            }),
            context_length: model.remote.context_length,
            max_output_length: model.remote.max_output_length,
            is_tee: model.bool_field("is_tee"),
            input_price_per_million: model.price_per_million("prompt"),
            output_price_per_million: model.price_per_million("completion"),
            cache_read_price_per_million: model.price_per_million("input_cache_read"),
            cache_write_price_per_million: model.price_per_million("input_cache_write"),
            input_modalities: model.string_array("input_modalities"),
            output_modalities: model.string_array("output_modalities"),
            capabilities: model.string_array("supported_features"),
            description: model.string_field("description"),
        })
        .collect();
    // Carry forward ids that disappeared until the service lists them
    // again, so a removed model is never quietly forgotten.
    let removed: BTreeSet<String> = previous
        .into_iter()
        .flat_map(|previous| {
            previous
                .models
                .iter()
                .map(|model| model.id.clone())
                .chain(previous.removed.iter().cloned())
        })
        .filter(|id| catalog.get(id).is_none())
        .collect();
    CatalogSummary {
        revision: catalog.revision.clone(),
        fetched_at: catalog.fetched_at,
        models,
        removed: removed.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests;
