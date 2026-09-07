mod autostart;
mod menu;
mod native_dialog;
mod notifications;
mod tray;
mod updates;

use std::{path::PathBuf, sync::Arc};

use desktop_runtime::{
    cli_install::Registration,
    client::Client,
    contracts::{
        AgentPreview, AgentStatus, ConfidentialProfileInput, ConnectOptions, GatewayState,
        LocalApiConfig, RequestActivity, StartGatewayConfig,
    },
    preferences::Appearance,
    protocol::Preference,
    usage::{UsagePage, UsageQuery},
};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_shell::ShellExt;

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
async fn get_launch_preferences(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<LaunchPreferences, String> {
    let client = client.inner().clone();
    run_blocking(move || load_launch_preferences(&app, &client)).await
}

fn load_launch_preferences(app: &AppHandle, client: &Client) -> Result<LaunchPreferences, String> {
    Ok(LaunchPreferences {
        open_at_login: autostart::is_enabled(app)?,
        connect_on_launch: client.preferences()?.connect_on_launch,
    })
}

fn refresh_preferences(app: &AppHandle, client: &Arc<Client>) {
    let app = app.clone();
    let client = client.clone();
    tauri::async_runtime::spawn_blocking(move || {
        notifications::initialize(&app);
        let result = (|| {
            let preferences = client.preferences()?;
            let launch = LaunchPreferences {
                open_at_login: autostart::is_enabled(&app)?,
                connect_on_launch: preferences.connect_on_launch,
            };
            Ok::<_, String>((preferences.appearance, launch))
        })();
        match result {
            Ok((appearance, launch)) => {
                let app_for_main_thread = app.clone();
                let _ = app.run_on_main_thread(move || {
                    apply_appearance(&app_for_main_thread, appearance);
                    let _ = app_for_main_thread.emit("gateway://appearance", appearance);
                    let _ = app_for_main_thread.emit("gateway://launch-preferences", launch);
                });
            }
            Err(error) => client.report_error(error),
        }
    });
}

#[tauri::command]
async fn set_launch_preference(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    name: String,
    enabled: bool,
) -> Result<LaunchPreferences, String> {
    let client = client.inner().clone();
    run_blocking(move || {
        match name.as_str() {
            "openAtLogin" => tray::set_open_at_login(&app, enabled)?,
            "connectOnLaunch" => {
                client.set_preference(Preference::ConnectOnLaunch(enabled))?;
            }
            _ => return Err("Unknown startup preference".to_string()),
        }
        let preferences = load_launch_preferences(&app, &client)?;
        let _ = app.emit("gateway://launch-preferences", &preferences);
        Ok(preferences)
    })
    .await
}

const AUTOSTART_ARG: &str = "--autostart";

#[tauri::command]
async fn get_appearance(client: State<'_, Arc<Client>>) -> Result<Appearance, String> {
    let client = client.inner().clone();
    run_blocking(move || Ok(client.preferences()?.appearance)).await
}

fn apply_appearance(app: &AppHandle, appearance: Appearance) {
    app.set_theme(match appearance {
        Appearance::System => None,
        Appearance::Light => Some(tauri::Theme::Light),
        Appearance::Dark => Some(tauri::Theme::Dark),
    });
}

#[tauri::command]
async fn set_appearance(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    appearance: Appearance,
) -> Result<(), String> {
    let client = client.inner().clone();
    run_blocking(move || {
        client.set_preference(Preference::Appearance(appearance))?;
        Ok(())
    })
    .await?;
    apply_appearance(&app, appearance);
    app.emit("gateway://appearance", appearance)
        .map_err(|_| "Could not sync appearance".to_string())
}

#[tauri::command]
async fn get_gateway_state(client: State<'_, Arc<Client>>) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.state()).await
}

#[tauri::command]
async fn read_profile_backup(
    path: PathBuf,
) -> Result<desktop_runtime::maintenance::ProfileBackup, String> {
    run_blocking(move || desktop_runtime::maintenance::ProfileBackup::read(&path)).await
}

#[tauri::command]
async fn import_profiles(
    runtime: State<'_, Arc<Client>>,
    backup: desktop_runtime::maintenance::ProfileBackup,
) -> Result<desktop_runtime::maintenance::ImportResult, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.import_profiles(backup)).await
}

#[tauri::command]
async fn export_profiles(runtime: State<'_, Arc<Client>>, path: PathBuf) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.export_profiles(path)).await
}

#[tauri::command]
async fn export_diagnostics(runtime: State<'_, Arc<Client>>, path: PathBuf) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.export_diagnostics(path)).await
}

#[tauri::command]
async fn start_gateway(
    client: State<'_, Arc<Client>>,
    config: StartGatewayConfig,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.start(config)).await
}

