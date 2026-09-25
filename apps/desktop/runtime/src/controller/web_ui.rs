use super::*;
use crate::web_ui;
use desktop_core::{config::WebUiConfig, contracts::WebUiStatus};

const NEEDS_PASSWORD: &str = "Web UI needs a sign-in password. Set one in Settings or with `pap settings set web-ui.password`, then turn it on.";

impl DesktopRuntime {
    pub(crate) fn admission(&self) -> Arc<crate::server::Admission> {
        self.admission.clone()
    }

    pub async fn save_web_ui(self: &Arc<Self>, config: WebUiConfig) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err(Error::invalid_state(
                "Web UI settings can change only in the primary backend instance",
            ));
        }
        let listen =
            settings_config::validate_web_ui(&config, self.manager.snapshot()?.local_api.port)
                .map_err(Error::invalid_state)?;
        if config.enabled && !self.web_ui.has_password() {
            return Err(Error::invalid_state(NEEDS_PASSWORD));
        }
        // Save the normalized address and client host.
        let config = WebUiConfig {
            listen_address: listen.config.listen_address,
            client_host: listen.config.client_host,
            ..config
        };
        self.update_config(|saved| {
            saved.web_ui = config.clone();
            Ok(())
        })?;
        self.apply_web_ui(&config).await;
        Ok(self.manager.snapshot()?)
    }

    /// Closes any running listener, revoking its sessions, then opens the configured one.
    pub(super) async fn apply_web_ui(self: &Arc<Self>, config: &WebUiConfig) {
        self.web_ui.stop().await;
        self.open_web_ui(config);
    }

    /// Opens the configured listener. Invalid saved settings and a missing
    /// password fail closed, and bind failures are reported in state rather
    /// than failing the service.
    pub(super) fn open_web_ui(self: &Arc<Self>, config: &WebUiConfig) {
        let mut status = WebUiStatus::from(config);
        status.password_set = self.web_ui.has_password();
        if config.enabled && !status.password_set {
            status.error = Some(NEEDS_PASSWORD.into());
        } else if config.enabled {
            let started = self
                .manager
                .snapshot()
                .and_then(|state| settings_config::validate_web_ui(config, state.local_api.port))
                .and_then(|listen| self.web_ui.start(self.clone(), &listen));
            match started {
                Ok(url) => status.url = Some(url),
                Err(error) => status.error = Some(error),
            }
        }
        self.manager.set_web_ui(status);
    }

    /// Saves or removes the sign-in password. The listener keeps running, but
    /// every browser session ends.
    pub fn set_web_ui_password(&self, password: Option<String>) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err(Error::invalid_state(
                "Web UI settings can change only in the primary backend instance",
            ));
        }
        let hash = match password {
            Some(password) => {
                Some(web_ui::password::hash(&password).map_err(Error::invalid_state)?)
            }
            None if self.settings.config()?.web_ui.enabled => {
                return Err(Error::invalid_state(
                    "Web UI is on. Turn it off before removing its password.",
                ));
            }
            None => None,
        };
        self.update_credentials(|saved| {
            saved.web_ui.password_hash = hash.clone();
            Ok(())
        })?;
        self.web_ui.set_password(hash);
        let mut status = self.manager.snapshot()?.web_ui;
        status.password_set = self.web_ui.has_password();
        self.manager.set_web_ui(status);
        Ok(self.manager.snapshot()?)
    }
}
