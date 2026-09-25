use super::*;

pub(super) async fn phala(
    client: Client,
    device: String,
    interval: u64,
) -> Result<Credential, Error> {
    phala_at(client, device, interval, PHALA_API).await
}

/// Polls Phala's device authorization (RFC 8628). Its endpoints take JSON and
/// nest errors under `detail`, as Phala's own CLI expects, rather than the
/// form bodies and top-level errors of RFC 8628 §3.4-3.5 that the `oauth2`
/// crate speaks, so the polling is written out here.
pub(super) async fn phala_at(
    client: Client,
    device: String,
    mut interval: u64,
    base: &str,
) -> Result<Credential, Error> {
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let (status, data) = request_json(client.post(format!("{base}/api/v1/auth/device/token"))
            .json(&json!({"device_code":device,"client_id":"private-ai-proxy","grant_type":"urn:ietf:params:oauth:grant-type:device_code"}))).await?;
        if !status.is_success() {
            match protocol_error(&data) {
                Some("authorization_pending") => continue,
                // RFC 8628 §3.5: 5 more seconds for this and every later request.
                Some("slow_down") => {
                    interval += 5;
                    continue;
                }
                _ => return Err(account_error(status, &data)),
            }
        }
        let key = desktop_core::config::validate_api_key(&string(&data, "access_token")?)?;
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
                    .ok_or(Error::account("Account: Missing workspace identity"))?,
                "name",
            )?;
            let account = string(
                metadata
                    .get("user")
                    .ok_or(Error::account("Account: Missing account identity"))?,
                "username",
            )?;
            Ok(ProfileAuth::OAuth {
                account_id: account.clone(),
                account_name: Some(account),
                images: None,
                scope: Some(Box::new(AccountScope {
                    workspace: Some(workspace),
                    workspace_slug: Some(string(
                        metadata.get("workspace").ok_or("Missing workspace")?,
                        "slug",
                    )?),
                    ..AccountScope::default()
                })),
            })
        });
        return Ok(Credential { key, auth: auth? });
    }
}
