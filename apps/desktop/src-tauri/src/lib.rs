#[macro_use]
mod native_commands;
mod commands;

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
mod app_data;
mod autostart;
mod cli_registration;
mod distribution;
mod menu;
mod notifications;
mod tray;
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod tray_theme;
mod ui_api;
mod updates;
mod window;
#[cfg(target_os = "windows")]
mod windows_window;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use desktop_core::{
    client::Client,
    contracts::AppState,
    ui_api::{Host, StateEventProjection},
};
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_window_state::StateFlags;

pub(crate) async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| "The background operation could not complete. Please try again.")?
}

/// The invoke handler: every renderer method (`desktop_core::renderer_methods!`)
/// and the shell's own commands (`native_commands!`).
macro_rules! invoke_handler {
    (
        commands { $($command:ident => $command_variant:ident),+ $(,)? }
        host { $($host:ident => $host_variant:ident),+ $(,)? }
    ) => {
        native_commands!(invoke_handler [$(commands::ui::$command,)+ $(commands::ui::$host,)+])
    };
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
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // The account return link reopens the window itself.
            if !args
                .iter()
                .any(|arg| arg == &desktop_core::account::account_return_url())
            {
                window::show(app);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filter(|label| label == window::MAIN)
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .manage(updates::PreparedUpdate::default())
        .manage(cli_registration::CliStartup::default())
        .manage(Mutex::new(window::WindowState::default()))
        .manage(notifications::Settings::default())
        .invoke_handler(desktop_core::renderer_methods!(invoke_handler))
        .on_menu_event(menu::handle_event)
        .setup(setup);
    #[cfg(target_os = "macos")]
    let builder = builder.menu(|app| {
        menu::menu_bar(app).or_else(|error| {
            tracing::warn!("The application menu is unavailable: {error}");
            tauri::menu::Menu::default(app)
        })
    });
    builder
        .build(tauri::generate_context!())
        .expect("error while building Tauri application")
        .run(|app, event| match event {
            #[cfg(any(feature = "mac-app-store", target_os = "windows", target_os = "linux"))]
            RunEvent::Exit => {
                #[cfg(feature = "mac-app-store")]
                if let Some(client) = app.try_state::<Arc<Client>>() {
                    if client.is_running().unwrap_or(false) {
                        if let Err(error) = client.shutdown() {
                            tracing::warn!(
                                "Cannot stop the App Store backend during exit: {error}"
                            );
                        }
                    }
                }
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                tray_theme::shutdown(app);
            }
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => window::show(app),
            _ => {}
        });
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
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
    if updates::configured(app.handle()) {
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
    window::create(app)?;
    if let Err(error) = tray::setup(app.handle()) {
        tracing::warn!("The system tray is unavailable: {error}");
    } else {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Err(error) = tray_theme::setup(app.handle()) {
            tracing::warn!("Cannot observe system tray appearance: {error}");
        }
    }
    follow_backend(app.handle(), client);
    if show_on_launch {
        window::show(app.handle());
    }
    Ok(())
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
        window::show(&handle);
    });
}

/// Shows each backend state in the window, the tray and notifications, and
/// applies the preferences to a backend that connected.
fn follow_backend(app: &AppHandle, client: Arc<Client>) {
    let app = app.clone();
    let host = ui_api::TauriHost::new(app.clone());
    let mut states = client.subscribe();
    let initial = states.borrow().clone();
    let mut projection = StateEventProjection::new(&initial);
    tray::sync(&app, &initial);
    let mut alerts = notifications::Observer::new(&initial);
    let mut backend = BackendInstance::default();
    // The notification permission is asked for once per launch, once a
    // backend answers with its preferences.
    let permission_asked = Arc::new(AtomicBool::new(!distribution::CAPABILITIES.notifications));
    // A backend that was already running may have answered before
    // this subscription: handle the current state as a change too.
    states.mark_changed();
    tauri::async_runtime::spawn(async move {
        while states.changed().await.is_ok() {
            let state = states.borrow_and_update().clone();
            let connected = backend.connected(&state);
            #[cfg(target_os = "macos")]
            if connected && distribution::CAPABILITIES.cli_registration {
                cli_registration::register_on_startup(&app);
            }
            if projection.settings_changed(&state) || connected {
                // A backend that (re)connected, or an edit of
                // config.toml, for example: reapply preferences.
                tauri::async_runtime::spawn(refresh_preferences(
                    app.clone(),
                    client.clone(),
                    host.clone(),
                    permission_asked.clone(),
                ));
            }
            for event in projection.project(&state) {
                let _ = host.emit(event);
            }
            tray::sync(&app, &state);
            alerts.update(&app, &state);
        }
    });
}

async fn refresh_preferences(
    app: AppHandle,
    client: Arc<Client>,
    host: ui_api::TauriHost,
    permission_asked: Arc<AtomicBool>,
) {
    if let Err(error) = desktop_core::ui_api::refresh_preferences(&client, &host).await {
        tracing::warn!("Cannot refresh desktop preferences: {}", error);
    } else if !permission_asked.swap(true, Ordering::SeqCst)
        && !notifications::request_startup_permission(&app, &client).await
    {
        // The preferences were unavailable: ask after a later refresh.
        permission_asked.store(false, Ordering::SeqCst);
    }
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
