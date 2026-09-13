use super::*;

impl DesktopRuntime {
    pub async fn save_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<GatewayState, String> {
        let saved = self
            .persist_configuration(profile, require_production_os, key, None, false)
            .await?;
        self.finish_configuration(saved)
    }

    pub async fn verify_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<GatewayState, String> {
        let saved = self
            .persist_configuration(profile, require_production_os, key, None, true)
            .await?;
        self.finish_configuration(saved)
    }

    pub(super) fn finish_configuration(
        self: &Arc<Self>,
        saved: SavedConfiguration<'_>,
    ) -> Result<GatewayState, String> {
        if saved.reconnect {
            self.start_inner(saved.config)
        } else {
            self.manager.snapshot()
        }
    }

    pub(super) async fn persist_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
        auth: Option<crate::contracts::ProfileAuth>,
        verify: bool,
    ) -> Result<SavedConfiguration<'_>, String> {
        let _operation = self.configuration_change()?;
        let initial = self.manager.snapshot()?;
        if initial.status == "verifying" {
            return Err("Wait for the current verification to finish".to_string());
        }
        let reconnect = initial.session_active
            || (self.manager.is_running()? && !initial.configuration_verification);
        let initial_settings = service_config::settings_from_state(
            initial.profiles.clone(),
            initial.active_profile_id.clone(),
            initial.config.require_production_os,
        )?;
        let existing = initial_settings
            .profiles
            .iter()
            .find(|entry| entry.id == profile.id)
            .cloned();
        let mut candidate =
            service_config::resolve_profile(profile, verify.then(service_config::now_secs))?;
        if let Some(auth) = auth {
            candidate.auth = auth;
        } else if key.is_none() {
            if let Some(existing) = &existing {
                candidate.auth = existing.auth.clone();
            }
        }
        let profile_changed = existing.as_ref().is_none_or(|existing| {
            existing.provider != candidate.provider
                || existing.remote_url != candidate.remote_url
                || existing.auth != candidate.auth
        });
        let replace_key = key.is_some();
        candidate.credential_ref = if replace_key {
            Some(format!("credential-{}", uuid::Uuid::new_v4()))
        } else {
            existing.as_ref().and_then(|p| p.credential_ref.clone())
        };
        let candidate_entry = service_config::profile_credential_entry(&candidate)?;
        let previous_entry = existing
            .as_ref()
            .map(service_config::profile_credential_entry)
            .transpose()?;
        let stored_candidate_key = match &previous_entry {
            Some(entry) => self.secrets.get(entry)?,
            None => None,
        };
        let candidate_key = match key {
            Some(key) => validate_api_key(&key)?,
            None if !profile_changed => stored_candidate_key
                .clone()
                .ok_or_else(|| "Enter an API key".to_string())?,
            None => return Err("Enter an API key for this profile".to_string()),
        };
        let current = self.manager.snapshot()?;
        let mut settings = service_config::settings_from_state(
            current.profiles,
            current.active_profile_id,
            current.config.require_production_os,
        )?;
        let config = StartGatewayConfig {
            remote_url: candidate.remote_url.clone(),
            require_production_os,
        };
        candidate.credential_saved = Some(true);
        settings.upsert(candidate.clone())?;
        settings.active_profile_id = candidate.id.clone();
        settings.require_production_os = require_production_os;

        if reconnect {
            self.stop_with_reconnect(true)?;
            self.manager.cancel_reconnection();
        }
        let previous = self.manager.snapshot()?;

        self.codex_sync.reset()?;
        if verify {
            self.proxy.set_api_key(Some(candidate_key.clone()));
            let started = match self
                .manager
                .clone()
                .begin_verification(config.clone(), profile_changed)
            {
                Ok(state) => state,
                Err(error) => {
                    self.proxy.set_api_key(None);
                    return Err(error);
                }
            };
            let Some(session_id) = started.session_id.clone() else {
                let _ = self.manager.stop();
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err("Configuration verification did not start".to_string());
            };
            let verified = self
                .manager
                .wait_for_verification(&session_id, std::time::Duration::from_secs(45))
                .await;
            let stop_result = self.manager.stop_with_reconnect(initial.session_active);
            if let Err(error) = verified {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(match stop_result {
                    Ok(_) => error,
                    Err(stop_error) => {
                        format!("{error}. The verifier also could not stop: {stop_error}")
                    }
                });
            }
            if let Err(error) = stop_result {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        } else {
            self.manager.stop_with_reconnect(initial.session_active)?;
            self.proxy.set_api_key(None);
        }

        let retiring = existing
            .as_ref()
            .zip(stored_candidate_key.as_ref())
            .filter(|_| replace_key);
        if let Some((old, old_key)) = retiring {
            if let Err(error) = self.queue_retired(RetiredCredential {
                profile_id: candidate.id.clone(),
                action: "revoke".into(),
                provider: old.provider.clone(),
                key: old_key.clone(),
                entry: service_config::profile_credential_entry(old)?,
                revoke: old.provider == ServiceProvider::Redpill
                    && old_key != &candidate_key
                    && matches!(old.auth, crate::contracts::ProfileAuth::OAuth { .. }),
            }) {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        }
        if replace_key {
            if let Err(error) = self.secrets.set(&candidate_entry, &candidate_key) {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        }
        if replace_key
            && candidate.provider == ServiceProvider::Redpill
            && matches!(candidate.auth, crate::contracts::ProfileAuth::OAuth { .. })
        {
            if let Err(error) = self.queue_retired(RetiredCredential {
                profile_id: candidate.id.clone(),
                action: "activate".into(),
                provider: candidate.provider.clone(),
                key: candidate_key.clone(),
                entry: candidate_entry.clone(),
                revoke: true,
            }) {
                let cleanup = self.secrets.delete(&candidate_entry);
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(cleanup.err().unwrap_or(error));
            }
        }
        let settings = match service_config::save(settings) {
            Ok(settings) => settings,
            Err(error) => {
                let restore_error = if replace_key {
                    self.secrets.delete(&candidate_entry).err()
                } else {
                    None
                };
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(match restore_error {
                    Some(restore_error) => format!(
                        "{error}. The previous credential could not be restored: {restore_error}"
                    ),
                    None => error,
                });
            }
        };
        self.recovery.cancel();
        self.manager.set_service_configuration(
            config.clone(),
            settings.profiles,
            settings.active_profile_id,
            true,
            verify,
        );
        if let Err(error) = self.cleanup_retired().await {
            self.manager.report_error(error);
        }
        Ok(SavedConfiguration {
            config,
            reconnect,
            _operation,
        })
    }

    pub fn activate_profile(self: &Arc<Self>, profile_id: String) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        let previous = self.manager.snapshot()?;
        if previous.status == "verifying" {
            return Err("Wait for the current verification to finish".to_string());
        }
        if previous.active_profile_id == profile_id {
            return Ok(previous);
        }
        let reconnect = previous.session_active
            || (self.manager.is_running()? && !previous.configuration_verification);
        let mut settings = service_config::settings_from_state(
            previous.profiles,
            previous.active_profile_id,
            previous.config.require_production_os,
        )?;
        let profile = settings
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
            .ok_or_else(|| "Confidential AI profile not found".to_string())?;
        if reconnect {
            self.stop_with_reconnect(true)?;
            self.manager.cancel_reconnection();
        }
        settings.active_profile_id = profile.id;
        let settings = service_config::save(settings)?;
        let config = settings.runtime_config()?;
        let credential_saved = settings
            .active_profile()
            .is_ok_and(service_config::profile_has_credential);
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        self.manager.set_service_configuration(
            config.clone(),
            settings.profiles,
            settings.active_profile_id,
            credential_saved,
            false,
        );
        if reconnect {
            self.start_inner(config)
        } else {
            self.manager.snapshot()
        }
    }

    pub async fn delete_profile(&self, profile_id: String) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err("Stop protection before deleting a profile".to_string());
        }
        let previous = self.manager.snapshot()?;
        let affects_active = previous.active_profile_id == profile_id;
        let mut settings = service_config::settings_from_state(
            previous.profiles,
            previous.active_profile_id,
            previous.config.require_production_os,
        )?;
        let removed = settings
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
            .ok_or_else(|| "Confidential AI profile not found".to_string())?;
        let entry = service_config::profile_credential_entry(&removed)?;
        let removed_key = self.secrets.get(&entry)?;
        if removed.provider == ServiceProvider::Redpill
            && matches!(removed.auth, crate::contracts::ProfileAuth::OAuth { .. })
        {
            if let Some(key) = &removed_key {
                self.queue_retired(RetiredCredential {
                    profile_id: removed.id.clone(),
                    action: "revoke".into(),
                    provider: removed.provider.clone(),
                    key: key.clone(),
                    entry: entry.clone(),
                    revoke: true,
                })?;
            }
        }
        self.secrets.delete(&entry)?;
        settings.profiles.retain(|profile| profile.id != profile_id);
        if settings.profiles.is_empty() {
            settings.active_profile_id.clear();
        } else if settings.active_profile_id == profile_id {
            settings.active_profile_id = settings.profiles[0].id.clone();
        }
        let settings = match service_config::save(settings) {
            Ok(settings) => settings,
            Err(error) => {
                let restore_error =
                    restore_secret_entry(&*self.secrets, &entry, removed_key.as_deref()).err();
                return Err(match restore_error {
                    Some(restore_error) => format!(
                        "{error}. The deleted credential could not be restored: {restore_error}"
                    ),
                    None => error,
                });
            }
        };
        let config = settings.runtime_config()?;
        let credential_saved = settings
            .active_profile()
            .is_ok_and(service_config::profile_has_credential);
        self.proxy.set_api_key(None);
        if affects_active {
            self.recovery.cancel();
        }
        self.manager.set_service_configuration(
            config,
            settings.profiles,
            settings.active_profile_id,
            credential_saved,
            false,
        );
        self.manager.snapshot()
    }

    pub async fn clear_api_key(&self) -> Result<GatewayState, String> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err("Stop protection before deleting a profile credential".to_string());
        }
        let state = self.manager.snapshot()?;
        if state.active_profile_id.is_empty() {
            return Err("There is no active Confidential AI profile".to_string());
        }
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == state.active_profile_id)
            .ok_or("Profile not found")?;
        let entry = service_config::profile_credential_entry(profile)?;
        let previous_key = self.secrets.get(&entry)?;
        if let Some(profile) = state
            .profiles
            .iter()
            .find(|p| p.id == state.active_profile_id)
        {
            if profile.provider == ServiceProvider::Redpill
                && matches!(profile.auth, crate::contracts::ProfileAuth::OAuth { .. })
            {
                if let Some(key) = &previous_key {
                    self.queue_retired(RetiredCredential {
                        profile_id: profile.id.clone(),
                        action: "revoke".into(),
                        provider: profile.provider.clone(),
                        key: key.clone(),
                        entry: entry.clone(),
                        revoke: true,
                    })?;
                }
            }
        }
        self.secrets.delete(&entry)?;
        let mut settings = service_config::settings_from_state(
            state.profiles,
            state.active_profile_id.clone(),
            state.config.require_production_os,
        )?;
        if let Some(profile) = settings
            .profiles
            .iter_mut()
            .find(|profile| profile.id == state.active_profile_id)
        {
            profile.credential_saved = Some(false);
        }
        if let Err(error) = service_config::save(settings) {
            let restore = restore_secret_entry(&*self.secrets, &entry, previous_key.as_deref());
            return Err(match restore {
                Ok(()) => error,
                Err(restore_error) => {
                    format!("{error}. The credential could not be restored: {restore_error}")
                }
            });
        }
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        self.manager
            .set_profile_credential_saved(&state.active_profile_id, false);
        self.manager.snapshot()
    }

    pub fn import_profiles(
        &self,
        backup: crate::maintenance::ProfileBackup,
    ) -> Result<crate::maintenance::ImportResult, String> {
        let _operation = self.configuration_change()?;
        let state = self.manager.snapshot()?;
        let mut settings = service_config::settings_from_state(
            state.profiles,
            state.active_profile_id,
            state.config.require_production_os,
        )?;
        let result = backup.merge(&mut settings)?;
        if result.imported > 0 {
            let settings = service_config::save(settings)?;
            let config = settings.runtime_config()?;
            self.manager
                .update_profile_list(settings.profiles, settings.active_profile_id, config);
        }
        Ok(result)
    }
}
