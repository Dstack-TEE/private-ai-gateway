use agent_bridge::agents::CodexService;
use desktop_core::protocol::{self, ErrorCode};

use super::*;

impl DesktopRuntime {
    fn require_agent_access(&self) -> Result<(), Error> {
        if let Some(error) = &self.agent_access_error {
            return Err(format!("Agent Home access is unavailable to the backend: {error}").into());
        }
        if !self.agent_configuration {
            return Err("Agent Home access is required".into());
        }
        Ok(())
    }

    pub(super) fn projector(&self, endpoint: &str) -> Result<Projector, Error> {
        #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
        let projector = Projector::new_for_home(
            self.agent_home
                .clone()
                .ok_or("Agent Home access is unavailable to the backend")?,
            app_data_dir()?,
            self.helper_path.clone(),
            endpoint,
            self.local_state.clone(),
        )?
        .with_home_credentials();
        #[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
        let projector =
            Projector::new(self.helper_path.clone(), endpoint, self.local_state.clone())?;
        Ok(projector)
    }

    pub(super) fn current_projector(&self) -> Result<Projector, Error> {
        self.projector(&self.manager.local_api()?.endpoint)
    }

    /// Publishes scanned agent tokens for as long as the user's protection
    /// session lasts, verified or not: the Local API refuses a recognized
    /// agent with `gateway_not_verified` until verification succeeds, rather
    /// than telling it to reconnect. Returns whether requests are admitted
    /// now. Call under [`Self::lock_agents`].
    pub(super) fn publish_agent_tokens(&self, tokens: TokenSet) -> Result<bool, Error> {
        let state = self.manager.snapshot();
        let tokens = if state.session_active {
            tokens
        } else {
            TokenSet::default()
        };
        self.proxy.set_tokens(self.credentials.with_token(tokens)?);
        Ok(state.is_protected() && self.proxy.session().verified)
    }

    /// Leaves the Local API accepting only the client key.
    pub(super) fn withdraw_agent_tokens(&self) -> Result<(), Error> {
        self.proxy
            .set_tokens(self.credentials.with_token(TokenSet::default())?);
        Ok(())
    }

    pub(super) fn reload_agent_tokens(&self) -> Result<(), Error> {
        if !self.agent_configuration {
            return self.withdraw_agent_tokens();
        }
        let projector = self.current_projector()?;
        projector.initialize_store()?;
        // Only a session the user ended restores the agents' configuration.
        if !self.manager.snapshot().session_active {
            all_applied(projector.reconcile(None)?)?;
        }
        self.publish_agent_tokens(projector.scan(None)?.1)?;
        Ok(())
    }

    pub(super) fn initialize_startup_tokens(&self) {
        let Err(agent_error) = self.reload_agent_tokens() else {
            return;
        };

        // A stale or unreadable agent connection record must fail closed, but
        // it must not prevent the desktop app from opening so the user can
        // inspect the error and restore the affected configuration.
        let message = match self.withdraw_agent_tokens() {
            Ok(()) => format!(
                "Agent configurations could not be loaded and remain disconnected: {agent_error}"
            ),
            Err(client_error) => {
                self.proxy.set_tokens(TokenSet::default());
                format!(
                    "Agent configurations could not be loaded: {agent_error}. The Local API credential is also unavailable: {client_error}"
                )
            }
        };
        self.report_error(message);
    }

    pub async fn refresh_catalog(self: &Arc<Self>) -> Result<AppState, Error> {
        self.manager.clone().refresh_catalog().await
    }

