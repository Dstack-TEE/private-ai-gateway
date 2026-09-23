use crate::*;
use desktop_runtime::ui_api::Method;
use serde_json::{json, Value};

#[tauri::command]
pub(crate) async fn complete_account_login(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    id: String,
    callback_url: String,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::CompleteAccountLogin,
        json!({ "id": id, "callbackUrl": callback_url }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn begin_account_login(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    profile: ConfidentialProfileInput,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::BeginAccountLogin,
        json!({ "profile": profile }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn poll_account_login(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    id: String,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::PollAccountLogin,
        json!({ "id": id }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn save_account_login(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    id: String,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    workspace_id: Option<i64>,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::SaveAccountLogin,
        json!({
            "id": id,
            "profile": profile,
            "requireProductionOs": require_production_os,
            "workspaceId": workspace_id
        }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn account_details(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::GetAccountDetails,
        json!({ "profileId": profile_id }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn account_balance(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    target: desktop_runtime::contracts::AccountBalanceTarget,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::GetAccountBalance,
        json!({ "target": target }),
    )
    .await
}

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

#[tauri::command]
pub(crate) async fn cancel_account_login(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    id: String,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::CancelAccountLogin,
        json!({ "id": id }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn activate_profile(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::ActivateProfile,
        json!({ "profileId": profile_id }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn delete_profile(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::DeleteProfile,
        json!({ "profileId": profile_id }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn save_configuration(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    key: Option<String>,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::SaveConfiguration,
        json!({
            "profile": profile,
            "requireProductionOs": require_production_os,
            "key": key
        }),
    )
    .await
}
