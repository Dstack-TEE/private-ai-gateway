use crate::*;

#[tauri::command]
pub(crate) async fn complete_account_login(
    client: State<'_, Arc<Client>>,
    id: String,
    callback_url: String,
) -> Result<(), String> {
    client
        .inner()
        .clone()
        .complete_account_login(id, callback_url)
        .await
}

#[tauri::command]
pub(crate) async fn begin_account_login(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    profile: ConfidentialProfileInput,
) -> Result<desktop_runtime::account_login::LoginPresentation, String> {
    use tauri_plugin_opener::OpenerExt;
    let client = client.inner().clone();
    let login = client.begin_account_login(profile).await?;
    if app.opener().open_url(&login.url, None::<&str>).is_err() {
        // Keep the authorization available for the copy-link/manual callback path.
        eprintln!("Cannot open sign-in browser; use the manual sign-in link");
    }
    Ok(login)
}

#[tauri::command]
pub(crate) async fn poll_account_login(
    client: State<'_, Arc<Client>>,
    id: String,
) -> Result<Option<desktop_runtime::contracts::AccountLoginDetails>, String> {
    client.inner().clone().poll_account_login(id).await
}

#[tauri::command]
pub(crate) async fn save_account_login(
    client: State<'_, Arc<Client>>,
    id: String,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    workspace_id: Option<i64>,
) -> Result<GatewayState, String> {
    client
        .inner()
        .clone()
        .save_account_login(id, profile, require_production_os, workspace_id)
        .await
}

#[tauri::command]
pub(crate) async fn account_details(
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<desktop_runtime::contracts::AccountLoginDetails, String> {
    client.inner().clone().account_details(profile_id).await
}

#[tauri::command]
pub(crate) async fn account_balance(
    client: State<'_, Arc<Client>>,
    target: desktop_runtime::contracts::AccountBalanceTarget,
) -> Result<Option<desktop_runtime::contracts::AccountBalance>, String> {
    client.inner().clone().account_balance(target).await
}

#[tauri::command]
pub(crate) async fn open_top_up(
    app: AppHandle,
    provider: desktop_runtime::contracts::ServiceProvider,
    scope_slug: Option<String>,
) -> Result<(), String> {
    let url = desktop_runtime::account_login::top_up_url(&provider, scope_slug.as_deref())?;
    open_account_url(app, url).await
}

#[tauri::command]
pub(crate) async fn open_organization(
    app: AppHandle,
    organization_slug: String,
) -> Result<(), String> {
    let url = desktop_runtime::account_login::organization_url(Some(&organization_slug))?;
    open_account_url(app, url).await
}

#[tauri::command]
pub(crate) async fn cancel_account_login(
    client: State<'_, Arc<Client>>,
    id: String,
) -> Result<(), String> {
    client.inner().clone().cancel_account_login(id).await
}

#[tauri::command]
pub(crate) async fn activate_profile(
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.activate_profile(profile_id)).await
}

#[tauri::command]
pub(crate) async fn delete_profile(
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.delete_profile(profile_id)).await
}

#[tauri::command]
pub(crate) async fn clear_api_key(client: State<'_, Arc<Client>>) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.clear_api_key()).await
}

#[tauri::command]
pub(crate) async fn verify_configuration(
    client: State<'_, Arc<Client>>,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    key: Option<String>,
) -> Result<GatewayState, String> {
    client
        .inner()
        .clone()
        .verify_configuration(profile, require_production_os, key)
        .await
}

#[tauri::command]
pub(crate) async fn save_configuration(
    client: State<'_, Arc<Client>>,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    key: Option<String>,
) -> Result<GatewayState, String> {
    client
        .inner()
        .clone()
        .save_configuration(profile, require_production_os, key)
        .await
}
