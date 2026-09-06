mod autostart;
#[cfg(unix)]
mod helper_staging;
mod menu;
mod native_dialog;
mod notifications;
mod runtime_adapter;
mod tray;
mod updates;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

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
use tauri_plugin_clipboard_manager::ClipboardExt;

pub(crate) async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| "The background operation could not complete. Please try again.")?
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchPreferences {
    open_at_login: bool,
    connect_on_launch: bool,
}

#[derive(Default)]
struct ExitState {
    pending: AtomicBool,
    allowed: AtomicBool,
}

#[derive(serde::Serialize)]
struct ListenAddress {
    address: String,
    name: String,
}

#[tauri::command]
async fn list_listen_addresses() -> Result<Vec<ListenAddress>, String> {
    run_blocking(|| {
        let interfaces = if_addrs::get_if_addrs()
            .map_err(|_| "Could not read network interfaces. Enter an IP address manually.".to_string())?;
        let mut addresses: Vec<_> = interfaces
            .into_iter()
            .filter(|interface| interface.is_oper_up())
            // Link-local IPv6 needs a scope ID, which the listener does not support.
            .filter(|interface| !matches!(interface.ip(), std::net::IpAddr::V6(ip) if ip.is_unicast_link_local()))
            .map(|interface| ListenAddress {
                address: interface.ip().to_string(),
                name: interface.name,
            })
            .collect();
        addresses.sort_by(|a, b| a.address.cmp(&b.address).then(a.name.cmp(&b.name)));
        addresses.dedup_by(|a, b| a.address == b.address);
        Ok(addresses)
    })
    .await
}

#[tauri::command]
async fn get_launch_preferences(app: AppHandle) -> Result<LaunchPreferences, String> {
    run_blocking(move || load_launch_preferences(&app)).await
}

fn load_launch_preferences(app: &AppHandle) -> Result<LaunchPreferences, String> {
    Ok(LaunchPreferences {
        open_at_login: autostart::is_enabled(app)?,
        connect_on_launch: desktop_runtime::preferences::load()?.connect_on_launch,
    })
}

#[tauri::command]
async fn set_launch_preference(
    app: AppHandle,
    name: String,
    enabled: bool,
) -> Result<LaunchPreferences, String> {
    run_blocking(move || {
        match name.as_str() {
            "openAtLogin" => tray::set_open_at_login(&app, enabled)?,
            "connectOnLaunch" => desktop_runtime::preferences::update(|preferences| {
                preferences.connect_on_launch = enabled
            })?,
            _ => return Err("Unknown startup preference".to_string()),
        }
        let preferences = load_launch_preferences(&app)?;
        let _ = app.emit("gateway://launch-preferences", &preferences);
        Ok(preferences)
    })
    .await
}

const AUTOSTART_ARG: &str = "--autostart";

#[tauri::command]
async fn get_appearance() -> Result<desktop_runtime::preferences::Appearance, String> {
    run_blocking(|| Ok(desktop_runtime::preferences::load()?.appearance)).await
}

fn apply_appearance(app: &AppHandle, appearance: desktop_runtime::preferences::Appearance) {
    use desktop_runtime::preferences::Appearance;
    app.set_theme(match appearance {
        Appearance::System => None,
        Appearance::Light => Some(tauri::Theme::Light),
        Appearance::Dark => Some(tauri::Theme::Dark),
    });
}

#[tauri::command]
async fn set_appearance(
    app: AppHandle,
    appearance: desktop_runtime::preferences::Appearance,
) -> Result<(), String> {
    run_blocking(move || {
        desktop_runtime::preferences::update(|preferences| preferences.appearance = appearance)
    })
    .await?;
    apply_appearance(&app, appearance);
    app.emit("gateway://appearance", appearance)
        .map_err(|_| "Could not sync appearance".to_string())
}

#[tauri::command]
async fn get_gateway_state(
    runtime: State<'_, Arc<DesktopRuntime>>,
) -> Result<GatewayState, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.state()).await
}

#[tauri::command]
async fn start_gateway(
    runtime: State<'_, Arc<DesktopRuntime>>,
    config: StartGatewayConfig,
) -> Result<GatewayState, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.start(config)).await
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
async fn activate_profile(
    runtime: State<'_, Arc<DesktopRuntime>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.activate_profile(profile_id)).await
}

