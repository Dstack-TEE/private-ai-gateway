//! Account authorization stays in the runtime; only presentation crosses IPC.
use std::{sync::Arc, time::Duration};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::Html,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
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
        // Verification retries reuse the issued key instead of rotating it again.
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

pub(crate) async fn transition_credential(
    provider: &ServiceProvider,
    key: &str,
    action: &str,
) -> Result<CredentialTransition, String> {
    if *provider != ServiceProvider::Redpill {
        return Ok(CredentialTransition::Applied);
    }
    transition_at(key, action, KEY_URL).await
}

async fn transition_at(
    key: &str,
    action: &str,
    base: &str,
) -> Result<CredentialTransition, String> {
    let http = client()?;
    let request = match action {
        "activate" | "abort" => http.post(format!("{base}/{action}")),
        "revoke" => http.delete(base),
        _ => return Err("Unsupported account operation".into()),
    };
    let response = request
        .timeout(Duration::from_secs(5))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| "Account: Credential update failed; retry the operation.")?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Ok(CredentialTransition::Unavailable);
    }
    if !response.status().is_success() {
        return Err(
            "Account: Credential update failed; retry or manage the key in your provider console."
                .into(),
        );
    }
    Ok(CredentialTransition::Applied)
}

fn client() -> Result<Client, String> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(35))
        .build()
        .map_err(|_| "Cannot initialize account login".into())
}

async fn request_json(request: reqwest::RequestBuilder) -> Result<(StatusCode, Value), String> {
    let mut result = request
        .send()
        .await
        .map_err(|_| "Account service could not be reached")?;
    let status = result.status();
    // Bound untrusted account responses and never include bodies, tokens or URLs in errors.
    let mut bytes = Vec::new();
    while let Some(chunk) = result
        .chunk()
        .await
        .map_err(|_| "Account response interrupted")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("Account response is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let data = serde_json::from_slice(&bytes).map_err(|_| {
        if status.is_success() {
            "Invalid account response".into()
        } else {
            format!(
                "Account: Service rejected the request (HTTP {}). Retry or contact support.",
                status.as_u16()
            )
        }
    })?;
    Ok((status, data))
}

fn protocol_error(data: &Value) -> Option<&str> {
    data.get("detail")
        .filter(|v| v.is_object())
        .unwrap_or(data)
        .get("error")
        .and_then(Value::as_str)
}

fn account_error(status: StatusCode, data: &Value) -> String {
    if let Some(message) = match protocol_error(data) {
        Some("org_required") => Some("Select an organization on the sign-in page and try again."),
        Some("keys_permission_required" | "organization_permission_required") => Some("Your organization must grant key-management permission before you can connect."),
        Some("rate_limited") => Some("Balance refresh is temporarily limited. Try again in a minute."),
        Some("balance_unavailable") => Some("Balance is temporarily unavailable. Try refreshing later."),
        Some("billing_permission_required") => Some("Your account does not have permission to view this balance."),
        Some("account_mapping_conflict") => Some("Account setup conflicts with an existing account. Contact RedPill support."),
        Some("account_setup_unavailable" | "account_service_unavailable" | "organization_unavailable" | "authorization_unavailable") => Some("Account setup is temporarily unavailable. Retry signing in."),
        Some("device_disabled") => Some("This device key was disabled. Manage it in RedPill Keys before reconnecting."),
        Some("device_migration_required" | "device_credential_mismatch") => Some("This device credential needs repair in RedPill Keys."),
        Some("invalid_authorization" | "invalid_identity" | "credentials_required" | "device_unavailable") => Some("Your authorization is no longer valid. Sign in again."),
        Some("key_store_unavailable") => Some("The credential service is unavailable. Retry saving; your previous credential is unchanged."),
        Some("account_unavailable") => Some("This account is unavailable. Contact your organization administrator."),
        Some("not_available") => Some("Account login is not enabled on this service."),
        _ => None,
    } { return format!("Account: {message}"); }

    match protocol_error(data) {
        Some("access_denied") => "Account: Authorization was declined.".into(),
        Some("expired_token") => "Account: Authorization expired; sign in again.".into(),
        _ => format!(
            "Account: Service rejected the request (HTTP {}). Retry or contact support.",
            status.as_u16()
        ),
    }
}

