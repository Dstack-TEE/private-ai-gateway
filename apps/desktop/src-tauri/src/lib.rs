mod commands;

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
mod app_data;
mod autostart;
mod distribution;
mod menu;
mod native_dialog;
mod notifications;
mod tray;
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod tray_theme;
mod updates;
mod window_state;

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
use tauri::{
    webview::{PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_shell::ShellExt;

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SurfaceErrorScope {
    Protection,
    Profiles,
    LocalApi,
    Agents,
    Usage,
    Settings,
}

#[derive(Clone, serde::Serialize)]
struct SurfaceError {
    scope: SurfaceErrorScope,
    message: String,
}

pub(crate) fn report_surface_error(
    app: &AppHandle,
    scope: SurfaceErrorScope,
    error: impl std::fmt::Display,
) {
    let message = error.to_string();
    eprintln!("{scope:?}: {message}");
    let _ = app.emit("gateway://surface-error", SurfaceError { scope, message });
}

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

fn load_launch_preferences(app: &AppHandle, client: &Client) -> Result<LaunchPreferences, String> {
    Ok(LaunchPreferences {
        open_at_login: autostart::is_enabled(app)?,
        connect_on_launch: client.preferences()?.connect_on_launch,
    })
}

fn refresh_preferences(app: &AppHandle, client: &Arc<Client>) {
    if client.cached_state().backend_connected == Some(false) {
        return;
    }
    let app = app.clone();
    let client = client.clone();
    tauri::async_runtime::spawn_blocking(move || {
        notifications::initialize(&app);
        let result = (|| {
            let preferences = client.preferences()?;
            let launch = load_launch_preferences(&app, &client)?;
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
            Err(error) => eprintln!("Cannot refresh desktop preferences: {error}"),
        }
    });
}

fn apply_appearance(app: &AppHandle, appearance: Appearance) {
    app.set_theme(match appearance {
        Appearance::System => None,
        Appearance::Light => Some(tauri::Theme::Light),
        Appearance::Dark => Some(tauri::Theme::Dark),
    });
}

async fn open_account_url(app: AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    run_blocking(move || {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|_| "Cannot open the account page".into())
    })
    .await
}

async fn run_cli_command(app: &AppHandle, arguments: Vec<&str>) -> Result<Registration, String> {
    let output = app
        .shell()
        .sidecar("private-ai-proxy")
        .map_err(|_| "The bundled private-ai-proxy command is unavailable in this installation")?
        .args(arguments)
        .output()
        .await
        .map_err(|_| "The private-ai-proxy command could not complete")?;
    if !output.status.success() {
        return Err(
            "The private-ai-proxy command could not update command-line access".to_string(),
        );
    }
    if output.stdout.len() > 64 * 1024 {
        return Err("The private-ai-proxy command returned an invalid response".to_string());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "The private-ai-proxy command returned an invalid response".to_string())
}

#[derive(Default)]
struct CliStartup(tokio::sync::Mutex<CliStartupState>);

#[derive(Default)]
struct CliStartupState {
    #[cfg(target_os = "macos")]
    attempted: bool,
    last_error: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CliRegistration {
    #[serde(flatten)]
    registration: Registration,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup_error: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
fn transient_macos_app_path(path: &std::path::Path) -> bool {
    path.starts_with("/Volumes")
        || path
            .components()
            .any(|component| component.as_os_str() == "AppTranslocation")
}

#[cfg(target_os = "macos")]
fn allow_automatic_cli_registration() -> Result<(), String> {
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| "Cannot locate the installed application".to_string())?;
    if transient_macos_app_path(&executable) {
        return Err(
            "Move Private AI Proxy to a stable location before registering private-ai-proxy"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn register_cli_on_startup(app: &AppHandle) {
    let startup = app.state::<CliStartup>();
    let mut state = startup.0.lock().await;
    if state.attempted {
        return;
    }
    let reader = app.state::<Arc<Client>>().inner().clone();
    if reader.cached_state().backend_connected == Some(false) {
        return;
    }
    let enabled =
        run_blocking(move || Ok(reader.preferences()?.auto_cli_registration.unwrap_or(true))).await;
    state.attempted = enabled.is_ok();
    let result = match enabled {
        Ok(true) => match allow_automatic_cli_registration() {
            Ok(()) => run_cli_command(app, vec!["cli", "install", "--json"])
                .await
                .map(|_| ()),
            Err(error) => Err(error),
        },
        Ok(false) => Ok(()),
        Err(error) => Err(error),
    };
    state.last_error = result
        .err()
        .map(|error| format!("Command-line registration failed: {error}"));
}

fn configure_account_return(app: &tauri::App) {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if let Err(error) = app.deep_link().register_all() {
        eprintln!("Cannot register app return link: {error}");
    }
    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        let expected = desktop_runtime::account_login::account_return_url();
        if !event.urls().iter().any(|url| url.as_str() == expected) {
            return;
        }
        tray::show_window(&handle);
        let app = handle.clone();
        if let Err(error) = handle.run_on_main_thread(move || {
            if let Err(error) = native_dialog::focus_account_editor(&app) {
                eprintln!("Cannot focus account editor: {error}");
            }
        }) {
            eprintln!("Cannot return to account editor: {error}");
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app =
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args
                .iter()
                .any(|arg| arg == &desktop_runtime::account_login::account_return_url())
            {
                tray::show_window(app);
            }
        }));
    let app = app
        .plugin(tauri_plugin_deep_link::init())
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
        .manage(updates::PreparedUpdate::default())
        .manage(CliStartup::default())
        .manage(native_dialog::DialogCache::default())
        .manage(tray::MainWindowPresentation::default())
        .plugin(tauri_plugin_notification::init())
        .manage(notifications::Settings::default())
        .invoke_handler(tauri::generate_handler![
            commands::gateway::start_backend_service,
            commands::gateway::get_gateway_state,
            commands::settings::reset_settings,
            commands::settings::read_profile_backup,
            commands::settings::import_profiles,
            commands::settings::export_profiles,
            commands::settings::export_diagnostics,
            notifications::get_notification_settings,
            notifications::save_notification_settings,
            notifications::request_notification_permission,
            notifications::open_notification_settings,
            commands::settings::get_appearance,
            commands::settings::set_appearance,
            updates::prepare_update,
            updates::set_update_channel,
            updates::restart_to_update,
            commands::settings::get_launch_preferences,
            commands::settings::set_launch_preference,
            commands::gateway::start_gateway,
            commands::accounts::begin_account_login,
            commands::accounts::complete_account_login,
            commands::accounts::save_configuration,
            commands::accounts::poll_account_login,
            commands::accounts::save_account_login,
            commands::accounts::account_details,
            commands::accounts::account_balance,
            commands::accounts::open_top_up,
            commands::accounts::open_organization,
            commands::accounts::cancel_account_login,
            commands::accounts::activate_profile,
            commands::accounts::delete_profile,
            commands::gateway::stop_gateway,
            commands::desktop::copy_text,
            commands::desktop::show_edit_menu,
            commands::desktop::show_error_alert,
            commands::desktop::open_native_dialog,
            commands::desktop::native_dialog_ready,
            commands::desktop::main_window_ready,
            commands::desktop::open_agent_website,
            commands::desktop::open_api_key_page,
            commands::desktop::close_native_dialog,
            commands::usage::query_usage,
            commands::usage::get_usage_record,
            commands::desktop::open_about_link,
            commands::gateway::get_client_key,
            commands::gateway::rotate_client_key,
            commands::gateway::save_local_api_config,
            commands::gateway::list_listen_addresses,
            commands::gateway::list_agents,
            commands::gateway::preview_agent_connection,
            commands::gateway::apply_agent_connection,
            commands::gateway::get_agent_access,
            commands::gateway::request_agent_access,
            commands::settings::get_cli_registration,
            commands::settings::set_cli_registration,
            commands::desktop::stop_all_and_quit
        ])
        .setup(move |app| {
            let show_on_launch = !autostart::launched_at_login();
            #[cfg(all(target_os = "macos", not(feature = "mac-app-store")))]
            autostart::migrate_legacy(app.handle());
            #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
            {
                let data_dir = app.path().app_data_dir()?;
                app_data::prepare(&data_dir)?;
                std::env::set_var(desktop_gateway::agents::APP_DATA_OVERRIDE_ENV, &data_dir);
                if let Err(error) = desktop_runtime::agent_access::prepare_for_service() {
                    eprintln!("Cannot prepare Agent Home access for the backend: {error}");
                }
            }
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            autostart::setup(app.handle())?;
            if distribution::CAPABILITIES.native_updates
                && app.config().plugins.0.contains_key("updater")
            {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
            }
            let client = Client::attach(tauri::async_runtime::handle().inner().clone())?;
            if let Ok(preferences) = client.preferences() {
                apply_appearance(app.handle(), preferences.appearance);
            }
            app.manage(client.clone());
            #[cfg(target_os = "macos")]
            let registration_app = app.handle().clone();
            #[cfg(target_os = "macos")]
            if distribution::CAPABILITIES.cli_registration {
                tauri::async_runtime::spawn(async move {
                    register_cli_on_startup(&registration_app).await;
                });
            }
            notifications::initialize(app.handle());

            configure_account_return(app);

            // The renderer invokes backend commands on mount. Create it only
            // after Client and native services are registered in managed state.
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .ok_or("Main window configuration is missing")?;
            let window = WebviewWindowBuilder::from_config(app, config)?
                .initialization_script(distribution::initialization_script())
                .on_page_load(|window, payload| {
                    if matches!(payload.event(), PageLoadEvent::Finished) {
                        tray::main_window_ready(&window).ok();
                    }
                })
                .build()?;
            window_state::migrate_legacy_default(app, &window, config.width, config.height)?;
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
                    tray::hide_window(&app_for_events);
                }
            });
            if let Err(error) = tray::setup(app.handle()) {
                eprintln!("The system tray is unavailable: {error}");
            } else {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                if let Err(error) = tray_theme::setup(app.handle()) {
                    eprintln!("Cannot observe system tray appearance: {error}");
                }
            }
            if let Err(error) = menu::setup(app.handle()) {
                eprintln!("The application menu is unavailable: {error}");
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
        #[cfg(any(feature = "mac-app-store", target_os = "windows", target_os = "linux"))]
        tauri::RunEvent::Exit => {
            #[cfg(feature = "mac-app-store")]
            if let Some(client) = _app.try_state::<Arc<Client>>() {
                if client.is_running().unwrap_or(false) {
                    if let Err(error) = client.shutdown() {
                        eprintln!("Cannot stop the App Store backend during exit: {error}");
                    }
                }
            }
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            tray_theme::shutdown(_app);
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_window(_app),
        _ => {}
    });
}

#[cfg(test)]
mod cli_startup_tests;
