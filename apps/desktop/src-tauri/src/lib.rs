#[macro_use]
mod native_commands;
mod commands;

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
mod app_data;
mod autostart;
mod distribution;
mod menu;
mod notifications;
mod tray;
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod tray_theme;
mod ui_api;
mod updates;
#[cfg(target_os = "windows")]
mod windows_window;

use desktop_core::{
    client::Client,
    config::Appearance,
    contracts::{AppState, CommandRegistration},
};
use tauri::{
    webview::{PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Manager, WindowEvent,
};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_shell::ShellExt;

pub(crate) async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| "The background operation could not complete. Please try again.")?
}

/// The native theme of an appearance; `None` follows the system. The webview's
/// `prefers-color-scheme` follows it: macOS WKWebView inherits the window's
/// appearance, WebView2 takes it as its preferred color scheme, and WebKitGTK
/// follows GTK's dark-theme preference, which does not always track the
/// desktop's dark style (see "Appearance" in docs/configuration.md).
fn native_theme(appearance: Appearance) -> Option<tauri::Theme> {
    match appearance {
        Appearance::System => None,
        Appearance::Light => Some(tauri::Theme::Light),
        Appearance::Dark => Some(tauri::Theme::Dark),
    }
}

fn apply_appearance(app: &AppHandle, appearance: Appearance) {
    app.set_theme(native_theme(appearance));
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

async fn run_cli_command(
    app: &AppHandle,
    arguments: Vec<&str>,
) -> Result<CommandRegistration, String> {
    let output = app
        .shell()
        .sidecar("private-ai-proxy")
        .map_err(|_| "The bundled pap command is unavailable in this installation")?
        .args(arguments)
        .output()
        .await
        .map_err(|_| "The pap command could not complete")?;
    if !output.status.success() {
        return Err("The pap command could not update command-line access".to_string());
    }
    if output.stdout.len() > 64 * 1024 {
        return Err("The pap command returned an invalid response".to_string());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "The pap command returned an invalid response".to_string())
}

/// The backend instance the shell last saw. A backend that (re)connected needs
/// the preferences it holds applied.
#[derive(Default)]
struct BackendInstance(Option<String>);

impl BackendInstance {
    /// Whether `state` comes from a backend other than the last one seen.
    fn connected(&mut self, state: &AppState) -> bool {
        let connected = state.backend_instance.is_some() && state.backend_instance != self.0;
        self.0.clone_from(&state.backend_instance);
        connected
    }
}

/// The automatic `pap` registration of a launch. It holds the lock while it
/// runs, so reading or changing the registration waits for it.
#[derive(Clone, Default)]
struct CliStartup(std::sync::Arc<tokio::sync::Mutex<CliStartupState>>);

#[derive(Default)]
struct CliStartupState {
    #[cfg(target_os = "macos")]
    attempted: bool,
    last_error: Option<String>,
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
            "Move Private AI Proxy to a stable location before installing the pap command"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn register_cli_on_startup(
    app: &AppHandle,
    mut state: tokio::sync::OwnedMutexGuard<CliStartupState>,
) {
    if state.attempted {
        return;
    }
    let reader = app.state::<std::sync::Arc<Client>>().inner().clone();
    if reader.cached_state().backend_connected == Some(false) {
        return;
    }
    let enabled = run_blocking(move || {
        Ok(reader
            .call(desktop_core::protocol::rpc::Settings)?
            .auto_cli_registration
            .unwrap_or(true))
    })
    .await;
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
        tracing::warn!("Cannot register app return link: {error}");
    }
    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        let expected = desktop_core::account::account_return_url();
        if !event.urls().iter().any(|url| url.as_str() == expected) {
            return;
        }
        // The profile editor that started the sign-in is still open in the window.
        tray::show_window(&handle);
    });
}