async fn response(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let (status, data) = request_json(request).await?;
    if !status.is_success() {
        return Err(account_error(status, &data));
    }
    Ok(data)
}

fn string(data: &Value, field: &str) -> Result<String, String> {
    data.get(field)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 16384)
        .map(str::to_owned)
        .ok_or_else(|| format!("Invalid account response: {field}"))
}

fn seconds(data: &Value, field: &str, max: u64) -> Result<u64, String> {
    data.get(field)
        .and_then(Value::as_u64)
        .filter(|v| *v > 0 && *v <= max)
        .ok_or_else(|| format!("Invalid account response: {field}"))
}

fn trusted_url(value: &str, origin: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "Invalid authorization URL")?;
    if url.origin().ascii_serialization() != origin
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Untrusted authorization URL".into());
    }
    Ok(url)
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

fn installation_id(profile_id: &str) -> Result<Uuid, String> {
    // Profile IDs are already random and persist with the credential. Deriving a
    // UUID mixes the profile with the local installation identity.
    let data = desktop_gateway::agents::app_data_dir()?;
    let path = data.join("installation-id");
    let device = match std::fs::read_to_string(&path) {
        Ok(value) => uuid::Uuid::parse_str(value.trim())
            .map_err(|_| "Account: Device identity needs repair.")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::new_v4();
            std::fs::write(&path, id.to_string())
                .map_err(|_| "Account: Cannot save device identity.")?;
            id
        }
        Err(_) => return Err("Account: Cannot read device identity.".into()),
    };
    let hash = Sha256::digest(format!("{device}:{profile_id}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    Ok(Uuid::from_bytes(bytes))
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn validate_discovery(data: &Value) -> Result<(), String> {
    for (field, required) in [
        ("grant_types_supported", "authorization_code"),
        ("code_challenge_methods_supported", "S256"),
        ("token_endpoint_auth_methods_supported", "none"),
    ] {
        if !data
            .get(field)
            .and_then(Value::as_array)
            .is_some_and(|v| v.iter().any(|v| v == required))
        {
            return Err("The account service does not support secure desktop login".into());
        }
    }
    if data.get("issuer").and_then(Value::as_str) != Some(ISSUER) {
        return Err("Unexpected account issuer".into());
    }
    Ok(())
}

async fn phala(client: Client, device: String, interval: u64) -> Result<Credential, String> {
    phala_at(client, device, interval, PHALA_API).await
}

async fn phala_at(
    client: Client,
    device: String,
    mut interval: u64,
    base: &str,
) -> Result<Credential, String> {
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let (status, data) = request_json(client.post(format!("{base}/api/v1/auth/device/token"))
            .json(&json!({"device_code":device,"client_id":"private-ai-proxy","grant_type":"urn:ietf:params:oauth:grant-type:device_code"}))).await?;
        if !status.is_success() {
            match protocol_error(&data) {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval = (interval + 5).min(30);
                    continue;
                }
                _ => return Err(account_error(status, &data)),
            }
        }
        let key = desktop_gateway::secrets::validate_api_key(&string(&data, "access_token")?)?;
        let metadata = response(
            client
                .get(format!("{base}/api/v1/private_ai/self"))
                .timeout(Duration::from_secs(5))
                .bearer_auth(&key),
        )
        .await;
        let auth = metadata.and_then(|metadata| {
            let workspace = string(
                metadata
                    .get("workspace")
                    .ok_or("Account: Missing workspace identity")?,
                "name",
            )?;
            let account = string(
                metadata
                    .get("user")
                    .ok_or("Account: Missing account identity")?,
                "username",
            )?;
            Ok(ProfileAuth::OAuth {
                account_id: account.clone(),
                account_name: Some(account),
                images: None,
                scope: Some(Box::new(AccountScope {
                    workspace: Some(workspace),
                    ..AccountScope::default()
                })),
            })
        });
        return Ok(Credential { key, auth: auth? });
    }
}

struct CallbackState {
    expected: String,
    sender: Mutex<Option<oneshot::Sender<Result<String, String>>>>,
}

