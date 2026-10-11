use super::*;

pub async fn account_details(key: &str) -> Result<AccountLoginDetails, Error> {
    let account = response(client()?.get(ACCOUNT_URL).bearer_auth(key)).await?;
    redpill_details(&account).map_err(|_| {
        Error::account(
            "Account: Could not refresh account details. Try again or reconnect the account.",
        )
    })
}

pub(super) fn amount(value: &Value, field: &str) -> Result<String, Error> {
    let text = string(value, field)?;
    if !text.parse::<f64>().is_ok_and(f64::is_finite) {
        return Err("Invalid balance response".into());
    }
    Ok(text)
}

pub(crate) async fn account_balance(
    provider: &ServiceProvider,
    secret: &str,
) -> Result<Option<AccountBalance>, Error> {
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

pub(super) fn parse_account_balance(
    provider: &ServiceProvider,
    status: StatusCode,
    data: &Value,
) -> Result<Option<AccountBalance>, Error> {
    if status == StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
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
                    workspace_slug: Some(string(
                        data.get("workspace").ok_or("Missing workspace")?,
                        "slug",
                    )?),
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
                workspace_slug: None,
                workspace_id: data.get("workspace_id").and_then(Value::as_i64),
            },
        })),
    }
}
