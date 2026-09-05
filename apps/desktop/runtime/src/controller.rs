use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
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
    manager: Arc<GatewayManager>,
    proxy: Arc<ProxyState>,
    usage: Arc<UsageStore>,
    secrets: Arc<dyn SecretStore>,
    credentials: ClientCredentials,
    legacy_credential_pending: Mutex<bool>,
    endpoint: EndpointRuntime,
    codex_sync: CodexCatalogSync,
    helper_path: PathBuf,
    #[allow(dead_code)]
    instance: Option<lock::InstanceLock>,
}

struct ClientCredentials(Mutex<TokenFiles>);

impl ClientCredentials {
    fn new() -> Result<Self, String> {
        Ok(Self(Mutex::new(TokenFiles::new(&app_data_dir()?))))
    }

    fn token(&self) -> Result<String, String> {
        self.0
            .lock()
            .map_err(|_| "Client credential store unavailable".to_string())?
            .ensure(LOCAL_TOOLS_AGENT)
    }

    fn rotate(&self) -> Result<String, String> {
        self.0
            .lock()
            .map_err(|_| "Client credential store unavailable".to_string())?
            .rotate(LOCAL_TOOLS_AGENT)
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
        let data_dir = app_data_dir()?;
        let (instance, listener, launch_error) = match lock::instance(&data_dir) {
            Ok(Some(instance)) => match proxy::bind_std(local.bind) {
                Ok(listener) => (Some(instance), Some(listener), None),
                Err(error) => (Some(instance), None, Some(error)),
            },
            Ok(None) => (
                None,
                None,
                Some("Another Private AI Gateway instance is already running".to_string()),
            ),
            Err(error) => (
                None,
                None,
                Some(format!("Cannot take the instance lock: {error}")),
            ),
        };
        let (proxy_events_tx, mut proxy_events) = tokio::sync::mpsc::channel::<ProxyEvent>(256);
        let proxy = ProxyState::new(proxy_events_tx);
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
            legacy_credential_pending: Mutex::new(migrated_legacy),
            endpoint: EndpointRuntime::new(task_runtime.clone()),
            codex_sync: CodexCatalogSync::default(),
            helper_path: options.helper_path,
            instance,
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
        runtime.initialize_startup_tokens();

        let events_runtime = runtime.clone();
        task_runtime.spawn(async move {
            while let Some(event) = proxy_events.recv().await {
                events_runtime.manager.record_proxy_event(event);
            }
        });
        Ok(runtime)
    }

