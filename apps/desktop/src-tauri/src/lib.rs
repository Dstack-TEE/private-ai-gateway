mod contracts;
mod gateway;
mod tray;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use contracts::{GatewayState, ReceiptSummary, StartGatewayConfig};
use gateway::GatewayManager;
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;

#[tauri::command]
fn get_gateway_state(manager: State<'_, GatewayManager>) -> Result<GatewayState, String> {
    manager.snapshot()
}

#[tauri::command]
fn start_gateway(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
    config: StartGatewayConfig,
) -> Result<GatewayState, String> {
    manager.start(&app, config)
}

#[tauri::command]
fn stop_gateway(
    app: AppHandle,
    manager: State<'_, GatewayManager>,
) -> Result<GatewayState, String> {
    manager.stop(&app)
}

#[tauri::command]
async fn list_receipts(manager: State<'_, GatewayManager>) -> Result<Vec<ReceiptSummary>, String> {
    manager.list_receipts().await
}

#[tauri::command]
fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    if text.is_empty() || text.len() > 4_096 {
        return Err("Invalid clipboard text".to_string());
    }
    app.clipboard()
        .write_text(text)
        .map_err(|error| format!("Cannot copy text: {error}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(GatewayManager::default())
        .invoke_handler(tauri::generate_handler![
            get_gateway_state,
            start_gateway,
            stop_gateway,
            list_receipts,
            copy_text
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let window = app
                .get_webview_window("main")
                .ok_or_else(|| "main window was not created".to_string())?;
            let window_for_events = window.clone();
            let was_focused = Arc::new(AtomicBool::new(false));
            window.on_window_event(move |event| match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    was_focused.store(false, Ordering::SeqCst);
                    let _ = window_for_events.hide();
                }
                WindowEvent::Focused(focused) => {
                    if *focused {
                        was_focused.store(true, Ordering::SeqCst);
                    } else if was_focused.swap(false, Ordering::SeqCst) {
                        let _ = window_for_events.hide();
                    }
                }
                _ => {}
            });
            tray::setup(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Tauri application");

    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { .. } => {
            let manager = app.state::<GatewayManager>();
            let _ = manager.stop(app);
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_popup(app, None),
        _ => {}
    });
}
