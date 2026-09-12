use super::*;

impl DesktopRuntime {
    pub(super) fn projector(&self, endpoint: &str) -> Result<Projector, String> {
        Projector::new(self.helper_path.clone(), endpoint, self.secrets.clone())
    }

    pub(super) fn current_projector(&self) -> Result<Projector, String> {
        self.projector(&self.manager.local_api()?.endpoint)
    }

    // Call under agent_policy so scans cannot reauthorize during recovery.
    pub(super) fn publish_agent_tokens(&self, tokens: TokenSet) -> Result<bool, String> {
        let protected =
            protection_active(&self.manager.snapshot()?) && self.proxy.session().verified;
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
        let projector = self.current_projector()?;
        projector.migrate_legacy()?;
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
        if !protection_active(&self.manager.snapshot()?) || !session.verified {
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
        self.publish_agent_tokens(tokens)?;
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
        let mut status = projector.apply(agent, connect, &revision, catalog.as_ref(), &options)?;
        if agent == Agent::Codex && connect {
            if let Some(catalog) = catalog.as_ref() {
                self.codex_sync.remember_success(&catalog.revision)?;
            }
        }
        status.authorized &= self.publish_agent_tokens(projector.scan(None)?.1)?;
        Ok(status)
    }

    pub fn disconnect_all_agents(&self) -> Result<Vec<AgentStatus>, String> {
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
                    Err(failures
                        .into_iter()
                        .map(|(agent, error)| format!("{agent}: {error}"))
                        .collect::<Vec<_>>()
                        .join("; "))
                }
            }
        }
    }

    pub(super) fn reconcile_agents(&self) -> Result<(), String> {
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
