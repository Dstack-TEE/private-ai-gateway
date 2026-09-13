mod accounts;
mod agents;
mod credentials;
mod endpoint;
mod lifecycle;
mod profiles;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use desktop_gateway::{
    agents::{app_data_dir, Agent, Projector},
    catalog::Catalog,
    lock,
    proxy::{self, ProxyEvent, ProxyState},
    secrets::{validate_api_key, KeyringStore, SecretStore, LEGACY_API_KEY_ENTRY},
    tokens::{TokenFiles, TokenSet, LOCAL_TOOLS_AGENT},
};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{
    contracts::{
        AgentPreview, AgentStatus, ConfidentialProfileInput, ConnectOptions, GatewayState,
        LocalApiConfig, RequestActivity, ServiceProvider, StartGatewayConfig,
    },
    gateway::{GatewayManager, SidecarLauncher},
    local_api::{self, ResolvedLocalApi},
    service_config,
    usage::{UsagePage, UsageQuery, UsageStore},
};

pub struct RuntimeOptions {
    pub launcher: Arc<dyn SidecarLauncher>,
    pub helper_path: PathBuf,
    pub task_runtime: Handle,
}

pub struct DesktopRuntime {
    balances: crate::balance_cache::BalanceCache,
    account_login: tokio::sync::Mutex<Option<crate::account_login::PendingLogin>>,
    account_save: Mutex<
        Option<(
            String,
            tokio::sync::watch::Receiver<crate::contracts::AccountSaveResult>,
        )>,
    >,
    manager: Arc<GatewayManager>,
    proxy: Arc<ProxyState>,
    usage: Arc<UsageStore>,
    secrets: Arc<dyn SecretStore>,
    credentials: ClientCredentials,
    legacy_credential_pending: Mutex<bool>,
    endpoint: EndpointRuntime,
    codex_sync: CodexCatalogSync,
    agent_policy: Mutex<()>,
    lifecycle: tokio::sync::Mutex<()>,
    exiting: AtomicBool,
    recovery: crate::recovery::Recovery,
    helper_path: PathBuf,
    instance: Option<lock::InstanceLock>,
}

struct SavedConfiguration<'a> {
    config: StartGatewayConfig,
    reconnect: bool,
    // Keep mutations serialized until the post-save restart has completed.
    _operation: tokio::sync::MutexGuard<'a, ()>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RetiredCredential {
    profile_id: String,
    action: String,
    provider: crate::contracts::ServiceProvider,
    key: String,
    entry: String,
    revoke: bool,
}

struct ClientCredentials(Mutex<ClientCredentialState>);

struct ClientCredentialState {
    files: TokenFiles,
    rotation_failed: bool,
}

impl ClientCredentials {
    fn new() -> Result<Self, String> {
        Ok(Self::from_files(TokenFiles::new(&app_data_dir()?)))
    }

    fn from_files(files: TokenFiles) -> Self {
        Self(Mutex::new(ClientCredentialState {
            files,
            rotation_failed: false,
        }))
    }

    fn token(&self) -> Result<String, String> {
        self.active_token()?.ok_or_else(|| {
            "Client key rotation failed; generate a new client key before using the Local API"
                .to_string()
        })
    }

    fn active_token(&self) -> Result<Option<String>, String> {
        let state = self
            .0
            .lock()
            .map_err(|_| "Client credential store unavailable".to_string())?;
        if state.rotation_failed {
            return Ok(None);
        }
        state.files.ensure(LOCAL_TOOLS_AGENT).map(Some)
    }

