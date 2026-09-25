use super::*;

impl DesktopRuntime {
    pub fn client_key(&self) -> Result<String, Error> {
        self.credentials.token()
    }

    pub fn rotate_client_key(&self) -> Result<String, Error> {
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
        config: ListenConfig,
    ) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err("Change Local API settings in the primary app instance".into());
        }
        if self.manager.snapshot()?.status == "verifying" {
            return Err("Wait for the current verification to finish".into());
        }
        self.apply_local_api(config).await
    }

    /// Saves and rebinds the Local API, pausing protection around the change.
    pub(super) async fn apply_local_api(
        self: &Arc<Self>,
        config: ListenConfig,
    ) -> Result<AppState, Error> {
        let previous = self.manager.snapshot()?;
        let current = self.manager.local_api()?;
        let resolved = settings_config::resolve_local_api(config.clone())?;
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
            return Ok(self.manager.snapshot()?);
        }
        if (reconnect || previous.reconnecting) && self.manager.snapshot()?.endpoint_error.is_none()
        {
            if let Err(error) = self.start_inner(previous.config) {
                self.manager.cancel_reconnection();
                return Err(match result {
                    Ok(_) => format!(
                        "Local API settings saved, but protection could not restart: {error}"
                    )
                    .into(),
                    Err(original) => {
                        format!("{original}. Protection could not restart: {error}").into()
                    }
                });
            }
        }
        result?;
        Ok(self.manager.snapshot()?)
    }

    pub(super) async fn rebind_local_api(
        self: &Arc<Self>,
        config: ListenConfig,
        current: ResolvedListen,
        resolved: ResolvedListen,
    ) -> Result<AppState, Error> {
        let needs_bind =
            current.bind != resolved.bind || self.manager.snapshot()?.proxy_url.is_none();
        if !needs_bind {
            let resolved = self.save_local_api(config)?;
            self.manager
                .set_endpoint(resolved.config, Ok(resolved.endpoint));
            return Ok(self.manager.snapshot()?);
        }

        // Different ports can be reserved without releasing the working listener.
        // Same-port address changes must release the original socket first.
        let prepared = if current.bind.port() != resolved.bind.port() {
            Some(proxy::bind_std(resolved.bind)?)
        } else {
            None
        };
        self.endpoint.stop().await?;
        let listener = match prepared.map(Ok).unwrap_or_else(|| rebind(resolved.bind)) {
            Ok(listener) => listener,
            Err(error) => {
                if let Err(restore_error) = self.restore_endpoint(current.clone()) {
                    self.manager
                        .set_endpoint(current.config, Err(restore_error.to_string()));
                    return Err(format!("{error}; {restore_error}").into());
                }
                return Err(error);
            }
        };
        let resolved = match self.save_local_api(config) {
            Ok(resolved) => resolved,
            Err(error) => {
                drop(listener);
                if let Err(restore_error) = self.restore_endpoint(current.clone()) {
                    self.manager
                        .set_endpoint(current.config, Err(restore_error.to_string()));
                    return Err(format!("{error}; {restore_error}").into());
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
                    .set_endpoint(resolved.config.clone(), Err(error.to_string()));
            })?;
        self.manager
            .set_endpoint(resolved.config, Ok(resolved.endpoint));
        Ok(self.manager.snapshot()?)
    }

    fn save_local_api(&self, config: ListenConfig) -> Result<ResolvedListen, Error> {
        let resolved = settings_config::resolve_local_api(config)?;
        self.update_config(|settings| {
            settings.local_api = resolved.config.clone();
            Ok(())
        })?;
        Ok(resolved)
    }

    pub(super) fn restore_endpoint(
        self: &Arc<Self>,
        previous: ResolvedListen,
    ) -> Result<(), Error> {
        let listener = rebind(previous.bind).map_err(|error| {
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

    pub async fn reset_settings(self: &Arc<Self>) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err("Reset settings in the primary backend instance".into());
        }
        self.recovery.cancel();
        self.stop_inner()?;
        if self.agent_configuration_enabled() {
            self.disconnect_all_agents_inner()?;
        }
        let current = self.manager.local_api()?;
        let defaults = ListenConfig::default();
        let resolved = settings_config::resolve_local_api(defaults.clone())?;
        self.rebind_local_api(defaults, current, resolved).await?;
        // Profiles and keys are kept; everything else returns to its default.
        self.update_config(|settings| {
            *settings = Config {
                active_profile: std::mem::take(&mut settings.active_profile),
                profiles: std::mem::take(&mut settings.profiles),
                ..Config::default()
            };
            Ok(())
        })?;
        self.update_credentials(|credentials| {
            credentials.web_ui = Default::default();
            Ok(())
        })?;
        self.publish_service_configuration(false)?;
        self.web_ui.set_password(None);
        self.apply_web_ui(&desktop_core::config::WebUiConfig::default());
        Ok(self.manager.snapshot()?)
    }
}

/// Binds a port whose Local API listener was just stopped.
fn rebind(address: std::net::SocketAddr) -> Result<std::net::TcpListener, Error> {
    Ok(desktop_core::listen::bind(address, true)
        .map_err(|error| format!("Cannot listen on {address}: {error}"))?)
}
