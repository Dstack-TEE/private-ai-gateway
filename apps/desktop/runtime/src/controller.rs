mod accounts;
mod agents;
mod credentials;
mod endpoint;
mod lifecycle;
mod profiles;
mod settings_files;
mod web_ui;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use agent_bridge::{
    agents::Projector,
    catalog::Catalog,
    proxy::{self, ProxyEvent, ProxyState},
    tokens::{TokenFiles, TokenSet, LOCAL_TOOLS_AGENT},
};
use desktop_core::{
    agents::Agent,
    config::{self as settings_config, Config},
    contracts::{
        AgentPreview, AgentStatus, AppState, ConfidentialProfile, ConfidentialProfileInput,
        ConnectOptions, ListenConfig, ProfileAuth, RequestActivity, ServiceProvider, StartConfig,
        VerificationStatus,
    },
    listen::ResolvedListen,
    lock,
    paths::{app_data_dir, config_dir, legacy_config_dir},
    usage::{UsagePage, UsageQuery},
};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{
    local_state::{LocalState, RetiredCredential},
    settings::{Credentials, Settings},
    usage::UsageStore,
    verifier_session::{SessionManager, VerifierLauncher},
    Error,
};

const USAGE_DATABASE: &str = "usage.sqlite3";

pub struct RuntimeOptions {
    pub launcher: Arc<dyn VerifierLauncher>,
    pub helper_path: PathBuf,
    pub task_runtime: Handle,
    pub agent_configuration: bool,
    pub agent_access_error: Option<String>,
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    pub agent_home: Option<PathBuf>,
}

pub struct DesktopRuntime {
    balances: crate::balance_cache::BalanceCache,
    account_login: tokio::sync::Mutex<Option<crate::account_login::PendingLogin>>,
    account_save: Mutex<
        Option<(
            String,
            tokio::sync::watch::Receiver<desktop_core::contracts::AccountSaveResult>,
        )>,
    >,
    manager: Arc<SessionManager>,
    proxy: Arc<ProxyState>,
    usage: Arc<UsageStore>,
    settings: Arc<Settings>,
    settings_watcher: Mutex<Option<crate::settings::Watcher>>,
    local_state: Arc<LocalState>,
    data_dir: PathBuf,
    credentials: ClientCredentials,
    endpoint: EndpointRuntime,
    agent_policy: Mutex<()>,
    /// The agents the last scan reported; see `report_agents`.
    reported_agents: Mutex<Vec<AgentStatus>>,
    /// The catalog of the last verified session, for re-projecting agents
    /// when the Local API address changes while protection is not verified.
    verified_catalog: Mutex<Option<Catalog>>,
    lifecycle: tokio::sync::Mutex<()>,
    exiting: AtomicBool,
    recovery: crate::recovery::Recovery,
    helper_path: PathBuf,
    agent_configuration: bool,
    agent_access_error: Option<String>,
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    agent_home: Option<PathBuf>,
    instance: Option<lock::InstanceLock>,
    web_ui: crate::web_ui::WebUi,
    admission: Arc<crate::server::Admission>,
}

struct SavedConfiguration<'a> {
    config: StartConfig,
    reconnect: bool,
    // Keep mutations serialized until the post-save restart has completed.
    _operation: tokio::sync::MutexGuard<'a, ()>,
}

struct ClientCredentials(Mutex<ClientCredentialState>);

struct ClientCredentialState {
    files: TokenFiles,
    rotation_failed: bool,
}

impl ClientCredentials {
    fn new() -> Result<Self, Error> {
        Ok(Self::from_files(TokenFiles::new(&app_data_dir()?)))
    }

    fn from_files(files: TokenFiles) -> Self {
        Self(Mutex::new(ClientCredentialState {
            files,
            rotation_failed: false,
        }))
    }