    pub fn subscribe(&self) -> watch::Receiver<GatewayState> {
        self.manager.subscribe()
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
        let entry = service_config::credential_entry(profile_id)?;
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

    pub fn start(self: &Arc<Self>, config: StartGatewayConfig) -> Result<GatewayState, String> {
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
        let result = self.manager.stop();
        self.proxy.set_api_key(None);
        result
    }

    pub fn toggle(self: &Arc<Self>) {
        let state = match self.manager.snapshot() {
            Ok(state) => state,
            Err(_) => return,
        };
        let running = matches!(state.status.as_str(), "verifying" | "verified" | "blocked");
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
        let gateway = self.stop().map(|_| ());
        let endpoint = self.endpoint.stop().await;
        gateway.and(endpoint)
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

    pub async fn verify_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<GatewayState, String> {
        if self.manager.is_running()? {
            return Err("Stop protection before verifying a different service".to_string());
        }
        let initial = self.manager.snapshot()?;
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
            service_config::resolve_profile(profile, Some(service_config::now_secs()))?;
        let profile_changed = existing.as_ref().is_none_or(|existing| {
            existing.provider != candidate.provider
                || existing.remote_url != candidate.remote_url
                || existing.auth != candidate.auth
        });
        let candidate_entry = service_config::credential_entry(&candidate.id)?;
        let replace_key = key.is_some();
        let stored_candidate_key = if replace_key {
            self.secrets.get(&candidate_entry)?
        } else if !profile_changed {
            self.load_profile_key(&candidate.id)?
        } else {
            None
        };
        let candidate_key = match key {
            Some(key) => validate_api_key(&key)?,
            None if !profile_changed => stored_candidate_key
                .clone()
                .ok_or_else(|| "Enter an API key".to_string())?,
            None => return Err("Enter an API key for this profile".to_string()),
        };
        let previous = self.manager.snapshot()?;
        let mut settings = service_config::settings_from_state(
            previous.profiles.clone(),
            previous.active_profile_id.clone(),
            previous.config.require_production_os,
        )?;
        let config = StartGatewayConfig {
            remote_url: candidate.remote_url.clone(),
            require_production_os,
        };
        candidate.credential_saved = Some(true);
        settings.upsert(candidate.clone())?;
        settings.active_profile_id = candidate.id.clone();
        settings.require_production_os = require_production_os;

        self.codex_sync.reset()?;
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
        let stop_result = self.manager.stop();
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

        if replace_key {
            if let Err(error) = self.secrets.set(&candidate_entry, &candidate_key) {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        }
        let settings = match service_config::save(settings) {
            Ok(settings) => settings,
            Err(error) => {
                let restore_error = replace_key
                    .then(|| {
                        restore_secret_entry(
                            &*self.secrets,
                            &candidate_entry,
                            stored_candidate_key.as_deref(),
                        )
                        .err()
                    })
                    .flatten();
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
        self.manager.set_service_configuration(
            config,
            settings.profiles,
            settings.active_profile_id,
            true,
            true,
        );
        self.manager.snapshot()
    }

    pub fn activate_profile(&self, profile_id: String) -> Result<GatewayState, String> {
        if self.manager.is_running()? {
            return Err("Stop protection before changing profiles".to_string());
        }
        let previous = self.manager.snapshot()?;
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
        settings.active_profile_id = profile.id;
        let settings = service_config::save(settings)?;
        let config = settings.runtime_config()?;
        let credential_saved = settings
            .active_profile()
            .is_ok_and(service_config::profile_has_credential);
        self.proxy.set_api_key(None);
        self.manager.set_service_configuration(
            config,
            settings.profiles,
            settings.active_profile_id,
            credential_saved,
            false,
        );
        self.manager.snapshot()
    }

    pub fn delete_profile(&self, profile_id: String) -> Result<GatewayState, String> {
        if self.manager.is_running()? {
            return Err("Stop protection before deleting a profile".to_string());
        }
        let previous = self.manager.snapshot()?;
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
        let entry = service_config::credential_entry(&removed.id)?;
        let removed_key = self.secrets.get(&entry)?;
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
        self.manager.set_service_configuration(
            config,
            settings.profiles,
            settings.active_profile_id,
            credential_saved,
            false,
        );
        self.manager.snapshot()
    }

    pub fn clear_api_key(&self) -> Result<GatewayState, String> {
        if self.manager.is_running()? {
            return Err("Stop protection before deleting a profile credential".to_string());
        }
        let state = self.manager.snapshot()?;
        if state.active_profile_id.is_empty() {
            return Err("There is no active Confidential AI profile".to_string());
        }
        let entry = service_config::credential_entry(&state.active_profile_id)?;
        let previous_key = self.secrets.get(&entry)?;
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
        self.manager
            .set_profile_credential_saved(&state.active_profile_id, false);
        self.manager.snapshot()
    }

    pub fn query_usage(&self, query: UsageQuery) -> Result<UsagePage, String> {
        self.usage.page(&query)
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
        self.proxy
            .set_tokens(self.proxy.tokens().without(LOCAL_TOOLS_AGENT));
        let token = self.credentials.rotate()?;
        let mut tokens = self.proxy.tokens();
        tokens.insert(token.clone(), LOCAL_TOOLS_AGENT.to_string());
        self.proxy.set_tokens(tokens);
        Ok(token)
    }

    pub async fn save_local_api_config(
        self: &Arc<Self>,
        config: LocalApiConfig,
    ) -> Result<GatewayState, String> {
        if self.manager.is_running()? {
            return Err("Stop protection before changing Local API settings".to_string());
        }
        let current = self.manager.local_api()?;
        let resolved = local_api::resolve(config.clone())?;
        if current.endpoint != resolved.endpoint {
            let statuses = self.projector(&current.endpoint)?.scan(None)?.0;
            if statuses.iter().any(|agent| agent.recorded) {
                return Err(
                    "Disconnect managed agents before changing the endpoint they use".to_string(),
                );
            }
        }
        let needs_bind =
            current.bind != resolved.bind || self.manager.snapshot()?.proxy_url.is_none();
        if !needs_bind {
            let resolved = local_api::save(config)?;
            self.manager
                .set_endpoint(resolved.config, Ok(resolved.endpoint));
            return self.manager.snapshot();
        }

        self.endpoint.stop().await?;
        let listener = match proxy::bind_std(resolved.bind) {
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
        self.endpoint.start(
            self.manager.clone(),
            self.proxy.clone(),
            listener,
            resolved.config.clone(),
        )?;
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
        let session = self.proxy.session();
        let catalog = session.verified.then_some(session.catalog).flatten();
        let projector = self.current_projector()?;
        let (mut statuses, tokens) = projector.scan(catalog.as_ref())?;
        if let Some(catalog) = catalog.as_ref() {
            if let Some(codex) = statuses
                .iter_mut()
                .find(|status| status.id == Agent::Codex.id() && status.connected)
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

    fn connection_catalog(&self, agent: Agent, connect: bool) -> Result<Option<Catalog>, String> {
        if !connect {
            return Ok(None);
        }
        if let Some(error) = self.manager.snapshot()?.endpoint_error {
            return Err(format!("The local endpoint is unavailable: {error}"));
        }
        let session = self.proxy.session();
        match (session.verified, session.catalog) {
            (true, Some(catalog)) => Ok(Some(catalog)),
            _ => Err(format!(
                "Start the gateway and wait until it is verified; the model list for {} comes from it",
                agent.name()
            )),
        }
    }
}

fn with_client_token(
    mut tokens: TokenSet,
    credentials: &ClientCredentials,
) -> Result<TokenSet, String> {
    tokens.insert(credentials.token()?, LOCAL_TOOLS_AGENT.to_string());
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
