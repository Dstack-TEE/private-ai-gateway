//! Account authorization stays in the runtime; only presentation crosses IPC.
mod billing;
mod http;
mod phala;
mod redpill;
pub(crate) use billing::account_balance;
#[cfg(test)]
use billing::*;
pub use billing::{account_details, organization_url, top_up_url};
use http::*;
use phala::*;
pub use redpill::account_return_url;
pub(crate) use redpill::transition_credential;
use redpill::*;

use std::{sync::Arc, time::Duration};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::Html,
    routing::get,
    Router,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use rand::RngCore;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    sync::{oneshot, Mutex},
    task::JoinHandle,
    time::{timeout, Instant},
};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use url::Url;
use uuid::Uuid;

use crate::contracts::{
    AccountBalance, AccountImages, AccountLoginDetails, AccountScope, AccountWorkspace,
    ConfidentialProfileInput, ProfileAuth, ServiceProvider,
};

const REDPILL_CLIENT_ID: &str = "cGrHCOWG3S91oa0A";
const ISSUER: &str = "https://clerk.redpill.ai";
const CALLBACK: &str = "http://127.0.0.1:4181/oauth/callback";
const KEY_URL: &str = "https://service.redpill.ai/api/oauth/key";
const PHALA_API: &str = "https://cloud-api.phala.com";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginPresentation {
    pub id: String,
    pub url: String,
    pub user_code: Option<String>,
}

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
    ) -> Result<Credential, String> {
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
                        return Err("Sign in again to change workspace".into());
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
                    key: desktop_gateway::secrets::validate_api_key(&string(&result, "api_key")?)?,
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
    Authorizing(JoinHandle<Result<Authorization, String>>),
    Authorized(Box<Authorization>),
    Failed(String),
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
    callback: Option<Arc<CallbackState>>,
}