#[derive(Debug)]
enum CallbackError {
    Invalid,
    Declined,
}

fn callback_code(uri: &Uri, headers: &HeaderMap, expected: &str) -> Result<String, CallbackError> {
    if uri.path() != "/oauth/callback"
        || headers.get("host").and_then(|v| v.to_str().ok()) != Some("127.0.0.1:4181")
    {
        return Err(CallbackError::Invalid);
    }
    let pairs: Vec<_> = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).collect();
    let single = |field: &str| -> Option<&str> {
        let mut values = pairs
            .iter()
            .filter(|(key, _)| key == field)
            .map(|(_, v)| v.as_ref());
        let first = values.next()?;
        if values.next().is_some() {
            None
        } else {
            Some(first)
        }
    };
    if single("state") != Some(expected) {
        return Err(CallbackError::Invalid);
    }
    if pairs.iter().any(|(k, _)| k == "iss") && single("iss") != Some(ISSUER) {
        return Err(CallbackError::Invalid);
    }
    if single("error").is_some() {
        return Err(CallbackError::Declined);
    }
    single("code")
        .filter(|v| !v.is_empty() && v.len() <= 4096)
        .map(str::to_owned)
        .ok_or(CallbackError::Invalid)
}

impl CallbackState {
    async fn accept(&self, uri: &Uri, headers: &HeaderMap) -> Result<(), CallbackError> {
        let result = match callback_code(uri, headers, &self.expected) {
            Ok(code) => Ok(code),
            Err(CallbackError::Declined) => Err("Account: Authorization was declined.".into()),
            Err(error) => return Err(error),
        };
        let declined = result.is_err();
        let sender = self
            .sender
            .lock()
            .await
            .take()
            .ok_or(CallbackError::Invalid)?;
        sender.send(result).map_err(|_| CallbackError::Invalid)?;
        if declined {
            Err(CallbackError::Declined)
        } else {
            Ok(())
        }
    }
}

fn callback_page(accepted: bool) -> String {
    let product = desktop_gateway::brand::PRODUCT_NAME
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    include_str!("account-callback.html")
        .replace("__PRODUCT__", &product)
        .replace(
            "__LOGO__",
            &include_str!("../../src/renderer/generated/brand-mark-light.svg")
                .replace("#303236", "currentColor"),
        )
        .replace(
            "__TITLE__",
            if accepted {
                "Authorization received"
            } else {
                "Sign-in could not complete"
            },
        )
        .replace(
            "__MESSAGE__",
            if accepted {
                "Return to the app or terminal to finish setting up your account."
            } else {
                "Return to the app or terminal and try signing in again."
            },
        )
        .replace("__TONE__", if accepted { "" } else { "error" })
        .replace("__SYMBOL__", if accepted { "✓" } else { "!" })
}

async fn callback(
    State(state): State<Arc<CallbackState>>,
    uri: Uri,
    headers: HeaderMap,
) -> (StatusCode, [(String, String); 4], Html<String>) {
    let accepted = state.accept(&uri, &headers).await.is_ok();
    (if accepted { StatusCode::OK } else { StatusCode::BAD_REQUEST }, [
        ("Cache-Control".into(), "no-store".into()),
        ("Content-Security-Policy".into(), "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".into()),
        ("Referrer-Policy".into(), "no-referrer".into()),
        ("X-Content-Type-Options".into(), "nosniff".into()),
    ], Html(callback_page(accepted)))
}

async fn redpill(
    client: Client,
    listener: TcpListener,
    state: Arc<CallbackState>,
    receiver: oneshot::Receiver<Result<String, String>>,
    verifier: String,
    token_url: Url,
) -> Result<Authorization, String> {
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    let app = Router::new()
        .route("/oauth/callback", get(callback))
        .with_state(state);
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await
    }));
    let code = receiver.await.map_err(|_| "Login callback stopped")?;
    shutdown.cancel();
    let _ = timeout(Duration::from_secs(2), server).await;
    let code = code?;
    let token = response(client.post(token_url).form(&[
        ("grant_type", "authorization_code"),
        ("client_id", REDPILL_CLIENT_ID),
        ("code", &code),
        ("code_verifier", &verifier),
        ("redirect_uri", CALLBACK),
    ]))
    .await?;
    let access_token = string(&token, "access_token")?;
    let info = response(
        client
            .get(format!("{ISSUER}/oauth/userinfo"))
            .bearer_auth(&access_token),
    )
    .await?;
    let account = response(
        client
            .get("https://service.redpill.ai/api/oauth/account")
            .bearer_auth(&access_token),
    )
    .await?;
    if string(&account, "user_id")? != string(&info, "sub")? {
        return Err("Unexpected account identity".into());
    }
    Ok(Authorization::Redpill {
        access_token,
        details: redpill_details(&account)?,
    })
}

