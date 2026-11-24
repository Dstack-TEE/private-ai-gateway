//! Account authorization stays in the runtime; only presentation crosses IPC.
mod billing;
mod http;
mod phala;
mod redpill;
pub(crate) use billing::account_balance;
pub use billing::account_details;
#[cfg(test)]
use billing::*;
use http::*;
use phala::*;
pub(crate) use redpill::transition_credential;
use redpill::*;

use std::time::Duration;

use axum::http::StatusCode;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{
    task::JoinHandle,
    time::{timeout, Instant},
};
use url::Url;
use uuid::Uuid;

use crate::Error;

use desktop_core::account::LoginPresentation;
#[cfg(test)]
use desktop_core::account::{organization_url, top_up_url};
use desktop_core::contracts::{
    AccountBalance, AccountImages, AccountLoginDetails, AccountScope, AccountWorkspace,
    ConfidentialProfileInput, ProfileAuth, ServiceProvider,
};

const REDPILL_CLIENT_ID: &str = "cGrHCOWG3S91oa0A";
const ISSUER: &str = "https://clerk.redpill.ai";
/// Where Clerk's device authorization sends the user to enter the code.
const VERIFICATION_ORIGIN: &str = "https://accounts.redpill.ai";
const KEY_URL: &str = "https://service.redpill.ai/api/oauth/key";
const ACCOUNT_URL: &str = "https://service.redpill.ai/api/oauth/account";
const PHALA_API: &str = "https://cloud-api.phala.com";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Clone)]
pub(crate) struct Credential {
    pub key: String,
    pub auth: ProfileAuth,
}

pub(crate) enum Authorization {
    Inference(Credential),
    Redpill {
        access_token: String,
        details: AccountLoginDetails,
    },
}

impl Authorization {
    fn details(&self) -> AccountLoginDetails {
        match self {
            Self::Inference(key) => AccountLoginDetails {
                auth: key.auth.clone(),
                workspaces: Vec::new(),
            },
            Self::Redpill { details, .. } => details.clone(),
        }
    }

    async fn issue(
        &self,
        profile: &ConfidentialProfileInput,
        workspace_id: Option<i64>,
    ) -> Result<Credential, Error> {
        match self {
            Self::Inference(key) => {
                if profile.provider == ServiceProvider::Redpill {
                    let saved = match &key.auth {
                        ProfileAuth::OAuth {
                            scope: Some(scope), ..
                        } => scope.workspace_id,
                        _ => None,
                    };
                    if saved != workspace_id {
                        return Err("Reconnect RedPill to change workspace".into());
                    }
                }
                Ok(key.clone())
            }
            Self::Redpill {
                access_token,
                details,
            } => {
                let selected = workspace_id
                    .filter(|id| details.workspaces.iter().any(|w| w.id == *id))
                    .ok_or("Choose a workspace before saving")?;
                let result = response(client()?.post(KEY_URL).bearer_auth(access_token).json(
                    &json!({"installation_id": installation_id(&profile.id)?.to_string(), "name": profile.name, "workspace_id": selected}),
                )).await?;
                if result.get("workspace_id").and_then(Value::as_i64) != Some(selected) {
                    return Err("Unexpected workspace in the account response".into());
                }
                let organization = string(&result, "account_name")?;
                Ok(Credential {
                    key: desktop_core::config::validate_api_key(&string(&result, "api_key")?)?,
                    auth: ProfileAuth::OAuth {
                        account_id: string(&result, "account_id")?,
                        account_name: match &details.auth {
                            ProfileAuth::OAuth { account_name, .. } => account_name.clone(),
                            _ => None,
                        },
                        images: match &details.auth {
                            ProfileAuth::OAuth { images, .. } => images.clone(),
                            _ => None,
                        },
                        scope: Some(Box::new(AccountScope {
                            organization_id: match &details.auth {
                                ProfileAuth::OAuth { scope, .. } => {
                                    scope.as_ref().and_then(|s| s.organization_id.clone())
                                }
                                _ => None,
                            },
                            organization_slug: match &details.auth {
                                ProfileAuth::OAuth { scope, .. } => {
                                    scope.as_ref().and_then(|s| s.organization_slug.clone())
                                }
                                _ => None,
                            },
                            organization: Some(organization),
                            workspace: Some(string(&result, "workspace_name")?),
                            workspace_slug: None,
                            workspace_id: Some(selected),
                        })),
                    },
                })
            }
        }
    }