impl PendingLogin {
    pub(crate) fn new(
        presentation: LoginPresentation,
        profile: ConfidentialProfileInput,
        worker: JoinHandle<Result<Authorization, String>>,
    ) -> Self {
        Self {
            presentation,
            profile,
            state: LoginState::Authorizing(worker),
            deadline: Instant::now() + LOGIN_TIMEOUT,
            saved: false,
            callback: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.deadline > Instant::now() && !matches!(self.state, LoginState::Failed(_))
    }

    fn validate(&self, id: &str) -> Result<(), String> {
        if self.presentation.id != id {
            return Err("Account login is no longer active".into());
        }
        if self.deadline <= Instant::now() {
            return Err("Account authorization expired; sign in again".into());
        }
        Ok(())
    }

    async fn resolve(&mut self) -> Result<Option<&mut Authorization>, String> {
        if let LoginState::Authorizing(task) = &mut self.state {
            if !task.is_finished() {
                return Ok(None);
            }
            self.state = match task.await {
                Ok(Ok(authorization)) => LoginState::Authorized(Box::new(authorization)),
                Ok(Err(error)) => LoginState::Failed(error),
                Err(_) => LoginState::Failed("Account login stopped".into()),
            };
        }
        match &mut self.state {
            LoginState::Authorized(authorization) => Ok(Some(authorization)),
            LoginState::Failed(error) => Err(error.clone()),
            LoginState::Authorizing(_) => Ok(None),
        }
    }

    pub async fn poll(&mut self, id: &str) -> Result<Option<AccountLoginDetails>, String> {
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
    ) -> Result<Credential, String> {
        self.validate(id)?;
        let candidate = crate::service_config::resolve_profile(profile.clone(), None)?;
        if candidate.id != self.profile.id
            || candidate.provider != self.profile.provider
            || candidate.remote_url != self.profile.remote_url
        {
            return Err("Sign in again for the selected provider".into());
        }
        let authorization = self.resolve().await?.ok_or("Finish signing in first")?;
        let credential = authorization.issue(profile, workspace_id).await?;
        // Save retries reuse this authorization’s issued key.
        *authorization = Authorization::Inference(credential.clone());
        Ok(credential)
    }

    pub async fn balance_credential(
        &mut self,
        id: &str,
    ) -> Result<(ServiceProvider, String), String> {
        self.validate(id)?;
        let provider = self.profile.provider.clone();
        let secret = self
            .resolve()
            .await?
            .ok_or("Finish signing in first")?
            .balance_secret()
            .to_owned();
        Ok((provider, secret))
    }

    pub async fn complete_callback(&self, id: &str, value: &str) -> Result<(), String> {
        self.validate(id)?;
        let state = self
            .callback
            .as_ref()
            .ok_or("Account: This provider uses a device code, not a callback link.")?;
        if value.len() > 16384 {
            return Err("Account: Callback link is too long.".into());
        }
        let url =
            Url::parse(value.trim()).map_err(|_| "Account: Paste the complete callback URL.")?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port() != Some(4181)
            || url.path() != "/oauth/callback"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("Account: This callback URL does not match the current sign-in.".into());
        }
        let uri: Uri = format!("{}?{}", url.path(), url.query().unwrap_or_default())
            .parse()
            .map_err(|_| "Account: Invalid callback URL.")?;
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:4181".parse().expect("constant host"));
        state
            .accept(&uri, &headers)
            .await
            .map_err(|_| "Account: This callback is invalid or has already been used.".into())
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

    pub async fn cancel(&mut self) -> Result<(), String> {
        let provider = self.profile.provider.clone();
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
                eprintln!("Pending authorization cleanup deferred to server expiry");
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

pub(crate) async fn begin(mut profile: ConfidentialProfileInput) -> Result<PendingLogin, String> {
    profile.remote_url = crate::service_config::resolve_profile(profile.clone(), None)?.remote_url;
    let client = client()?;
    let id = Uuid::new_v4().to_string();
    let (url, user_code, worker, callback_state) = match profile.provider {
        ServiceProvider::Phala => {
            let data = response(
                client
                    .post(format!("{PHALA_API}/api/v1/auth/device/code"))
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
                .map_err(|_| "Authorization expired; sign in again".to_string())?
                .map(Authorization::Inference)
            });
            (url, Some(code), worker, None)
        }
        ServiceProvider::Redpill => {
            let discovery =
                response(client.get(format!("{ISSUER}/.well-known/openid-configuration"))).await?;
            validate_discovery(&discovery)?;
            let listener = TcpListener::bind("127.0.0.1:4181")
                .await
                .map_err(|_| "Login port 4181 is in use; close the other login and retry")?;
            let verifier = random_secret();
            let state = random_secret();
            let mut url = trusted_url(&string(&discovery, "authorization_endpoint")?, ISSUER)?;
            url.query_pairs_mut().extend_pairs([
                ("client_id", REDPILL_CLIENT_ID),
                ("response_type", "code"),
                ("response_mode", "query"),
                ("redirect_uri", CALLBACK),
                ("scope", "openid profile user:org:read"),
                ("state", &state),
                ("code_challenge_method", "S256"),
                (
                    "code_challenge",
                    &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                ),
            ]);
            let token_url = trusted_url(&string(&discovery, "token_endpoint")?, ISSUER)?;
            let (sender, receiver) = oneshot::channel();
            let callback = Arc::new(CallbackState {
                expected: state,
                sender: Mutex::new(Some(sender)),
            });
            let worker_callback = callback.clone();
            let worker = tokio::spawn(async move {
                timeout(
                    LOGIN_TIMEOUT,
                    redpill(
                        client,
                        listener,
                        worker_callback,
                        receiver,
                        verifier,
                        token_url,
                    ),
                )
                .await
                .map_err(|_| "Authorization expired; sign in again".to_string())?
            });
            (url.to_string(), None, worker, Some(callback))
        }
        ServiceProvider::Custom => {
            return Err("Account login is only available for Phala and RedPill".into())
        }
    };
    let mut pending = PendingLogin::new(LoginPresentation { id, url, user_code }, profile, worker);
    pending.callback = callback_state;
    Ok(pending)
}

#[cfg(test)]
mod tests;
