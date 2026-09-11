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
        LocalApiConfig, RequestActivity, StartGatewayConfig,
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
        if runtime.is_some() {
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
        let manager = Arc::new(GatewayManager::new(
            proxy.clone(),
            usage.clone(),
            options.launcher,
            task_runtime.clone(),
            initial_state,
        ));
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
                    if runtime.recovery.needs_check() {
                        if let Err(error) = runtime.recover_network() { runtime.report_error(error); }
                    }
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

    fn persist_profile_credential_saved(
        &self,
        profile_id: &str,
        saved: bool,
    ) -> Result<(), String> {
        let state = self.manager.snapshot()?;
        let mut settings = service_config::settings_from_state(
            state.profiles,
            state.active_profile_id,
            state.config.require_production_os,
        )?;
        if service_config::set_profile_credential_saved(&mut settings, profile_id, saved)? {
            service_config::save(settings)?;
        }
        self.manager.set_profile_credential_saved(profile_id, saved);
        Ok(())
    }

    fn load_profile_key(&self, profile_id: &str) -> Result<Option<String>, String> {
        let profile = self
            .manager
            .snapshot()?
            .profiles
            .into_iter()
            .find(|p| p.id == profile_id)
            .ok_or("Profile not found")?;
        let entry = service_config::profile_credential_entry(&profile)?;
        let stored_key = self.secrets.get(&entry)?;

        let mut pending = self
            .legacy_credential_pending
            .lock()
            .map_err(|_| "The credential migration state is unavailable".to_string())?;
        if !*pending {
            self.persist_profile_credential_saved(profile_id, stored_key.is_some())?;
            return Ok(stored_key);
        }

        let had_stored_key = stored_key.is_some();
        let legacy_key = self.secrets.get(LEGACY_API_KEY_ENTRY)?;
        let key = stored_key.or(legacy_key.clone());
        let wrote_profile_key = !had_stored_key && legacy_key.is_some();
        if let (true, Some(key)) = (wrote_profile_key, key.as_deref()) {
            self.secrets.set(&entry, key)?;
        }
        if let Err(error) = self.persist_profile_credential_saved(profile_id, key.is_some()) {
            if wrote_profile_key {
                let _ = self.secrets.delete(&entry);
            }
            return Err(format!(
                "The previous Confidential AI credential could not be migrated: {error}"
            ));
        }
        if legacy_key.is_some() {
            if let Err(error) = self.secrets.delete(LEGACY_API_KEY_ENTRY) {
                return Err(format!(
                    "The previous Confidential AI credential was migrated, but its old copy could not be removed: {error}"
                ));
            }
        }
        *pending = false;
        Ok(key)
    }

    fn configuration_change(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
        let operation = self
            .lifecycle
            .try_lock()
            .map_err(|_| "A configuration change is in progress")?;
        if self.exiting.load(Ordering::Acquire) {
            return Err("The app is closing".to_string());
        }
        Ok(operation)
    }

    fn recover_network(self: &Arc<Self>) -> Result<(), String> {
        let Ok(_operation) = self.lifecycle.try_lock() else {
            return Ok(());
        };
        if self.exiting.load(Ordering::Acquire) {
            return Ok(());
        }
        let state = self.manager.snapshot()?;
        if state.status == "verifying" && self.recovery.online() {
            return Ok(());
        }
        self.recovery.clear_request();
        if !crate::recovery::should_recover(
            &state.status,
            state.configuration_verification,
            self.recovery.pending(),
        ) {
            return Ok(());
        }
        let remote = url::Url::parse(&state.config.remote_url)
            .map_err(|_| "The active service URL is invalid")?;
        let local_service = match remote.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        };
        if local_service {
            return Ok(());
        }
        self.recovery.clear_wait();
        if let Err(error) = self.stop_with_reconnect(true) {
            self.manager.cancel_reconnection();
            self.recovery.cancel();
            return Err(error);
        }
        if !self.recovery.online() {
            self.recovery.wait();
            self.manager.report_error("Network unavailable. Connect to a network; protection will resume automatically after verification.".into());
            return Ok(());
        }
        if let Err(error) = self.start_inner(state.config) {
            self.manager.cancel_reconnection();
            return Err(format!("Could not reconnect. Check the network and active profile, then enable protection again: {error}"));
        }
        Ok(())
    }

    pub fn start(self: &Arc<Self>, config: StartGatewayConfig) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        self.recovery.cancel();
        self.start_inner(config)
    }

    fn start_inner(self: &Arc<Self>, config: StartGatewayConfig) -> Result<GatewayState, String> {
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        let config = service_config::resolve_runtime_config(config)?;
        let state = self.manager.snapshot()?;
        if config.remote_url != state.config.remote_url {
            return Err("Select or verify the Confidential AI profile before starting".to_string());
        }
        let profile = state
            .profiles
            .iter()
            .find(|profile| profile.id == state.active_profile_id)
            .ok_or_else(|| "Create a Confidential AI profile before starting".to_string())?;
        let key = self
            .load_profile_key(&profile.id)?
            .ok_or_else(|| "Add a credential to the active Confidential AI profile".to_string())?;
        self.proxy.set_api_key(Some(key));
        self.manager.set_api_key_saved(true);
        self.codex_sync.reset()?;
        match self.manager.clone().start(config) {
            Ok(state) => Ok(state),
            Err(error) => {
                self.proxy.set_api_key(None);
                Err(error)
            }
        }
    }

    pub fn stop(&self) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        self.recovery.cancel();
        self.stop_inner()
    }

    fn stop_inner(&self) -> Result<GatewayState, String> {
        self.stop_with_reconnect(false)
    }

    fn stop_with_reconnect(&self, reconnecting: bool) -> Result<GatewayState, String> {
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        let result = self.manager.stop_with_reconnect(reconnecting);
        self.proxy.set_api_key(None);
        if self.instance.is_none() {
            return result;
        }
        self.proxy
            .set_tokens(with_client_token(TokenSet::default(), &self.credentials)?);
        let failures = self.current_projector()?.reconcile(None)?;
        if !failures.is_empty() {
            return Err(agent_failures(failures));
        }
        result
    }

    pub fn toggle(self: &Arc<Self>) {
        let state = match self.manager.snapshot() {
            Ok(state) => state,
            Err(_) => return,
        };
        let running = state.reconnecting
            || matches!(state.status.as_str(), "verifying" | "verified" | "blocked");
        let result = if running {
            self.stop()
        } else {
            self.start(state.config)
        };
        if let Err(error) = result {
            self.manager.report_error(error);
        }
    }

    pub fn report_error(&self, message: String) {
        self.manager.report_error(message);
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        // Shutdown waits for a configuration transaction to commit or roll back.
        // Cancelling that future midway could split credential and config state.
        let _operation = self.lifecycle.lock().await;
        if self.exiting.load(Ordering::Acquire) {
            return Ok(());
        }
        self.recovery.cancel();
        self.stop_inner()?;
        self.endpoint.stop().await?;
        self.exiting.store(true, Ordering::Release);
        Ok(())
    }

    fn projector(&self, endpoint: &str) -> Result<Projector, String> {
        Projector::new(self.helper_path.clone(), endpoint, self.secrets.clone())
    }

    fn current_projector(&self) -> Result<Projector, String> {
        self.projector(&self.manager.local_api()?.endpoint)
    }

    fn reload_agent_tokens(&self) -> Result<(), String> {
        let projector = self.current_projector()?;
        projector.migrate_legacy()?;
        let failures = projector.reconcile(None)?;
        if !failures.is_empty() {
            return Err(agent_failures(failures));
        }
        let (_, tokens) = projector.scan(None)?;
        self.proxy
            .set_tokens(with_client_token(tokens, &self.credentials)?);
        Ok(())
    }

    fn initialize_startup_tokens(&self) {
        let Err(agent_error) = self.reload_agent_tokens() else {
            return;
        };

        // A stale or unreadable agent connection record must fail closed, but
        // it must not prevent the desktop app from opening so the user can
        // inspect the error and restore the affected configuration.
        let client_error = match with_client_token(TokenSet::default(), &self.credentials) {
            Ok(tokens) => {
                self.proxy.set_tokens(tokens);
                None
            }
            Err(error) => {
                self.proxy.set_tokens(TokenSet::default());
                Some(error)
            }
        };
        let message = match client_error {
            Some(client_error) => format!(
                "Agent configurations could not be loaded: {agent_error}. The Local API credential is also unavailable: {client_error}"
            ),
            None => format!(
                "Agent configurations could not be loaded and remain disconnected: {agent_error}"
            ),
        };
        self.manager.report_error(message);
    }

    fn cleanup_manifest(&self) -> Result<Vec<String>, String> {
        let path = app_data_dir()?.join("account-cleanup.pending");
        match std::fs::read_to_string(&path) {
            Ok(value) => match serde_json::from_str(&value) {
                Ok(entries) => Ok(entries),
                Err(_) => {
                    let quarantine =
                        path.with_extension(format!("{}.corrupt", uuid::Uuid::new_v4()));
                    std::fs::rename(&path, quarantine).map_err(|_| {
                        "Account: Cannot isolate damaged credential cleanup manifest."
                    })?;
                    self.manager.report_error("Account: Damaged credential cleanup records were isolated. Review unused Private AI Proxy keys in your provider console.".into());
                    Ok(Vec::new())
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err("Account: Cannot read credential cleanup manifest.".into()),
        }
    }

    fn save_cleanup_manifest(&self, entries: &[String]) -> Result<(), String> {
        let path = app_data_dir()?.join("account-cleanup.pending");
        if entries.is_empty() {
            if path.exists() {
                std::fs::remove_file(path).map_err(|_| "Cannot finish credential cleanup")?;
            }
            return Ok(());
        }
        desktop_gateway::agents::write_atomic(
            &path,
            &serde_json::to_string(entries).map_err(|_| "Cannot encode cleanup manifest")?,
            None,
        )
        .map_err(|_| "Cannot save credential cleanup manifest".into())
    }

    fn queue_retired(&self, retired: RetiredCredential) -> Result<(), String> {
        let mut entries = self.cleanup_manifest()?;
        for entry in &entries {
            if let Some(value) = self.secrets.get(entry)? {
                let previous: RetiredCredential = serde_json::from_str(&value)
                    .map_err(|_| "Account: Credential cleanup record needs repair.")?;
                if previous.key == retired.key && previous.action == retired.action {
                    // Re-login may select a new local ref for the same stable key.
                    self.secrets.set(
                        entry,
                        &serde_json::to_string(&retired).map_err(|_| "Cannot encode cleanup")?,
                    )?;
                    return Ok(());
                }
            }
        }
        if entries.len() >= 128 {
            return Err(
                "Account: Credential cleanup queue is full. Reconnect and retry cleanup.".into(),
            );
        }
        let entry = format!("account-cleanup-{}", uuid::Uuid::new_v4());
        entries.push(entry.clone());
        // Manifest first: interrupted enqueue leaves at most a missing entry,
        // never a secret with no recoverable cleanup reference.
        self.save_cleanup_manifest(&entries)?;
        self.secrets.set(
            &entry,
            &serde_json::to_string(&retired).map_err(|_| "Cannot encode cleanup")?,
        )
    }

    async fn cleanup_retired(&self) -> Result<(), String> {
        let mut records = Vec::new();
        for entry in self.cleanup_manifest()? {
            if let Some(value) = self.secrets.get(&entry)? {
                let record: RetiredCredential = serde_json::from_str(&value)
                    .map_err(|_| "Account: Credential cleanup record needs repair.")?;
                records.push((entry, record));
            }
        }
        records.sort_by_key(|(_, record)| record.action != "activate");
        let profiles = self.manager.snapshot()?.profiles;
        let mut selected = Vec::new();
        for profile in profiles
            .iter()
            .filter(|p| service_config::profile_has_credential(p))
        {
            let entry = service_config::profile_credential_entry(profile)?;
            if let Some(secret) = self.secrets.get(&entry)? {
                selected.push((entry, secret));
            }
        }
        let mut remaining = Vec::new();
        let mut waiting_activation = std::collections::HashSet::new();
        for (index, (entry, record)) in records.into_iter().enumerate() {
            if index >= 4 {
                remaining.push(entry);
                continue;
            }
            let in_use = selected.iter().any(|(_, key)| key == &record.key);
            let referenced = selected.iter().any(|(active, _)| active == &record.entry);
            if (record.action == "activate" && !in_use) || (record.action == "revoke" && in_use) {
                if !referenced {
                    self.secrets.delete(&record.entry)?;
                }
                self.secrets.delete(&entry)?;
                continue;
            }
            if record.action == "revoke" && waiting_activation.contains(&record.profile_id) {
                remaining.push(entry);
                continue;
            }
            if record.revoke {
                match crate::account_login::transition_credential(
                    &record.provider,
                    &record.key,
                    &record.action,
                )
                .await
                {
                    Err(_) => {
                        if record.action == "activate" {
                            waiting_activation.insert(record.profile_id);
                        }
                        remaining.push(entry);
                        continue;
                    }
                    Ok(crate::account_login::CredentialTransition::Unavailable)
                        if record.action == "activate" =>
                    {
                        self.manager.report_error("Account: This device authorization is no longer active. Sign in again to reconnect.".into());
                    }
                    _ => {}
                }
            }
            if !referenced {
                self.secrets.delete(&record.entry)?;
            }
            self.secrets.delete(&entry)?;
        }
        self.save_cleanup_manifest(&remaining)
    }

    async fn cancel_pending(
        &self,
        pending: &mut crate::account_login::PendingLogin,
    ) -> Result<(), String> {
        let profile =
            self.manager.snapshot()?.profiles.into_iter().find(|p| {
                p.id == pending.profile_id() && service_config::profile_has_credential(p)
            });
        let key = match profile {
            Some(profile) => self
                .secrets
                .get(&service_config::profile_credential_entry(&profile)?)?,
            None => None,
        };
        pending.protect_saved_key(key.as_deref()).await;
        pending.cancel().await
    }

    pub async fn begin_account_login(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
    ) -> Result<crate::account_login::LoginPresentation, String> {
        let mut slot = self.account_login.try_lock().map_err(|_| {
            "Account: An account operation is in progress. Finish it before signing in again."
        })?;
        if let Some(pending) = slot.as_mut() {
            if pending.is_active() && pending.profile_id() != profile.id {
                return Err(
                    "Account: Another sign-in is open. Finish or cancel it in the other window."
                        .into(),
                );
            }
            self.cancel_pending(pending).await?;
        }
        *slot = None;
        let pending = crate::account_login::begin(profile).await?;
        let presentation = pending.presentation.clone();
        *slot = Some(pending);
        Ok(presentation)
    }

    pub async fn poll_account_login(
        self: &Arc<Self>,
        id: String,
    ) -> Result<Option<crate::contracts::AccountLoginDetails>, String> {
        self.account_login
            .lock()
            .await
            .as_mut()
            .ok_or("Account login is no longer active")?
            .poll(&id)
            .await
    }

    pub fn begin_account_save(
        self: &Arc<Self>,
        operation_id: String,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    ) -> Result<crate::contracts::AccountSaveResult, String> {
        use crate::contracts::AccountSaveResult;
        uuid::Uuid::parse_str(&operation_id).map_err(|_| "Invalid save operation ID")?;
        let mut operation = self
            .account_save
            .lock()
            .map_err(|_| "Account save unavailable")?;
        if let Some((previous_id, result)) = operation.as_ref() {
            if previous_id == &operation_id {
                return Ok(result.borrow().clone());
            }
            if matches!(*result.borrow(), AccountSaveResult::Running) {
                return Ok(AccountSaveResult::Failed {
                    error: "Account: Another save is in progress. Wait for it to finish.".into(),
                });
            }
        }
        let (sender, receiver) = tokio::sync::watch::channel(AccountSaveResult::Running);
        *operation = Some((operation_id, receiver));
        let runtime = self.clone();
        // The operation belongs to the service, not to the lifetime of an IPC
        // request. A reconnect can read its final outcome without issuing again.
        tokio::spawn(async move {
            let result = match runtime
                .save_account_login(id, profile, require_production_os, workspace_id)
                .await
            {
                Ok(state) => AccountSaveResult::Complete {
                    state: Box::new(state),
                },
                Err(error) => AccountSaveResult::Failed {
                    error: crate::protocol::RpcError::operation(&error).message,
                },
            };
            sender.send_replace(result);
        });
        Ok(AccountSaveResult::Running)
    }

    pub fn account_save_result(
        &self,
        operation_id: &str,
    ) -> Result<crate::contracts::AccountSaveResult, String> {
        let operation = self
            .account_save
            .lock()
            .map_err(|_| "Account save unavailable")?;
        let (_, result) = operation
            .as_ref()
            .filter(|(id, _)| id == operation_id)
            .ok_or(
                "Account: Save outcome is unavailable. Check the saved profile before retrying.",
            )?;
        let outcome = result.borrow().clone();
        if matches!(outcome, crate::contracts::AccountSaveResult::Running)
            && result.has_changed().is_err()
        {
            return Ok(crate::contracts::AccountSaveResult::Failed {
                error: "Account: Save was interrupted. Check the saved profile before retrying."
                    .into(),
            });
        }
        Ok(outcome)
    }

    pub async fn save_account_login(
        self: &Arc<Self>,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    ) -> Result<GatewayState, String> {
        let mut slot = self.account_login.lock().await;
        let credential = slot
            .as_mut()
            .ok_or("Account login is no longer active")?
            .credential(&id, &profile, workspace_id)
            .await?;
        let saved = self
            .persist_configuration(
                profile,
                require_production_os,
                Some(credential.key.clone()),
                Some(credential.auth),
                false,
            )
            .await?;
        if let Some(pending) = slot.as_mut() {
            pending.mark_saved();
        }
        *slot = None;
        self.finish_configuration(saved)
    }

    pub async fn complete_account_login(
        &self,
        id: String,
        callback_url: String,
    ) -> Result<(), String> {
        self.account_login
            .lock()
            .await
            .as_ref()
            .ok_or("Account login is no longer active")?
            .complete_callback(&id, &callback_url)
            .await
    }

    pub async fn save_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<GatewayState, String> {
        let saved = self
            .persist_configuration(profile, require_production_os, key, None, false)
            .await?;
        self.finish_configuration(saved)
    }

    pub async fn account_workspaces(
        &self,
        profile_id: String,
    ) -> Result<Vec<crate::contracts::AccountWorkspace>, String> {
        use crate::contracts::{ProfileAuth, ServiceProvider};
        let state = self.manager.snapshot()?;
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or("Profile not found")?;
        if profile.provider != ServiceProvider::Redpill
            || !matches!(profile.auth, ProfileAuth::OAuth { .. })
        {
            return Err("Sign in with RedPill to select a workspace".into());
        }
        let entry = service_config::profile_credential_entry(profile)?;
        let key = self
            .secrets
            .get(&entry)?
            .ok_or("This profile has no saved credential")?;
        crate::account_login::account_workspaces(&key).await
    }

    pub async fn account_balance(
        &self,
        target: crate::contracts::AccountBalanceTarget,
    ) -> Result<crate::contracts::AccountBalance, String> {
        use crate::contracts::{AccountBalanceTarget, ProfileAuth};
        match target {
            AccountBalanceTarget::Login { id } => {
                self.balances
                    .get(format!("login:{id}"), async {
                        let (provider, secret) = self
                            .account_login
                            .lock()
                            .await
                            .as_mut()
                            .ok_or("Account login is no longer active")?
                            .balance_credential(&id)
                            .await?;
                        crate::account_login::account_balance(&provider, &secret).await
                    })
                    .await
            }
            AccountBalanceTarget::Profile { profile_id } => {
                let state = self.manager.snapshot()?;
                let profile = state
                    .profiles
                    .iter()
                    .find(|p| p.id == profile_id)
                    .ok_or("Profile not found")?;
                if !matches!(profile.auth, ProfileAuth::OAuth { .. })
                    || !service_config::profile_has_credential(profile)
                {
                    return Err("Sign in with an account to view its balance".into());
                }
                let entry = service_config::profile_credential_entry(profile)?;
                self.balances
                    .get(format!("profile:{profile_id}:{entry}"), async {
                        let key = self
                            .secrets
                            .get(&entry)?
                            .ok_or("This profile has no saved credential")?;
                        crate::account_login::account_balance(&profile.provider, &key).await
                    })
                    .await
            }
        }
    }

    pub async fn cancel_account_login(&self, id: String) -> Result<(), String> {
        let mut slot = self.account_login.lock().await;
        if let Some(pending) = slot
            .as_mut()
            .filter(|pending| pending.presentation.id == id)
        {
            self.cancel_pending(pending).await?;
            *slot = None;
        }
        Ok(())
    }

    pub async fn verify_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<GatewayState, String> {
        let saved = self
            .persist_configuration(profile, require_production_os, key, None, true)
            .await?;
        self.finish_configuration(saved)
    }

    fn finish_configuration(
        self: &Arc<Self>,
        saved: SavedConfiguration<'_>,
    ) -> Result<GatewayState, String> {
        if saved.reconnect {
            self.start_inner(saved.config)
        } else {
            self.manager.snapshot()
        }
    }

    async fn persist_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
        auth: Option<crate::contracts::ProfileAuth>,
        verify: bool,
    ) -> Result<SavedConfiguration<'_>, String> {
        let _operation = self.configuration_change()?;
        let initial = self.manager.snapshot()?;
        if initial.status == "verifying" {
            return Err("Wait for the current verification to finish".to_string());
        }
        let reconnect = initial.session_active
            || (self.manager.is_running()? && !initial.configuration_verification);
        let initial_settings = service_config::settings_from_state(
            initial.profiles.clone(),
            initial.active_profile_id.clone(),
            initial.config.require_production_os,
        )?;
        let existing = initial_settings
            .profiles
            .iter()
            .find(|entry| entry.id == profile.id)
            .cloned();
        let mut candidate =
            service_config::resolve_profile(profile, verify.then(service_config::now_secs))?;
        if let Some(auth) = auth {
            candidate.auth = auth;
        } else if key.is_none() {
            if let Some(existing) = &existing {
                candidate.auth = existing.auth.clone();
            }
        }
        let profile_changed = existing.as_ref().is_none_or(|existing| {
            existing.provider != candidate.provider
                || existing.remote_url != candidate.remote_url
                || existing.auth != candidate.auth
        });
        let replace_key = key.is_some();
        candidate.credential_ref = if replace_key {
            Some(format!("credential-{}", uuid::Uuid::new_v4()))
        } else {
            existing.as_ref().and_then(|p| p.credential_ref.clone())
        };
        let candidate_entry = service_config::profile_credential_entry(&candidate)?;
        let previous_entry = existing
            .as_ref()
            .map(service_config::profile_credential_entry)
            .transpose()?;
        let stored_candidate_key = match &previous_entry {
            Some(entry) => self.secrets.get(entry)?,
            None => None,
        };
        let candidate_key = match key {
            Some(key) => validate_api_key(&key)?,
            None if !profile_changed => stored_candidate_key
                .clone()
                .ok_or_else(|| "Enter an API key".to_string())?,
            None => return Err("Enter an API key for this profile".to_string()),
        };
        let current = self.manager.snapshot()?;
        let mut settings = service_config::settings_from_state(
            current.profiles,
            current.active_profile_id,
            current.config.require_production_os,
        )?;
        let config = StartGatewayConfig {
            remote_url: candidate.remote_url.clone(),
            require_production_os,
        };
        candidate.credential_saved = Some(true);
        settings.upsert(candidate.clone())?;
        settings.active_profile_id = candidate.id.clone();
        settings.require_production_os = require_production_os;

        if reconnect {
            self.stop_with_reconnect(true)?;
            self.manager.cancel_reconnection();
        }
        let previous = self.manager.snapshot()?;

        self.codex_sync.reset()?;
        if verify {
            self.proxy.set_api_key(Some(candidate_key.clone()));
            let started = match self
                .manager
                .clone()
                .begin_verification(config.clone(), profile_changed)
            {
                Ok(state) => state,
                Err(error) => {
                    self.proxy.set_api_key(None);
                    return Err(error);
                }
            };
            let Some(session_id) = started.session_id.clone() else {
                let _ = self.manager.stop();
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err("Configuration verification did not start".to_string());
            };
            let verified = self.manager.wait_for_verification(&session_id).await;
            let stop_result = self.manager.stop_with_reconnect(initial.session_active);
            if let Err(error) = verified {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(match stop_result {
                    Ok(_) => error,
                    Err(stop_error) => {
                        format!("{error}. The verifier also could not stop: {stop_error}")
                    }
                });
            }
            if let Err(error) = stop_result {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        } else {
            self.manager.stop_with_reconnect(initial.session_active)?;
            self.proxy.set_api_key(None);
        }

        let retiring = existing
            .as_ref()
            .zip(stored_candidate_key.as_ref())
            .filter(|_| replace_key);
        if let Some((old, old_key)) = retiring {
            if let Err(error) = self.queue_retired(RetiredCredential {
                profile_id: candidate.id.clone(),
                action: "revoke".into(),
                provider: old.provider.clone(),
                key: old_key.clone(),
                entry: service_config::profile_credential_entry(old)?,
                revoke: old_key != &candidate_key
                    && matches!(old.auth, crate::contracts::ProfileAuth::OAuth { .. }),
            }) {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        }
        if replace_key {
            if let Err(error) = self.secrets.set(&candidate_entry, &candidate_key) {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        }
        if replace_key && matches!(candidate.auth, crate::contracts::ProfileAuth::OAuth { .. }) {
            if let Err(error) = self.queue_retired(RetiredCredential {
                profile_id: candidate.id.clone(),
                action: "activate".into(),
                provider: candidate.provider.clone(),
                key: candidate_key.clone(),
                entry: candidate_entry.clone(),
                revoke: true,
            }) {
                let cleanup = self.secrets.delete(&candidate_entry);
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(cleanup.err().unwrap_or(error));
            }
        }
        let settings = match service_config::save(settings) {
            Ok(settings) => settings,
            Err(error) => {
                let restore_error = if replace_key {
                    self.secrets.delete(&candidate_entry).err()
                } else {
                    None
                };
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(match restore_error {
                    Some(restore_error) => format!(
                        "{error}. The previous credential could not be restored: {restore_error}"
                    ),
                    None => error,
                });
            }
        };
        self.recovery.cancel();
        self.manager.set_service_configuration(
            config.clone(),
            settings.profiles,
            settings.active_profile_id,
            true,
            verify,
        );
        if let Err(error) = self.cleanup_retired().await {
            self.manager.report_error(error);
        }
        Ok(SavedConfiguration {
            config,
            reconnect,
            _operation,
        })
    }

    pub fn activate_profile(self: &Arc<Self>, profile_id: String) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        let previous = self.manager.snapshot()?;
        if previous.status == "verifying" {
            return Err("Wait for the current verification to finish".to_string());
        }
        if previous.active_profile_id == profile_id {
            return Ok(previous);
        }
        let reconnect = previous.session_active
            || (self.manager.is_running()? && !previous.configuration_verification);
        let mut settings = service_config::settings_from_state(
            previous.profiles,
            previous.active_profile_id,
            previous.config.require_production_os,
        )?;
        let profile = settings
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
            .ok_or_else(|| "Confidential AI profile not found".to_string())?;
        if reconnect {
            self.stop_with_reconnect(true)?;
            self.manager.cancel_reconnection();
        }
        settings.active_profile_id = profile.id;
        let settings = service_config::save(settings)?;
        let config = settings.runtime_config()?;
        let credential_saved = settings
            .active_profile()
            .is_ok_and(service_config::profile_has_credential);
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        self.manager.set_service_configuration(
            config.clone(),
            settings.profiles,
            settings.active_profile_id,
            credential_saved,
            false,
        );
        if reconnect {
            self.start_inner(config)
        } else {
            self.manager.snapshot()
        }
    }

    pub async fn delete_profile(&self, profile_id: String) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err("Stop protection before deleting a profile".to_string());
        }
        let previous = self.manager.snapshot()?;
        let affects_active = previous.active_profile_id == profile_id;
        let mut settings = service_config::settings_from_state(
            previous.profiles,
            previous.active_profile_id,
            previous.config.require_production_os,
        )?;
        let removed = settings
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
            .ok_or_else(|| "Confidential AI profile not found".to_string())?;
        let entry = service_config::profile_credential_entry(&removed)?;
        let removed_key = self.secrets.get(&entry)?;
        if matches!(removed.auth, crate::contracts::ProfileAuth::OAuth { .. }) {
            if let Some(key) = &removed_key {
                self.queue_retired(RetiredCredential {
                    profile_id: removed.id.clone(),
                    action: "revoke".into(),
                    provider: removed.provider.clone(),
                    key: key.clone(),
                    entry: entry.clone(),
                    revoke: true,
                })?;
            }
        }
        self.secrets.delete(&entry)?;
        settings.profiles.retain(|profile| profile.id != profile_id);
        if settings.profiles.is_empty() {
            settings.active_profile_id.clear();
        } else if settings.active_profile_id == profile_id {
            settings.active_profile_id = settings.profiles[0].id.clone();
        }
        let settings = match service_config::save(settings) {
            Ok(settings) => settings,
            Err(error) => {
                let restore_error =
                    restore_secret_entry(&*self.secrets, &entry, removed_key.as_deref()).err();
                return Err(match restore_error {
                    Some(restore_error) => format!(
                        "{error}. The deleted credential could not be restored: {restore_error}"
                    ),
                    None => error,
                });
            }
        };
        let config = settings.runtime_config()?;
        let credential_saved = settings
            .active_profile()
            .is_ok_and(service_config::profile_has_credential);
        self.proxy.set_api_key(None);
        if affects_active {
            self.recovery.cancel();
        }
        self.manager.set_service_configuration(
            config,
            settings.profiles,
            settings.active_profile_id,
            credential_saved,
            false,
        );
        self.manager.snapshot()
    }

    pub async fn clear_api_key(&self) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err("Stop protection before deleting a profile credential".to_string());
        }
        let state = self.manager.snapshot()?;
        if state.active_profile_id.is_empty() {
            return Err("There is no active Confidential AI profile".to_string());
        }
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == state.active_profile_id)
            .ok_or("Profile not found")?;
        let entry = service_config::profile_credential_entry(profile)?;
        let previous_key = self.secrets.get(&entry)?;
        if let Some(profile) = state
            .profiles
            .iter()
            .find(|p| p.id == state.active_profile_id)
        {
            if matches!(profile.auth, crate::contracts::ProfileAuth::OAuth { .. }) {
                if let Some(key) = &previous_key {
                    self.queue_retired(RetiredCredential {
                        profile_id: profile.id.clone(),
                        action: "revoke".into(),
                        provider: profile.provider.clone(),
                        key: key.clone(),
                        entry: entry.clone(),
                        revoke: true,
                    })?;
                }
            }
        }
        self.secrets.delete(&entry)?;
        let mut settings = service_config::settings_from_state(
            state.profiles,
            state.active_profile_id.clone(),
            state.config.require_production_os,
        )?;
        if let Some(profile) = settings
            .profiles
            .iter_mut()
            .find(|profile| profile.id == state.active_profile_id)
        {
            profile.credential_saved = Some(false);
        }
        if let Err(error) = service_config::save(settings) {
            let restore = restore_secret_entry(&*self.secrets, &entry, previous_key.as_deref());
            return Err(match restore {
                Ok(()) => error,
                Err(restore_error) => {
                    format!("{error}. The credential could not be restored: {restore_error}")
                }
            });
        }
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        self.manager
            .set_profile_credential_saved(&state.active_profile_id, false);
        self.manager.snapshot()
    }

    pub fn query_usage(&self, query: UsageQuery) -> Result<UsagePage, String> {
        self.usage.page(&query)
    }

    pub fn import_profiles(
        &self,
        backup: crate::maintenance::ProfileBackup,
    ) -> Result<crate::maintenance::ImportResult, String> {
        let _operation = self.configuration_change()?;
        let state = self.manager.snapshot()?;
        let mut settings = service_config::settings_from_state(
            state.profiles,
            state.active_profile_id,
            state.config.require_production_os,
        )?;
        let result = backup.merge(&mut settings)?;
        if result.imported > 0 {
            let settings = service_config::save(settings)?;
            let config = settings.runtime_config()?;
            self.manager
                .update_profile_list(settings.profiles, settings.active_profile_id, config);
        }
        Ok(result)
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

    pub fn client_key(&self) -> Result<String, String> {
        self.credentials.token()
    }

    pub fn rotate_client_key(&self) -> Result<String, String> {
        let _operation = self.configuration_change()?;
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        self.proxy
            .set_tokens(self.proxy.tokens().without(LOCAL_TOOLS_AGENT));
        let token = match self.credentials.rotate() {
            Ok(token) => token,
            Err(error) => {
                self.manager.client_key_changed(false);
                return Err(error);
            }
        };
        let mut tokens = self.proxy.tokens();
        tokens.insert(token.clone(), LOCAL_TOOLS_AGENT.to_string());
        self.proxy.set_tokens(tokens);
        self.manager.client_key_changed(true);
        Ok(token)
    }

    pub async fn save_local_api_config(
        self: &Arc<Self>,
        config: LocalApiConfig,
    ) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err("Change Local API settings in the primary app instance".to_string());
        }
        let previous = self.manager.snapshot()?;
        if previous.status == "verifying" {
            return Err("Wait for the current verification to finish".to_string());
        }
        let current = self.manager.local_api()?;
        let resolved = local_api::resolve(config.clone())?;
        if current.config == resolved.config && previous.endpoint_error.is_none() {
            return Ok(previous);
        }
        let reconnect = self.manager.is_running()? && !previous.configuration_verification;
        // Suspend projections before changing the URL; reconnect rebuilds them against the new endpoint.
        self.stop_with_reconnect(reconnect || previous.reconnecting)?;
        let result = self.rebind_local_api(config, current, resolved).await;
        if self.manager.snapshot()?.endpoint_error.is_some() {
            self.recovery.cancel();
            self.manager.cancel_reconnection();
        } else if previous.reconnecting && !self.recovery.online() {
            self.recovery.wait();
            result?;
            return self.manager.snapshot();
        }
        if (reconnect || previous.reconnecting) && self.manager.snapshot()?.endpoint_error.is_none()
        {
            if let Err(error) = self.start_inner(previous.config) {
                self.manager.cancel_reconnection();
                return Err(match result {
                    Ok(_) => format!(
                        "Local API settings saved, but protection could not restart: {error}"
                    ),
                    Err(original) => format!("{original}. Protection could not restart: {error}"),
                });
            }
        }
        result?;
        self.manager.snapshot()
    }

    async fn rebind_local_api(
        self: &Arc<Self>,
        config: LocalApiConfig,
        current: ResolvedLocalApi,
        resolved: ResolvedLocalApi,
    ) -> Result<GatewayState, String> {
        let needs_bind =
            current.bind != resolved.bind || self.manager.snapshot()?.proxy_url.is_none();
        if !needs_bind {
            let resolved = local_api::save(config)?;
            self.manager
                .set_endpoint(resolved.config, Ok(resolved.endpoint));
            return self.manager.snapshot();
        }

        // Different ports can be reserved without releasing the working listener.
        // Same-port address changes must release the original socket first.
        let prepared = if current.bind.port() != resolved.bind.port() {
            Some(proxy::bind_std(resolved.bind)?)
        } else {
            None
        };
        self.endpoint.stop().await?;
        let listener = match prepared
            .map(Ok)
            .unwrap_or_else(|| proxy::bind_std(resolved.bind))
        {
            Ok(listener) => listener,
            Err(error) => {
                if let Err(restore_error) = self.restore_endpoint(current.clone()) {
                    self.manager
                        .set_endpoint(current.config, Err(restore_error.clone()));
                    return Err(format!("{error}; {restore_error}"));
                }
                return Err(error);
            }
        };
        let resolved = match local_api::save(config) {
            Ok(resolved) => resolved,
            Err(error) => {
                drop(listener);
                if let Err(restore_error) = self.restore_endpoint(current.clone()) {
                    self.manager
                        .set_endpoint(current.config, Err(restore_error.clone()));
                    return Err(format!("{error}; {restore_error}"));
                }
                return Err(error);
            }
        };
        self.endpoint
            .start(
                self.manager.clone(),
                self.proxy.clone(),
                listener,
                resolved.config.clone(),
            )
            .inspect_err(|error| {
                self.manager
                    .set_endpoint(resolved.config.clone(), Err(error.clone()));
            })?;
        self.manager
            .set_endpoint(resolved.config, Ok(resolved.endpoint));
        self.manager.snapshot()
    }

    fn restore_endpoint(self: &Arc<Self>, previous: ResolvedLocalApi) -> Result<(), String> {
        let listener = proxy::bind_std(previous.bind).map_err(|error| {
            format!(
                "The new Local API settings failed and the previous listener could not be restored: {error}"
            )
        })?;
        self.endpoint.start(
            self.manager.clone(),
            self.proxy.clone(),
            listener,
            previous.config,
        )
    }

    pub async fn refresh_catalog(self: &Arc<Self>) -> Result<GatewayState, String> {
        let state = self.manager.clone().refresh_catalog().await?;
        self.codex_sync.reset()?;
        Ok(state)
    }

    pub fn list_agents(&self) -> Result<Vec<AgentStatus>, String> {
        if let Err(error) = self.reconcile_agents() {
            if self.state()?.error.as_deref() != Some(&error) {
                self.report_error(error);
            }
        }
        // Publish the scan under the same lock as connect/disconnect and key
        // rotation, so an older scan cannot restore revoked credentials.
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        let session = self.proxy.session();
        let catalog = session.verified.then_some(session.catalog).flatten();
        let projector = self.current_projector()?;
        let (mut statuses, tokens) = projector.scan(catalog.as_ref())?;
        if self.instance.is_none() {
            return Ok(statuses);
        }
        if let Some(catalog) = catalog.as_ref() {
            if let Some(codex) = statuses
                .iter_mut()
                .find(|status| status.id == Agent::Codex.id() && status.authorized)
            {
                if let Some(error) = self.codex_sync.refresh_error(&projector, catalog) {
                    let refresh = format!(
                        "Codex model metadata could not be refreshed: {error}. Disconnect remains available"
                    );
                    codex.attention = Some(match codex.attention.take() {
                        Some(attention) => format!("{attention} {refresh}"),
                        None => refresh,
                    });
                }
            }
        }
        self.proxy
            .set_tokens(with_client_token(tokens, &self.credentials)?);
        Ok(statuses)
    }

    pub fn preview_agent(
        &self,
        agent_id: String,
        connect: bool,
        options: ConnectOptions,
    ) -> Result<AgentPreview, String> {
        let agent = Agent::from_id(&agent_id)?;
        let catalog = self.connection_catalog(agent, connect)?;
        self.current_projector()?
            .preview(agent, connect, catalog.as_ref(), &options)
    }

    pub fn apply_agent(
        &self,
        agent_id: String,
        connect: bool,
        revision: String,
        options: ConnectOptions,
    ) -> Result<AgentStatus, String> {
        let _operation = self.configuration_change()?;
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        if self.exiting.load(Ordering::Acquire) {
            return Err("The app is closing".to_string());
        }
        if self.instance.is_none() {
            return Err("Another app instance owns the agent configurations".to_string());
        }
        let agent = Agent::from_id(&agent_id)?;
        let catalog = self.connection_catalog(agent, connect)?;
        let projector = self.current_projector()?;
        if !connect {
            self.proxy
                .set_tokens(self.proxy.tokens().without(agent.id()));
        }
        let status = projector.apply(agent, connect, &revision, catalog.as_ref(), &options)?;
        if agent == Agent::Codex && connect {
            if let Some(catalog) = catalog.as_ref() {
                self.codex_sync.remember_success(&catalog.revision)?;
            }
        }
        self.proxy.set_tokens(with_client_token(
            projector.scan(None)?.1,
            &self.credentials,
        )?);
        Ok(status)
    }

    pub fn disconnect_all_agents(&self) -> Result<Vec<AgentStatus>, String> {
        let _operation = self.configuration_change()?;
        self.disconnect_all_agents_inner()
    }

    fn disconnect_all_agents_inner(&self) -> Result<Vec<AgentStatus>, String> {
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        if self.instance.is_none() {
            return Err("Another app instance owns the agent configurations".to_string());
        }
        self.proxy
            .set_tokens(with_client_token(TokenSet::default(), &self.credentials)?);
        let projector = self.current_projector()?;
        match projector.disconnect_all() {
            Err(error) => Err(format!(
                "Restore all could not revoke the agents ({error}); access stays revoked until it is retried"
            )),
            Ok(failures) => {
                let (statuses, tokens) = projector.scan(None)?;
                self.proxy
                    .set_tokens(with_client_token(tokens, &self.credentials)?);
                if failures.is_empty() {
                    Ok(statuses)
                } else {
                    Err(failures
                        .into_iter()
                        .map(|(agent, error)| format!("{agent}: {error}"))
                        .collect::<Vec<_>>()
                        .join("; "))
                }
            }
        }
    }

    pub async fn reset_settings(self: &Arc<Self>) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err("Reset settings in the primary backend instance".into());
        }
        self.recovery.cancel();
        self.stop_inner()?;
        self.disconnect_all_agents_inner()?;
        let current = self.manager.local_api()?;
        let defaults = LocalApiConfig::default();
        let resolved = local_api::resolve(defaults.clone())?;
        self.rebind_local_api(defaults, current, resolved).await?;
        let state = self.manager.snapshot()?;
        let settings =
            service_config::settings_from_state(state.profiles, state.active_profile_id, true)?;
        let settings = service_config::save(settings)?;
        let config = settings.runtime_config()?;
        let credential_saved = settings
            .active_profile()
            .is_ok_and(service_config::profile_has_credential);
        self.manager.set_service_configuration(
            config,
            settings.profiles,
            settings.active_profile_id,
            credential_saved,
            false,
        );
        crate::preferences::reset()?;
        self.codex_sync.reset()?;
        self.manager.snapshot()
    }

    fn reconcile_agents(&self) -> Result<(), String> {
        if self.instance.is_none() {
            return Ok(());
        }
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        let state = self.manager.snapshot()?;
        let session = self.proxy.session();
        let protected = protection_active(&state) && session.verified;
        let catalog = if protected { session.catalog } else { None };
        let projector = self.current_projector()?;
        let failures = projector.reconcile(catalog.as_ref())?;
        let tokens = projector.scan(catalog.as_ref())?.1;
        self.proxy
            .set_tokens(with_client_token(tokens, &self.credentials)?);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(agent_failures(failures))
        }
    }

    fn connection_catalog(&self, agent: Agent, connect: bool) -> Result<Option<Catalog>, String> {
        if !connect {
            return Ok(None);
        }
        if !self
            .current_projector()?
            .scan(None)?
            .0
            .iter()
            .any(|status| status.id == agent.id() && status.installed)
        {
            return Ok(None);
        }
        let state = self.manager.snapshot()?;
        if state.status != "verified"
            || state.configuration_verification
            || state.endpoint_error.is_some()
            || !state.api_key_saved
        {
            return Ok(None);
        }
        let session = self.proxy.session();
        match (session.verified, session.catalog) {
            (true, Some(catalog)) => Ok(Some(catalog)),
            _ => Ok(None),
        }
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
mod tests {
    use super::*;
    use crate::contracts::{GatewayState, UsageSummary};

    struct NoSidecar;
    impl SidecarLauncher for NoSidecar {
        fn spawn(
            &self,
            _: Vec<String>,
        ) -> Result<
            (
                tokio::sync::mpsc::Receiver<crate::gateway::SidecarEvent>,
                Box<dyn crate::gateway::SidecarChild>,
            ),
            String,
        > {
            Err("No sidecar in this listener test".to_string())
        }
    }

    #[test]
    fn launch_requires_instance_ownership_before_initialization() {
        const CASE_ENV: &str = "PAP_TEST_INSTANCE_OWNERSHIP";
        if let Ok(case) = std::env::var(CASE_ENV) {
            let executor = tokio::runtime::Runtime::new().unwrap();
            let result = DesktopRuntime::launch(RuntimeOptions {
                launcher: Arc::new(NoSidecar),
                helper_path: app_data_dir().unwrap().join("helper"),
                task_runtime: executor.handle().clone(),
            });
            let error = result
                .err()
                .expect("Launch must refuse an unavailable lock");
            match case.as_str() {
                "held" => assert_eq!(
                    error,
                    "Another Private AI Proxy instance is already running"
                ),
                "invalid" => assert!(error.starts_with("Cannot take the instance lock:")),
                _ => panic!("Unknown instance ownership test case"),
            }
            return;
        }

        for case in ["held", "invalid"] {
            let home = tempfile::tempdir().unwrap();
            let data = home.path().join(".private-ai-proxy");
            std::fs::create_dir(&data).unwrap();
            let _owner = if case == "held" {
                Some(lock::instance(&data).unwrap().unwrap())
            } else {
                std::fs::create_dir(data.join("instance.lock")).unwrap();
                None
            };
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "controller::tests::launch_requires_instance_ownership_before_initialization",
                    "--nocapture",
                ])
                .env(CASE_ENV, case)
                .env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let entries: Vec<_> = std::fs::read_dir(&data)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(entries, vec![std::ffi::OsString::from("instance.lock")]);
        }
    }

    fn test_runtime(
        executor: &tokio::runtime::Runtime,
        directory: &std::path::Path,
    ) -> Arc<DesktopRuntime> {
        let (events, _) = tokio::sync::mpsc::channel(8);
        let proxy = ProxyState::new(events).unwrap();
        let usage = Arc::new(UsageStore::memory().unwrap());
        let manager = Arc::new(GatewayManager::new(
            proxy.clone(),
            usage.clone(),
            Arc::new(NoSidecar),
            executor.handle().clone(),
            GatewayState::default(),
        ));
        Arc::new(DesktopRuntime {
            manager,
            proxy,
            usage,
            secrets: Arc::new(desktop_gateway::secrets::MemoryStore::default()),
            credentials: ClientCredentials::from_files(TokenFiles::new(directory)),
            account_login: tokio::sync::Mutex::new(None),
            account_save: Mutex::new(None),
            balances: crate::balance_cache::BalanceCache::default(),
            legacy_credential_pending: Mutex::new(false),
            endpoint: EndpointRuntime::new(executor.handle().clone()),
            codex_sync: CodexCatalogSync::default(),
            agent_policy: Mutex::new(()),
            lifecycle: tokio::sync::Mutex::new(()),
            exiting: AtomicBool::new(false),
            helper_path: directory.join("helper"),
            recovery: crate::recovery::Recovery::default(),
            instance: None,
        })
    }

    #[test]
    fn completed_authorization_is_staged_until_explicit_save_and_bound_to_its_provider() {
        use crate::{
            account_login::{Authorization, Credential, LoginPresentation, PendingLogin},
            contracts::{ProfileAuth, ServiceProvider},
        };
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let runtime = test_runtime(&executor, directory.path());
        executor.block_on(async {
            let profile = ConfidentialProfileInput {
                id: "profile-test".into(),
                name: "Phala".into(),
                provider: ServiceProvider::Phala,
                remote_url: "https://inference.phala.com".into(),
            };
            let auth = ProfileAuth::OAuth {
                account_id: "account-test".into(),
                account_name: Some("Personal".into()),
                scope: None,
            };
            let expected = auth.clone();
            let worker = tokio::spawn(async move {
                Ok(Authorization::Inference(Credential {
                    key: "secret-not-for-the-renderer".into(),
                    auth,
                }))
            });
            *runtime.account_login.lock().await = Some(PendingLogin::new(
                LoginPresentation {
                    id: "login-test".into(),
                    url: "https://cloud.phala.com/cli/verify".into(),
                    user_code: None,
                },
                profile.clone(),
                worker,
            ));
            let completed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if let Some(auth) = runtime
                        .poll_account_login("login-test".into())
                        .await
                        .unwrap()
                    {
                        break auth;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(completed.auth, expected);
            assert!(!serde_json::to_string(&completed)
                .unwrap()
                .contains("secret-not-for-the-renderer"));
            assert_eq!(
                runtime
                    .poll_account_login("login-test".into())
                    .await
                    .unwrap()
                    .map(|details| details.auth),
                Some(expected)
            );
            assert!(runtime.state().unwrap().profiles.is_empty());
            let different_provider = ConfidentialProfileInput {
                provider: ServiceProvider::Redpill,
                remote_url: "https://tee.redpill.ai".into(),
                ..profile
            };
            assert_eq!(
                runtime
                    .save_account_login("login-test".into(), different_provider, true, None)
                    .await
                    .unwrap_err(),
                "Sign in again for the selected provider"
            );
            // A different editor must not replace the active authorization.
            let other = ConfidentialProfileInput {
                id: "another-profile".into(),
                name: "Other".into(),
                provider: ServiceProvider::Custom,
                remote_url: "https://example.com".into(),
            };
            assert!(runtime
                .begin_account_login(other.clone())
                .await
                .err()
                .unwrap()
                .contains("other window"));
            // Simulate a saved authorization left behind by a closed window.
            runtime
                .account_login
                .lock()
                .await
                .as_mut()
                .unwrap()
                .mark_saved();
            let reopened = ConfidentialProfileInput {
                id: "profile-test".into(),
                ..other
            };
            assert_eq!(
                runtime.begin_account_login(reopened).await.err().unwrap(),
                "Account login is only available for Phala and RedPill"
            );
            assert!(runtime.state().unwrap().profiles.is_empty());
            assert!(runtime.account_login.lock().await.is_none());
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        });
    }

    #[test]
    fn offline_removal_queues_cleanup_and_uncommitted_retirement_preserves_active_key() {
        const CHILD: &str = "PAP_TEST_CREDENTIAL_CLEANUP";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "controller::tests::offline_removal_queues_cleanup_and_uncommitted_retirement_preserves_active_key", "--nocapture"])
                .env(CHILD, "1").env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path()).output().unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = app_data_dir().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let runtime = test_runtime(&executor, &directory);
        std::fs::write(directory.join("account-cleanup.pending"), "invalid JSON").unwrap();
        assert!(runtime.cleanup_manifest().unwrap().is_empty());
        assert!(!directory.join("account-cleanup.pending").exists());
        assert!(runtime.cleanup_manifest().unwrap().is_empty());
        let mut profile = service_config::resolve_profile(
            ConfidentialProfileInput {
                id: "profile-test".into(),
                name: "Test".into(),
                provider: crate::contracts::ServiceProvider::Redpill,
                remote_url: "https://tee.redpill.ai".into(),
            },
            Some(1),
        )
        .unwrap();
        profile.credential_ref = Some("credential-old".into());
        profile.credential_saved = Some(true);
        profile.auth = crate::contracts::ProfileAuth::OAuth {
            account_id: "user_test".into(),
            account_name: None,
            scope: None,
        };
        let entry = service_config::profile_credential_entry(&profile).unwrap();
        runtime.secrets.set(&entry, "old-secret").unwrap();
        let config = StartGatewayConfig {
            remote_url: profile.remote_url.clone(),
            require_production_os: true,
        };
        runtime.manager.set_service_configuration(
            config,
            vec![profile.clone()],
            profile.id.clone(),
            true,
            false,
        );
        runtime
            .queue_retired(RetiredCredential {
                profile_id: profile.id.clone(),
                action: "revoke".into(),
                provider: profile.provider.clone(),
                key: "old-secret".into(),
                entry: entry.clone(),
                revoke: true,
            })
            .unwrap();
        // A new local ref can select the same stable provider secret.
        let mut reselected = profile.clone();
        reselected.credential_ref = Some("credential-new".into());
        let selected_entry = service_config::profile_credential_entry(&reselected).unwrap();
        runtime.secrets.set(&selected_entry, "old-secret").unwrap();
        let config = runtime.state().unwrap().config;
        runtime.manager.set_service_configuration(
            config,
            vec![reselected],
            profile.id.clone(),
            true,
            false,
        );
        executor.block_on(runtime.cleanup_retired()).unwrap();
        assert!(runtime.secrets.get(&entry).unwrap().is_none());
        assert_eq!(
            runtime.secrets.get(&selected_entry).unwrap().as_deref(),
            Some("old-secret")
        );
        executor
            .block_on(runtime.delete_profile(profile.id))
            .unwrap();
        assert!(runtime.state().unwrap().profiles.is_empty());
        assert!(runtime.secrets.get(&entry).unwrap().is_none());
        let entries = runtime.cleanup_manifest().unwrap();
        assert_eq!(entries.len(), 1);
        let pending: RetiredCredential =
            serde_json::from_str(&runtime.secrets.get(&entries[0]).unwrap().unwrap()).unwrap();
        assert_eq!(pending.action, "revoke");
        assert!(directory.join("account-cleanup.pending").exists());
    }

    #[test]
    fn saving_an_offline_profile_does_not_launch_verification() {
        const CHILD: &str = "PAP_TEST_SAVE_WITHOUT_VERIFY";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "controller::tests::saving_an_offline_profile_does_not_launch_verification",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = app_data_dir().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let runtime = test_runtime(&executor, &directory);
        let profile = ConfidentialProfileInput {
            id: "offline".into(),
            name: "Offline".into(),
            provider: crate::contracts::ServiceProvider::Custom,
            remote_url: "https://offline.invalid".into(),
        };
        let saved = executor
            .block_on(runtime.save_configuration(profile, true, Some("test-key".into())))
            .unwrap();
        assert_eq!(saved.active_profile_id, "offline");
        assert_eq!(saved.status, "stopped");
        assert!(saved.profiles[0].verified_at.is_none());
        assert!(service_config::profile_has_credential(&saved.profiles[0]));
        assert!(
            runtime.start(saved.config).is_err(),
            "Start must invoke the missing verifier"
        );
    }

    #[test]
    fn busy_save_is_a_definite_rejection_not_an_unknown_operation() {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let runtime = test_runtime(&executor, directory.path());
        let (_sender, receiver) =
            tokio::sync::watch::channel(crate::contracts::AccountSaveResult::Running);
        *runtime.account_save.lock().unwrap() = Some((uuid::Uuid::new_v4().to_string(), receiver));
        let profile = ConfidentialProfileInput {
            id: "profile-test".into(),
            name: "Test".into(),
            provider: crate::contracts::ServiceProvider::Redpill,
            remote_url: "https://tee.redpill.ai".into(),
        };
        let result = runtime
            .begin_account_save(
                uuid::Uuid::new_v4().to_string(),
                "login-test".into(),
                profile,
                true,
                None,
            )
            .unwrap();
        assert!(
            matches!(result, crate::contracts::AccountSaveResult::Failed { error } if error.contains("Another save"))
        );
    }

    #[test]
    fn shutdown_blocks_later_configuration_changes() {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let runtime = test_runtime(&executor, directory.path());
        executor.block_on(runtime.shutdown()).unwrap();
        let state = runtime.state().unwrap();
        assert_eq!(state.status, "stopped");
        assert_eq!(
            runtime.start(state.config).unwrap_err(),
            "The app is closing"
        );
        assert_eq!(
            runtime
                .apply_agent(
                    "codex".into(),
                    true,
                    "revision".into(),
                    ConnectOptions::default()
                )
                .unwrap_err(),
            "The app is closing"
        );
    }

    #[test]
    fn network_loss_revokes_session_and_manual_stop_cancels_recovery() {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let runtime = test_runtime(&executor, directory.path());
        runtime.manager.restore_snapshot(GatewayState {
            status: "verified".into(),
            session_id: Some("network-session".into()),
            session_active: true,
            protected_since: Some(123),
            session_usage: UsageSummary {
                requests: 7,
                ..Default::default()
            },
            config: StartGatewayConfig {
                remote_url: "https://inference.phala.com".into(),
                require_production_os: true,
            },
            ..Default::default()
        });
        runtime.proxy.publish(proxy::Session {
            verified: true,
            ..Default::default()
        });
        runtime.recovery.available.store(false, Ordering::Release);
        runtime.system_resumed();
        let operation = runtime.lifecycle.try_lock().unwrap();
        runtime.recover_network().unwrap();
        assert!(runtime.recovery.needs_check());
        drop(operation);
        let mut verifying = runtime.state().unwrap();
        verifying.status = "verifying".into();
        runtime.manager.restore_snapshot(verifying.clone());
        runtime.recovery.available.store(true, Ordering::Release);
        runtime.recover_network().unwrap();
        assert!(runtime.recovery.needs_check());
        runtime.recovery.available.store(false, Ordering::Release);
        runtime.manager.restore_snapshot(verifying);
        runtime.recover_network().unwrap();
        assert!(!runtime.recovery.needs_check());
        assert!(!runtime.proxy.session().verified);
        assert_eq!(runtime.state().unwrap().status, "stopped");
        assert!(runtime.state().unwrap().reconnecting);
        assert_eq!(
            runtime.state().unwrap().session_id.as_deref(),
            Some("network-session")
        );
        assert_eq!(runtime.state().unwrap().protected_since, Some(123));
        assert_eq!(runtime.state().unwrap().session_usage.requests, 7);
        assert!(runtime.recovery.pending());
        runtime.stop().unwrap();
        assert!(!runtime.state().unwrap().reconnecting);
        assert!(runtime.state().unwrap().protected_since.is_none());
        assert!(!runtime.recovery.pending());
        runtime.recovery.available.store(true, Ordering::Release);
        runtime.recover_network().unwrap();
        assert_eq!(runtime.state().unwrap().status, "stopped");
    }

    #[test]
    fn failed_and_noop_imports_preserve_recovery_and_monitor_state() {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let runtime = test_runtime(&executor, directory.path());
        runtime.recovery.wait();
        runtime.system_resumed();
        assert!(runtime
            .import_profiles(crate::maintenance::ProfileBackup {
                version: 99,
                profiles: vec![]
            })
            .is_err());
        assert!(runtime.recovery.pending());
        assert!(runtime.recovery.needs_check());
        assert_eq!(
            runtime
                .import_profiles(crate::maintenance::ProfileBackup {
                    version: 1,
                    profiles: vec![]
                })
                .unwrap()
                .imported,
            0
        );
        assert!(runtime.recovery.pending());
        assert!(runtime.recovery.needs_check());
        let snapshot = runtime.state().unwrap();
        assert!(runtime.set_wake_monitor_available(false));
        assert!(!runtime.set_wake_monitor_available(false));
        runtime.manager.restore_snapshot(snapshot);
        assert_eq!(runtime.state().unwrap().wake_monitor_available, Some(false));
        runtime.stop().unwrap();
        assert!(!runtime.recovery.needs_check());
        assert_eq!(runtime.state().unwrap().wake_monitor_available, Some(false));
        assert!(runtime.set_wake_monitor_available(true));
        assert_eq!(runtime.state().unwrap().wake_monitor_available, Some(true));
    }

    #[test]
    fn client_key_rotation_preserves_agent_tokens_and_fails_closed() {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let runtime = test_runtime(&executor, directory.path());
        let original = runtime.client_key().unwrap();
        let mut tokens = TokenSet::default();
        tokens.insert(original.clone(), LOCAL_TOOLS_AGENT.to_string());
        tokens.insert("agent-token".to_string(), "codex".to_string());
        runtime.proxy.set_tokens(tokens);

        let rotated = runtime.rotate_client_key().unwrap();
        // Subscribers arriving after rotation must see the new non-secret revision.
        assert_eq!(runtime.subscribe().borrow().client_key_revision, 1);
        assert_eq!(
            runtime.subscribe().borrow().client_key_available,
            Some(true)
        );
        assert_ne!(rotated, original);
        assert_eq!(runtime.client_key().unwrap(), rotated);
        assert_eq!(runtime.proxy.tokens().agent_for(&original), None);
        assert_eq!(
            runtime.proxy.tokens().agent_for(&rotated),
            Some(LOCAL_TOOLS_AGENT)
        );
        assert_eq!(
            runtime.proxy.tokens().agent_for("agent-token"),
            Some("codex")
        );

        let token_path = TokenFiles::new(directory.path()).path(LOCAL_TOOLS_AGENT);
        std::fs::remove_file(&token_path).unwrap();
        std::fs::create_dir(&token_path).unwrap();
        assert!(runtime.rotate_client_key().is_err());
        assert_eq!(runtime.subscribe().borrow().client_key_revision, 2);
        assert_eq!(
            runtime.subscribe().borrow().client_key_available,
            Some(false)
        );
        assert_eq!(runtime.proxy.tokens().agent_for(&rotated), None);
        assert_eq!(
            runtime.proxy.tokens().agent_for("agent-token"),
            Some("codex")
        );
        // A readable leftover must not be admitted by a later scan or key read.
        std::fs::remove_dir(&token_path).unwrap();
        std::fs::write(&token_path, &rotated).unwrap();
        assert!(runtime.client_key().is_err());
        let active = with_client_token(runtime.proxy.tokens(), &runtime.credentials).unwrap();
        assert_eq!(active.agent_for(&rotated), None);
        assert_eq!(active.agent_for("agent-token"), Some("codex"));
        let replacement = runtime.rotate_client_key().unwrap();
        assert_ne!(replacement, rotated);
        assert_eq!(runtime.client_key().unwrap(), replacement);
    }

    #[test]
    fn occupied_listener_preserves_previous_endpoint_and_serializes_mutations() {
        let executor = tokio::runtime::Runtime::new().unwrap();
        executor.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let old_listener = proxy::bind_std("127.0.0.1:0".parse().unwrap()).unwrap();
            let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let config = LocalApiConfig {
                port: old_listener.local_addr().unwrap().port(),
                ..LocalApiConfig::default()
            };
            let original = local_api::resolve(config.clone()).unwrap();
            let runtime = test_runtime(&executor, temp.path());
            runtime
                .manager
                .set_endpoint(config.clone(), Ok(original.endpoint.clone()));
            runtime
                .endpoint
                .start(
                    runtime.manager.clone(),
                    runtime.proxy.clone(),
                    old_listener,
                    config.clone(),
                )
                .unwrap();
            let gate = runtime.lifecycle.lock().await;
            assert!(runtime.stop().unwrap_err().contains("in progress"));
            assert!(runtime
                .save_local_api_config(config.clone())
                .await
                .unwrap_err()
                .contains("in progress"));
            drop(gate);
            let candidate = LocalApiConfig {
                port: occupied.local_addr().unwrap().port(),
                ..config.clone()
            };
            // A secondary instance cannot touch the listener or persisted config.
            assert!(runtime
                .save_local_api_config(candidate.clone())
                .await
                .unwrap_err()
                .contains("primary"));
            assert!(std::net::TcpStream::connect(original.bind).is_ok());
            // A failed reservation keeps the existing listener alive.
            assert!(runtime
                .rebind_local_api(
                    candidate.clone(),
                    original.clone(),
                    local_api::resolve(candidate).unwrap()
                )
                .await
                .is_err());
            let state = runtime.state().unwrap();
            assert_eq!(state.local_api, config);
            assert_eq!(state.proxy_url.as_deref(), Some(original.endpoint.as_str()));
            assert!(state.endpoint_error.is_none());
            assert!(std::net::TcpStream::connect(original.bind).is_ok());
            runtime.endpoint.stop().await.unwrap();
        });
    }

    #[test]
    fn only_live_protection_allows_agent_projection() {
        let mut state = GatewayState {
            api_key_saved: true,
            ..GatewayState::default()
        };
        for status in ["stopped", "verifying", "blocked", "error"] {
            state.status = status.to_string();
            assert!(!protection_active(&state));
        }
        state.status = "verified".to_string();
        assert!(protection_active(&state));
        state.configuration_verification = true;
        assert!(!protection_active(&state));
        state.configuration_verification = false;
        state.api_key_saved = false;
        assert!(!protection_active(&state));
        state.api_key_saved = true;
        state.endpoint_error = Some("Listener unavailable".to_string());
        assert!(!protection_active(&state));
    }
}
