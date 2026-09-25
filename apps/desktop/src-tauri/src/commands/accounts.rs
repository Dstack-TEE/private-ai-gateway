use std::sync::Arc;

use desktop_core::{
    client::{CallError, Client},
    contracts::ServiceProvider,
    ui_api::Method,
};
use serde_json::{json, Value};
use tauri::{Manager, State, WebviewWindow};

use crate::{distribution, open_account_url};

/// Opens the account page the renderer method `method` resolves.
async fn open_account_page(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    method: Method,
    params: Value,
) -> Result<(), CallError> {
    let value = crate::ui_api::invoke(window.clone(), client, method, params).await?;
    let url = serde_json::from_value(value).map_err(|_| "Management response failed")?;
    Ok(open_account_url(window.app_handle().clone(), url).await?)
}

#[tauri::command]
pub(crate) async fn open_top_up(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    provider: ServiceProvider,
    scope_slug: Option<String>,
) -> Result<(), CallError> {
    open_account_page(
        window,
        client,
        Method::GetTopUpUrl,
        json!({ "provider": provider, "scopeSlug": scope_slug }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn open_organization(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    organization_slug: String,
) -> Result<(), CallError> {
    distribution::require(
        distribution::CAPABILITIES.account_portal_links,
        "Account portal links are unavailable in this distribution",
    )?;
    open_account_page(
        window,
        client,
        Method::GetOrganizationUrl,
        json!({ "organizationSlug": organization_slug }),
    )
    .await
}
