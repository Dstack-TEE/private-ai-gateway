use super::*;

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

pub(super) fn amount(value: &Value, field: &str) -> Result<String, String> {
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

pub(super) fn parse_account_balance(
    provider: &ServiceProvider,
    status: StatusCode,
    data: &Value,
) -> Result<Option<AccountBalance>, String> {
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

pub fn top_up_url(provider: &ServiceProvider, scope_slug: Option<&str>) -> Result<String, String> {
    let slug = validated_scope_slug(scope_slug)?;
    match provider {
        ServiceProvider::Phala => Ok(format!("https://cloud.phala.com/{slug}/billing")),
        ServiceProvider::Redpill => Ok(format!("https://redpill.ai/{slug}/credits")),
        ServiceProvider::Custom => Err("Billing is only available for Phala and RedPill".into()),
    }
}

pub fn organization_url(organization_slug: Option<&str>) -> Result<String, String> {
    Ok(format!(
        "https://redpill.ai/{}",
        validated_scope_slug(organization_slug)?
    ))
}

pub(super) fn validated_scope_slug(slug: Option<&str>) -> Result<&str, String> {
    slug.filter(|slug| {
        !slug.is_empty()
            && slug.len() <= 255
            && slug.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
    })
    .ok_or("Refresh account details or sign in again to open billing.".into())
}
