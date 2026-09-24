use super::*;
use crate::settings::Snapshot;

impl DesktopRuntime {
    /// The settings in effect, as `config.toml` holds them.
    pub fn settings(&self) -> Result<Config, String> {
        self.settings.config()
    }

    /// Saves one preference; clients apply it themselves.
    pub fn set_preference(
        &self,
        change: desktop_core::protocol::Preference,
    ) -> Result<Config, String> {
        self.update_config(|saved| {
            change.apply(saved);
            Ok(())
        })
    }

    /// Applies edits of `config.toml` or `credentials.toml` made outside the
    /// app, the way the matching management commands would. An invalid file
    /// changes nothing; its error is published with the file status.
    pub(crate) async fn apply_settings_files(self: &Arc<Self>) {
        // Under the lifecycle lock, so switching the settings in effect and
        // applying them is one step for every other operation.
        let _operation = self.lifecycle.lock().await;
        if self.exiting.load(Ordering::Acquire) {
            return;
        }
        let Some((previous, current)) = self.settings.reload() else {
            return;
        };
        if let Err(error) = self.apply_settings(&previous, &current).await {
            self.report_error(format!("Settings: {error}"));
        }
        self.manager.set_config_files(self.settings.files());
    }

    /// Imports this device's 0.1 credential store entries (see
    /// `settings::legacy`). Called once the service is listening, so a slow
    /// or prompting credential store never delays its readiness.
    pub(crate) fn import_legacy_secrets(&self) {
        if !self.local_state.importing() {
            return;
        }
        let result = crate::settings::legacy::import_secrets(
            &self.settings,
            &self.local_state,
            &self.data_dir,
            &crate::settings::legacy::OsKeychain,
        );
        self.local_state.set_importing(false);
        let notices = result.unwrap_or_else(|error| {
            vec![format!(
                "Settings: Saved credentials could not be imported from 0.1: {error}. The import is retried on the next start."
            )]
        });
        self.settings.add_import_notices(&notices);
        for notice in notices {
            self.report_error(notice);
        }
        if let Err(error) = self.publish_profiles() {
            self.report_error(error);
        }
        self.manager.set_config_files(self.settings.files());
    }

    async fn apply_settings(
        self: &Arc<Self>,
        previous: &Snapshot,
        current: &Snapshot,
    ) -> Result<(), String> {
        let (old, new) = (&previous.config, &current.config);
        if old.local_api != new.local_api {
            self.apply_local_api(new.local_api.clone()).await?;
        }
        let key = |snapshot: &Snapshot| {
            snapshot
                .credentials
                .profiles
                .get(&snapshot.config.active_profile)
                .cloned()
        };
        if old.runtime_config() != new.runtime_config()
            || old.active_profile != new.active_profile
            || key(previous) != key(current)
        {
            // As when switching profiles: protection restarts on the new one.
            let state = self.manager.snapshot()?;
            let reconnect = state.session_active
                || (self.manager.is_running()? && !state.configuration_verification);
            if reconnect {
                self.stop_with_reconnect(true)?;
                self.manager.cancel_reconnection();
            }
            self.proxy.set_api_key(None);
            self.recovery.cancel();
            let config = self.publish_service_configuration(false)?;
            if reconnect {
                self.start_inner(config)?;
            }
        } else if old.profiles != new.profiles
            || previous.credentials.profiles != current.credentials.profiles
        {
            self.publish_profiles()?;
        }
        if previous.credentials.web_ui != current.credentials.web_ui {
            self.web_ui
                .set_password(current.credentials.web_ui.password_hash.clone());
        }
        if old.web_ui != new.web_ui || previous.credentials.web_ui != current.credentials.web_ui {
            self.apply_web_ui(&new.web_ui);
        }
        Ok(())
    }
}
