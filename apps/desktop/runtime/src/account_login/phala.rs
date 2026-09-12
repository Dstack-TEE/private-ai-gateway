use super::*;

pub(super) async fn phala(
    client: Client,
    device: String,
    interval: u64,
) -> Result<Credential, String> {
    phala_at(client, device, interval, PHALA_API).await
}

pub(super) async fn phala_at(
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
