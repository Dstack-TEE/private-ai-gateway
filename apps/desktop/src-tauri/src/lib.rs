mod menu;
mod runtime_adapter;
mod tray;

use std::{path::PathBuf, sync::Arc};

use desktop_gateway::agents::helper_binary_name;
use desktop_runtime::{
    contracts::{
        AgentPreview, AgentStatus, ConfidentialProfileInput, ConnectOptions, GatewayState,
        LocalApiConfig, StartGatewayConfig,
    },
    controller::{DesktopRuntime, RuntimeOptions},
    usage::{UsagePage, UsageQuery},
};
use runtime_adapter::TauriSidecarLauncher;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;

const AUTOSTART_ARG: &str = "--autostart";

#[tauri::command]
fn get_gateway_state(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<GatewayState, String> {
    runtime.state()
}

#[tauri::command]
fn start_gateway(
    runtime: State<'_, Arc<DesktopRuntime>>,
    config: StartGatewayConfig,
) -> Result<GatewayState, String> {
    runtime.inner().clone().start(config)
}

#[tauri::command]
async fn verify_configuration(
    runtime: State<'_, Arc<DesktopRuntime>>,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    key: Option<String>,
) -> Result<GatewayState, String> {
    runtime
        .inner()
        .clone()
        .verify_configuration(profile, require_production_os, key)
        .await
}

#[tauri::command]
fn activate_profile(
    runtime: State<'_, Arc<DesktopRuntime>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    runtime.activate_profile(profile_id)
}

#[tauri::command]
fn delete_profile(
    runtime: State<'_, Arc<DesktopRuntime>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    runtime.delete_profile(profile_id)
}

#[tauri::command]
fn stop_gateway(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<GatewayState, String> {
    runtime.stop()
}

#[tauri::command]
fn clear_api_key(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<GatewayState, String> {
    runtime.clear_api_key()
}

#[tauri::command]
fn query_usage(
    runtime: State<'_, Arc<DesktopRuntime>>,
    query: UsageQuery,
) -> Result<UsagePage, String> {
    runtime.query_usage(query)
}

#[tauri::command]
fn export_usage_csv(
    runtime: State<'_, Arc<DesktopRuntime>>,
    query: UsageQuery,
    path: String,
) -> Result<usize, String> {
    runtime.export_usage_csv(query, PathBuf::from(path))
}

#[tauri::command]
fn clear_usage(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<u64, String> {
    runtime.clear_usage()
}

#[tauri::command]
fn get_client_key(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<String, String> {
    runtime.client_key()
}

#[tauri::command]
fn rotate_client_key(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<String, String> {
    runtime.rotate_client_key()
}

#[tauri::command]
async fn save_local_api_config(
    runtime: State<'_, Arc<DesktopRuntime>>,
    config: LocalApiConfig,
) -> Result<GatewayState, String> {
    runtime.inner().clone().save_local_api_config(config).await
}

#[tauri::command]
async fn refresh_catalog(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<GatewayState, String> {
    runtime.inner().clone().refresh_catalog().await
}

#[tauri::command]
fn list_agents(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<Vec<AgentStatus>, String> {
    runtime.list_agents()
}

#[tauri::command]
fn preview_agent_connection(
    runtime: State<'_, Arc<DesktopRuntime>>,
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
) -> Result<AgentPreview, String> {
    runtime.preview_agent(agent_id, connect, options)
}

#[tauri::command]
fn apply_agent_connection(
    runtime: State<'_, Arc<DesktopRuntime>>,
    agent_id: String,
    connect: bool,
    revision: String,
    options: ConnectOptions,
) -> Result<AgentStatus, String> {
    runtime.apply_agent(agent_id, connect, revision, options)
}

#[tauri::command]
fn disconnect_all_agents(
    runtime: State<'_, Arc<DesktopRuntime>>,
) -> Result<Vec<AgentStatus>, String> {
    runtime.disconnect_all_agents()
}

#[tauri::command]
fn open_support(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(desktop_gateway::brand::SUPPORT_URL, None::<&str>)
        .map_err(|error| format!("Cannot open the support page: {error}"))
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
    let show_on_launch =
        !std::env::args_os().any(|argument| argument == std::ffi::OsStr::new(AUTOSTART_ARG));
    let launcher = Arc::new(TauriSidecarLauncher::default());
    let launcher_for_setup = launcher.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            get_gateway_state,
            start_gateway,
            verify_configuration,
            activate_profile,
            delete_profile,
            stop_gateway,
            copy_text,
            query_usage,
            export_usage_csv,
            clear_usage,
            open_support,
            clear_api_key,
            get_client_key,
            rotate_client_key,
            save_local_api_config,
            refresh_catalog,
            list_agents,
            preview_agent_connection,
            apply_agent_connection,
            disconnect_all_agents
        ])
        .setup(move |app| {
            launcher_for_setup.initialize(app.handle().clone())?;
            let helper_path = std::env::current_exe()
                .map_err(|error| format!("Cannot locate the app executable: {error}"))?
                .parent()
                .ok_or_else(|| "Cannot locate the app directory".to_string())?
                .join(helper_binary_name());
            let runtime = DesktopRuntime::launch(RuntimeOptions {
                launcher: launcher_for_setup.clone(),
                helper_path,
            })?;
            app.manage(runtime.clone());

            let window = app
                .get_webview_window("main")
                .ok_or_else(|| "main window was not created".to_string())?;
            window.set_title(desktop_gateway::brand::PRODUCT_NAME)?;
            let window_for_events = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_for_events.hide();
                }
            });
            tray::setup(app.handle())?;
            menu::setup(app.handle())?;

            let handle = app.handle().clone();
            let mut states = runtime.subscribe();
            let initial = runtime.state()?;
            tray::sync(&handle, &initial);
            tauri::async_runtime::spawn(async move {
                while states.changed().await.is_ok() {
                    let state = states.borrow().clone();
                    tray::sync(&handle, &state);
                    let _ = handle.emit("gateway://state", state);
                }
            });
            if show_on_launch {
                tray::show_window(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Tauri application");

    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { .. } => {
            if let Some(runtime) = app.try_state::<Arc<DesktopRuntime>>() {
                let _ = runtime.stop();
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_window(app),
        _ => {}
    });
}
