use crate::*;

#[tauri::command]
pub(crate) async fn open_agent_website(app: AppHandle, agent_id: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = match agent_id.as_str() {
        "claude-code" => "https://code.claude.com",
        "codex" => "https://developers.openai.com/codex/cli/",
        "opencode" => "https://opencode.ai",
        "pi" => "https://pi.dev",
        "hermes" => "https://hermes-agent.nousresearch.com",
        "openclaw" => "https://openclaw.ai",
        "oh-my-pi" => "https://omp.sh",
        _ => return Err("Unknown agent".to_string()),
    };
    run_blocking(move || {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|_| "Cannot open the agent website".to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn open_about_link(app: AppHandle, target: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = match target.as_str() {
        "documentation" => desktop_gateway::brand::SUPPORT_URL,
        "github" => "https://github.com/Dstack-TEE/private-ai-gateway",
        _ => return Err("Unknown resource".to_string()),
    };
    run_blocking(move || {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|_| "Cannot open the resource in your browser".to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    if text.is_empty() || text.len() > 4_096 {
        return Err("Invalid clipboard text".to_string());
    }
    run_blocking(move || {
        app.clipboard()
            .write_text(text)
            .map_err(|_| "Cannot copy text".to_string())
    })
    .await
}

#[tauri::command]
pub(crate) fn show_edit_menu(window: tauri::WebviewWindow, editable: bool) -> Result<(), String> {
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
        .map_err(|_| "Cannot open editing menu".to_string())
}

#[tauri::command]
pub(crate) async fn open_native_dialog(
    app: AppHandle,
    kind: String,
    repair: bool,
    record_id: Option<String>,
    profile_id: Option<String>,
) -> Result<(), String> {
    run_blocking(move || {
        native_dialog::open(
            &app,
            &kind,
            repair,
            record_id.as_deref(),
            profile_id.as_deref(),
        )
    })
    .await
}

#[tauri::command]
pub(crate) fn native_dialog_ready(window: tauri::WebviewWindow) -> Result<(), String> {
    native_dialog::ready(&window)
}

#[tauri::command]
pub(crate) fn main_window_ready(window: tauri::WebviewWindow) -> Result<(), String> {
    tray::main_window_ready(&window)
}

#[tauri::command]
pub(crate) fn close_native_dialog(window: tauri::WebviewWindow) -> Result<(), String> {
    native_dialog::close(&window)
}

#[tauri::command]
pub(crate) async fn stop_all_and_quit(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<(), String> {
    let client = client.inner().clone();
    run_blocking(move || client.shutdown()).await?;
    app.exit(0);
    Ok(())
}