#[tauri::command]
async fn start_backend_service(client: State<'_, Arc<Client>>) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || {
        Client::ensure_service()?;
        client.state()
    })
    .await
}

#[tauri::command]
async fn verify_configuration(
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
async fn activate_profile(
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.activate_profile(profile_id)).await
}

#[tauri::command]
async fn delete_profile(
    client: State<'_, Arc<Client>>,
    profile_id: String,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.delete_profile(profile_id)).await
}

#[tauri::command]
async fn stop_gateway(client: State<'_, Arc<Client>>) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.stop()).await
}

#[tauri::command]
async fn clear_api_key(client: State<'_, Arc<Client>>) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.clear_api_key()).await
}

#[tauri::command]
async fn query_usage(
    client: State<'_, Arc<Client>>,
    query: UsageQuery,
) -> Result<UsagePage, String> {
    let client = client.inner().clone();
    run_blocking(move || client.query_usage(query)).await
}

#[tauri::command]
async fn get_usage_record(
    client: State<'_, Arc<Client>>,
    record_id: String,
) -> Result<RequestActivity, String> {
    let client = client.inner().clone();
    run_blocking(move || {
        client
            .usage_record(&record_id)?
            .ok_or_else(|| "Usage record not found".to_string())
    })
    .await
}

#[tauri::command]
async fn export_usage_csv(
    client: State<'_, Arc<Client>>,
    query: UsageQuery,
    path: String,
) -> Result<usize, String> {
    let client = client.inner().clone();
    run_blocking(move || client.export_usage_csv(query, PathBuf::from(path))).await
}

#[tauri::command]
async fn clear_usage(client: State<'_, Arc<Client>>) -> Result<u64, String> {
    let client = client.inner().clone();
    run_blocking(move || client.clear_usage()).await
}

#[tauri::command]
async fn get_client_key(client: State<'_, Arc<Client>>) -> Result<String, String> {
    let client = client.inner().clone();
    run_blocking(move || client.client_key()).await
}

#[tauri::command]
async fn rotate_client_key(client: State<'_, Arc<Client>>) -> Result<String, String> {
    let client = client.inner().clone();
    run_blocking(move || client.rotate_client_key()).await
}

#[tauri::command]
async fn save_local_api_config(
    client: State<'_, Arc<Client>>,
    config: LocalApiConfig,
) -> Result<GatewayState, String> {
    client.inner().clone().save_local_api_config(config).await
}

#[tauri::command]
async fn refresh_catalog(client: State<'_, Arc<Client>>) -> Result<GatewayState, String> {
    client.inner().clone().refresh_catalog().await
}

