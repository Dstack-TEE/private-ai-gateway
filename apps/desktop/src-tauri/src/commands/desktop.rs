use std::sync::Arc;

use desktop_core::{
    agents::Agent,
    brand::AboutLink,
    client::{CallError, Client},
};
use tauri::{
    menu::{Menu, MenuBuilder},
    AppHandle, Manager, Runtime, State, WebviewWindow,
};
use tauri_plugin_clipboard_manager::ClipboardExt;

use super::open_url;
use crate::{distribution, run_blocking};

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

#[tauri::command]
pub(crate) fn show_edit_menu(window: WebviewWindow, editable: bool) -> Result<(), CallError> {
    let menu = edit_menu(window.app_handle(), editable).map_err(|_| "Cannot build editing menu")?;
    window
        .popup_menu(&menu)
        .map_err(|_| "Cannot open editing menu".into())
}

/// The system editing actions a text field's context menu offers. Only macOS
/// has native Undo and Redo items.
fn edit_menu<R: Runtime>(app: &AppHandle<R>, editable: bool) -> tauri::Result<Menu<R>> {
    let mut menu = MenuBuilder::new(app);
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
    menu.select_all().build()
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

#[cfg(test)]
mod tests {
    #[test]
    #[cfg_attr(target_os = "macos", ignore = "muda menus need the main thread")]
    fn the_edit_menu_offers_what_the_field_allows() {
        let app = tauri::test::mock_app();
        let describe = |editable| {
            crate::menu::describe(
                super::edit_menu(app.handle(), editable)
                    .unwrap()
                    .items()
                    .unwrap(),
            )
        };
        let mut editable = vec!["Cut", "Copy", "Paste", "Select All"];
        if cfg!(target_os = "macos") {
            editable.splice(0..0, ["Undo", "Redo", "---"]);
        }
        assert_eq!(describe(true), editable);
        assert_eq!(describe(false), ["Copy", "Select All"]);
    }
}