    fn rotate(&self) -> Result<String, String> {
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
        manager: Arc<GatewayManager>,
        proxy: Arc<ProxyState>,
        listener: std::net::TcpListener,
        config: LocalApiConfig,
    ) -> Result<(), String> {
        let mut runtime = self
            .task
            .lock()
            .map_err(|_| "The Local API runtime is unavailable".to_string())?;
        if runtime.as_ref().is_some_and(|task| !task.is_finished()) {
            return Err("The Local API runtime is already active".to_string());
        }
        *runtime = Some(self.task_runtime.spawn(async move {
            if let Err(error) = proxy::serve(proxy, listener).await {
                manager.set_endpoint(config, Err(error));
            }
        }));
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
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

#[derive(Clone)]
struct CodexCatalogSyncAttempt {
    revision: String,
    error: Option<String>,
}

#[derive(Default)]
struct CodexCatalogSync(Mutex<Option<CodexCatalogSyncAttempt>>);

impl CodexCatalogSync {
    fn reset(&self) -> Result<(), String> {
        *self
            .0
            .lock()
            .map_err(|_| "The Codex model metadata state is unavailable".to_string())? = None;
        Ok(())
    }

    fn remember_success(&self, revision: &str) -> Result<(), String> {
        *self
            .0
            .lock()
            .map_err(|_| "The Codex model metadata state is unavailable".to_string())? =
            Some(CodexCatalogSyncAttempt {
                revision: revision.to_string(),
                error: None,
            });
        Ok(())
    }

    fn refresh_error(&self, projector: &Projector, catalog: &Catalog) -> Option<String> {
        let mut previous = match self.0.lock() {
            Ok(previous) => previous,
            Err(_) => return Some("The Codex model metadata state is unavailable".to_string()),
        };
        if let Some(attempt) = previous.as_ref() {
            if attempt.revision == catalog.revision {
                return attempt.error.clone();
            }
        }
        let error = projector.sync_codex_catalog(catalog).err();
        *previous = Some(CodexCatalogSyncAttempt {
            revision: catalog.revision.clone(),
            error: error.clone(),
        });
        error
    }
}

impl DesktopRuntime {
    pub fn launch(options: RuntimeOptions) -> Result<Arc<Self>, String> {
        if !options.helper_path.is_absolute() {
            return Err("The credential helper path must be absolute".to_string());
        }
        // Establish ownership before settings migration, storage, or listeners.
        let data_dir = app_data_dir()?;
        let instance = lock::instance(&data_dir)
            .map_err(|error| format!("Cannot take the instance lock: {error}"))?
            .ok_or_else(|| "Another Private AI Proxy instance is already running".to_string())?;
        #[cfg(unix)]
        if let Err(error) = crate::helper_staging::stage(&options.helper_path, &data_dir) {
            // OpenClaw independently rejects an unavailable or mismatched staged copy.
            eprintln!("Cannot stage the credential helper: {error}");
        }
        let secrets: Arc<dyn SecretStore> = Arc::new(KeyringStore);
        let (mut settings, mut settings_error, migrated_legacy) = match service_config::load() {
            Ok(loaded) => (loaded.settings, None, loaded.migrated_legacy),
            Err(error) => (
                service_config::ServiceSettings::default(),
                Some(error),
                false,
            ),
        };
        let runtime_config = match settings.runtime_config() {
            Ok(config) => config,
            Err(error) => {
                settings_error = Some(error);
                settings = service_config::ServiceSettings::default();
                settings.runtime_config().map_err(|error| {
                    format!("The built-in Confidential AI profile is invalid: {error}")
                })?
            }
        };
        let credential_saved = migrated_legacy
            || settings
                .active_profile()
                .is_ok_and(service_config::profile_has_credential);

        let (local, local_error) = match local_api::load() {
            Ok(config) => (config, None),
            Err(error) => (
                local_api::resolve(LocalApiConfig::default()).map_err(|fallback| {
                    format!("The built-in Local API settings are invalid: {fallback}")
                })?,
                Some(error),
            ),
        };
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
                    return Err(format!(
                        "Cannot initialize usage storage: {error}; {fallback}"
                    ));
                }
            },
        };
        let task_runtime = options.task_runtime;
        let initial_state = GatewayState {
            local_api: local.config.clone(),
            config: runtime_config,
            profiles: settings.profiles.clone(),
            active_profile_id: settings.active_profile_id.clone(),
            ..GatewayState::default()
        };
        let manager = Arc::new(
            GatewayManager::new(
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
            secrets,
            credentials: ClientCredentials::new()?,
            account_login: tokio::sync::Mutex::new(None),
            account_save: Mutex::new(None),
            balances: crate::balance_cache::BalanceCache::default(),
            legacy_credential_pending: Mutex::new(migrated_legacy),
            endpoint: EndpointRuntime::new(task_runtime.clone()),
            codex_sync: CodexCatalogSync::default(),
            agent_policy: Mutex::new(()),
            lifecycle: tokio::sync::Mutex::new(()),
            exiting: AtomicBool::new(false),
            recovery: crate::recovery::Recovery::default(),
            helper_path: options.helper_path,
            instance: Some(instance),
        });

