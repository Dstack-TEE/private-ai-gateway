use super::*;

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

pub(super) fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
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
            return Err("The account service does not support secure desktop login".into());
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

/// A window-activation link only; OAuth credentials stay on the loopback channel.
pub fn account_return_url() -> String {
    format!("{}://oauth/return", desktop_gateway::brand::APP_IDENTIFIER)
}

pub(super) fn callback_page(accepted: bool) -> String {
    let product = desktop_gateway::brand::PRODUCT_NAME
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
            &desktop_gateway::brand::BYLINE
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;"),
        )
        .replace(
            "__LOGO__",
            &format!(
                r#"<img src="data:image/png;base64,{}" alt="">"#,
                STANDARD.encode(include_bytes!(
                    "../../../src/renderer/generated/app-icon-light.png"
                ))
            ),
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

pub(super) async fn redpill(
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

pub(super) fn parse_workspaces(account: &Value) -> Result<Vec<AccountWorkspace>, String> {
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
