use super::*;

impl DesktopRuntime {
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

    pub(super) async fn rebind_local_api(
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

    pub(super) fn restore_endpoint(
        self: &Arc<Self>,
        previous: ResolvedLocalApi,
    ) -> Result<(), String> {
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
}
