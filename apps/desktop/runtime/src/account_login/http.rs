use super::*;

pub(super) fn client() -> Result<Client, Error> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(35))
        .build()
        .map_err(|_| "Cannot initialize account connection".into())
}

pub(super) async fn request_json(
    request: reqwest::RequestBuilder,
) -> Result<(StatusCode, Value), Error> {
    let result = request
        .send()
        .await
        .map_err(|_| "Account service could not be reached")?;
    let status = result.status();
    let bytes = body(result).await?;
    let data = serde_json::from_slice(&bytes).map_err(|_| {
        if status.is_success() {
            "Invalid account response".into()
        } else {
            Error::account(format!(
                "Account: Service rejected the request (HTTP {}). Retry or contact support.",
                status.as_u16()
            ))
        }
    })?;
    Ok((status, data))
}

/// Bound untrusted account responses and never include bodies, tokens or URLs in errors.
async fn body(mut response: reqwest::Response) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Account response interrupted")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("Account response is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The account client for the `oauth2` crate, through its documented custom
/// client hook: `oauth2` 5 bundles `reqwest` 0.12, and this client keeps the
/// redirect policy, timeouts and response bound above.
pub(super) async fn oauth_http(
    client: Client,
    request: oauth2::HttpRequest,
) -> Result<oauth2::HttpResponse, std::io::Error> {
    let request = reqwest::Request::try_from(request)
        .map_err(|_| std::io::Error::other("Invalid account request"))?;
    let response = client
        .execute(request)
        .await
        .map_err(|_| std::io::Error::other("Account service could not be reached"))?;
    let mut http = oauth2::http::Response::builder().status(response.status());
    if let Some(headers) = http.headers_mut() {
        *headers = response.headers().clone();
    }
    http.body(body(response).await.map_err(std::io::Error::other)?)
        .map_err(|_| std::io::Error::other("Invalid account response"))
}

pub(super) fn protocol_error(data: &Value) -> Option<&str> {
    data.get("detail")
        .filter(|v| v.is_object())
        .unwrap_or(data)
        .get("error")
        .and_then(Value::as_str)
}

pub(super) fn account_error(status: StatusCode, data: &Value) -> Error {
    if let Some(message) = match protocol_error(data) {
        Some("org_required") => Some("Select an organization on the connection page and try again."),
        Some("keys_permission_required" | "organization_permission_required") => Some("Your organization must grant key-management permission before you can connect."),
        Some("rate_limited") => Some("Balance refresh is temporarily limited. Try again in a minute."),
        Some("balance_unavailable") => Some("Balance is temporarily unavailable. Try refreshing later."),
        Some("billing_permission_required") => Some("Your account does not have permission to view this balance."),
        Some("account_mapping_conflict") => Some("Account setup conflicts with an existing account. Contact RedPill support."),
        Some("account_setup_unavailable" | "account_service_unavailable" | "organization_unavailable" | "authorization_unavailable") => Some("Account setup is temporarily unavailable. Retry connecting."),
        Some("device_disabled") => Some("This device key was disabled. Manage it in RedPill Keys before reconnecting."),
        Some("device_migration_required" | "device_credential_mismatch") => Some("This device credential needs repair in RedPill Keys."),
        Some("invalid_authorization" | "invalid_identity" | "credentials_required" | "device_unavailable") => Some("Your authorization is no longer valid. Reconnect the account."),
        Some("key_store_unavailable") => Some("The credential service is unavailable. Retry saving; your previous credential is unchanged."),
        Some("account_unavailable") => Some("This account is unavailable. Contact your organization administrator."),
        Some("not_available") => Some("Account connection is not enabled on this service."),
        _ => None,
    } { return Error::account(format!("Account: {message}")); }

    match protocol_error(data) {
        Some("access_denied") => Error::account("Account: Authorization was declined."),
        Some("expired_token") => Error::account("Account: Authorization expired; reconnect the account."),
        _ => Error::account(format!(
            "Account: Service rejected the request (HTTP {}). Retry or contact support.",
            status.as_u16()
        )),
    }
}

pub(super) async fn response(request: reqwest::RequestBuilder) -> Result<Value, Error> {
    let (status, data) = request_json(request).await?;
    if !status.is_success() {
        return Err(account_error(status, &data));
    }
    Ok(data)
}

pub(super) fn string(data: &Value, field: &str) -> Result<String, Error> {
    data.get(field)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 16384)
        .map(str::to_owned)
        .ok_or_else(|| format!("Invalid account response: {field}").into())
}

pub(super) fn seconds(data: &Value, field: &str, max: u64) -> Result<u64, Error> {
    data.get(field)
        .and_then(Value::as_u64)
        .filter(|v| *v > 0 && *v <= max)
        .ok_or_else(|| format!("Invalid account response: {field}").into())
}

pub(super) fn trusted_url(value: &str, origin: &str) -> Result<Url, Error> {
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
