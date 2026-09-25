use super::*;

impl DesktopRuntime {
    pub async fn save_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<AppState, Error> {
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
    ) -> Result<AppState, Error> {
        let saved = self
            .persist_configuration(profile, require_production_os, key, None, true)
            .await?;
        self.finish_configuration(saved)
    }

    pub(super) fn finish_configuration(
        self: &Arc<Self>,
        saved: SavedConfiguration<'_>,
    ) -> Result<AppState, Error> {
        if saved.reconnect {
            self.start_inner(saved.config)
        } else {
            Ok(self.manager.snapshot()?)
        }
    }

    pub(super) async fn persist_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
        auth: Option<desktop_core::contracts::ProfileAuth>,
        verify: bool,
    ) -> Result<SavedConfiguration<'_>, Error> {
        let _operation = self.configuration_change()?;
        let initial = self.manager.snapshot()?;
        if initial.status == "verifying" {
            return Err("Wait for the current verification to finish".into());
        }
        let reconnect = initial.session_active
            || (self.manager.is_running()? && !initial.configuration_verification);
        let saved = self.settings.snapshot()?;
        let existing = saved.config.profiles.get(&profile.id).cloned();
        let resolved =
            settings_config::resolve_profile(profile, verify.then(desktop_core::now_secs))?;
        let id = resolved.id;
        let mut candidate = settings_config::Profile {
            name: resolved.name,
            provider: resolved.provider,
            remote_url: resolved.remote_url,
            auth: resolved.auth,
            verified_at: resolved.verified_at,
        };
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
        let stored_key = saved
            .credentials
            .profiles
            .get(&id)
            .map(|credential| credential.api_key.clone());
        let candidate_key = match key {
            Some(key) => settings_config::validate_api_key(&key).map_err(Error::invalid_state)?,
            None if !profile_changed => stored_key
                .clone()
                .ok_or_else(|| Error::invalid_state("Enter an API key"))?,
            None => return Err(Error::invalid_state("Enter an API key for this profile")),
        };
        let config = StartConfig {
            remote_url: candidate.remote_url.clone(),
            require_production_os,
        };

        if reconnect {
            self.stop_with_reconnect(true)?;
            self.manager.cancel_reconnection();
        }
        let previous = self.manager.snapshot()?;

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
                return Err("Configuration verification did not start".into());
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
                    Ok(_) => error.into(),
                    Err(stop_error) => {
                        format!("{error}. The verifier also could not stop: {stop_error}").into()
                    }
                });
            }
            if let Err(error) = stop_result {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error.into());
            }
        } else {
            self.manager.stop_with_reconnect(initial.session_active)?;
            self.proxy.set_api_key(None);
        }

        let retiring = existing
            .as_ref()
            .zip(stored_key.as_ref())
            .filter(|_| replace_key);
        if let Some((old, old_key)) = retiring {
            if let Err(error) = self.queue_retired(RetiredCredential {
                profile_id: id.clone(),
                action: "revoke".into(),
                provider: old.provider,
                key: old_key.clone(),
                revoke: old.provider == ServiceProvider::Redpill
                    && old_key != &candidate_key
                    && matches!(old.auth, desktop_core::contracts::ProfileAuth::OAuth { .. }),
            }) {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        }
        if replace_key {
            if let Err(error) = self.set_profile_key(&id, Some(&candidate_key)) {
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(error);
            }
        }
        if replace_key
            && candidate.provider == ServiceProvider::Redpill
            && matches!(
                candidate.auth,
                desktop_core::contracts::ProfileAuth::OAuth { .. }
            )
        {
            if let Err(error) = self.queue_retired(RetiredCredential {
                profile_id: id.clone(),
                action: "activate".into(),
                provider: candidate.provider,
                key: candidate_key.clone(),
                revoke: true,
            }) {
                let restore = self.set_profile_key(&id, stored_key.as_deref());
                self.proxy.set_api_key(None);
                self.manager.restore_snapshot(previous);
                return Err(restore.err().unwrap_or(error));
            }
        }
        let saved = self.update_config(|settings| {
            settings.upsert(id.clone(), candidate)?;
            settings.active_profile = id.clone();
            settings.require_production_os = require_production_os;
            Ok(())
        });
        if let Err(error) = saved {
            let restore_error = if replace_key {
                self.set_profile_key(&id, stored_key.as_deref()).err()
            } else {
                None
            };
            self.proxy.set_api_key(None);
            self.manager.restore_snapshot(previous);
            return Err(match restore_error {
                Some(restore_error) => format!(
                    "{error}. The previous credential could not be restored: {restore_error}"
                )
                .into(),
                None => error,
            });
        }
        self.recovery.cancel();
        self.publish_service_configuration(verify)?;
        if let Err(error) = self.cleanup_retired().await {
            self.manager.report_error(error.to_string());
        }
        Ok(SavedConfiguration {
            config,
            reconnect,
            _operation,
        })
    }

    pub fn activate_profile(self: &Arc<Self>, profile_id: String) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        let previous = self.manager.snapshot()?;
        if previous.status == "verifying" {
            return Err("Wait for the current verification to finish".into());
        }
        if previous.active_profile_id == profile_id {
            return Ok(previous);
        }
        let reconnect = previous.session_active
            || (self.manager.is_running()? && !previous.configuration_verification);
        if !self.settings.config()?.profiles.contains_key(&profile_id) {
            return Err(Error::invalid_state("Confidential AI profile not found"));
        }
        if reconnect {
            self.stop_with_reconnect(true)?;
            self.manager.cancel_reconnection();
        }
        self.update_config(|settings| {
            settings.active_profile = profile_id;
            Ok(())
        })?;
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        let config = self.publish_service_configuration(false)?;
        if reconnect {
            self.start_inner(config)
        } else {
            Ok(self.manager.snapshot()?)
        }
    }

    pub async fn delete_profile(&self, profile_id: String) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err(Error::invalid_state(
                "Stop protection before deleting a profile",
            ));
        }
        let previous = self.manager.snapshot()?;
        let affects_active = previous.active_profile_id == profile_id;
        let saved = self.settings.snapshot()?;
        let removed = saved
            .config
            .profiles
            .get(&profile_id)
            .ok_or_else(|| Error::invalid_state("Confidential AI profile not found"))?;
        let removed_key = saved
            .credentials
            .profiles
            .get(&profile_id)
            .map(|credential| credential.api_key.clone());
        if removed.provider == ServiceProvider::Redpill
            && matches!(
                removed.auth,
                desktop_core::contracts::ProfileAuth::OAuth { .. }
            )
        {
            if let Some(key) = &removed_key {
                self.queue_retired(RetiredCredential {
                    profile_id: profile_id.clone(),
                    action: "revoke".into(),
                    provider: removed.provider,
                    key: key.clone(),
                    revoke: true,
                })?;
            }
        }
        self.set_profile_key(&profile_id, None)?;
        let deleted = self.update_config(|settings| {
            settings.profiles.shift_remove(&profile_id);
            if settings.active_profile == profile_id {
                settings.active_profile.clear();
            }
            Ok(())
        });
        if let Err(error) = deleted {
            return Err(
                match self.set_profile_key(&profile_id, removed_key.as_deref()) {
                    Ok(()) => error,
                    Err(restore_error) => format!(
                        "{error}. The deleted credential could not be restored: {restore_error}"
                    )
                    .into(),
                },
            );
        }
        self.proxy.set_api_key(None);
        if affects_active {
            self.recovery.cancel();
        }
        self.publish_service_configuration(false)?;
        Ok(self.manager.snapshot()?)
    }

    pub async fn clear_api_key(&self) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err(Error::invalid_state(
                "Stop protection before deleting a profile credential",
            ));
        }
        let state = self.manager.snapshot()?;
        if state.active_profile_id.is_empty() {
            return Err("There is no active Confidential AI profile".into());
        }
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == state.active_profile_id)
            .ok_or("Profile not found")?;
        let previous_key = self.load_profile_key(&profile.id)?;
        if profile.provider == ServiceProvider::Redpill
            && matches!(
                profile.auth,
                desktop_core::contracts::ProfileAuth::OAuth { .. }
            )
        {
            if let Some(key) = &previous_key {
                self.queue_retired(RetiredCredential {
                    profile_id: profile.id.clone(),
                    action: "revoke".into(),
                    provider: profile.provider,
                    key: key.clone(),
                    revoke: true,
                })?;
            }
        }
        self.set_profile_key(&profile.id, None)?;
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        self.publish_profiles()?;
        Ok(self.manager.snapshot()?)
    }

    pub fn import_profiles(
        &self,
        backup: desktop_core::maintenance::ProfileBackup,
    ) -> Result<desktop_core::maintenance::ImportResult, Error> {
        let _operation = self.configuration_change()?;
        let mut result = None;
        self.update_config(|settings| {
            result = Some(backup.merge(settings)?);
            Ok(())
        })?;
        let result = result.ok_or("The profile import did not run")?;
        if result.imported > 0 {
            self.publish_profiles()?;
        }
        Ok(result)
    }
}
