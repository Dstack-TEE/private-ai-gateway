use super::*;
use desktop_core::protocol::RpcError;

#[derive(Debug)]
pub enum AgentOperationError {
    Agent(agent_bridge::agents::AgentError),
    Runtime(String),
}

impl From<agent_bridge::agents::AgentError> for AgentOperationError {
    fn from(error: agent_bridge::agents::AgentError) -> Self {
        Self::Agent(error)
    }
}

impl From<AgentOperationError> for RpcError {
    fn from(error: AgentOperationError) -> Self {
        match error {
            AgentOperationError::Agent(error) => error.into(),
            AgentOperationError::Runtime(message) => Self::operation(&message),
        }
    }
}

impl From<String> for AgentOperationError {
    fn from(error: String) -> Self {
        Self::Runtime(error)
    }
}

impl From<&str> for AgentOperationError {
    fn from(error: &str) -> Self {
        Self::Runtime(error.to_string())
    }
}

impl DesktopRuntime {
    fn require_agent_access(&self) -> Result<(), String> {
        if let Some(error) = &self.agent_access_error {
            return Err(format!(
                "Agent Home access is unavailable to the backend: {error}"
            ));
        }
        self.agent_configuration_enabled()
            .then_some(())
            .ok_or_else(|| "Agent Home access is required".to_string())
    }

    pub(super) fn agent_configuration_enabled(&self) -> bool {
        self.agent_configuration
    }

    pub(super) fn projector(&self, endpoint: &str) -> Result<Projector, String> {
        let projector = {
            #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
            {
                Projector::new_for_home(
                    self.agent_home
                        .clone()
                        .ok_or("Agent Home access is unavailable to the backend")?,
                    app_data_dir()?,
                    self.helper_path.clone(),
                    endpoint,
                    self.local_state.clone(),
                )?
            }
            #[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
            {
                Projector::new(self.helper_path.clone(), endpoint, self.local_state.clone())?
            }
        };
        Ok(
            if cfg!(all(target_os = "macos", feature = "mac-app-store")) {
                projector.with_home_credentials()
            } else {
                projector
            },
        )
    }

    pub(super) fn current_projector(&self) -> Result<Projector, String> {
        self.projector(&self.manager.local_api()?.endpoint)
    }

    // Call under agent_policy so scans cannot reauthorize during recovery.
    pub(super) fn publish_agent_tokens(&self, tokens: TokenSet) -> Result<bool, String> {
        let protected = self.manager.snapshot()?.is_protected() && self.proxy.session().verified;
        self.proxy.set_tokens(with_client_token(
            if protected {
                tokens
            } else {
                TokenSet::default()
            },
            &self.credentials,
        )?);
        Ok(protected)
    }

    pub(super) fn reload_agent_tokens(&self) -> Result<(), String> {
        if !self.agent_configuration_enabled() {
            self.proxy
                .set_tokens(with_client_token(TokenSet::default(), &self.credentials)?);
            return Ok(());
        }
        let projector = self.current_projector()?;
        projector.initialize_store()?;
        if !crate::recovery::connection_intended(&self.manager.snapshot()?) {
            let failures = projector.reconcile(None)?;
            if !failures.is_empty() {
                return Err(agent_failures(failures));
            }
        }
        let (_, tokens) = projector.scan(None)?;
        self.publish_agent_tokens(tokens)?;
        Ok(())
    }