    fn lock(&self) -> Result<MutexGuard<'_, ClientCredentialState>, Error> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "Client credential store unavailable")?)
    }

    fn token(&self) -> Result<String, Error> {
        Ok(self.active_token()?.ok_or(
            "Client key rotation failed; generate a new client key before using the Local API",
        )?)
    }

    fn active_token(&self) -> Result<Option<String>, Error> {
        let state = self.lock()?;
        if state.rotation_failed {
            return Ok(None);
        }
        Ok(state.files.ensure(LOCAL_TOOLS_AGENT).map(Some)?)
    }

    /// `tokens` with the client key the Local API accepts from local tools.
    fn with_token(&self, mut tokens: TokenSet) -> Result<TokenSet, Error> {
        if let Some(token) = self.active_token()? {
            tokens.insert(token, LOCAL_TOOLS_AGENT.to_string());
        }
        Ok(tokens)
    }

    fn rotate(&self) -> Result<String, Error> {
        let mut state = self.lock()?;
        state.rotation_failed = true;
        let token = state.files.rotate(LOCAL_TOOLS_AGENT)?;
        state.rotation_failed = false;
        Ok(token)
    }
}

struct EndpointRuntime {
    task_runtime: Handle,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl EndpointRuntime {
    fn new(task_runtime: Handle) -> Self {
        Self {
            task_runtime,
            task: Mutex::new(None),
        }
    }

    fn start(
        &self,
        manager: Arc<SessionManager>,
        proxy: Arc<ProxyState>,
        listener: std::net::TcpListener,
        config: ListenConfig,
    ) -> Result<(), Error> {
        let mut runtime = self
            .task
            .lock()
            .map_err(|_| "The Local API runtime is unavailable".to_string())?;
        if runtime.as_ref().is_some_and(|task| !task.is_finished()) {
            return Err("The Local API runtime is already active".into());
        }
        *runtime = Some(self.task_runtime.spawn(async move {
            if let Err(error) = proxy::serve(proxy, listener).await {
                manager.set_endpoint(config, Err(error));
            }
        }));
        Ok(())
    }

    async fn stop(&self) -> Result<(), Error> {
        let previous = self
            .task
            .lock()
            .map_err(|_| "The Local API runtime is unavailable".to_string())?
            .take();
        if let Some(previous) = previous {
            previous.abort();
            let _ = previous.await;
        }
        Ok(())
    }
}

/// Why the backend did not launch.
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum LaunchError {
    /// Another backend owns the instance lock.
    #[error("Another Private AI Proxy instance is already running. Stop the existing private-ai-proxy-service process before retrying.")]
    AlreadyRunning,
    #[error("{0}")]
    Failed(String),
}

impl From<String> for LaunchError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for LaunchError {
    fn from(message: &str) -> Self {
        Self::Failed(message.into())
    }
}

impl DesktopRuntime {
    pub fn launch(options: RuntimeOptions) -> Result<Arc<Self>, LaunchError> {
        let RuntimeOptions {
            launcher,
            helper_path,
            task_runtime,
            agent_configuration,
            agent_access_error,
            #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
            agent_home,
        } = options;
        if !helper_path.is_absolute() {
            return Err("The credential helper path must be absolute".into());
        }
        // Establish ownership before settings, storage, or listeners.
        let data_dir = app_data_dir()?;
        let instance = lock::instance(&data_dir)
            .map_err(|error| format!("Cannot take the instance lock: {error}"))?
            .ok_or(LaunchError::AlreadyRunning)?;
        #[cfg(all(unix, not(all(target_os = "macos", feature = "mac-app-store"))))]
        if agent_configuration {
            if let Err(error) = crate::helper_staging::stage(&helper_path, &data_dir) {
                // OpenClaw independently rejects an unavailable or mismatched staged copy.
                tracing::warn!("Cannot stage the credential helper: {error}");
            }
        }
        let (settings_dir, relocation_notices) = match legacy_config_dir() {
            Some(legacy) => desktop_core::relocation::relocate(&legacy, &config_dir()?, &data_dir),
            None => (config_dir()?, Vec::new()),
        };
        let (settings, mut settings_problems) = Settings::open(settings_dir, &data_dir);
        settings.add_import_notices(&relocation_notices);
        settings_problems.extend(relocation_notices);
        let settings = Arc::new(settings);
        let local_state = Arc::new(LocalState::open(&data_dir));
        settings_problems.extend(local_state.read().err());
        // Until the 0.1 credential store import (run once the service is
        // listening) is recorded, missing agent restore values may still be there.
        local_state.set_importing(crate::settings::legacy::secrets_pending(&data_dir));
        let snapshot = settings.snapshot()?;
        let runtime_config = snapshot.config.runtime_config();
        let profiles = settings.profile_views(&snapshot);
        let credential_saved =
            crate::verifier_session::active_key_saved(&profiles, &snapshot.config.active_profile);
        let local = settings_config::resolve_local_api(snapshot.config.local_api.clone())
            .map_err(|error| format!("The Local API settings are invalid: {error}"))?;
        let listener = proxy::bind_std(local.bind);
        let (proxy_events_tx, proxy_events) = tokio::sync::mpsc::channel::<ProxyEvent>(256);
        let proxy = ProxyState::new(proxy_events_tx)?;
        let (usage, usage_error) = match UsageStore::open(data_dir.join(USAGE_DATABASE)) {
            Ok(store) => (store, None),
            Err(error) => (
                UsageStore::memory().map_err(|fallback| {
                    format!("Cannot initialize usage storage: {error}; {fallback}")
                })?,
                Some(format!(
                    "Usage history is unavailable for this launch: {error}"
                )),
            ),
        };
        let usage = Arc::new(usage);
        let initial_state = AppState {
            local_api: local.config.clone(),
            config: runtime_config,
            profiles,
            active_profile_id: snapshot.config.active_profile.clone(),
            config_files: settings.files(),
            ..AppState::default()
        };
        let manager = Arc::new(
            SessionManager::new(
                proxy.clone(),
                usage.clone(),
                launcher,
                task_runtime.clone(),
                initial_state,
            )
            .with_endpoint_inventory(crate::endpoint_inventory::InventoryUpdater::new(
                data_dir.join("model-endpoints-cache.json"),
            )?),
        );
        let runtime = Arc::new(Self {
            manager: manager.clone(),
            proxy: proxy.clone(),
            usage,
            settings,
            settings_watcher: Mutex::new(None),
            local_state,
            data_dir,
            credentials: ClientCredentials::new().map_err(|error| error.to_string())?,
            account_login: tokio::sync::Mutex::new(None),
            account_save: Mutex::new(None),
            balances: crate::balance_cache::BalanceCache::default(),
            endpoint: EndpointRuntime::new(task_runtime.clone()),
            agent_policy: Mutex::new(()),
            reported_agents: Mutex::new(Vec::new()),
            verified_catalog: Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
            exiting: AtomicBool::new(false),
            recovery: crate::recovery::Recovery::default(),
            helper_path,
            agent_configuration,
            agent_access_error,
            #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
            agent_home,
            instance: Some(instance),
            web_ui: crate::web_ui::WebUi::new(task_runtime.clone()),
            admission: Arc::default(),
        });

        match listener {
            Ok(listener) => {
                manager.set_endpoint(local.config.clone(), Ok(local.endpoint.clone()));
                runtime
                    .endpoint
                    .start(
                        manager.clone(),
                        proxy.clone(),
                        listener,
                        local.config.clone(),
                    )
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => manager.set_endpoint(local.config.clone(), Err(error)),
        }
        // The active key is loaded only when verification or protection uses it.
        manager.set_api_key_saved(credential_saved);
        proxy.set_api_key(None);
        for error in usage_error.into_iter().chain(settings_problems) {
            manager.report_error(error);
        }
        runtime
            .web_ui
            .set_password(snapshot.credentials.web_ui.secret());
        runtime.open_web_ui(&snapshot.config.web_ui);
        match crate::settings::spawn_watcher(
            &runtime.settings,
            Arc::downgrade(&runtime),
            &task_runtime,
        ) {
            Ok(watcher) => {
                if let Ok(mut slot) = runtime.settings_watcher.lock() {
                    *slot = Some(watcher);
                }
            }
            Err(error) => runtime.report_error(error),
        }
        runtime.initialize_startup_tokens();
        if let Err(error) = runtime.recovery.start() {
            runtime.report_error(error);
        }
        runtime.spawn_background_tasks(&task_runtime, proxy_events);
        Ok(runtime)
    }

    /// What keeps the backend current while it lives: network recovery and
    /// agent reconciliation, request activity, and the retry of queued
    /// account key cleanups.
    fn spawn_background_tasks(
        self: &Arc<Self>,
        task_runtime: &Handle,
        mut proxy_events: tokio::sync::mpsc::Receiver<ProxyEvent>,
    ) {
        let weak = Arc::downgrade(self);
        let mut states = self.subscribe();
        let network_changed = self.recovery.changed.clone();
        task_runtime.spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut previous = None;
            loop {
                tokio::select! {
                    _ = network_changed.notified() => {},
                    result = states.changed() => {
                        if result.is_err() { break; }
                        let state = states.borrow();
                        let policy = (state.is_protected(), state.catalog.as_ref().map(|catalog| catalog.revision.clone()));
                        if previous.as_ref() == Some(&policy) { continue; }
                        previous = Some(policy);
                    },
                    _ = interval.tick() => {},
                }
                let Some(runtime) = weak.upgrade() else { break };
                let result = tokio::task::spawn_blocking(move || {
                    if let Err(error) = runtime.recover_network() { runtime.report_error(error); }
                    if let Err(error) = runtime.reconcile_agents() { runtime.report_once(error); }
                })
                .await;
                if result.is_err() {
                    if let Some(runtime) = weak.upgrade() {
                        runtime.report_error("Agent reconciliation could not complete; stop protection and retry".to_string());
                    }
                }
            }
        });