#[tauri::command]
async fn delete_profile(
    runtime: State<'_, Arc<DesktopRuntime>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.delete_profile(profile_id)).await
}

#[tauri::command]
async fn stop_gateway(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<GatewayState, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.stop()).await
}

#[tauri::command]
async fn clear_api_key(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<GatewayState, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.clear_api_key()).await
}

#[tauri::command]
async fn query_usage(
    runtime: State<'_, Arc<DesktopRuntime>>,
    query: UsageQuery,
) -> Result<UsagePage, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.query_usage(query)).await
}

#[tauri::command]
async fn get_usage_record(
    runtime: State<'_, Arc<DesktopRuntime>>,
    record_id: String,
) -> Result<RequestActivity, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || {
        runtime
            .usage_record(&record_id)?
            .ok_or_else(|| "Usage record not found".to_string())
    })
    .await
}

#[tauri::command]
async fn export_usage_csv(
    runtime: State<'_, Arc<DesktopRuntime>>,
    query: UsageQuery,
    path: String,
) -> Result<usize, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.export_usage_csv(query, PathBuf::from(path))).await
}

#[tauri::command]
async fn clear_usage(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<u64, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.clear_usage()).await
}

#[tauri::command]
async fn get_client_key(runtime: State<'_, Arc<DesktopRuntime>>) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.client_key()).await
}

#[tauri::command]
async fn rotate_client_key(
    app: AppHandle,
    runtime: State<'_, Arc<DesktopRuntime>>,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    let result = run_blocking(move || runtime.rotate_client_key()).await;
    let _ = app.emit("gateway://client-key-changed", result.is_ok());
    result
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
async fn list_agents(
    app: AppHandle,
    runtime: State<'_, Arc<DesktopRuntime>>,
) -> Result<Vec<AgentStatus>, String> {
    let runtime = runtime.inner().clone();
    let agents = run_blocking(move || runtime.list_agents()).await?;
    tray::sync_agents(&app, &agents);
    Ok(agents)
}

#[tauri::command]
async fn preview_agent_connection(
    runtime: State<'_, Arc<DesktopRuntime>>,
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
) -> Result<AgentPreview, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.preview_agent(agent_id, connect, options)).await
}

#[tauri::command]
async fn apply_agent_connection(
    runtime: State<'_, Arc<DesktopRuntime>>,
    agent_id: String,
    connect: bool,
    revision: String,
    options: ConnectOptions,
) -> Result<AgentStatus, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.apply_agent(agent_id, connect, revision, options)).await
}

#[tauri::command]
async fn disconnect_all_agents(
    runtime: State<'_, Arc<DesktopRuntime>>,
) -> Result<Vec<AgentStatus>, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.disconnect_all_agents()).await
}