    pub(super) fn initialize_startup_tokens(&self) {
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

    pub async fn refresh_catalog(self: &Arc<Self>) -> Result<AppState, String> {
        self.manager.clone().refresh_catalog().await
    }

    pub fn list_agents(&self) -> Result<Vec<AgentStatus>, String> {
        if let Some(error) = &self.agent_access_error {
            let _guard = self
                .agent_policy
                .lock()
                .map_err(|_| "Agent state unavailable")?;
            self.publish_agent_tokens(TokenSet::default())?;
            return Err(format!(
                "Agent detection cannot access the authorized Home folder: {error}. Re-enable Agent integrations and try again."
            ));
        }
        if !self.agent_configuration_enabled() {
            let _guard = self
                .agent_policy
                .lock()
                .map_err(|_| "Agent state unavailable")?;
            self.publish_agent_tokens(TokenSet::default())?;
            return Ok(Vec::new());
        }
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
        if !self.manager.snapshot()?.is_protected() || !session.verified {
            for status in &mut statuses {
                status.authorized = false;
            }
        }
        if self.instance.is_none() {
            return Ok(statuses);
        }
        if let Some(catalog) = catalog.as_ref() {
            if let Some(codex) = statuses
                .iter_mut()
                .find(|status| status.id == Agent::Codex.id() && status.authorized)
            {
                if let Err(error) = projector.sync_codex_catalog(catalog) {
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
        self.publish_agent_tokens(tokens)?;
        Ok(statuses)
    }

    pub fn preview_agent(
        &self,
        agent_id: String,
        connect: bool,
        options: ConnectOptions,
    ) -> Result<AgentPreview, AgentOperationError> {
        self.require_agent_access()?;
        let agent = Agent::from_id(&agent_id)?;
        let catalog = self.connection_catalog(agent, connect)?;
        Ok(self
            .current_projector()?
            .preview(agent, connect, catalog.as_ref(), &options)?)
    }

    pub fn apply_agent(
        &self,
        agent_id: String,
        connect: bool,
        revision: String,
        options: ConnectOptions,
    ) -> Result<AgentStatus, AgentOperationError> {
        self.require_agent_access()?;
        let _operation = self.configuration_change()?;
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        if self.exiting.load(Ordering::Acquire) {
            return Err("The app is closing".into());
        }
        if self.instance.is_none() {
            return Err("Another app instance owns the agent configurations".into());
        }
        let agent = Agent::from_id(&agent_id)?;
        let catalog = self.connection_catalog(agent, connect)?;
        let projector = self.current_projector()?;
        if !connect {
            self.proxy
                .set_tokens(self.proxy.tokens().without(agent.id()));
        }
        let mut status = projector.apply(agent, connect, &revision, catalog.as_ref(), &options)?;
        status.authorized &= self.publish_agent_tokens(projector.scan(None)?.1)?;
        Ok(status)
    }

    pub fn disconnect_all_agents(&self) -> Result<Vec<AgentStatus>, String> {
        self.require_agent_access()?;
        let _operation = self.configuration_change()?;
        self.disconnect_all_agents_inner()
    }

    pub(super) fn disconnect_all_agents_inner(&self) -> Result<Vec<AgentStatus>, String> {
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
                self.publish_agent_tokens(tokens)?;
                if failures.is_empty() {
                    Ok(statuses)
                } else {
                    Err(agent_failures(failures))
                }
            }
        }
    }

    pub(super) fn reconcile_agents(&self) -> Result<(), String> {
        if !self.agent_configuration_enabled() {
            let _guard = self
                .agent_policy
                .lock()
                .map_err(|_| "Agent state unavailable")?;
            self.publish_agent_tokens(TokenSet::default())?;
            return Ok(());
        }
        if self.instance.is_none() {
            return Ok(());
        }
        let _guard = self
            .agent_policy
            .lock()
            .map_err(|_| "Agent state unavailable")?;
        let state = self.manager.snapshot()?;
        let session = self.proxy.session();
        let protected = state.is_protected() && session.verified;
        if !protected && crate::recovery::connection_intended(&state) {
            self.publish_agent_tokens(TokenSet::default())?;
            return Ok(());
        }
        let catalog = if protected { session.catalog } else { None };
        if protected && !self.recovery.agents_ready() {
            return Ok(());
        }
        let projector = self.current_projector()?;
        let outcome = projector.reconcile(catalog.as_ref());
        self.recovery.agents_finished(
            outcome
                .as_ref()
                .map_or(true, |failures| !failures.is_empty()),
        );
        let failures = outcome?;
        let tokens = projector.scan(catalog.as_ref())?.1;
        self.publish_agent_tokens(tokens)?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(agent_failures(failures))
        }
    }

    pub(super) fn connection_catalog(
        &self,
        agent: Agent,
        connect: bool,
    ) -> Result<Option<Catalog>, String> {
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
        if !state.is_protected() {
            return Ok(None);
        }
        let session = self.proxy.session();
        match (session.verified, session.catalog) {
            (true, Some(catalog)) => Ok(Some(catalog)),
            _ => Ok(None),
        }
    }
}
