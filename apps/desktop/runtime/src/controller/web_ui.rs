use super::*;
use crate::{
    contracts::WebUiStatus,
    preferences::{self, WebUiConfig},
    web_ui::{self, WebUiLogin},
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
        web_ui::validate(config, self.manager.snapshot()?.local_api.port)?;
        preferences::update(|saved| saved.web_ui = config)?;
        self.apply_web_ui(config);
        self.manager.snapshot()
    }

    /// Closes any running listener, revoking its sessions, then opens the configured one.
    /// Bind failures are reported in state rather than failing the service.
    pub(super) fn apply_web_ui(self: &Arc<Self>, config: WebUiConfig) {
        let previous = self.web_ui.stop();
        let mut status = WebUiStatus {
            enabled: config.enabled,
            port: config.port,
            url: None,
            error: None,
        };
        if config.enabled {
            match self
                .web_ui
                .start(self.clone(), config.port, previous == Some(config.port))
            {
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