/// The invoke handler: every renderer method (`desktop_core::renderer_methods!`)
/// and the shell's own commands (`native_commands!`).
macro_rules! invoke_handler {
    (
        commands { $($command:ident => $command_variant:ident),+ $(,)? }
        host { $($host:ident => $host_variant:ident),+ $(,)? }
    ) => {
        native_commands!(generate_handler [$(commands::ui::$command,)+ $(commands::ui::$host,)+])
    };
}

macro_rules! generate_handler {
    ([$($renderer:tt)*] $($($segment:ident)::+),+ $(,)?) => {
        tauri::generate_handler![$($renderer)* $($($segment)::+),+]
    };
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    desktop_core::logging::init();
    // desktop_core's update check (the pacman notice) uses reqwest's rustls
    // without a built-in crypto provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app =
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args
                .iter()
                .any(|arg| arg == &desktop_core::account::account_return_url())
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
        .manage(tray::MainWindowPresentation::default())
        .plugin(tauri_plugin_notification::init())
        .manage(notifications::Settings::default())
        .invoke_handler(desktop_core::renderer_methods!(invoke_handler))
        .setup(move |app| {
            let show_on_launch = !autostart::launched_at_login();
            #[cfg(all(target_os = "macos", not(feature = "mac-app-store")))]
            autostart::migrate_legacy(app.handle());
            #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
            {
                let data_dir = app.path().app_data_dir()?;
                app_data::prepare(&data_dir)?;
                std::env::set_var(desktop_core::paths::APP_DATA_OVERRIDE_ENV, &data_dir);
                if let Err(error) = desktop_core::agent_access::prepare_for_service() {
                    tracing::warn!("Cannot prepare Agent Home access for the backend: {error}");
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
            // Starting the backend can take a while (the first launch after an
            // update); it runs in the background while the window shows the
            // backend as starting. Preferences apply once it answers.
            let client = Client::attach(tauri::async_runtime::handle().inner().clone());
            app.manage(client.clone());

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
            // Created hidden (`visible: false`) in the saved appearance, so
            // the page paints in it from the start, and shown once the page
            // has loaded. The settings file is read directly: the backend may
            // still be starting, and it reapplies the appearance on connecting.
            let appearance = desktop_core::config::load()
                .map(|saved| saved.appearance)
                .unwrap_or_default();
            #[cfg(target_os = "windows")]
            let backdrop = windows_window::mica_supported().then_some("mica");
            #[cfg(not(target_os = "windows"))]
            let backdrop = None;
            let builder = WebviewWindowBuilder::from_config(app, config)?
                .initialization_script(distribution::initialization_script(backdrop))
                .on_page_load(|window, payload| {
                    if matches!(payload.event(), PageLoadEvent::Finished) {
                        tray::main_window_ready(window.app_handle());
                    }
                });
            // Windows 11 draws Mica behind a transparent window; the page
            // shows it only where it has no background of its own.
            #[cfg(target_os = "windows")]
            let builder = if backdrop.is_some() {
                builder.transparent(true).effects(
                    tauri::window::EffectsBuilder::new()
                        .effect(tauri::window::Effect::Mica)
                        .build(),
                )
            } else {
                builder
            };
            let window = builder.build()?;
            #[cfg(target_os = "windows")]
            windows_window::disable_browser_accelerator_keys(&window);
            // A new window follows the system, so only a saved light or dark
            // appearance is set, before the page loads. (On Linux the window
            // builder's theme does not apply; `set_theme` does everywhere.)
            if let Some(theme) = native_theme(appearance) {
                window.set_theme(Some(theme))?;
            }
            let app_for_events = app.handle().clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    tray::hide_window(&app_for_events);
                    #[cfg(target_os = "windows")]
                    windows_window::explain_close_to_tray(&app_for_events);
                }
            });
            if let Err(error) = tray::setup(app.handle()) {
                tracing::warn!("The system tray is unavailable: {error}");
            } else {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                if let Err(error) = tray_theme::setup(app.handle()) {
                    tracing::warn!("Cannot observe system tray appearance: {error}");
                }
            }
            if let Err(error) = menu::setup(app.handle()) {
                tracing::warn!("The application menu is unavailable: {error}");
            }
            app.on_menu_event(menu::handle_event);

            let handle = app.handle().clone();
            let mut states = client.subscribe();
            let initial = states.borrow().clone();
            let mut projection = desktop_core::ui_api::StateEventProjection::new(&initial);
            let host = ui_api::TauriHost::new(window.clone());
            tray::sync(&handle, &initial);
            let mut alerts = notifications::Observer::new(&initial);
            // A backend that was already running may have answered before
            // this subscription: handle the current state as a change too.
            let mut backend = BackendInstance::default();
            // The notification permission is asked for once per launch, once a
            // backend answers with its preferences.
            let permission_asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                !distribution::CAPABILITIES.notifications,
            ));
            states.mark_changed();
            tauri::async_runtime::spawn(async move {
                while states.changed().await.is_ok() {
                    let state = states.borrow_and_update().clone();
                    let connected = backend.connected(&state);
                    // Registration runs once per launch; see `CliStartup`.
                    #[cfg(target_os = "macos")]
                    if connected && distribution::CAPABILITIES.cli_registration {
                        let app = handle.clone();
                        let startup = handle.state::<CliStartup>().inner().clone();
                        // Taken before the window hears of this backend, so its
                        // first read of the registration waits for this one.
                        let taken = startup.0.clone().try_lock_owned();
                        tauri::async_runtime::spawn(async move {
                            let state = match taken {
                                Ok(state) => state,
                                Err(_) => startup.0.lock_owned().await,
                            };
                            register_cli_on_startup(&app, state).await;
                        });
                    }
                    if projection.settings_changed(&state) || connected {
                        // A backend that (re)connected, or an edit of
                        // config.toml, for example: reapply preferences.
                        let (client, host) = (client.clone(), host.clone());
                        let (app, asked) = (handle.clone(), permission_asked.clone());
                        tauri::async_runtime::spawn(async move {
                            if let Err(error) =
                                desktop_core::ui_api::refresh_preferences(&client, &host).await
                            {
                                tracing::warn!("Cannot refresh desktop preferences: {}", error);
                            } else if !asked.swap(true, std::sync::atomic::Ordering::SeqCst)
                                && !notifications::request_startup_permission(&app, &client).await
                            {
                                // The preferences were unavailable: ask after a later refresh.
                                asked.store(false, std::sync::atomic::Ordering::SeqCst);
                            }
                        });
                    }
                    for event in projection.project(&state) {
                        let _ = desktop_core::ui_api::Host::emit(&host, event);
                    }
                    tray::sync(&handle, &state);
                    alerts.update(&handle, &state);
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
            if let Some(client) = _app.try_state::<std::sync::Arc<Client>>() {
                if client.is_running().unwrap_or(false) {
                    if let Err(error) = client.shutdown() {
                        tracing::warn!("Cannot stop the App Store backend during exit: {error}");
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
mod tests {
    use super::*;

    #[test]
    fn a_backend_that_answered_before_the_subscription_counts_as_connected() {
        let (sender, _) = tokio::sync::watch::channel(AppState::default());
        let answered = |instance: &str| AppState {
            backend_instance: Some(instance.into()),
            ..AppState::default()
        };
        sender.send_replace(answered("first"));
        let mut states = sender.subscribe();
        states.mark_changed();
        assert!(states.has_changed().unwrap());
        let mut backend = BackendInstance::default();
        assert!(backend.connected(&states.borrow_and_update()));
        // Later states of the same backend, including a disconnection that
        // keeps its instance, are not a new connection; a replacement is.
        assert!(!backend.connected(&answered("first")));
        assert!(backend.connected(&answered("second")));
    }
}

#[cfg(test)]
mod capability_tests;
#[cfg(test)]
mod cli_startup_tests;
