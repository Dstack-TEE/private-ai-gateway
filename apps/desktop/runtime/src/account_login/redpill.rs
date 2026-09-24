use super::*;
use desktop_core::private_fs;
use oauth2::{
    basic::{BasicClient, BasicErrorResponse},
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RequestTokenError, Scope, TokenResponse,
    TokenUrl,
};

impl CallbackState {
    pub(super) async fn accept(&self, uri: &Uri, headers: &HeaderMap) -> Result<(), CallbackError> {
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

#[derive(Debug)]
pub(super) enum CallbackError {
    Invalid,
    Declined,
}

pub(super) struct CallbackState {
    pub(super) expected: String,
    pub(super) sender: Mutex<Option<oneshot::Sender<Result<String, String>>>>,
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

pub(super) async fn transition_at(
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

pub(super) fn installation_id(profile_id: &str) -> Result<Uuid, String> {
    // Profile IDs are already random and persist with the credential. Deriving a
    // UUID mixes the profile with the local installation identity.
    let data = desktop_core::paths::app_data_dir()?;
    let path = data.join("installation-id");
    let device = match private_fs::read_private_text(&path) {
        Ok(Some(value)) => uuid::Uuid::parse_str(value.trim())
            .map_err(|_| "Account: Device identity needs repair.")?,
        Ok(None) => {
            let id = Uuid::new_v4();
            // Owner-only and complete or absent, never over an existing identity.
            private_fs::publish(&path, private_fs::Publish::NoClobber, |file| {
                std::io::Write::write_all(file, id.to_string().as_bytes())
            })
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

/// The RedPill public client: no secret, so `oauth2` sends the client ID in
/// the token request body (RFC 6749 §2.3.1 does not apply).
pub(super) type RedpillClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

pub(super) fn redpill_client(authorize: Url, token: Url) -> Result<RedpillClient, String> {
    Ok(BasicClient::new(ClientId::new(REDPILL_CLIENT_ID.into()))
        .set_auth_uri(AuthUrl::from_url(authorize))
        .set_token_uri(TokenUrl::from_url(token))
        .set_redirect_uri(
            RedirectUrl::new(callback_url()).map_err(|_| "Invalid account callback URL")?,
        ))
}

/// The browser URL of an authorization code request with S256 PKCE
/// (RFC 7636) and a random `state`, which the callback must return.
pub(super) fn authorization_request(oauth: &RedpillClient) -> (Url, CsrfToken, PkceCodeVerifier) {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state) = oauth
        .authorize_url(CsrfToken::new_random)
        .add_scopes(["openid", "profile", "user:org:read"].map(|scope| Scope::new(scope.into())))
        .add_extra_param("response_mode", "query")
        .set_pkce_challenge(challenge)
        .url();
    (url, state, verifier)
}

pub(super) fn validate_discovery(data: &Value) -> Result<(), String> {
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
            return Err("The account service does not support secure desktop connection".into());
        }
    }
    if data.get("issuer").and_then(Value::as_str) != Some(ISSUER) {
        return Err("Unexpected account issuer".into());
    }
    Ok(())
}

pub(super) fn callback_code(
    uri: &Uri,
    headers: &HeaderMap,
    expected: &str,
) -> Result<String, CallbackError> {
    if uri.path() != CALLBACK_PATH
        || headers.get("host").and_then(|v| v.to_str().ok())
            != Some(CALLBACK_ADDRESS.to_string().as_str())
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

/// A window-activation link only; OAuth credentials stay on the loopback channel.
pub(super) fn callback_page(accepted: bool) -> String {
    let product = desktop_core::brand::PRODUCT_NAME
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    include_str!("../account-callback.html")
        .replace(
            "__OPEN_APP__",
            if accepted {
                format!(
                    "<a class=\"open-app\" href=\"{}\">Open {product}</a>",
                    account_return_url()
                )
            } else {
                String::new()
            }
            .as_str(),
        )
        .replace("__PRODUCT__", &product)
        .replace(
            "__BYLINE__",
            &desktop_core::brand::BYLINE
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;"),
        )
        .replace(
            "__LOGO__",
            &format!(
                r#"<img src="data:image/png;base64,{}" alt="">"#,
                STANDARD.encode(include_bytes!(
                    "../../../src/renderer/brand/app-icon-light.png"
                ))
            ),
        )
        .replace(
            "__TITLE__",
            if accepted {
                "Authorization received"
            } else {
                "Account connection could not complete"
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

pub(super) async fn callback(
    State(state): State<Arc<CallbackState>>,
    uri: Uri,
    headers: HeaderMap,
) -> (StatusCode, [(String, String); 4], Html<String>) {
    let accepted = state.accept(&uri, &headers).await.is_ok();
    (if accepted { StatusCode::OK } else { StatusCode::BAD_REQUEST }, [
        ("Cache-Control".into(), "no-store".into()),
        ("Content-Security-Policy".into(), "default-src 'none'; img-src data:; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".into()),
        ("Referrer-Policy".into(), "no-referrer".into()),
        ("X-Content-Type-Options".into(), "nosniff".into()),
    ], Html(callback_page(accepted)))
}

/// Serves the loopback callback until it (or a pasted link) delivers the code.
pub(super) async fn receive_code(
    listener: TcpListener,
    state: Arc<CallbackState>,
    receiver: oneshot::Receiver<Result<String, String>>,
) -> Result<String, String> {
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
    code
}

/// Exchanges the code with its PKCE verifier and checks that the account
/// service and the issuer name the same user.
pub(super) async fn redpill(
    client: &Client,
    oauth: &RedpillClient,
    code: String,
    verifier: PkceCodeVerifier,
    userinfo_url: &str,
    account_url: &str,
) -> Result<Authorization, String> {
    let http = |request| oauth_http(client.clone(), request);
    let token = oauth
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(verifier)
        .request_async(&http)
        .await
        .map_err(token_error)?;
    let access_token = token.access_token().secret().clone();
    let info = response(client.get(userinfo_url).bearer_auth(&access_token)).await?;
    let account = response(client.get(account_url).bearer_auth(&access_token)).await?;
    if string(&account, "user_id")? != string(&info, "sub")? {
        return Err("Unexpected account identity".into());
    }
    Ok(Authorization::Redpill {
        access_token,
        details: redpill_details(&account)?,
    })
}

/// Token endpoint errors (RFC 6749 §5.2, HTTP 400) as account errors; a
/// malformed response is never echoed.
fn token_error(error: RequestTokenError<std::io::Error, BasicErrorResponse>) -> String {
    // RFC 6749 §5.1 requires a 200 JSON body with `token_type`; name only the
    // offending field, never a value.
    if let RequestTokenError::Parse(error, _) = &error {
        tracing::warn!(
            "The account token response is invalid at `{}`",
            error.path()
        );
    }
    match error {
        RequestTokenError::ServerResponse(response) => account_error(
            StatusCode::BAD_REQUEST,
            &json!({ "error": response.error().as_ref() }),
        ),
        RequestTokenError::Request(error) => error.to_string(),
        RequestTokenError::Parse(..) | RequestTokenError::Other(_) => {
            "Invalid account response".into()
        }
    }
}

pub(super) fn redpill_details(account: &Value) -> Result<AccountLoginDetails, String> {
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

/// One entry of RedPill's `workspaces` account field.
#[derive(serde::Deserialize)]
struct RedpillWorkspace {
    id: i64,
    name: String,
    is_default: bool,
}

pub(super) fn parse_workspaces(account: &Value) -> Result<Vec<AccountWorkspace>, String> {
    let workspaces: Vec<RedpillWorkspace> = serde_json::from_value(
        account
            .get("workspaces")
            .cloned()
            .ok_or("Missing workspace list")?,
    )
    .map_err(|_| "Invalid workspace list")?;
    let workspaces: Vec<_> = workspaces
        .into_iter()
        .map(|workspace| AccountWorkspace {
            id: workspace.id,
            name: workspace.name,
            is_default: workspace.is_default,
        })
        .collect();
    validate_workspaces(&workspaces)?;
    Ok(workspaces)
}

pub(super) fn validate_workspaces(workspaces: &[AccountWorkspace]) -> Result<(), String> {
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

pub(super) fn avatar_url(data: &Value, field: &str) -> Option<String> {
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
