use super::*;
use desktop_core::protocol::ShutdownMode;

impl DesktopRuntime {
    pub(super) fn configuration_change(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, Error> {
        let operation = self.lifecycle.try_lock().map_err(|_| Error::busy())?;
        if self.exiting.load(Ordering::Acquire) {
            return Err("The app is closing".into());
        }
        Ok(operation)
    }

    pub(super) fn recover_network(self: &Arc<Self>) -> Result<(), Error> {
        let Ok(_operation) = self.lifecycle.try_lock() else {
            return Ok(());
        };
        if self.exiting.load(Ordering::Acquire) {
            return Ok(());
        }
        let state = self.manager.snapshot()?;
        if state.is_protected() && self.proxy.session().verified {
            self.recovery.reset_retry();
        }
        let retry = crate::recovery::should_retry(&state)
            || (state.reconnecting && state.status == "stopped" && self.recovery.pending());
        if !retry && !self.recovery.needs_check() {
            return Ok(());
        }
        if state.status == "verifying" && self.recovery.online() {
            return Ok(());
        }
        self.recovery.clear_request();
        if !retry
            && !crate::recovery::should_recover(
                &state.status,
                state.configuration_verification,
                self.recovery.pending(),
            )
        {
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
        if local_service && !retry {
            return Ok(());
        }
        if retry && ((!local_service && !self.recovery.online()) || !self.recovery.retry_due()) {
            return Ok(());
        }
        self.recovery.clear_wait();
        let paused = (|| {
            let _guard = self
                .agent_policy
                .lock()
                .map_err(|_| "Agent state unavailable")?;
            let result = self.manager.stop_with_reconnect(true);
            self.proxy.set_api_key(None);
            self.publish_agent_tokens(TokenSet::default())
                .map_err(|error| error.to_string())?;
            result
        })();
        if let Err(error) = paused {
            self.manager.cancel_reconnection();
            self.recovery.cancel();
            return Err(error.into());
        }
        if !local_service && !self.recovery.online() {
            self.recovery.wait();
            self.manager.report_error("Network unavailable. Connect to a network; protection will resume automatically after verification.".into());
            return Ok(());
        }
        let resumed = (|| {
            if state.endpoint_error.is_some() {
                let resolved = settings_config::resolve_local_api(state.local_api)?;
                self.restore_endpoint(resolved.clone())?;
                self.manager
                    .set_endpoint(resolved.config, Ok(resolved.endpoint));
            }
            self.start_inner(state.config)
        })();
        if let Err(error) = resumed {
            self.recovery.wait();
            return Err(format!("Could not reconnect; retrying automatically: {error}").into());
        }
        Ok(())
    }

    /// Called only by the server's blocking startup worker, after IPC is bound.
    pub fn start_on_launch(self: &Arc<Self>) -> Result<(), Error> {
        let _operation = self.lifecycle.blocking_lock();
        if self.exiting.load(Ordering::Acquire) {
            return Ok(());
        }
        let state = self.manager.snapshot()?;
        if self.manager.is_running()? {
            return Ok(());
        }
        self.recovery.cancel();
        self.start_inner(state.config).map(|_| ())
    }

    pub fn start(self: &Arc<Self>, config: StartConfig) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        self.recovery.cancel();
        self.start_inner(config)
    }

    pub(super) fn start_inner(self: &Arc<Self>, config: StartConfig) -> Result<AppState, Error> {
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        let config = settings_config::resolve_runtime_config(config)?;
        let state = self.manager.snapshot()?;
        if config.remote_url != state.config.remote_url {
            return Err(Error::invalid_state(
                "Select or verify the Confidential AI profile before starting",
            ));
        }
        let profile = state
            .profiles
            .iter()
            .find(|profile| profile.id == state.active_profile_id)
            .ok_or_else(|| {
                Error::invalid_state("Create a Confidential AI profile before starting")
            })?;
        let key = self.load_profile_key(&profile.id)?.ok_or_else(|| {
            Error::invalid_state("Add a credential to the active Confidential AI profile")
        })?;
        self.proxy.set_api_key(Some(key));
        self.manager.set_api_key_saved(true);
        match self.manager.clone().start(config) {
            Ok(state) => Ok(state),
            Err(error) => {
                self.proxy.set_api_key(None);
                Err(error)
            }
        }
    }

    pub fn stop(&self) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        self.recovery.cancel();
        self.stop_inner()
    }

    pub(super) fn stop_inner(&self) -> Result<AppState, Error> {
        self.stop_with_reconnect(false)
    }

    pub(super) fn stop_with_reconnect(&self, reconnecting: bool) -> Result<AppState, Error> {
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        let result = self.manager.stop_with_reconnect(reconnecting);
        self.proxy.set_api_key(None);
        if self.instance.is_none() {
            return Ok(result?);
        }
        self.proxy
            .set_tokens(with_client_token(TokenSet::default(), &self.credentials)?);
        if self.agent_configuration_enabled() {
            let failures = self.current_projector()?.reconcile(None)?;
            if !failures.is_empty() {
                return Err(agent_failures(failures).into());
            }
        }
        Ok(result?)
    }

    /// Stops protection, the Local API and the web UI. A `refusable` shutdown
    /// (a client's request) fails before stopping anything when the agent
    /// configurations cannot be restored; any other one continues.
    pub async fn shutdown(&self, mode: ShutdownMode, refusable: bool) -> Result<(), Error> {
        // Shutdown waits for a configuration transaction to commit or roll back.
        // Cancelling that future midway could split credential and config state;
        // the server's shutdown watchdog bounds the wait.
        tracing::info!("Shutdown: waiting for configuration changes to finish");
        let _operation = self.lifecycle.lock().await;
        if self.exiting.load(Ordering::Acquire) {
            return Ok(());
        }
        self.recovery.cancel();
        let preserve_session =
            mode == ShutdownMode::UpdateRestart && self.manager.snapshot()?.session_active;
        tracing::info!("Shutdown: stopping protection and restoring agent configuration");
        let restored = self.stop_with_reconnect(preserve_session);
        // Outside the Mac App Store, a client's shutdown never leaves agents
        // pointing at a stopped Local API: a failed restore keeps the backend
        // running. A signal or the owning app's exit stops it regardless.
        if refusable && !cfg!(all(target_os = "macos", feature = "mac-app-store")) {
            restored.as_ref().map_err(Clone::clone)?;
        }
        tracing::info!("Shutdown: stopping the Local API and the web UI");
        self.endpoint.stop().await?;
        self.web_ui.stop().await;
        self.exiting.store(true, Ordering::Release);
        restored.map(|_| ())
    }
}
