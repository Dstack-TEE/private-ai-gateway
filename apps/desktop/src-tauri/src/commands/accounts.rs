//! Account portal pages, opened in the default browser.

use std::sync::Arc;

use desktop_core::{
    client::{CallError, Client},
    contracts::ServiceProvider,
    ui_api::Method,
};
use serde_json::{json, Value};
use tauri::{AppHandle, State};

use super::open_url;
use crate::distribution;

const ACCOUNT_PAGE_FAILURE: &str = "Cannot open the account page";

fn require_portal_links() -> Result<(), String> {
    distribution::require(
        distribution::CAPABILITIES.account_portal_links,
        "Account portal links are unavailable in this distribution",
    )
}

/// Opens the account page the renderer method `method` resolves.
async fn open_account_page(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    method: Method,
    params: Value,
) -> Result<(), CallError> {
    let value = crate::ui_api::invoke(app.clone(), client, method, params).await?;
    let url = serde_json::from_value(value).map_err(|_| "Management response failed")?;
    open_url(app, url, ACCOUNT_PAGE_FAILURE).await
}

#[tauri::command]
pub(crate) async fn open_top_up(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    provider: ServiceProvider,
    scope_slug: Option<String>,
) -> Result<(), CallError> {
    open_account_page(
        app,
        client,
        Method::GetTopUpUrl,
        json!({ "provider": provider, "scopeSlug": scope_slug }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn open_organization(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    organization_slug: String,
) -> Result<(), CallError> {
    require_portal_links()?;
    open_account_page(
        app,
        client,
        Method::GetOrganizationUrl,
        json!({ "organizationSlug": organization_slug }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn open_api_key_page(
    app: AppHandle,
    provider: ServiceProvider,
) -> Result<(), CallError> {
    require_portal_links()?;
    let url = desktop_core::account::api_key_page(provider)
        .ok_or("Custom providers do not have a built-in API key page")?;
    open_url(app, url.to_string(), ACCOUNT_PAGE_FAILURE).await
}
