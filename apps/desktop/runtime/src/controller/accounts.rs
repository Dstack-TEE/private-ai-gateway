use super::*;

impl DesktopRuntime {
    pub(super) async fn cancel_pending(
        &self,
        pending: &mut crate::account_login::PendingLogin,
    ) -> Result<(), String> {
        let profile =
            self.manager.snapshot()?.profiles.into_iter().find(|p| {
                p.id == pending.profile_id() && service_config::profile_has_credential(p)
            });
        let key = match profile {
            Some(profile) => self
                .secrets
                .get(&service_config::profile_credential_entry(&profile)?)?,
            None => None,
        };
        pending.protect_saved_key(key.as_deref()).await;
        pending.cancel().await
    }

    pub async fn begin_account_login(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
    ) -> Result<crate::account_login::LoginPresentation, String> {
        let mut slot = self.account_login.try_lock().map_err(|_| {
            "Account: An account operation is in progress. Finish it before signing in again."
        })?;
        if let Some(pending) = slot.as_mut() {
            if pending.is_active() && pending.profile_id() != profile.id {
                return Err(
                    "Account: Another sign-in is open. Finish or cancel it in the other window."
                        .into(),
                );
            }
            self.cancel_pending(pending).await?;
        }
        *slot = None;
        let pending = crate::account_login::begin(profile).await?;
        let presentation = pending.presentation.clone();
        *slot = Some(pending);
        Ok(presentation)
    }

    pub async fn poll_account_login(
        self: &Arc<Self>,
        id: String,
    ) -> Result<Option<crate::contracts::AccountLoginDetails>, String> {
        self.account_login
            .lock()
            .await
            .as_mut()
            .ok_or("Account login is no longer active")?
            .poll(&id)
            .await
    }

    pub fn begin_account_save(
        self: &Arc<Self>,
        operation_id: String,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    ) -> Result<crate::contracts::AccountSaveResult, String> {
        use crate::contracts::AccountSaveResult;
        uuid::Uuid::parse_str(&operation_id).map_err(|_| "Invalid save operation ID")?;
        let mut operation = self
            .account_save
            .lock()
            .map_err(|_| "Account save unavailable")?;
        if let Some((previous_id, result)) = operation.as_ref() {
            if previous_id == &operation_id {
                return Ok(result.borrow().clone());
            }
            if matches!(*result.borrow(), AccountSaveResult::Running) {
                return Ok(AccountSaveResult::Failed {
                    error: "Account: Another save is in progress. Wait for it to finish.".into(),
                });
            }
        }
        let (sender, receiver) = tokio::sync::watch::channel(AccountSaveResult::Running);
        *operation = Some((operation_id, receiver));
        let runtime = self.clone();
        // The operation belongs to the service, not to the lifetime of an IPC
        // request. A reconnect can read its final outcome without issuing again.
        tokio::spawn(async move {
            let result = match runtime
                .save_account_login(id, profile, require_production_os, workspace_id)
                .await
            {
                Ok(state) => AccountSaveResult::Complete {
                    state: Box::new(state),
                },
                Err(error) => AccountSaveResult::Failed {
                    error: crate::protocol::RpcError::operation(&error).message,
                },
            };
            sender.send_replace(result);
        });
        Ok(AccountSaveResult::Running)
    }

    pub fn account_save_result(
        &self,
        operation_id: &str,
    ) -> Result<crate::contracts::AccountSaveResult, String> {
        let operation = self
            .account_save
            .lock()
            .map_err(|_| "Account save unavailable")?;
        let (_, result) = operation
            .as_ref()
            .filter(|(id, _)| id == operation_id)
            .ok_or(
                "Account: Save outcome is unavailable. Check the saved profile before retrying.",
            )?;
        let outcome = result.borrow().clone();
        if matches!(outcome, crate::contracts::AccountSaveResult::Running)
            && result.has_changed().is_err()
        {
            return Ok(crate::contracts::AccountSaveResult::Failed {
                error: "Account: Save was interrupted. Check the saved profile before retrying."
                    .into(),
            });
        }
        Ok(outcome)
    }

    pub async fn save_account_login(
        self: &Arc<Self>,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    ) -> Result<GatewayState, String> {
        let mut slot = self.account_login.lock().await;
        let credential = slot
            .as_mut()
            .ok_or("Account login is no longer active")?
            .credential(&id, &profile, workspace_id)
            .await?;
        let saved = self
            .persist_configuration(
                profile,
                require_production_os,
                Some(credential.key.clone()),
                Some(credential.auth),
                false,
            )
            .await?;
        if let Some(pending) = slot.as_mut() {
            pending.mark_saved();
        }
        *slot = None;
        self.finish_configuration(saved)
    }

    pub async fn complete_account_login(
        &self,
        id: String,
        callback_url: String,
    ) -> Result<(), String> {
        self.account_login
            .lock()
            .await
            .as_ref()
            .ok_or("Account login is no longer active")?
            .complete_callback(&id, &callback_url)
            .await
    }

    pub async fn account_details(
        &self,
        profile_id: String,
    ) -> Result<crate::contracts::AccountLoginDetails, String> {
        use crate::contracts::{ProfileAuth, ServiceProvider};
        let state = self.manager.snapshot()?;
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or("Profile not found")?;
        if profile.provider != ServiceProvider::Redpill
            || !matches!(profile.auth, ProfileAuth::OAuth { .. })
        {
            return Err("Sign in with RedPill to select a workspace".into());
        }
        let entry = service_config::profile_credential_entry(profile)?;
        let key = self
            .secrets
            .get(&entry)?
            .ok_or("This profile has no saved credential")?;
        crate::account_login::account_details(&key).await
    }

    pub async fn account_balance(
        &self,
        target: crate::contracts::AccountBalanceTarget,
    ) -> Result<Option<crate::contracts::AccountBalance>, String> {
        use crate::contracts::{AccountBalanceTarget, ProfileAuth};
        match target {
            AccountBalanceTarget::Login { id } => {
                self.balances
                    .get(format!("login:{id}"), async {
                        let (provider, secret) = self
                            .account_login
                            .lock()
                            .await
                            .as_mut()
                            .ok_or("Account login is no longer active")?
                            .balance_credential(&id)
                            .await?;
                        crate::account_login::account_balance(&provider, &secret).await
                    })
                    .await
            }
            AccountBalanceTarget::Profile { profile_id } => {
                let state = self.manager.snapshot()?;
                let profile = state
                    .profiles
                    .iter()
                    .find(|p| p.id == profile_id)
                    .ok_or("Profile not found")?;
                if !matches!(profile.auth, ProfileAuth::OAuth { .. })
                    || !service_config::profile_has_credential(profile)
                {
                    return Err("Sign in with an account to view its balance".into());
                }
                let entry = service_config::profile_credential_entry(profile)?;
                self.balances
                    .get(format!("profile:{profile_id}:{entry}"), async {
                        let key = self
                            .secrets
                            .get(&entry)?
                            .ok_or("This profile has no saved credential")?;
                        crate::account_login::account_balance(&profile.provider, &key).await
                    })
                    .await
            }
        }
    }

    pub async fn cancel_account_login(&self, id: String) -> Result<(), String> {
        let mut slot = self.account_login.lock().await;
        if let Some(pending) = slot
            .as_mut()
            .filter(|pending| pending.presentation.id == id)
        {
            self.cancel_pending(pending).await?;
            *slot = None;
        }
        Ok(())
    }
}