    fn balance_secret(&self) -> &str {
        match self {
            Self::Inference(credential) => &credential.key,
            Self::Redpill { access_token, .. } => access_token,
        }
    }
}

enum LoginState {
    Authorizing(JoinHandle<Result<Authorization, Error>>),
    Authorized(Box<Authorization>),
    Failed(Error),
}

impl Drop for LoginState {
    fn drop(&mut self) {
        if let Self::Authorizing(task) = self {
            task.abort();
        }
    }
}

pub(crate) struct PendingLogin {
    pub presentation: LoginPresentation,
    profile: ConfidentialProfileInput,
    state: LoginState,
    deadline: Instant,
    saved: bool,
}

impl PendingLogin {
    pub(crate) fn new(
        presentation: LoginPresentation,
        profile: ConfidentialProfileInput,
        worker: JoinHandle<Result<Authorization, Error>>,
    ) -> Self {
        Self {
            presentation,
            profile,
            state: LoginState::Authorizing(worker),
            deadline: Instant::now() + LOGIN_TIMEOUT,
            saved: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.deadline > Instant::now() && !matches!(self.state, LoginState::Failed(_))
    }

    fn validate(&self, id: &str) -> Result<(), Error> {
        if self.presentation.id != id {
            return Err("Account connection is no longer active".into());
        }
        if self.deadline <= Instant::now() {
            return Err("Account authorization expired; reconnect the account".into());
        }
        Ok(())
    }

    async fn resolve(&mut self) -> Result<Option<&mut Authorization>, Error> {
        if let LoginState::Authorizing(task) = &mut self.state {
            if !task.is_finished() {
                return Ok(None);
            }
            self.state = match task.await {
                Ok(Ok(authorization)) => LoginState::Authorized(Box::new(authorization)),
                Ok(Err(error)) => LoginState::Failed(error),
                Err(_) => LoginState::Failed("Account connection stopped".into()),
            };
        }
        match &mut self.state {
            LoginState::Authorized(authorization) => Ok(Some(authorization)),
            LoginState::Failed(error) => Err(error.clone()),
            LoginState::Authorizing(_) => Ok(None),
        }
    }

    pub async fn poll(&mut self, id: &str) -> Result<Option<AccountLoginDetails>, Error> {
        self.validate(id)?;
        Ok(self
            .resolve()
            .await?
            .map(|authorization| authorization.details()))
    }

    pub async fn credential(
        &mut self,
        id: &str,
        profile: &ConfidentialProfileInput,
        workspace_id: Option<i64>,
    ) -> Result<Credential, Error> {
        self.validate(id)?;
        let candidate = desktop_core::config::resolve_profile(profile.clone(), None)?;
        if candidate.id != self.profile.id
            || candidate.provider != self.profile.provider
            || candidate.remote_url != self.profile.remote_url
        {
            return Err("Reconnect the selected provider".into());
        }
        let authorization = self.resolve().await?.ok_or("Finish connecting first")?;
        let credential = authorization.issue(profile, workspace_id).await?;
        // Save retries reuse this authorization’s issued key.
        *authorization = Authorization::Inference(credential.clone());
        Ok(credential)
    }

    pub async fn balance_credential(
        &mut self,
        id: &str,
    ) -> Result<(ServiceProvider, String), Error> {
        self.validate(id)?;
        let provider = self.profile.provider;
        let secret = self
            .resolve()
            .await?
            .ok_or("Finish connecting first")?
            .balance_secret()
            .to_owned();
        Ok((provider, secret))
    }

    pub fn profile_id(&self) -> &str {
        &self.profile.id
    }

    pub async fn protect_saved_key(&mut self, saved_key: Option<&str>) {
        if let Ok(Some(Authorization::Inference(credential))) = self.resolve().await {
            if saved_key == Some(&credential.key) {
                self.saved = true;
            }
        }
    }

    pub fn mark_saved(&mut self) {
        self.saved = true;
    }

    pub async fn cancel(&mut self) -> Result<(), Error> {
        let provider = self.profile.provider;
        if self.saved || provider != ServiceProvider::Redpill {
            return Ok(());
        }
        let action = "abort";
        if let Ok(Some(Authorization::Inference(credential))) = self.resolve().await {
            if transition_credential(&provider, &credential.key, action)
                .await
                .is_err()
            {
                // Pending credentials also expire server-side. Offline cleanup
                // must not trap the user in an editor or prevent a fresh login.
                tracing::info!("Pending authorization cleanup deferred to server expiry");
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum CredentialTransition {
    Applied,
    Unavailable,
}

pub(crate) async fn begin(mut profile: ConfidentialProfileInput) -> Result<PendingLogin, Error> {
    profile.remote_url = desktop_core::config::resolve_profile(profile.clone(), None)?.remote_url;
    let client = client()?;
    let id = Uuid::new_v4().to_string();
    let (url, user_code, worker) = match profile.provider {
        ServiceProvider::Phala => {
            let data = response(
                client
                    .post(desktop_core::endpoint(
                        PHALA_API,
                        &["api", "v1", "auth", "device", "code"],
                    )?)
                    .json(&json!({"client_id":"private-ai-proxy", "scope":"redpill:api-key"})),
            )
            .await?;
            let device = string(&data, "device_code")?;
            let code = string(&data, "user_code")?;
            let url = data
                .get("verification_uri_complete")
                .and_then(Value::as_str)
                .or_else(|| data.get("verification_uri").and_then(Value::as_str))
                .ok_or("Missing verification URL")?;
            let url = trusted_url(url, "https://cloud.phala.com")?.to_string();
            let expires = seconds(&data, "expires_in", 900)?;
            let interval = seconds(&data, "interval", 30)?;
            let worker = tokio::spawn(async move {
                timeout(
                    Duration::from_secs(expires),
                    phala(client, device, interval),
                )
                .await
                .map_err(|_| "Authorization expired; reconnect the account".to_string())?
                .map(Authorization::Inference)
            });
            (url, Some(code), worker)
        }
        ServiceProvider::Redpill => {
            let discovery = response(client.get(desktop_core::endpoint(
                ISSUER,
                &[".well-known", "openid-configuration"],
            )?))
            .await?;
            validate_discovery(&discovery)?;
            let oauth = redpill_client(
                DeviceAuthorizationUrl::from_url(trusted_url(
                    &string(&discovery, "device_authorization_endpoint")?,
                    ISSUER,
                )?),
                TokenUrl::from_url(trusted_url(&string(&discovery, "token_endpoint")?, ISSUER)?),
            );
            let userinfo = desktop_core::endpoint(ISSUER, &["oauth", "userinfo"])?;
            let device = device_authorization(&client, &oauth).await?;
            let url = trusted_url(
                device
                    .verification_uri_complete()
                    .map(|url| url.secret().as_str())
                    .unwrap_or(device.verification_uri().as_str()),
                VERIFICATION_ORIGIN,
            )?
            .to_string();
            let code = device.user_code().secret().clone();
            let worker = tokio::spawn(async move {
                timeout(LOGIN_TIMEOUT, async move {
                    redpill(&client, &oauth, &device, userinfo.as_str(), ACCOUNT_URL).await
                })
                .await
                .map_err(|_| "Authorization expired; reconnect the account".to_string())?
            });
            (url, Some(code), worker)
        }
        ServiceProvider::Custom => {
            return Err("Account connection is only available for Phala and RedPill".into())
        }
    };
    Ok(PendingLogin::new(
        LoginPresentation { id, url, user_code },
        profile,
        worker,
    ))
}

#[cfg(test)]
mod tests;
