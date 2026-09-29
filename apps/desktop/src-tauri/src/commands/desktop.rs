use std::sync::Arc;

use desktop_core::{
    agents::Agent,
    brand::AboutLink,
    client::{CallError, Client},
    contracts::ServiceProvider,
    ui_api::Method,
};
use serde_json::{json, Value};
use tauri::{menu::MenuBuilder, AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;

use crate::{distribution, run_blocking};

/// Opens `url` in the default browser, reporting `failure` if it does not open.
pub(crate) async fn open_url(
    app: AppHandle,
    url: String,
    failure: &'static str,
) -> Result<(), CallError> {
    Ok(run_blocking(move || {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|_| failure.to_string())
    })
    .await?)
}

#[tauri::command]
pub(crate) async fn open_agent_website(app: AppHandle, agent_id: String) -> Result<(), CallError> {
    let url = Agent::from_id(&agent_id)?.website();
    open_url(app, url.into(), "Cannot open the agent website").await
}

#[tauri::command]
pub(crate) async fn open_about_link(app: AppHandle, target: AboutLink) -> Result<(), CallError> {
    let url = target.url().into();
    open_url(app, url, "Cannot open the resource in your browser").await
}

/// Opens the listening web UI in the system browser. The address carries no
/// secret; the page asks for the web UI password.
#[tauri::command]
pub(crate) async fn open_web_ui(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<(), CallError> {
    distribution::require(
        distribution::CAPABILITIES.web_ui,
        "The web UI is unavailable in this distribution",
    )?;
    let client = client.inner().clone();
    let url = run_blocking(move || {
        client
            .state()?
            .web_ui
            .url
            .ok_or_else(|| "The web UI is not listening".to_string())
    })
    .await?;
    open_url(app, url, "Cannot open the web UI in your browser").await
}

#[tauri::command]
pub(crate) async fn copy_text(app: AppHandle, text: String) -> Result<(), CallError> {
    if text.is_empty() || text.len() > 4_096 {
        return Err("Invalid clipboard text".into());
    }
    Ok(run_blocking(move || {
        app.clipboard()
            .write_text(text)
            .map_err(|_| "Cannot copy text".to_string())
    })
    .await?)
}

/// Shows the system editing actions a text field allows. Only macOS has
/// native Undo and Redo items.
#[tauri::command]
pub(crate) fn show_edit_menu(window: WebviewWindow, editable: bool) -> Result<(), CallError> {
    let mut menu = MenuBuilder::new(window.app_handle());
    if editable {
        if cfg!(target_os = "macos") {
            menu = menu.undo().redo().separator();
        }
        menu = menu.cut();
    }
    menu = menu.copy();
    if editable {
        menu = menu.paste();
    }
    let menu = menu
        .select_all()
        .build()
        .map_err(|_| "Cannot build editing menu")?;
    window
        .popup_menu(&menu)
        .map_err(|_| "Cannot open editing menu".into())
}

/// Quits the app and leaves the background service running, like Quit in the
/// tray menu.
#[tauri::command]
pub(crate) fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub(crate) async fn stop_all_and_quit(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<(), CallError> {
    let client = client.inner().clone();
    run_blocking(move || client.shutdown()).await?;
    app.exit(0);
    Ok(())
}

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
