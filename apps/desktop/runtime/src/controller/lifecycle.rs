use super::*;

impl DesktopRuntime {
    pub(super) fn configuration_change(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
        let operation = self
            .lifecycle
            .try_lock()
            .map_err(|_| "A configuration change is in progress")?;
        if self.exiting.load(Ordering::Acquire) {
            return Err("The app is closing".to_string());
        }
        Ok(operation)
    }

    pub(super) fn recover_network(self: &Arc<Self>) -> Result<(), String> {
        let Ok(_operation) = self.lifecycle.try_lock() else {
            return Ok(());
        };
        if self.exiting.load(Ordering::Acquire) {
            return Ok(());
        }
        let state = self.manager.snapshot()?;
        if protection_active(&state) && self.proxy.session().verified {
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
            self.publish_agent_tokens(TokenSet::default())?;
            result
        })();
        if let Err(error) = paused {
            self.manager.cancel_reconnection();
            self.recovery.cancel();
            return Err(error);
        }
        if !local_service && !self.recovery.online() {
            self.recovery.wait();
            self.manager.report_error("Network unavailable. Connect to a network; protection will resume automatically after verification.".into());
            return Ok(());
        }
        let resumed = (|| {
            if state.endpoint_error.is_some() {
                let resolved = local_api::resolve(state.local_api)?;
                self.restore_endpoint(resolved.clone())?;
                self.manager
                    .set_endpoint(resolved.config, Ok(resolved.endpoint));
            }
            self.start_inner(state.config)
        })();
        if let Err(error) = resumed {
            self.recovery.wait();
            return Err(format!(
                "Could not reconnect; retrying automatically: {error}"
            ));
        }
        Ok(())
    }

    /// Called only by the server's blocking startup worker, after IPC is bound.
    pub fn start_on_launch(self: &Arc<Self>) -> Result<(), String> {
        let _operation = self.lifecycle.blocking_lock();
        if self.exiting.load(Ordering::Acquire) {
            return Ok(());
        }
        let state = self.manager.snapshot()?;
        if self.manager.is_running()? || state.reconnecting {
            return Ok(());
        }
        self.recovery.cancel();
        self.start_inner(state.config).map(|_| ())
    }

    pub fn start(self: &Arc<Self>, config: StartGatewayConfig) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        self.recovery.cancel();
        self.start_inner(config)
    }

    pub(super) fn start_inner(
        self: &Arc<Self>,
        config: StartGatewayConfig,
    ) -> Result<GatewayState, String> {
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

    pub(super) fn stop_inner(&self) -> Result<GatewayState, String> {
        self.stop_with_reconnect(false)
    }

    pub(super) fn stop_with_reconnect(&self, reconnecting: bool) -> Result<GatewayState, String> {
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
}
