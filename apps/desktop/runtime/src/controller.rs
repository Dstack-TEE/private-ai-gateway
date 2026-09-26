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
        Arc, Mutex,
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
        AgentPreview, AgentStatus, AppState, ConfidentialProfileInput, ConnectOptions,
        ListenConfig, RequestActivity, ServiceProvider, StartConfig, VerificationStatus,
    },
    listen::ResolvedListen,
    lock,
    paths::{app_data_dir, config_dir},
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

    fn token(&self) -> Result<String, Error> {
        Ok(self.active_token()?.ok_or_else(|| {
            "Client key rotation failed; generate a new client key before using the Local API"
                .to_string()
        })?)
    }

    fn active_token(&self) -> Result<Option<String>, Error> {
        let state = self
            .0
            .lock()
            .map_err(|_| "Client credential store unavailable".to_string())?;
        if state.rotation_failed {
            return Ok(None);
        }
        Ok(state.files.ensure(LOCAL_TOOLS_AGENT).map(Some)?)
    }

    fn rotate(&self) -> Result<String, Error> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "Client credential store unavailable".to_string())?;
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
#[derive(Debug, PartialEq)]
pub enum LaunchError {
    /// Another backend owns the instance lock.
    AlreadyRunning,
    Failed(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyRunning => "Another Private AI Proxy instance is already running. Stop the existing private-ai-proxy-service process before retrying.",
            Self::Failed(message) => message,
        })
    }
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
        if !options.helper_path.is_absolute() {
            return Err("The credential helper path must be absolute".into());
        }
        // Establish ownership before settings, storage, or listeners.
        let data_dir = app_data_dir()?;
        let instance = lock::instance(&data_dir)
            .map_err(|error| format!("Cannot take the instance lock: {error}"))?
            .ok_or(LaunchError::AlreadyRunning)?;
        let agent_configuration = options.agent_configuration;
        let agent_access_error = options.agent_access_error;
        #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
        let agent_home = options.agent_home;
        let helper_path = options.helper_path;
        #[cfg(all(unix, not(all(target_os = "macos", feature = "mac-app-store"))))]
        if agent_configuration {
            if let Err(error) = crate::helper_staging::stage(&helper_path, &data_dir) {
                // OpenClaw independently rejects an unavailable or mismatched staged copy.
                tracing::warn!("Cannot stage the credential helper: {error}");
            }
        }
        let (settings, mut settings_problems) = Settings::open(config_dir()?, &data_dir);
        let settings = Arc::new(settings);
        let local_state = Arc::new(LocalState::open(&data_dir));
        settings_problems.extend(local_state.read().err());
        // Until the 0.1 credential store import (run once the service is
        // listening) is recorded, missing agent restore values may still be there.
        local_state.set_importing(crate::settings::legacy::secrets_pending(&data_dir));
        let snapshot = settings.snapshot()?;
        let runtime_config = snapshot.config.runtime_config();
        let profiles = settings.profile_views(&snapshot);
        let credential_saved = profiles.iter().any(|profile| {
            profile.id == snapshot.config.active_profile && profile.credential_saved
        });
        let local = settings_config::resolve_local_api(snapshot.config.local_api.clone())
            .map_err(|error| format!("The Local API settings are invalid: {error}"))?;
        let (listener, launch_error) = match proxy::bind_std(local.bind) {
            Ok(listener) => (Some(listener), None),
            Err(error) => (None, Some(error)),
        };
        let (proxy_events_tx, mut proxy_events) = tokio::sync::mpsc::channel::<ProxyEvent>(256);
        let proxy = ProxyState::new(proxy_events_tx)?;
        let (usage, usage_error) = match UsageStore::open(data_dir.join("usage.sqlite3")) {
            Ok(store) => (Arc::new(store), None),
            Err(error) => match UsageStore::memory() {
                Ok(store) => (
                    Arc::new(store),
                    Some(format!(
                        "Usage history is unavailable for this launch: {error}"
                    )),
                ),
                Err(fallback) => {
                    return Err(
                        format!("Cannot initialize usage storage: {error}; {fallback}").into(),
                    );
                }
            },
        };
        let task_runtime = options.task_runtime;
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
                options.launcher,
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

        match (listener, launch_error) {
            (Some(listener), _) => {
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
            (None, Some(error)) => manager.set_endpoint(local.config.clone(), Err(error)),
            (None, None) => manager.set_endpoint(
                local.config.clone(),
                Err("The Local API listener was not created".into()),
            ),
        }
        // The active key is loaded only when verification or protection uses it.
        manager.set_api_key_saved(credential_saved);
        proxy.set_api_key(None);
        for error in usage_error.into_iter().chain(settings_problems) {
            manager.report_error(error);
        }
        if runtime.instance.is_some() {
            runtime
                .web_ui
                .set_password(snapshot.credentials.web_ui.password_hash.clone());
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
        }

        let weak = Arc::downgrade(&runtime);
        let mut states = runtime.subscribe();
        let network_changed = runtime.recovery.changed.clone();
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
                    if let Err(error) = runtime.reconcile_agents() {
                        if runtime
                            .state()
                            .is_ok_and(|state| state.error != Some(error.to_string()))
                        {
                            runtime.report_error(error);
                        }
                    }
                })
                .await;
                if result.is_err() {
                    if let Some(runtime) = weak.upgrade() {
                        runtime.report_error("Agent reconciliation could not complete; stop protection and retry".to_string());
                    }
                }
            }
        });

        let events_runtime = runtime.clone();
        task_runtime.spawn(async move {
            while let Some(event) = proxy_events.recv().await {
                events_runtime.manager.record_proxy_event(event);
            }
        });
        let weak = Arc::downgrade(&runtime);
        task_runtime.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let Some(runtime) = weak.upgrade() else {
                    break;
                };
                if runtime.exiting.load(Ordering::Acquire) {
                    break;
                }
                if runtime
                    .local_state
                    .read()
                    .is_ok_and(|state| !state.account_cleanup.is_empty())
                {
                    if let Ok(_operation) = runtime.lifecycle.try_lock() {
                        if let Err(error) = runtime.cleanup_retired().await {
                            runtime.manager.report_error(error.to_string());
                        }
                    }
                }
            }
        });
        Ok(runtime)
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
        Ok(self.manager.snapshot()?)
    }

    pub fn report_error(&self, error: impl std::fmt::Display) {
        self.manager.report_error(error.to_string());
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

    pub fn usage_receipt(&self, record_id: &str) -> Result<Option<String>, Error> {
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

fn agent_failures(failures: Vec<(String, String)>) -> String {
    failures
        .into_iter()
        .map(|(agent, error)| format!("{agent}: {error}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn with_client_token(
    mut tokens: TokenSet,
    credentials: &ClientCredentials,
) -> Result<TokenSet, Error> {
    if let Some(token) = credentials.active_token()? {
        tokens.insert(token, LOCAL_TOOLS_AGENT.to_string());
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests;