pub async fn account_details(key: &str) -> Result<AccountLoginDetails, String> {
    let account = response(
        client()?
            .get("https://service.redpill.ai/api/oauth/account")
            .bearer_auth(key),
    )
    .await?;
    redpill_details(&account).map_err(|_| {
        "Account: Could not refresh account details. Try again or sign in again.".to_string()
    })
}

fn avatar_url(data: &Value, field: &str) -> Option<String> {
    let url = Url::parse(data.get(field)?.as_str()?).ok()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some("img.clerk.com" | "images.clerk.dev" | "clerk.redpill.ai")
        ))
    .then(|| url.to_string())
}

fn redpill_details(account: &Value) -> Result<AccountLoginDetails, String> {
    Ok(AccountLoginDetails {
        auth: ProfileAuth::OAuth {
            account_id: string(account, "user_id")?,
            account_name: Some(string(account, "user_name")?),
            images: Some(AccountImages {
                user: avatar_url(account, "user_image_url"),
                organization: avatar_url(account, "organization_image_url"),
            }),
            scope: Some(Box::new(AccountScope {
                organization_id: Some(string(account, "organization_id")?),
                organization_slug: Some(string(account, "organization_slug")?),
                organization: Some(string(account, "organization_name")?),
                ..AccountScope::default()
            })),
        },
        workspaces: parse_workspaces(account)?,
    })
}

fn parse_workspaces(account: &Value) -> Result<Vec<AccountWorkspace>, String> {
    let workspaces: Vec<AccountWorkspace> = serde_json::from_value(
        account
            .get("workspaces")
            .cloned()
            .ok_or("Missing workspace list")?,
    )
    .map_err(|_| "Invalid workspace list")?;
    validate_workspaces(&workspaces)?;
    Ok(workspaces)
}

fn validate_workspaces(workspaces: &[AccountWorkspace]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    if workspaces.is_empty() {
        return Err("No accessible workspace is available".into());
    }
    for workspace in workspaces {
        if workspace.id <= 0
            || workspace.id > 9_007_199_254_740_991
            || workspace.name.trim().is_empty()
            || !ids.insert(workspace.id)
        {
            return Err("Invalid workspace list".into());
        }
    }
    Ok(())
}

fn amount(value: &Value, field: &str) -> Result<String, String> {
    let text = string(value, field)?;
    if !text.parse::<f64>().is_ok_and(f64::is_finite) {
        return Err("Invalid balance response".into());
    }
    Ok(text)
}

pub(crate) async fn account_balance(
    provider: &ServiceProvider,
    secret: &str,
) -> Result<Option<AccountBalance>, String> {
    let url = match provider {
        ServiceProvider::Phala => "https://cloud-api.phala.com/api/v1/private_ai/self",
        ServiceProvider::Redpill => "https://service.redpill.ai/api/oauth/balance",
        ServiceProvider::Custom => {
            return Err("Balance is only available for Phala and RedPill accounts".into())
        }
    };
    let (status, data) = request_json(client()?.get(url).bearer_auth(secret)).await?;
    parse_account_balance(provider, status, &data)
}

