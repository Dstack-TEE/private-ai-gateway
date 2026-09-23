use crate::*;
use desktop_runtime::ui_api::Method;
use serde_json::json;

#[tauri::command]
pub(crate) async fn open_top_up(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    provider: desktop_runtime::contracts::ServiceProvider,
    scope_slug: Option<String>,
) -> Result<(), String> {
    let value = crate::ui_api::invoke(
        window.clone(),
        client,
        Method::GetTopUpUrl,
        json!({ "provider": provider, "scopeSlug": scope_slug }),
    )
    .await?;
    let url = serde_json::from_value(value).map_err(|_| "Management response failed")?;
    open_account_url(window.app_handle().clone(), url).await
}

#[tauri::command]
pub(crate) async fn open_organization(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    organization_slug: String,
) -> Result<(), String> {
    distribution::require(
        distribution::CAPABILITIES.account_portal_links,
        "Account portal links are unavailable in this distribution",
    )?;
    let value = crate::ui_api::invoke(
        window.clone(),
        client,
        Method::GetOrganizationUrl,
        json!({ "organizationSlug": organization_slug }),
    )
    .await?;
    let url = serde_json::from_value(value).map_err(|_| "Management response failed")?;
    open_account_url(window.app_handle().clone(), url).await
}
