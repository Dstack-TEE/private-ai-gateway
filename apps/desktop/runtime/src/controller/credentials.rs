use super::*;

impl DesktopRuntime {
    /// Changes `config.toml` and publishes the file status.
    pub(super) fn update_config(
        &self,
        change: impl FnOnce(&mut Config) -> Result<(), String>,
    ) -> Result<Config, String> {
        let result = self.settings.update_config(change);
        self.manager.set_config_files(self.settings.files());
        result
    }

    /// Changes `credentials.toml` and publishes the file status.
    pub(super) fn update_credentials(
        &self,
        change: impl FnOnce(&mut Credentials) -> Result<(), String>,
    ) -> Result<(), String> {
        let result = self.settings.update_credentials(change);
        self.manager.set_config_files(self.settings.files());
        result
    }

    /// Saves or removes a profile's API key.
    pub(super) fn set_profile_key(
        &self,
        profile_id: &str,
        key: Option<&str>,
    ) -> Result<(), String> {
        self.update_credentials(|credentials| {
            match key {
                Some(key) => {
                    credentials.profiles.insert(
                        profile_id.to_string(),
                        crate::settings::ProfileCredential {
                            api_key: key.to_string(),
                        },
                    );
                }
                None => {
                    credentials.profiles.shift_remove(profile_id);
                }
            }
            Ok(())
        })
    }

    /// Publishes the saved profiles without touching the session.
    pub(super) fn publish_profiles(&self) -> Result<StartConfig, String> {
        let snapshot = self.settings.snapshot()?;
        let config = snapshot.config.runtime_config();
        self.manager.update_profile_list(
            self.settings.profile_views(&snapshot),
            snapshot.config.active_profile.clone(),
            config.clone(),
        );
        Ok(config)
    }

    /// Publishes a changed service configuration; verification state resets.
    pub(super) fn publish_service_configuration(
        &self,
        retain_catalog: bool,
    ) -> Result<StartConfig, String> {
        let snapshot = self.settings.snapshot()?;
        let config = snapshot.config.runtime_config();
        let profiles = self.settings.profile_views(&snapshot);
        let credential_saved = profiles.iter().any(|profile| {
            profile.id == snapshot.config.active_profile && profile.credential_saved
        });
        self.manager.set_service_configuration(
            config.clone(),
            profiles,
            snapshot.config.active_profile,
            credential_saved,
            retain_catalog,
        );
        Ok(config)
    }

    pub(super) fn load_profile_key(&self, profile_id: &str) -> Result<Option<String>, String> {
        self.settings.profile_key(profile_id)
    }

    pub(super) fn queue_retired(&self, retired: RetiredCredential) -> Result<(), String> {
        self.local_state.update(|state| {
            if let Some(previous) = state
                .account_cleanup
                .values_mut()
                .find(|previous| previous.key == retired.key && previous.action == retired.action)
            {
                *previous = retired;
                return Ok(());
            }
            if state.account_cleanup.len() >= 128 {
                return Err(
                    "Account: Credential cleanup queue is full. Reconnect and retry cleanup."
                        .into(),
                );
            }
            state
                .account_cleanup
                .insert(uuid::Uuid::new_v4().to_string(), retired);
            Ok(())
        })
    }

    pub(super) async fn cleanup_retired(&self) -> Result<(), String> {
        let mut records: Vec<_> = self
            .local_state
            .read()?
            .account_cleanup
            .into_iter()
            .collect();
        if records.is_empty() {
            return Ok(());
        }
        records.sort_by_key(|(_, record)| record.action != "activate");
        let snapshot = self.settings.snapshot()?;
        let selected: Vec<&str> = snapshot
            .config
            .profiles
            .keys()
            .filter_map(|id| snapshot.credentials.profiles.get(id))
            .map(|credential| credential.api_key.as_str())
            .collect();
        let mut finished = Vec::new();
        let mut waiting_activation = std::collections::HashSet::new();
        for (id, record) in records.into_iter().take(4) {
            let in_use = selected.contains(&record.key.as_str());
            if (record.action == "activate" && !in_use) || (record.action == "revoke" && in_use) {
                finished.push(id);
                continue;
            }
            if record.action == "revoke" && waiting_activation.contains(&record.profile_id) {
                continue;
            }
            if record.revoke {
                match crate::account_login::transition_credential(
                    &record.provider,
                    &record.key,
                    &record.action,
                )
                .await
                {
                    Err(_) => {
                        if record.action == "activate" {
                            waiting_activation.insert(record.profile_id);
                        }
                        continue;
                    }
                    Ok(crate::account_login::CredentialTransition::Unavailable)
                        if record.action == "activate" =>
                    {
                        self.manager.report_error("Account: This device authorization is no longer active. Reconnect the account.".into());
                    }
                    _ => {}
                }
            }
            finished.push(id);
        }
        if finished.is_empty() {
            return Ok(());
        }
        self.local_state.update(|state| {
            for id in &finished {
                state.account_cleanup.shift_remove(id);
            }
            Ok(())
        })
    }
}
