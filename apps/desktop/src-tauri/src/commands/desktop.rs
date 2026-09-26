use std::sync::Arc;

use desktop_core::{
    agents::Agent,
    brand::AboutLink,
    client::{CallError, Client},
    contracts::ServiceProvider,
};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::{distribution, open_account_url, run_blocking};

async fn open_url(app: AppHandle, url: String, failure: &'static str) -> Result<(), CallError> {
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
    open_url(
        app,
        target.url().into(),
        "Cannot open the resource in your browser",
    )
    .await
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

#[tauri::command]
pub(crate) fn show_edit_menu(window: WebviewWindow, editable: bool) -> Result<(), CallError> {
    use tauri::menu::{Menu, PredefinedMenuItem};
    let app = window.app_handle();
    let menu = Menu::new(app).map_err(|_| "Cannot create editing menu")?;
    #[cfg(target_os = "macos")]
    if editable {
        menu.append_items(&[
            &PredefinedMenuItem::undo(app, None).map_err(|_| "Cannot create Undo action")?,
            &PredefinedMenuItem::redo(app, None).map_err(|_| "Cannot create Redo action")?,
            &PredefinedMenuItem::separator(app).map_err(|_| "Cannot create menu separator")?,
        ])
        .map_err(|_| "Cannot build editing menu")?;
    }
    if editable {
        menu.append(&PredefinedMenuItem::cut(app, None).map_err(|_| "Cannot create Cut action")?)
            .map_err(|_| "Cannot build editing menu")?;
    }
    menu.append(&PredefinedMenuItem::copy(app, None).map_err(|_| "Cannot create Copy action")?)
        .map_err(|_| "Cannot build editing menu")?;
    if editable {
        menu.append(
            &PredefinedMenuItem::paste(app, None).map_err(|_| "Cannot create Paste action")?,
        )
        .map_err(|_| "Cannot build editing menu")?;
    }
    menu.append(
        &PredefinedMenuItem::select_all(app, None)
            .map_err(|_| "Cannot create Select All action")?,
    )
    .map_err(|_| "Cannot build editing menu")?;
    window
        .popup_menu(&menu)
        .map_err(|_| "Cannot open editing menu".into())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Confirmation {
    title: String,
    message: String,
    confirm_label: String,
    cancel_label: Option<String>,
    destructive: bool,
}

/// Asks for a decision in the system alert, attached to the window: a sheet on
/// macOS, a task dialog on Windows and a message dialog on Linux. The confirm
/// button is the default (Return) button and Cancel takes Escape; `true` when
/// the user confirms.
#[tauri::command]
pub(crate) async fn show_confirmation(
    window: WebviewWindow,
    confirmation: Confirmation,
) -> Result<bool, CallError> {
    let Confirmation {
        title,
        message,
        confirm_label,
        cancel_label,
        destructive,
    } = confirmation;
    let cancel_label = cancel_label.unwrap_or_else(|| "Cancel".into());
    if [&title, &message, &confirm_label, &cancel_label]
        .iter()
        .any(|text| text.trim().is_empty() || text.len() > 4_096)
    {
        return Err("Invalid confirmation".into());
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .message(message)
        .title(title)
        .kind(if destructive {
            MessageDialogKind::Warning
        } else {
            MessageDialogKind::Info
        })
        .buttons(MessageDialogButtons::OkCancelCustom(
            confirm_label,
            cancel_label,
        ))
        .parent(&window)
        .show(move |confirmed| {
            let _ = send.send(confirmed);
        });
    Ok(receive
        .await
        .map_err(|_| "The confirmation could not complete")?)
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

#[tauri::command]
pub(crate) async fn open_api_key_page(
    app: AppHandle,
    provider: ServiceProvider,
) -> Result<(), CallError> {
    distribution::require(
        distribution::CAPABILITIES.account_portal_links,
        "Account portal links are unavailable in this distribution",
    )?;
    let url = desktop_core::account::api_key_page(provider)
        .ok_or("Custom providers do not have a built-in API key page")?;
    Ok(open_account_url(app, url.to_string()).await?)
}
