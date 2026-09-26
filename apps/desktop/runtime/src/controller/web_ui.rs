use super::*;
use crate::web_ui::password::{self, Secret};
use desktop_core::{config::WebUiConfig, contracts::WebUiStatus};

const PRIMARY_ONLY: &str = "Web UI settings can change only in the primary backend instance";

impl DesktopRuntime {
    pub(crate) fn admission(&self) -> Arc<crate::server::Admission> {
        self.admission.clone()
    }

    pub async fn save_web_ui(self: &Arc<Self>, config: WebUiConfig) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err(Error::invalid_state(PRIMARY_ONLY));
        }
        let listen =
            settings_config::validate_web_ui(&config, self.manager.snapshot()?.local_api.port)
                .map_err(Error::invalid_state)?;
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

    /// Generates the password if there is none, then opens the configured
    /// listener. Invalid saved settings fail closed, and bind failures are
    /// reported in state rather than failing the service.
    pub(super) fn open_web_ui(self: &Arc<Self>, config: &WebUiConfig) {
        let generated = self.ensure_web_ui_password();
        let mut status = WebUiStatus::from(config);
        if config.enabled {
            let started = generated
                .map_err(|error| error.to_string())
                .and_then(|()| self.manager.snapshot())
                .and_then(|state| settings_config::validate_web_ui(config, state.local_api.port))
                .and_then(|listen| self.web_ui.start(self.clone(), &listen));
            match started {
                Ok(url) => status.url = Some(url),
                Err(error) => status.error = Some(error),
            }
        }
        self.manager.set_web_ui(status);
    }

    /// The sign-in password, or `None` while only the hash an earlier version
    /// kept is set. Without either, it could not be generated or read.
    pub fn web_ui_password(&self) -> Result<Option<String>, Error> {
        let saved = self.settings.snapshot()?.credentials.web_ui;
        if saved.password.is_some() || saved.password_hash.is_some() {
            return Ok(saved.password);
        }
        Err(Error::invalid_state(match self.settings.files().error {
            Some(error) => format!("The web UI password is unavailable: {error}"),
            None => "No web UI password is saved. Run `pap web-ui password rotate` to create one."
                .into(),
        }))
    }

    /// Replaces the password with a generated one; every browser session ends.
    pub fn rotate_web_ui_password(&self) -> Result<String, Error> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err(Error::invalid_state(PRIMARY_ONLY));
        }
        let password = password::generate();
        self.save_web_ui_password(password.clone())?;
        Ok(password)
    }

    /// Saves a chosen password; every browser session ends.
    pub fn set_web_ui_password(&self, password: String) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.instance.is_none() {
            return Err(Error::invalid_state(PRIMARY_ONLY));
        }
        password::validate(&password).map_err(Error::invalid_state)?;
        self.save_web_ui_password(password)?;
        Ok(self.manager.snapshot()?)
    }

    /// Generates a password when none is set, as code-server does on first run.
    fn ensure_web_ui_password(&self) -> Result<(), Error> {
        if cfg!(feature = "web-ui") && !self.web_ui.has_password() {
            self.save_web_ui_password(password::generate())?;
        }
        Ok(())
    }

    /// Saves the password in place of any earlier one or its hash. The
    /// listener keeps running, but every browser session ends.
    fn save_web_ui_password(&self, password: String) -> Result<(), Error> {
        self.update_credentials(|saved| {
            saved.web_ui = crate::settings::WebUiCredential {
                password: Some(password.clone()),
                password_hash: None,
            };
            Ok(())
        })?;
        self.web_ui.set_password(Some(Secret::Password(password)));
        Ok(())
    }
}