#[tauri::command]
async fn open_agent_website(app: AppHandle, agent_id: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = match agent_id.as_str() {
        "claude-code" => "https://code.claude.com",
        "codex" => "https://developers.openai.com/codex/cli/",
        "opencode" => "https://opencode.ai",
        "pi" => "https://pi.dev",
        "hermes" => "https://hermes-agent.nousresearch.com",
        "openclaw" => "https://openclaw.ai",
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
async fn open_about_link(app: AppHandle, target: String) -> Result<(), String> {
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
async fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
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
fn show_edit_menu(window: tauri::WebviewWindow, editable: bool) -> Result<(), String> {
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
fn open_native_dialog(
    app: AppHandle,
    kind: String,
    repair: bool,
    record_id: Option<String>,
    profile_id: Option<String>,
) -> Result<(), String> {
    native_dialog::open(
        &app,
        &kind,
        repair,
        record_id.as_deref(),
        profile_id.as_deref(),
    )
}

#[tauri::command]
async fn native_dialog_ready(window: tauri::WebviewWindow) -> Result<(), String> {
    native_dialog::ready(&window).await
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

    let app =
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_window(app);
        }));
    #[cfg(target_os = "macos")]
    let app = app.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec![AUTOSTART_ARG]),
    ));
    let app = app
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filter(|label| label == "main")
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .manage(updates::PendingUpdate::default())
        .manage(updates::UpdateProgress::default())
        .manage(ExitState::default())
        .plugin(tauri_plugin_notification::init())
        .manage(notifications::Settings::default())
        .invoke_handler(tauri::generate_handler![
            get_gateway_state,
            notifications::get_notification_settings,
            notifications::save_notification_settings,
            notifications::request_notification_permission,
            notifications::open_notification_settings,
            get_appearance,
            set_appearance,
            updates::check_update,
            updates::get_update_channel,
            updates::set_update_channel,
            updates::install_update,
            updates::get_update_progress,
            get_launch_preferences,
            set_launch_preference,
            start_gateway,
            verify_configuration,
            activate_profile,
            delete_profile,
            stop_gateway,
            copy_text,
            show_edit_menu,
            open_native_dialog,
            native_dialog_ready,
            open_agent_website,
            close_native_dialog,
            query_usage,
            get_usage_record,
            export_usage_csv,
            clear_usage,
            open_about_link,
            clear_api_key,
            get_client_key,
            rotate_client_key,
            save_local_api_config,
            list_listen_addresses,
            refresh_catalog,
            list_agents,
            preview_agent_connection,
            apply_agent_connection,
            disconnect_all_agents
        ])
        .setup(move |app| {
            notifications::initialize(app.handle());
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            autostart::setup(app.handle())?;
            if let Ok(preferences) = desktop_runtime::preferences::load() {
                apply_appearance(app.handle(), preferences.appearance);
            }
            if app.config().plugins.0.contains_key("updater") {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
            }
            launcher_for_setup.initialize(app.handle().clone())?;
            let helper_path = std::env::current_exe()
                .map_err(|error| format!("Cannot locate the app executable: {error}"))?
                .parent()
                .ok_or_else(|| "Cannot locate the app directory".to_string())?
                .join(helper_binary_name());
            #[cfg(unix)]
            let helper_path = {
                #[cfg(target_os = "linux")]
                let required = app.env().appimage.is_some();
                #[cfg(not(target_os = "linux"))]
                let required = false;
                let staged = desktop_gateway::agents::app_data_dir().and_then(|directory| {
                    helper_staging::stage(&helper_path, &directory)
                        .map_err(|error| format!("Cannot stage the credential helper: {error}"))
                });
                match staged {
                    Ok(path) if required => path,
                    Ok(_) => helper_path,
                    Err(error) if required => return Err(error.into()),
                    Err(error) => {
                        // OpenClaw independently validates the staged copy before use.
                        eprintln!("{error}");
                        helper_path
                    }
                }
            };
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
                if matches!(event, WindowEvent::Focused(true)) {
                    let _ = window_for_events.emit("gateway://agents-changed", ());
                }
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
            let mut alerts = notifications::Observer::new(&initial);
            tauri::async_runtime::spawn(async move {
                while states.changed().await.is_ok() {
                    let state = states.borrow().clone();
                    tray::sync(&handle, &state);
                    alerts.update(&handle, &state);
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
        tauri::RunEvent::ExitRequested { api, code, .. } => {
            // The updater already restores configurations before restart,
            // and Tauri restart requests cannot be deferred.
            if code == Some(tauri::RESTART_EXIT_CODE) {
                return;
            }
            let exit = app.state::<ExitState>();
            if exit.allowed.load(Ordering::Acquire) {
                return;
            }
            if let Some(runtime) = app.try_state::<Arc<DesktopRuntime>>() {
                api.prevent_exit();
                if exit.pending.swap(true, Ordering::AcqRel) {
                    return;
                }
                let app = app.clone();
                let runtime = runtime.inner().clone();
                tauri::async_runtime::spawn(async move {
                    let worker = runtime.clone();
                    let result = run_blocking(move || worker.prepare_exit()).await;
                    let exit = app.state::<ExitState>();
                    match result {
                        Ok(()) => {
                            exit.allowed.store(true, Ordering::Release);
                            app.exit(code.unwrap_or(0));
                        }
                        Err(error) => {
                            exit.pending.store(false, Ordering::Release);
                            runtime.report_error(format!(
                                "Cannot quit until agent configurations are restored: {error}"
                            ));
                            tray::show_window(&app);
                        }
                    }
                });
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_window(app),
        _ => {}
    });
}