        match (listener, launch_error) {
            (Some(listener), _) => {
                manager.set_endpoint(local.config.clone(), Ok(local.endpoint.clone()));
                runtime.endpoint.start(
                    manager.clone(),
                    proxy.clone(),
                    listener,
                    local.config.clone(),
                )?;
            }
            (None, Some(error)) => manager.set_endpoint(local.config.clone(), Err(error)),
            (None, None) => manager.set_endpoint(
                local.config.clone(),
                Err("The Local API listener was not created".to_string()),
            ),
        }
        // Opening the app must not touch the OS credential store. The active
        // key is loaded only when verification or protection actually uses it.
        manager.set_api_key_saved(credential_saved);
        proxy.set_api_key(None);
        for error in [usage_error, local_error, settings_error]
            .into_iter()
            .flatten()
        {
            manager.report_error(error);
        }
        if runtime.instance.is_some() {
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
                        let policy = (protection_active(&state), state.catalog.as_ref().map(|catalog| catalog.revision.clone()));
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
                            .is_ok_and(|state| state.error.as_deref() != Some(&error))
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
                if app_data_dir().is_ok_and(|path| path.join("account-cleanup.pending").exists()) {
                    if let Ok(_operation) = runtime.lifecycle.try_lock() {
                        if let Err(error) = runtime.cleanup_retired().await {
                            runtime.manager.report_error(error);
                        }
                    }
                }
            }
        });
        Ok(runtime)
    }

    pub fn subscribe(&self) -> watch::Receiver<GatewayState> {
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

    pub fn state(&self) -> Result<GatewayState, String> {
        self.manager.snapshot()
    }

    pub fn report_error(&self, message: String) {
        self.manager.report_error(message);
    }

    pub fn query_usage(&self, query: UsageQuery) -> Result<UsagePage, String> {
        self.usage.page(&query)
    }

    pub fn export_profiles(&self, path: PathBuf) -> Result<(), String> {
        let backup = crate::maintenance::ProfileBackup::from_profiles(&self.state()?.profiles);
        crate::maintenance::write_json(&path, &backup)
    }

    pub fn export_diagnostics(&self, path: PathBuf, version: &str) -> Result<(), String> {
        crate::maintenance::write_json(
            &path,
            &crate::maintenance::diagnostics(&self.state()?, version),
        )
    }

    pub fn usage_record(&self, record_id: &str) -> Result<Option<RequestActivity>, String> {
        self.usage.get(record_id)
    }

    pub fn export_usage_csv(&self, query: UsageQuery, path: PathBuf) -> Result<usize, String> {
        self.usage.export_csv(&query, &path)
    }

    pub fn clear_usage(&self) -> Result<u64, String> {
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

fn protection_active(state: &GatewayState) -> bool {
    state.status == "verified"
        && !state.configuration_verification
        && state.api_key_saved
        && state.endpoint_error.is_none()
}

fn with_client_token(
    mut tokens: TokenSet,
    credentials: &ClientCredentials,
) -> Result<TokenSet, String> {
    if let Some(token) = credentials.active_token()? {
        tokens.insert(token, LOCAL_TOOLS_AGENT.to_string());
    }
    Ok(tokens)
}

fn restore_secret_entry(
    secrets: &dyn SecretStore,
    entry: &str,
    value: Option<&str>,
) -> Result<(), String> {
    match value {
        Some(value) => secrets.set(entry, value),
        None => secrets.delete(entry),
    }
}

#[cfg(test)]
mod tests;