    pub fn list_agents(&self) -> Result<Vec<AgentStatus>, Error> {
        if self.agent_access_error.is_some() || !self.agent_configuration {
            let _guard = self.lock_agents()?;
            self.withdraw_agent_tokens()?;
            return match &self.agent_access_error {
                Some(error) => Err(format!(
                    "Agent detection cannot access the authorized Home folder: {error}. Re-enable Agent integrations and try again."
                ).into()),
                None => Ok(Vec::new()),
            };
        }
        if let Err(error) = self.reconcile_agents() {
            self.report_once(error);
        }
        // Publish the scan under the same lock as connect/disconnect and key
        // rotation, so an older scan cannot restore revoked credentials.
        let _guard = self.lock_agents()?;
        let session = self.proxy.session();
        let catalog = session.verified.then_some(session.catalog).flatten();
        let projector = self.current_projector()?;
        let (mut statuses, tokens) = projector.scan(catalog.as_ref())?;
        if !self.manager.snapshot().is_protected() || !session.verified {
            for status in &mut statuses {
                status.authorized = false;
            }
        }
        if self.instance.is_none() {
            self.report_agents(&statuses);
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
        self.report_agents(&statuses);
        Ok(statuses)
    }

    /// Agents also change without an app action: one is installed or removed,
    /// or a catalog change needs attention. The backend learns of that only
    /// by scanning, so a scan that differs from the last one publishes a new
    /// agents revision for every client.
    fn report_agents(&self, statuses: &[AgentStatus]) {
        let Ok(mut reported) = self.reported_agents.lock() else {
            return;
        };
        if reported.as_slice() != statuses {
            statuses.clone_into(&mut reported);
            self.manager.agents_changed();
        }
    }

    pub fn preview_agent(
        &self,
        agent_id: String,
        connect: bool,
        options: ConnectOptions,
    ) -> Result<AgentPreview, crate::Error> {
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
    ) -> Result<AgentStatus, crate::Error> {
        self.require_agent_access()?;
        let _operation = self.configuration_change()?;
        let _guard = self.lock_agents()?;
        if self.closing() {
            return Err(crate::Error::closing());
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
        let applied = projector.apply(agent, connect, &revision, catalog.as_ref(), &options);
        self.manager.agents_changed();
        let mut status = applied?;
        status.authorized &= self.publish_agent_tokens(projector.scan(None)?.1)?;
        Ok(status)
    }

    pub fn set_agent_connection(
        &self,
        agent_id: String,
        connect: bool,
    ) -> Result<AgentStatus, crate::Error> {
        let options = ConnectOptions::default();
        let preview = self.preview_agent(agent_id.clone(), connect, options.clone())?;
        self.apply_agent(agent_id, connect, preview.revision, options)
    }

    pub async fn agent_service_running(&self, agent_id: &str) -> Result<bool, Error> {
        Ok(self.agent_service(agent_id)?.running().await)
    }

    pub async fn stop_agent_service(&self, agent_id: &str) -> Result<(), Error> {
        self.agent_service(agent_id)?
            .stop()
            .await
            .map_err(|failure| {
                tracing::warn!("Cannot stop Codex's background service: {failure}");
                protocol::Error::from(failure).into()
            })
    }

    /// Only Codex keeps a background service.
    fn agent_service(&self, agent_id: &str) -> Result<CodexService, Error> {
        if agent_id != Agent::Codex.id() {
            return Err(protocol::Error::new(
                ErrorCode::InvalidRequest,
                "Only Codex has a background service",
            )
            .into());
        }
        self.require_agent_access()?;
        Ok(self.current_projector()?.codex_service())
    }

    pub fn disconnect_all_agents(&self) -> Result<Vec<AgentStatus>, Error> {
        self.require_agent_access()?;
        let _operation = self.configuration_change()?;
        self.disconnect_all_agents_inner()
    }

    pub(super) fn disconnect_all_agents_inner(&self) -> Result<Vec<AgentStatus>, Error> {
        let _guard = self.lock_agents()?;
        if self.instance.is_none() {
            return Err("Another app instance owns the agent configurations".into());
        }
        self.withdraw_agent_tokens()?;
        let projector = self.current_projector()?;
        let disconnected = projector.disconnect_all();
        self.manager.agents_changed();
        let failures = disconnected.map_err(|error| {
            format!("Restore all could not revoke the agents ({error}); access stays revoked until it is retried")
        })?;
        let (statuses, tokens) = projector.scan(None)?;
        self.publish_agent_tokens(tokens)?;
        all_applied(failures)?;
        Ok(statuses)
    }

    pub(super) fn reconcile_agents(&self) -> Result<(), Error> {
        if !self.agent_configuration {
            let _guard = self.lock_agents()?;
            return self.withdraw_agent_tokens();
        }
        if self.instance.is_none() {
            return Ok(());
        }
        let _guard = self.lock_agents()?;
        let state = self.manager.snapshot();
        let session = self.proxy.session();
        let protected = state.is_protected() && session.verified;
        // Until the user ends the session, agents stay pointed at the Local
        // API, which refuses them while protection is not verified: during a
        // restart, a network loss, or a verification failure or block.
        if !protected && state.session_active {
            self.publish_agent_tokens(self.current_projector()?.scan(None)?.1)?;
            return Ok(());
        }
        let catalog = if protected { session.catalog } else { None };
        if let (Some(catalog), Ok(mut verified)) = (&catalog, self.verified_catalog.lock()) {
            *verified = Some(catalog.clone());
        }
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
        self.publish_agent_tokens(projector.scan(catalog.as_ref())?.1)?;
        Ok(all_applied(failures)?)
    }

    /// Points connected agents at the current Local API address; see
    /// `Projector::retarget`.
    pub(super) fn retarget_agents(&self, catalog: &Catalog) -> Result<(), Error> {
        if !self.agent_configuration || self.instance.is_none() {
            return Ok(());
        }
        let _guard = self.lock_agents()?;
        let failures = match self
            .current_projector()
            .and_then(|projector| Ok(projector.retarget(catalog)?))
        {
            Ok(failures) => failures,
            // Agents left on the old address would keep calling it and
            // presenting their tokens there: restore them instead.
            Err(error) => {
                tracing::warn!("Cannot re-project agents to the new Local API address: {error}");
                self.current_projector()?.reconcile(None)?
            }
        };
        self.manager.agents_changed();
        Ok(all_applied(failures)?)
    }

    pub(super) fn connection_catalog(
        &self,
        agent: Agent,
        connect: bool,
    ) -> Result<Option<Catalog>, Error> {
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
        if !self.manager.snapshot().is_protected() {
            return Ok(None);
        }
        let session = self.proxy.session();
        Ok(session.verified.then_some(session.catalog).flatten())
    }
}
