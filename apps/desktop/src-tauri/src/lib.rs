mod menu;
mod native_dialog;
mod runtime_adapter;
mod tray;

use std::{path::PathBuf, sync::Arc};

use desktop_gateway::agents::helper_binary_name;
use desktop_runtime::{
    contracts::{
        AgentPreview, AgentStatus, ConfidentialProfileInput, ConnectOptions, GatewayState,
        LocalApiConfig, RequestActivity, StartGatewayConfig,
    },
    controller::{DesktopRuntime, RuntimeOptions},
    usage::{UsagePage, UsageQuery},
};
use runtime_adapter::TauriSidecarLauncher;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_clipboard_manager::ClipboardExt;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchPreferences {
    open_at_login: bool,
    connect_on_launch: bool,
}

#[tauri::command]
fn get_launch_preferences(app: AppHandle) -> Result<LaunchPreferences, String> {
    Ok(LaunchPreferences {
        open_at_login: app
            .autolaunch()
            .is_enabled()
            .map_err(|error| error.to_string())?,
        connect_on_launch: desktop_runtime::preferences::load()?.connect_on_launch,
    })
}

#[tauri::command]
fn set_launch_preference(
    app: AppHandle,
    name: String,
    enabled: bool,
) -> Result<LaunchPreferences, String> {
    match name.as_str() {
        "openAtLogin" => tray::set_open_at_login(&app, enabled)?,
        "connectOnLaunch" => {
            desktop_runtime::preferences::save(desktop_runtime::preferences::Preferences {
                connect_on_launch: enabled,
            })?
        }
        _ => return Err("Unknown startup preference".to_string()),
    }
    let preferences = get_launch_preferences(app.clone())?;
    let _ = app.emit("gateway://launch-preferences", &preferences);
    Ok(preferences)
}

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
fn get_usage_record(
    runtime: State<'_, Arc<DesktopRuntime>>,
    record_id: String,
) -> Result<RequestActivity, String> {
    runtime
        .usage_record(&record_id)?
        .ok_or_else(|| "Usage record not found".to_string())
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
fn rotate_client_key(
    app: AppHandle,
    runtime: State<'_, Arc<DesktopRuntime>>,
) -> Result<String, String> {
    let key = runtime.rotate_client_key()?;
    let _ = app.emit("gateway://client-key-changed", ());
    Ok(key)
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

#[tauri::command]
fn open_native_dialog(
    app: AppHandle,
    kind: String,
    repair: bool,
    record_id: Option<String>,
) -> Result<(), String> {
    native_dialog::open(&app, &kind, repair, record_id.as_deref())
}

#[tauri::command]
fn close_native_dialog(window: tauri::WebviewWindow) -> Result<(), String> {
    native_dialog::close(&window)
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
            get_launch_preferences,
            set_launch_preference,
            start_gateway,
            verify_configuration,
            activate_profile,
            delete_profile,
            stop_gateway,
            copy_text,
            open_native_dialog,
            close_native_dialog,
            query_usage,
            get_usage_record,
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
                task_runtime: tauri::async_runtime::handle().inner().clone(),
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
            if let Err(error) = tray::setup(app.handle()) {
                runtime.report_error(format!("The system tray is unavailable: {error}"));
            }
            if let Err(error) = menu::setup(app.handle()) {
                runtime.report_error(format!("The application menu is unavailable: {error}"));
            }

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
            match desktop_runtime::preferences::load() {
                Ok(preferences) if preferences.connect_on_launch => {
                    tauri::async_runtime::spawn_blocking(move || {
                        let result = runtime
                            .state()
                            .and_then(|state| runtime.clone().start(state.config));
                        if let Err(error) = result {
                            runtime.report_error(format!("Automatic connection failed: {error}"));
                        }
                    });
                }
                Err(error) => runtime.report_error(error),
                _ => {}
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Tauri application");

    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { api, .. } => {
            if let Some(runtime) = app.try_state::<Arc<DesktopRuntime>>() {
                if let Err(error) = runtime.stop() {
                    api.prevent_exit();
                    runtime.report_error(format!(
                        "Cannot quit until agent configurations are restored: {error}"
                    ));
                    tray::show_window(app);
                }
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_window(app),
        _ => {}
    });
}
