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
            Ok(self.state())
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
        let initial = self.state();
        if initial.status == VerificationStatus::Verifying {
            return Err(Error::verifying(&initial));
        }
        let reconnect = self.restart_needed(&initial)?;
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
            self.pause_protection()?;
        }
        let previous = self.state();
        // Every failure below leaves the key unloaded and the state as it was.
        let rollback = |error: Error| {
            self.proxy.set_api_key(None);
            self.manager.restore_snapshot(previous.clone());
            error
        };

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
                return Err(rollback("Configuration verification did not start".into()));
            };
            let verified = self
                .manager
                .wait_for_verification(&session_id, std::time::Duration::from_secs(45))
                .await;
            let stop_result = self.manager.stop_with_reconnect(initial.session_active);
            if let Err(error) = verified {
                return Err(rollback(match stop_result {
                    Ok(_) => error,
                    Err(stop_error) => {
                        format!("{error}. The verifier also could not stop: {stop_error}").into()
                    }
                }));
            }
            stop_result.map_err(|error| rollback(error.into()))?;
        } else {
            self.manager.stop_with_reconnect(initial.session_active)?;
            self.proxy.set_api_key(None);
        }

        let retiring = existing
            .as_ref()
            .zip(stored_key.as_ref())
            .filter(|_| replace_key);
        if let Some((old, old_key)) = retiring {
            self.queue_retired(RetiredCredential {
                profile_id: id.clone(),
                action: "revoke".into(),
                provider: old.provider,
                key: old_key.clone(),
                revoke: old_key != &candidate_key && account_key(old.provider, &old.auth),
            })
            .map_err(rollback)?;
        }
        if replace_key {
            self.set_profile_key(&id, Some(&candidate_key))
                .map_err(rollback)?;
        }
        if replace_key && account_key(candidate.provider, &candidate.auth) {
            if let Err(error) = self.queue_retired(RetiredCredential {
                profile_id: id.clone(),
                action: "activate".into(),
                provider: candidate.provider,
                key: candidate_key.clone(),
                revoke: true,
            }) {
                let restore = self.set_profile_key(&id, stored_key.as_deref());
                return Err(rollback(restore.err().unwrap_or(error)));
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
            return Err(rollback(match restore_error {
                Some(restore_error) => format!(
                    "{error}. The previous credential could not be restored: {restore_error}"
                )
                .into(),
                None => error,
            }));
        }
        self.recovery.cancel();
        self.publish_service_configuration(verify)?;
        if let Err(error) = self.cleanup_retired().await {
            self.report_error(error);
        }
        Ok(SavedConfiguration {
            config,
            reconnect,
            _operation,
        })
    }

    pub fn activate_profile(self: &Arc<Self>, profile_id: String) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        let previous = self.state();
        if previous.status == VerificationStatus::Verifying {
            return Err(Error::verifying(&previous));
        }
        if previous.active_profile_id == profile_id {
            return Ok(previous);
        }
        let reconnect = self.restart_needed(&previous)?;
        if !self.settings.config()?.profiles.contains_key(&profile_id) {
            return Err(Error::invalid_state("Confidential AI profile not found"));
        }
        // Protection restarts on the new profile, which could not start
        // without a credential: refuse before anything changes. The app opens
        // the profile's setup instead.
        if reconnect && self.load_profile_key(&profile_id)?.is_none() {
            return Err(Error::invalid_state(
                "This profile has no credential. Add one before switching to it while protection is on.",
            ));
        }
        if reconnect {
            self.pause_protection()?;
        }
        self.update_config(|settings| {
            settings.active_profile = profile_id;
            Ok(())
        })?;
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        let config = self.publish_service_configuration(false)?;
        // The switch is applied: a failure to start protection on the new
        // profile is the resulting status, not a failed switch.
        if reconnect {
            if let Err(error) = self.start_inner(config) {
                self.report_error(error);
            }
        }
        Ok(self.state())
    }

    pub async fn delete_profile(&self, profile_id: String) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err(Error::invalid_state(
                "Stop protection before deleting a profile",
            ));
        }
        let previous = self.state();
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
        self.retire_removed_key(
            &profile_id,
            removed.provider,
            &removed.auth,
            removed_key.as_deref(),
        )?;
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
        Ok(self.state())
    }

    pub async fn clear_api_key(&self) -> Result<AppState, Error> {
        let _operation = self.configuration_change()?;
        if self.manager.is_running()? {
            return Err(Error::invalid_state(
                "Stop protection before deleting a profile credential",
            ));
        }
        let state = self.state();
        if state.active_profile_id.is_empty() {
            return Err("There is no active Confidential AI profile".into());
        }
        let profile = find_profile(&state, &state.active_profile_id).ok_or("Profile not found")?;
        let previous_key = self.load_profile_key(&profile.id)?;
        self.retire_removed_key(
            &profile.id,
            profile.provider,
            &profile.auth,
            previous_key.as_deref(),
        )?;
        self.set_profile_key(&profile.id, None)?;
        self.proxy.set_api_key(None);
        self.recovery.cancel();
        self.publish_profiles()?;
        Ok(self.state())
    }

    /// Queues the revocation of a removed account key; a key entered by hand
    /// is only removed.
    fn retire_removed_key(
        &self,
        profile_id: &str,
        provider: ServiceProvider,
        auth: &ProfileAuth,
        key: Option<&str>,
    ) -> Result<(), Error> {
        match key {
            Some(key) if account_key(provider, auth) => self.queue_retired(RetiredCredential {
                profile_id: profile_id.to_string(),
                action: "revoke".into(),
                provider,
                key: key.to_string(),
                revoke: true,
            }),
            _ => Ok(()),
        }
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