        let events_runtime = self.clone();
        task_runtime.spawn(async move {
            while let Some(event) = proxy_events.recv().await {
                events_runtime.manager.record_proxy_event(event);
            }
        });
        let weak = Arc::downgrade(self);
        task_runtime.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let Some(runtime) = weak.upgrade() else {
                    break;
                };
                if runtime.closing() {
                    break;
                }
                if runtime
                    .local_state
                    .read()
                    .is_ok_and(|state| !state.account_cleanup.is_empty())
                {
                    if let Ok(_operation) = runtime.lifecycle.try_lock() {
                        if let Err(error) = runtime.cleanup_retired().await {
                            runtime.report_error(error);
                        }
                    }
                }
            }
        });
    }

    pub fn subscribe(&self) -> watch::Receiver<AppState> {
        self.manager.subscribe()
    }

    pub fn system_resumed(&self) {
        self.recovery.request();
    }
    pub fn set_wake_monitor_available(&self, available: bool) -> bool {
        self.manager.set_wake_monitor_available(available)
    }

    pub fn instance_lock(&self) -> Result<&lock::InstanceLock, String> {
        self.instance
            .as_ref()
            .ok_or_else(|| "The backend does not own its instance lock".to_string())
    }

    pub fn state(&self) -> Result<AppState, Error> {
        Ok(self.manager.snapshot())
    }

    /// Whether the app is closing and accepts no further changes.
    fn closing(&self) -> bool {
        self.exiting.load(Ordering::Acquire)
    }

    /// Serializes changes of the agents' configuration and of the tokens the
    /// Local API accepts, so an older scan cannot restore revoked ones.
    fn lock_agents(&self) -> Result<MutexGuard<'_, ()>, Error> {
        Ok(self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?)
    }

    pub fn report_error(&self, error: impl std::fmt::Display) {
        self.manager.report_error(error.to_string());
    }

    /// Reports `error` unless the state shows it already, so a failure that
    /// repeats on every attempt publishes no new state.
    fn report_once(&self, error: Error) {
        if self.manager.snapshot().error != Some(error.to_string()) {
            self.report_error(error);
        }
    }

    pub fn query_usage(&self, query: UsageQuery) -> Result<UsagePage, Error> {
        Ok(self.usage.page(&query)?)
    }

    pub fn export_profiles(&self, path: PathBuf) -> Result<(), Error> {
        Ok(desktop_core::maintenance::write_export(
            &path,
            &self.export_profiles_content()?,
        )?)
    }

    pub fn export_profiles_content(&self) -> Result<String, Error> {
        let backup =
            desktop_core::maintenance::ProfileBackup::from_profiles(&self.state()?.profiles);
        Ok(desktop_core::maintenance::json_content(&backup)?)
    }

    pub fn export_diagnostics(&self, path: PathBuf, version: &str) -> Result<(), Error> {
        Ok(desktop_core::maintenance::write_export(
            &path,
            &self.export_diagnostics_content(version)?,
        )?)
    }

    pub fn export_diagnostics_content(&self, version: &str) -> Result<String, Error> {
        Ok(desktop_core::maintenance::json_content(
            &desktop_core::maintenance::diagnostics(&self.state()?, version),
        )?)
    }

    pub fn usage_record(&self, record_id: &str) -> Result<Option<RequestActivity>, Error> {
        Ok(self.usage.get(record_id)?)
    }

    pub fn usage_receipt(&self, record_id: &str) -> Result<Option<Option<String>>, Error> {
        Ok(self.usage.receipt(record_id)?)
    }

    pub fn export_usage_csv(&self, query: UsageQuery, path: PathBuf) -> Result<usize, Error> {
        Ok(self.usage.export_csv(&query, &path)?)
    }

    pub fn clear_usage(&self) -> Result<u64, Error> {
        let changed = self.usage.clear()?;
        self.manager.clear_session_usage();
        Ok(changed)
    }
}