#[tauri::command]
async fn list_agents(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<Vec<AgentStatus>, String> {
    let client = client.inner().clone();
    let agents = run_blocking(move || client.list_agents()).await?;
    tray::sync_agents(&app, &agents);
    Ok(agents)
}

#[tauri::command]
async fn preview_agent_connection(
    client: State<'_, Arc<Client>>,
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
) -> Result<AgentPreview, String> {
    let client = client.inner().clone();
    run_blocking(move || client.preview_agent(agent_id, connect, options)).await
}

#[tauri::command]
async fn apply_agent_connection(
    client: State<'_, Arc<Client>>,
    agent_id: String,
    connect: bool,
    revision: String,
    options: ConnectOptions,
) -> Result<AgentStatus, String> {
    let client = client.inner().clone();
    run_blocking(move || client.apply_agent(agent_id, connect, revision, options)).await
}

#[tauri::command]
async fn disconnect_all_agents(client: State<'_, Arc<Client>>) -> Result<Vec<AgentStatus>, String> {
    let client = client.inner().clone();
    run_blocking(move || client.disconnect_all_agents()).await
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
async fn open_native_dialog(
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
fn native_dialog_ready(window: tauri::WebviewWindow) -> Result<(), String> {
    native_dialog::ready(&window)
}

#[tauri::command]
fn close_native_dialog(window: tauri::WebviewWindow) -> Result<(), String> {
    native_dialog::close(&window)
}

async fn run_pag_cli(app: &AppHandle, arguments: Vec<&str>) -> Result<Registration, String> {
    let output = app
        .shell()
        .sidecar("pag")
        .map_err(|_| "The bundled pag command is unavailable in this installation")?
        .args(arguments)
        .output()
        .await
        .map_err(|_| "The pag command could not complete")?;
    if !output.status.success() {
        return Err("The pag command could not update command-line access".to_string());
    }
    if output.stdout.len() > 64 * 1024 {
        return Err("The pag command returned an invalid response".to_string());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "The pag command returned an invalid response".to_string())
}

#[derive(Default)]
struct CliStartup(tokio::sync::Mutex<bool>);

async fn register_cli_on_startup(app: &AppHandle) {
    let startup = app.state::<CliStartup>();
    let mut attempted = startup.0.lock().await;
    if *attempted {
        return;
    }
    *attempted = true;
    let client = app.state::<Arc<Client>>().inner().clone();
    let reader = client.clone();
    let enabled =
        run_blocking(move || Ok(reader.preferences()?.auto_cli_registration.unwrap_or(true))).await;
    let result = match enabled {
        Ok(true) => run_pag_cli(app, vec!["cli", "install", "--json"])
            .await
            .map(|_| ()),
        Ok(false) => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        client.report_error(format!("Command-line registration failed: {error}"));
    }
}

#[tauri::command]
async fn get_cli_registration(app: AppHandle) -> Result<Registration, String> {
    register_cli_on_startup(&app).await;
    run_pag_cli(&app, vec!["cli", "status", "--json"]).await
}

#[tauri::command]
async fn set_cli_registration(app: AppHandle, installed: bool) -> Result<Registration, String> {
    register_cli_on_startup(&app).await;
    let startup = app.state::<CliStartup>();
    let _guard = startup.0.lock().await;
    let registration = if installed {
        run_pag_cli(&app, vec!["cli", "install", "--json"]).await
    } else {
        run_pag_cli(&app, vec!["cli", "uninstall", "--json", "--yes"]).await
    }?;
    let client = app.state::<Arc<Client>>().inner().clone();
    run_blocking(move || client.set_preference(Preference::AutoCliRegistration(installed))).await?;
    Ok(registration)
}

#[tauri::command]
async fn stop_all_and_quit(app: AppHandle, client: State<'_, Arc<Client>>) -> Result<(), String> {
    let client = client.inner().clone();
    run_blocking(move || client.shutdown()).await?;
    app.exit(0);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let show_on_launch =
        !std::env::args_os().any(|argument| argument == std::ffi::OsStr::new(AUTOSTART_ARG));

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
        .manage(CliStartup::default())
        .manage(updates::UpdateProgress::default())
        .plugin(tauri_plugin_notification::init())
        .manage(notifications::Settings::default())
        .invoke_handler(tauri::generate_handler![
            start_backend_service,
            get_gateway_state,
            read_profile_backup,
            import_profiles,
            export_profiles,
            export_diagnostics,
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
            disconnect_all_agents,
            get_cli_registration,
            set_cli_registration,
            stop_all_and_quit
        ])
        .setup(move |app| {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            autostart::setup(app.handle())?;
            if app.config().plugins.0.contains_key("updater") {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
            }
            let client = Client::attach(tauri::async_runtime::handle().inner().clone())?;
            if let Ok(preferences) = client.preferences() {
                apply_appearance(app.handle(), preferences.appearance);
            }
            app.manage(client.clone());
            let registration_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                register_cli_on_startup(&registration_app).await;
            });
            notifications::initialize(app.handle());

            let window = app
                .get_webview_window("main")
                .ok_or_else(|| "main window was not created".to_string())?;
            window.set_title(desktop_gateway::brand::PRODUCT_NAME)?;
            let window_for_events = window.clone();
            let app_for_events = app.handle().clone();
            let client_for_events = client.clone();
            window.on_window_event(move |event| {
                if matches!(event, WindowEvent::Focused(true)) {
                    let _ = window_for_events.emit("gateway://agents-changed", ());
                    refresh_preferences(&app_for_events, &client_for_events);
                }
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_for_events.hide();
                }
            });
            if let Err(error) = tray::setup(app.handle()) {
                client.report_error(format!("The system tray is unavailable: {error}"));
            }
            if let Err(error) = menu::setup(app.handle()) {
                client.report_error(format!("The application menu is unavailable: {error}"));
            }

            let handle = app.handle().clone();
            let mut states = client.subscribe();
            let initial = states.borrow().clone();
            let mut client_key_revision = initial.client_key_revision;
            let mut backend_instance = initial.backend_instance.clone();
            tray::sync(&handle, &initial);
            let mut alerts = notifications::Observer::new(&initial);
            tauri::async_runtime::spawn(async move {
                while states.changed().await.is_ok() {
                    let state = states.borrow().clone();
                    if state.client_key_revision != client_key_revision
                        || state.backend_instance != backend_instance
                    {
                        client_key_revision = state.client_key_revision;
                        backend_instance = state.backend_instance.clone();
                        // A restarted backend may retain a token without a rotation result yet.
                        let _ = handle.emit(
                            "gateway://client-key-changed",
                            state.client_key_available.unwrap_or(true),
                        );
                    }
                    tray::sync(&handle, &state);
                    alerts.update(&handle, &state);
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

    app.run(|_app, event| match event {
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_window(_app),
        _ => {}
    });
}
