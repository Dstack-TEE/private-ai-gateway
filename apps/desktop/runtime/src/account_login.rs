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

use crate::contracts::{ConfidentialProfileInput, ProfileAuth, ServiceProvider};

pub const REDPILL_CLIENT_ID: &str = "cGrHCOWG3S91oa0A";
const ISSUER: &str = "https://clerk.redpill.ai";
const CALLBACK: &str = "http://127.0.0.1:4181/oauth/callback";
const KEY_URL: &str = "https://service.redpill.ai/api/desktop/key";
const PHALA_API: &str = "https://cloud-api.phala.com";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginPresentation {
    pub id: String,
    pub url: String,
    pub user_code: Option<String>,
}

pub struct Credential {
    pub key: String,
    pub auth: ProfileAuth,
}

pub struct PendingLogin {
    pub presentation: LoginPresentation,
    pub profile: ConfidentialProfileInput,
    pub require_production_os: bool,
    pub worker: JoinHandle<Result<Credential, String>>,
    pub deadline: Instant,
}

impl Drop for PendingLogin {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

pub async fn revoke_redpill_key(key: &str) -> Result<(), String> {
    let response = client()?
        .delete(KEY_URL)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| "Could not revoke the RedPill device key".to_string())?;
    if !response.status().is_success() {
        return Err("Could not revoke the RedPill device key; remove it in RedPill Keys".into());
    }
    Ok(())
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
                "Account service rejected the request (HTTP {})",
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
    match protocol_error(data) {
        Some("access_denied") => "Authorization was declined".into(),
        Some("expired_token") => "Authorization expired; sign in again".into(),
        _ => format!(
            "Account service rejected the request (HTTP {})",
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

pub async fn begin(
    profile: ConfidentialProfileInput,
    require_production_os: bool,
) -> Result<PendingLogin, String> {
    crate::service_config::resolve_profile(profile.clone(), None)?;
    let client = client()?;
    let id = Uuid::new_v4().to_string();
    let (url, user_code, worker) = match profile.provider {
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
            });
            (url, Some(code), worker)
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
            let installation = installation_id(&profile.id);
            let name = profile.name.clone();
            let worker = tokio::spawn(async move {
                timeout(
                    LOGIN_TIMEOUT,
                    redpill(
                        client,
                        listener,
                        state,
                        verifier,
                        token_url,
                        installation,
                        name,
                    ),
                )
                .await
                .map_err(|_| "Authorization expired; sign in again".to_string())?
            });
            (url.to_string(), None, worker)
        }
        ServiceProvider::Custom => {
            return Err("Account login is only available for Phala and RedPill".into())
        }
    };
    Ok(PendingLogin {
        presentation: LoginPresentation { id, url, user_code },
        profile,
        require_production_os,
        worker,
        deadline: Instant::now() + LOGIN_TIMEOUT,
    })
}

fn installation_id(profile_id: &str) -> Uuid {
    // Profile IDs are already random and persist with the credential. Deriving a
    // UUID keeps re-login stable without another installation file or secret.
    let hash = Sha256::digest(profile_id.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    Uuid::from_bytes(bytes)
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

async fn phala(client: Client, device: String, mut interval: u64) -> Result<Credential, String> {
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let (status, data) = request_json(client.post(format!("{PHALA_API}/api/v1/auth/device/token"))
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
                .get(format!("{PHALA_API}/api/v1/private_ai/self"))
                .timeout(Duration::from_secs(5))
                .bearer_auth(&key),
        )
        .await
        .ok();
        let name = metadata
            .as_ref()
            .and_then(|v| v.pointer("/workspace/name"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let account = metadata
            .as_ref()
            .and_then(|v| v.pointer("/user/username"))
            .and_then(Value::as_str)
            .unwrap_or("phala")
            .to_owned();
        return Ok(Credential {
            key,
            auth: ProfileAuth::OAuth {
                account_id: account,
                account_name: name,
            },
        });
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
    if headers.get("host").and_then(|v| v.to_str().ok()) != Some("127.0.0.1:4181") {
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

async fn callback(
    State(state): State<Arc<CallbackState>>,
    uri: Uri,
    headers: HeaderMap,
) -> (StatusCode, [(String, String); 2], Html<&'static str>) {
    let result = match callback_code(&uri, &headers, &state.expected) {
        Ok(code) => Some(Ok(code)),
        Err(CallbackError::Declined) => Some(Err("Authorization was declined".into())),
        Err(CallbackError::Invalid) => None,
    };
    let accepted = result.is_some();
    if let Some(result) = result {
        if let Some(sender) = state.sender.lock().await.take() {
            let _ = sender.send(result);
        }
    }
    (
        if accepted {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        [
            ("Cache-Control".into(), "no-store".into()),
            (
                "Content-Security-Policy".into(),
                "default-src 'none'; frame-ancestors 'none'".into(),
            ),
        ],
        Html(if accepted {
            "<!doctype html><title>Private AI Proxy</title><p>Return to Private AI Proxy to finish signing in.</p>"
        } else {
            "<!doctype html><title>Private AI Proxy</title><p>This login response was rejected. Return to the app and retry.</p>"
        }),
    )
}

async fn redpill(
    client: Client,
    listener: TcpListener,
    state: String,
    verifier: String,
    token_url: Url,
    installation: Uuid,
    name: String,
) -> Result<Credential, String> {
    let (sender, receiver) = oneshot::channel();
    let state = Arc::new(CallbackState {
        expected: state,
        sender: Mutex::new(Some(sender)),
    });
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
    let access = string(&token, "access_token")?;
    let result = response(
        client
            .post(KEY_URL)
            .bearer_auth(access)
            .json(&json!({"installation_id": installation, "name": name})),
    )
    .await?;
    Ok(Credential {
        key: desktop_gateway::secrets::validate_api_key(&string(&result, "api_key")?)?,
        auth: ProfileAuth::OAuth {
            account_id: string(&result, "account_id")?,
            account_name: Some(string(&result, "account_name")?),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
