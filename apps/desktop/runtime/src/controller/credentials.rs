use super::*;

impl DesktopRuntime {
    pub(super) fn persist_profile_credential_saved(
        &self,
        profile_id: &str,
        saved: bool,
    ) -> Result<(), String> {
        let state = self.manager.snapshot()?;
        let mut settings = service_config::settings_from_state(
            state.profiles,
            state.active_profile_id,
            state.config.require_production_os,
        )?;
        if service_config::set_profile_credential_saved(&mut settings, profile_id, saved)? {
            service_config::save(settings)?;
        }
        self.manager.set_profile_credential_saved(profile_id, saved);
        Ok(())
    }

    pub(super) fn load_profile_key(&self, profile_id: &str) -> Result<Option<String>, String> {
        let profile = self
            .manager
            .snapshot()?
            .profiles
            .into_iter()
            .find(|p| p.id == profile_id)
            .ok_or("Profile not found")?;
        let entry = service_config::profile_credential_entry(&profile)?;
        let stored_key = self.secrets.get(&entry)?;

        let mut pending = self
            .legacy_credential_pending
            .lock()
            .map_err(|_| "The credential migration state is unavailable".to_string())?;
        if !*pending {
            self.persist_profile_credential_saved(profile_id, stored_key.is_some())?;
            return Ok(stored_key);
        }

        let had_stored_key = stored_key.is_some();
        let legacy_key = self.secrets.get(LEGACY_API_KEY_ENTRY)?;
        let key = stored_key.or(legacy_key.clone());
        let wrote_profile_key = !had_stored_key && legacy_key.is_some();
        if let (true, Some(key)) = (wrote_profile_key, key.as_deref()) {
            self.secrets.set(&entry, key)?;
        }
        if let Err(error) = self.persist_profile_credential_saved(profile_id, key.is_some()) {
            if wrote_profile_key {
                let _ = self.secrets.delete(&entry);
            }
            return Err(format!(
                "The previous Confidential AI credential could not be migrated: {error}"
            ));
        }
        if legacy_key.is_some() {
            if let Err(error) = self.secrets.delete(LEGACY_API_KEY_ENTRY) {
                return Err(format!(
                    "The previous Confidential AI credential was migrated, but its old copy could not be removed: {error}"
                ));
            }
        }
        *pending = false;
        Ok(key)
    }

    pub(super) fn cleanup_manifest(&self) -> Result<Vec<String>, String> {
        let path = app_data_dir()?.join("account-cleanup.pending");
        match std::fs::read_to_string(&path) {
            Ok(value) => match serde_json::from_str(&value) {
                Ok(entries) => Ok(entries),
                Err(_) => {
                    let quarantine =
                        path.with_extension(format!("{}.corrupt", uuid::Uuid::new_v4()));
                    std::fs::rename(&path, quarantine).map_err(|_| {
                        "Account: Cannot isolate damaged credential cleanup manifest."
                    })?;
                    self.manager.report_error("Account: Damaged credential cleanup records were isolated. Review unused Private AI Proxy keys in your provider console.".into());
                    Ok(Vec::new())
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err("Account: Cannot read credential cleanup manifest.".into()),
        }
    }

    pub(super) fn save_cleanup_manifest(&self, entries: &[String]) -> Result<(), String> {
        let path = app_data_dir()?.join("account-cleanup.pending");
        if entries.is_empty() {
            if path.exists() {
                std::fs::remove_file(path).map_err(|_| "Cannot finish credential cleanup")?;
            }
            return Ok(());
        }
        desktop_gateway::agents::write_atomic(
            &path,
            &serde_json::to_string(entries).map_err(|_| "Cannot encode cleanup manifest")?,
            None,
        )
        .map_err(|_| "Cannot save credential cleanup manifest".into())
    }

    pub(super) fn queue_retired(&self, retired: RetiredCredential) -> Result<(), String> {
        let mut entries = self.cleanup_manifest()?;
        for entry in &entries {
            if let Some(value) = self.secrets.get(entry)? {
                let previous: RetiredCredential = serde_json::from_str(&value)
                    .map_err(|_| "Account: Credential cleanup record needs repair.")?;
                if previous.key == retired.key && previous.action == retired.action {
                    // Re-login may select a new local ref for the same stable key.
                    self.secrets.set(
                        entry,
                        &serde_json::to_string(&retired).map_err(|_| "Cannot encode cleanup")?,
                    )?;
                    return Ok(());
                }
            }
        }
        if entries.len() >= 128 {
            return Err(
                "Account: Credential cleanup queue is full. Reconnect and retry cleanup.".into(),
            );
        }
        let entry = format!("account-cleanup-{}", uuid::Uuid::new_v4());
        entries.push(entry.clone());
        // Manifest first: interrupted enqueue leaves at most a missing entry,
        // never a secret with no recoverable cleanup reference.
        self.save_cleanup_manifest(&entries)?;
        self.secrets.set(
            &entry,
            &serde_json::to_string(&retired).map_err(|_| "Cannot encode cleanup")?,
        )
    }

    pub(super) async fn cleanup_retired(&self) -> Result<(), String> {
        let mut records = Vec::new();
        for entry in self.cleanup_manifest()? {
            if let Some(value) = self.secrets.get(&entry)? {
                let record: RetiredCredential = serde_json::from_str(&value)
                    .map_err(|_| "Account: Credential cleanup record needs repair.")?;
                records.push((entry, record));
            }
        }
        records.sort_by_key(|(_, record)| record.action != "activate");
        let profiles = self.manager.snapshot()?.profiles;
        let mut selected = Vec::new();
        for profile in profiles
            .iter()
            .filter(|p| service_config::profile_has_credential(p))
        {
            let entry = service_config::profile_credential_entry(profile)?;
            if let Some(secret) = self.secrets.get(&entry)? {
                selected.push((entry, secret));
            }
        }
        let mut remaining = Vec::new();
        let mut waiting_activation = std::collections::HashSet::new();
        for (index, (entry, record)) in records.into_iter().enumerate() {
            if index >= 4 {
                remaining.push(entry);
                continue;
            }
            let in_use = selected.iter().any(|(_, key)| key == &record.key);
            let referenced = selected.iter().any(|(active, _)| active == &record.entry);
            if (record.action == "activate" && !in_use) || (record.action == "revoke" && in_use) {
                if !referenced {
                    self.secrets.delete(&record.entry)?;
                }
                self.secrets.delete(&entry)?;
                continue;
            }
            if record.action == "revoke" && waiting_activation.contains(&record.profile_id) {
                remaining.push(entry);
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
                        remaining.push(entry);
                        continue;
                    }
                    Ok(crate::account_login::CredentialTransition::Unavailable)
                        if record.action == "activate" =>
                    {
                        self.manager.report_error("Account: This device authorization is no longer active. Sign in again to reconnect.".into());
                    }
                    _ => {}
                }
            }
            if !referenced {
                self.secrets.delete(&record.entry)?;
            }
            self.secrets.delete(&entry)?;
        }
        self.save_cleanup_manifest(&remaining)
    }
}
