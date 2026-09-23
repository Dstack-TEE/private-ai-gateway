use super::*;
use crate::web_ui;
use desktop_core::{
    contracts::{WebUiLogin, WebUiStatus},
    preferences::{self, WebUiConfig},
};

impl DesktopRuntime {
    pub(crate) fn admission(&self) -> Arc<crate::server::Admission> {
        self.admission.clone()
    }

    pub fn save_web_ui(self: &Arc<Self>, config: WebUiConfig) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err("Web UI settings can change only in the primary backend instance".into());
        }
        let listen = web_ui::validate(&config, self.manager.snapshot()?.local_api.port)?;
        // Save the normalized address and client host.
        let config = WebUiConfig {
            listen_address: listen.config.listen_address,
            client_host: listen.config.client_host,
            ..config
        };
        preferences::update(|saved| saved.web_ui = config.clone())?;
        self.apply_web_ui(&config);
        self.manager.snapshot()
    }

    /// Closes any running listener, revoking its sessions, then opens the configured one.
    /// Invalid saved settings fail closed, and bind failures are reported in state
    /// rather than failing the service.
    pub(super) fn apply_web_ui(self: &Arc<Self>, config: &WebUiConfig) {
        let previous = self.web_ui.stop();
        let mut status = WebUiStatus::from(config);
        if config.enabled {
            let started = self
                .manager
                .snapshot()
                .and_then(|state| web_ui::validate(config, state.local_api.port))
                .and_then(|listen| {
                    // Wait for the closing listener when the new one reuses its port.
                    let reopening = previous.is_some_and(|bind| bind.port() == listen.bind.port());
                    self.web_ui.start(self.clone(), &listen, reopening)
                });
            match started {
                Ok(url) => status.url = Some(url),
                Err(error) => status.error = Some(error),
            }
        }
        self.manager.set_web_ui(status);
    }

    pub fn web_ui_login(&self) -> Result<WebUiLogin, String> {
        let status = self.manager.snapshot()?.web_ui;
        if !status.enabled {
            return Err("Web UI is off. Enable it with `pap settings set webUi true`.".into());
        }
        let url = status.url.ok_or_else(|| {
            format!(
                "Web UI is not listening: {}",
                status
                    .error
                    .unwrap_or_else(|| "the listener is unavailable".into())
            )
        })?;
        Ok(self.web_ui.login(&url))
    }
}