fn parse_account_balance(
    provider: &ServiceProvider,
    status: StatusCode,
    data: &Value,
) -> Result<Option<AccountBalance>, String> {
    if status == StatusCode::FORBIDDEN
        && (*provider == ServiceProvider::Phala
            || protocol_error(data) == Some("billing_permission_required"))
    {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(account_error(status, data));
    }
    match provider {
        ServiceProvider::Phala => {
            let credits = data.get("credits").ok_or("Missing balance response")?;
            Ok(Some(AccountBalance {
                balance_usd: amount(credits, "balance")?,
                can_top_up: true,
                organization_id: None,
                granted_usd: Some(amount(credits, "granted_balance")?),
                scope: AccountScope {
                    workspace: Some(string(
                        data.get("workspace").ok_or("Missing workspace")?,
                        "name",
                    )?),
                    ..AccountScope::default()
                },
            }))
        }
        _ => Ok(Some(AccountBalance {
            balance_usd: amount(data, "balance_usd")?,
            can_top_up: data
                .get("can_top_up")
                .and_then(Value::as_bool)
                .ok_or("Invalid billing permissions response")?,
            organization_id: Some(string(data, "organization_id")?),
            granted_usd: None,
            scope: AccountScope {
                organization_id: Some(string(data, "organization_id")?),
                organization_slug: Some(string(data, "organization_slug")?),
                organization: Some(string(data, "organization_name")?),
                workspace: data
                    .get("workspace_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                workspace_id: data.get("workspace_id").and_then(Value::as_i64),
            },
        })),
    }
}

pub fn top_up_url(
    provider: &ServiceProvider,
    organization_slug: Option<&str>,
) -> Result<String, String> {
    match provider {
        ServiceProvider::Phala => Ok("https://cloud.phala.com/cost".into()),
        ServiceProvider::Redpill => Ok(format!("{}/credits", organization_url(organization_slug)?)),
        ServiceProvider::Custom => Err("Top up is only available for Phala and RedPill".into()),
    }
}

pub fn organization_url(organization_slug: Option<&str>) -> Result<String, String> {
    let slug = organization_slug
        .filter(|slug| {
            !slug.is_empty()
                && slug.len() <= 255
                && slug.split('-').all(|part| {
                    !part.is_empty()
                        && part
                            .bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                })
        })
        .ok_or("Refresh account details or sign in again to open this organization.")?;
    Ok(format!("https://redpill.ai/{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_presentation_uses_clerk_identity_and_restricts_avatar_sources() {
        let data = json!({
            "user_id": "user_alice", "user_name": "Alice Example",
            "user_image_url": "https://img.clerk.com/alice",
            "organization_id": "org_test", "organization_slug": "research-team", "organization_name": "Research", "organization_image_url": "https://images.clerk.dev/research",
            "workspaces": [{"id": 1, "name": "Default", "is_default": true}]
        });
        let details = redpill_details(&data).unwrap();
        let ProfileAuth::OAuth {
            account_id,
            account_name,
            images,
            scope,
        } = details.auth
        else {
            panic!("Expected OAuth identity")
        };
        assert_eq!(account_id, "user_alice");
        assert_eq!(account_name.as_deref(), Some("Alice Example"));
        assert_eq!(
            images.unwrap().organization.as_deref(),
            Some("https://images.clerk.dev/research")
        );
        assert_eq!(scope.unwrap().organization.as_deref(), Some("Research"));
        for url in [
            "http://img.clerk.com/a",
            "https://example.com/a",
            "https://img.clerk.com.evil.test/a",
            "https://user:secret@img.clerk.com/a",
        ] {
            assert!(avatar_url(&json!({"image":url}), "image").is_none());
        }
    }

    #[test]
    fn billing_permissions_hide_denied_balances_and_bind_links_to_the_organization() {
        let denied = json!({"detail":{"error":"billing_permission_required"}});
        assert!(
            parse_account_balance(&ServiceProvider::Redpill, StatusCode::FORBIDDEN, &denied)
                .unwrap()
                .is_none()
        );
        assert!(parse_account_balance(
            &ServiceProvider::Redpill,
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({})
        )
        .is_err());
        let data = json!({"balance_usd":"0", "organization_id":"org_test", "organization_slug":"research-team", "organization_name":"Research", "can_top_up":false});
        let balance = parse_account_balance(&ServiceProvider::Redpill, StatusCode::OK, &data)
            .unwrap()
            .unwrap();
        assert_eq!(balance.balance_usd, "0");
        assert!(!balance.can_top_up);
        assert_eq!(
            organization_url(Some("research-team")).unwrap(),
            "https://redpill.ai/research-team"
        );
        for field in ["organization_id", "organization_slug", "can_top_up"] {
            let mut incomplete = data.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(
                parse_account_balance(&ServiceProvider::Redpill, StatusCode::OK, &incomplete)
                    .is_err()
            );
        }
        assert_eq!(
            top_up_url(
                &ServiceProvider::Redpill,
                balance.scope.organization_slug.as_deref()
            )
            .unwrap(),
            "https://redpill.ai/research-team/credits"
        );
        for id in [
            None,
            Some(""),
            Some("org_"),
            Some("org_../other"),
            Some("org_test?other"),
        ] {
            assert!(top_up_url(&ServiceProvider::Redpill, id).is_err());
            assert!(organization_url(id).is_err());
        }
    }

    #[tokio::test]
    async fn failed_authorization_is_terminal_and_repeatable() {
        let profile = ConfidentialProfileInput {
            id: "profile-test".into(),
            name: "Phala".into(),
            provider: ServiceProvider::Phala,
            remote_url: "https://inference.phala.com".into(),
        };
        let presentation = LoginPresentation {
            id: "login-test".into(),
            url: "https://cloud.phala.com/cli/verify".into(),
            user_code: None,
        };
        let mut pending = PendingLogin::new(
            presentation,
            profile,
            tokio::spawn(async { Err("Authorization was declined".into()) }),
        );
        let error = timeout(Duration::from_secs(5), async {
            loop {
                match pending.poll("login-test").await {
                    Err(error) => break error,
                    Ok(_) => tokio::task::yield_now().await,
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(error, "Authorization was declined");
        assert_eq!(pending.poll("login-test").await.unwrap_err(), error);
        assert!(!pending.is_active());
        pending.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn redpill_requires_an_explicit_accessible_workspace() {
        let profile = ConfidentialProfileInput {
            id: "profile-test".into(),
            name: "RedPill".into(),
            provider: ServiceProvider::Redpill,
            remote_url: "https://tee.redpill.ai".into(),
        };
        let authorization = Authorization::Redpill {
            access_token: "must-not-be-sent".into(),
            details: AccountLoginDetails {
                auth: ProfileAuth::OAuth {
                    account_id: "user_test".into(),
                    account_name: None,
                    images: None,
                    scope: None,
                },
                workspaces: vec![AccountWorkspace {
                    id: 7,
                    name: "Research".into(),
                    is_default: false,
                }],
            },
        };
        for selection in [None, Some(999)] {
            let result = authorization.issue(&profile, selection).await;
            assert_eq!(result.err().unwrap(), "Choose a workspace before saving");
        }
    }

    #[test]
    fn workspace_and_balance_responses_are_validated() {
        let valid = AccountWorkspace {
            id: 7,
            name: "Research".into(),
            is_default: false,
        };
        assert!(validate_workspaces(std::slice::from_ref(&valid)).is_ok());
        assert!(validate_workspaces(&[valid.clone(), valid]).is_err());
        assert!(validate_workspaces(&[]).is_err());
        for value in ["NaN", "inf", "not-a-number"] {
            assert!(amount(&json!({"balance": value}), "balance").is_err());
        }
        assert_eq!(
            amount(&json!({"balance": "-1.25"}), "balance").unwrap(),
            "-1.25"
        );
    }

    #[tokio::test]
    async fn phala_polling_uses_the_device_authorization_contract() {
        use axum::{routing::post, Json};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let token_calls = calls.clone();
        let tokens = post(move |Json(body): Json<Value>| {
            let calls = token_calls.clone();
            async move {
                assert_eq!(body["device_code"], "test-device");
                assert_eq!(
                    body["grant_type"],
                    "urn:ietf:params:oauth:grant-type:device_code"
                );
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"detail":{"error":"authorization_pending"}})),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Json(json!({"access_token":"sk-test-credential"})),
                    )
                }
            }
        });
        let app = Router::new()
            .route("/api/v1/auth/device/token", tokens)
            .route(
                "/api/v1/private_ai/self",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer sk-test-credential");
                    Json(json!({"user":{"username":"alice"},"workspace":{"name":"Research"}}))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server =
            AbortOnDropHandle::new(tokio::spawn(
                async move { axum::serve(listener, app).await },
            ));
        let credential = phala_at(client().unwrap(), "test-device".into(), 0, &base)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            matches!(credential.auth, ProfileAuth::OAuth { ref account_id, .. } if account_id == "alice")
        );
        server.abort();
    }

    #[test]
    fn structured_account_errors_survive_the_management_boundary_without_raw_details() {
        let error = account_error(
            StatusCode::FORBIDDEN,
            &json!({"detail":{"error":"device_disabled","internal":"secret-do-not-show"}}),
        );
        let public = crate::protocol::RpcError::operation(&error);
        assert!(public.message.contains("disabled"));
        assert!(!public.message.contains("secret"));
    }

    #[tokio::test]
    async fn pasted_callback_is_bound_to_session_and_consumed_once() {
        let profile = ConfidentialProfileInput {
            id: "test".into(),
            name: "Test".into(),
            provider: ServiceProvider::Redpill,
            remote_url: "https://tee.redpill.ai".into(),
        };
        let mut pending = PendingLogin::new(
            LoginPresentation {
                id: "login".into(),
                url: "https://clerk.redpill.ai".into(),
                user_code: None,
            },
            profile,
            tokio::spawn(std::future::pending()),
        );
        let (sender, receiver) = oneshot::channel();
        pending.callback = Some(Arc::new(CallbackState {
            expected: "expected".into(),
            sender: Mutex::new(Some(sender)),
        }));
        for url in [
            "https://attacker.test/oauth/callback?state=expected&code=secret",
            "http://127.0.0.1:4181/oauth/callback?state=wrong&code=secret",
            "http://127.0.0.1:4181/other?state=expected&code=secret",
            "http://127.0.0.1:4181/oauth/callback?state=expected&state=expected&code=secret",
        ] {
            assert!(pending.complete_callback("login", url).await.is_err());
        }
        let url = "http://127.0.0.1:4181/oauth/callback?state=expected&code=secret";
        assert!(pending.complete_callback("other-login", url).await.is_err());
        pending.complete_callback("login", url).await.unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), "secret");
        assert!(pending.complete_callback("login", url).await.is_err());
        assert!(!callback_page(true).contains("secret"));
    }

    #[test]
    fn callback_binds_host_state_and_issuer_and_rejects_duplicates() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:4181".parse().unwrap());
        let valid: Uri =
            "/oauth/callback?state=expected&code=one-use&iss=https%3A%2F%2Fclerk.redpill.ai"
                .parse()
                .unwrap();
        assert_eq!(
            callback_code(&valid, &headers, "expected").unwrap(),
            "one-use"
        );
        for query in [
            "state=wrong&code=x",
            "state=expected&state=other&code=x",
            "state=expected&code=x&code=y",
            "state=expected&code=x&iss=https://attacker.invalid",
            "state=expected&code=x&iss=https://clerk.redpill.ai&iss=https://attacker.invalid",
        ] {
            let uri = format!("/oauth/callback?{query}").parse().unwrap();
            assert!(callback_code(&uri, &headers, "expected").is_err());
        }
        headers.insert("host", "attacker.invalid:4181".parse().unwrap());
        assert!(callback_code(&valid, &headers, "expected").is_err());
    }

    #[test]
    fn authorization_urls_cannot_redirect_credentials_to_another_origin() {
        for url in [
            "http://clerk.redpill.ai/oauth/token",
            "https://clerk.redpill.ai.attacker.invalid/oauth/token",
            "https://user@clerk.redpill.ai/oauth/token",
            "https://clerk.redpill.ai/oauth/token#fragment",
        ] {
            assert!(trusted_url(url, ISSUER).is_err());
        }
        assert!(trusted_url("https://clerk.redpill.ai/oauth/token", ISSUER).is_ok());
    }

    #[test]
    fn unsupported_pkce_discovery_fails_closed() {
        let mut discovery = json!({"issuer":ISSUER,"grant_types_supported":["authorization_code"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]});
        assert!(validate_discovery(&discovery).is_ok());
        discovery["code_challenge_methods_supported"] = json!(["plain"]);
        assert!(validate_discovery(&discovery).is_err());
    }
}