/// Does without a backend what a stop that ends the session does: ends the
/// saved protection session, so the next backend start does not resume it,
/// and restores the agents' own configuration, suspending each connection
/// through the same projector, apply lock and restoration journal until
/// protection is started again. The instance lock is held throughout, so no
/// backend starts meanwhile. `None` while a backend runs: stop through it.
pub fn restore_agents_offline(helper_path: PathBuf) -> Result<Option<Vec<AgentStatus>>, String> {
    if cfg!(all(target_os = "macos", feature = "mac-app-store")) {
        return Err(
            "The Mac App Store build restores agents only through the app: choose Stop All and Quit"
                .into(),
        );
    }
    let data_dir = app_data_dir()?;
    // A backend that `service stop` just ended may hold it for a moment longer.
    let Some(_instance) = lock::instance_within(&data_dir, std::time::Duration::from_secs(2))
        .map_err(|error| format!("Cannot take the instance lock: {error}"))?
    else {
        return Ok(None);
    };
    // As in `SessionManager::stop_with_reconnect`, the session ends before the
    // agents are restored; without a usage database there is no session.
    let usage = data_dir.join(USAGE_DATABASE);
    let session = if usage.exists() {
        UsageStore::open(usage).and_then(|usage| usage.end_session())
    } else {
        Ok(())
    };
    let local_state = LocalState::open(&data_dir);
    local_state.set_importing(crate::settings::legacy::secrets_pending(&data_dir));
    // Restoring never depends on the Local API endpoint.
    let projector = Projector::new(helper_path, "", Arc::new(local_state))?;
    all_applied(projector.reconcile(None)?)?;
    session?;
    Ok(Some(projector.scan(None)?.0))
}

/// Succeeds when no agent failed; otherwise names each failure.
fn all_applied(failures: Vec<(String, String)>) -> Result<(), String> {
    if failures.is_empty() {
        return Ok(());
    }
    Err(failures
        .into_iter()
        .map(|(agent, error)| format!("{agent}: {error}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// A saved profile, by ID.
fn find_profile<'a>(state: &'a AppState, id: &str) -> Option<&'a ConfidentialProfile> {
    state.profiles.iter().find(|profile| profile.id == id)
}

/// Whether a profile's key was issued for an account the app signed in to,
/// which the provider revokes when it is replaced or removed.
fn account_key(provider: ServiceProvider, auth: &ProfileAuth) -> bool {
    provider == ServiceProvider::Redpill && matches!(auth, ProfileAuth::OAuth { .. })
}

#[cfg(test)]
mod tests;
