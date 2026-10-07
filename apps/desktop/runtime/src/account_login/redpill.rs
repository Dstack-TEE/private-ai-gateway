use super::*;
use desktop_core::private_fs;
use oauth2::{
    basic::BasicClient, ClientId, EndpointNotSet, EndpointSet, ErrorResponseType,
    RequestTokenError, Scope, StandardDeviceAuthorizationResponse, StandardErrorResponse,
    TokenResponse,
};
pub(super) use oauth2::{DeviceAuthorizationUrl, TokenUrl};

/// The device authorization grant (RFC 8628) Clerk advertises in discovery.
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

pub(crate) async fn transition_credential(
    provider: &ServiceProvider,
    key: &str,
    action: &str,
) -> Result<CredentialTransition, Error> {
    if *provider != ServiceProvider::Redpill {
        return Ok(CredentialTransition::Applied);
    }
    transition_at(key, action, KEY_URL).await
}

pub(super) async fn transition_at(
    key: &str,
    action: &str,
    base: &str,
) -> Result<CredentialTransition, Error> {
    let http = client()?;
    let request = match action {
        "activate" | "abort" => http.post(desktop_core::endpoint(base, &[action])?),
        "revoke" => http.delete(base),
        _ => return Err("Unsupported account operation".into()),
    };
    let response = request
        .timeout(Duration::from_secs(5))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| Error::account("Account: Credential update failed; retry the operation."))?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Ok(CredentialTransition::Unavailable);
    }
    if !response.status().is_success() {
        return Err(Error::account(
            "Account: Credential update failed; retry or manage the key in your provider console.",
        ));
    }
    Ok(CredentialTransition::Applied)
}

pub(super) fn installation_id(profile_id: &str) -> Result<Uuid, Error> {
    // Profile IDs are already random and persist with the credential. Deriving a
    // UUID mixes the profile with the local installation identity.
    let data = desktop_core::paths::app_data_dir()?;
    let path = data.join("installation-id");
    let device = match private_fs::read_private_text(&path) {
        Ok(Some(value)) => uuid::Uuid::parse_str(value.trim())
            .map_err(|_| Error::account("Account: Device identity needs repair."))?,
        Ok(None) => {
            let id = Uuid::new_v4();
            // Owner-only and complete or absent, never over an existing identity.
            private_fs::publish(&path, private_fs::Publish::NoClobber, |file| {
                std::io::Write::write_all(file, id.to_string().as_bytes())
            })
            .map_err(|_| Error::account("Account: Cannot save device identity."))?;
            id
        }
        Err(_) => return Err(Error::account("Account: Cannot read device identity.")),
    };
    let hash = Sha256::digest(format!("{device}:{profile_id}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    Ok(Uuid::from_bytes(bytes))
}

/// The RedPill public client: no secret, so `oauth2` sends the client ID in
/// the token request body (RFC 6749 §2.3.1 does not apply).
pub(super) type RedpillClient =
    BasicClient<EndpointNotSet, EndpointSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

pub(super) fn redpill_client(device: DeviceAuthorizationUrl, token: TokenUrl) -> RedpillClient {
    BasicClient::new(ClientId::new(REDPILL_CLIENT_ID.into()))
        .set_device_authorization_url(device)
        .set_token_uri(token)
}

pub(super) fn validate_discovery(data: &Value) -> Result<(), Error> {
    for (field, required) in [
        ("grant_types_supported", DEVICE_CODE_GRANT),
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

/// Starts a device authorization (RFC 8628 §3.1-3.2) for the account scopes.
pub(super) async fn device_authorization(
    client: &Client,
    oauth: &RedpillClient,
) -> Result<StandardDeviceAuthorizationResponse, Error> {
    let http = |request| oauth_http(client.clone(), request);
    oauth
        .exchange_device_code()
        .add_scopes(["openid", "profile", "user:org:read"].map(|scope| Scope::new(scope.into())))
        .request_async(&http)
        .await
        .map_err(token_error)
}

/// Polls for the device authorization's token (RFC 8628 §3.4-3.5) until it
/// expires, then checks that the account service and the issuer name the
/// same user.
pub(super) async fn redpill(
    client: &Client,
    oauth: &RedpillClient,
    device: &StandardDeviceAuthorizationResponse,
    userinfo_url: &str,
    account_url: &str,
) -> Result<Authorization, Error> {
    let http = |request| oauth_http(client.clone(), request);
    let token = oauth
        .exchange_device_access_token(device)
        .request_async(&http, tokio::time::sleep, None)
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
fn token_error<T: ErrorResponseType + AsRef<str> + std::fmt::Display + Send + Sync + 'static>(
    error: RequestTokenError<std::io::Error, StandardErrorResponse<T>>,
) -> Error {
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
        RequestTokenError::Request(error) => error.to_string().into(),
        RequestTokenError::Parse(..) | RequestTokenError::Other(_) => {
            "Invalid account response".into()
        }
    }
}

pub(super) fn redpill_details(account: &Value) -> Result<AccountLoginDetails, Error> {
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

pub(super) fn parse_workspaces(account: &Value) -> Result<Vec<AccountWorkspace>, Error> {
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

pub(super) fn validate_workspaces(workspaces: &[AccountWorkspace]) -> Result<(), Error> {
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
