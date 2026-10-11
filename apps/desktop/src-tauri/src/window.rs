//! The main window: created hidden and shown once its page has loaded, hidden
//! rather than closed, and shown at the page the tray or the menu bar asks for.

use std::sync::{Mutex, MutexGuard, PoisonError};

use desktop_core::{config::Appearance, contracts::NavigationTarget, ui_api::NAVIGATE_EVENT};
use tauri::{
    utils::config::WindowConfig,
    webview::{PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Emitter, Manager, State, WindowEvent,
};

pub(crate) const MAIN: &str = "main";

/// Creates the main window from its configuration. It is created hidden
/// (`visible: false`) in the saved appearance, so the page paints in it from
/// the start, and shown once the page has loaded. The settings file is read
/// directly: the backend may still be starting, and it reapplies the
/// appearance on connecting.
pub fn create(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let config = main_config(app.handle()).ok_or("Main window configuration is missing")?;
    let appearance = desktop_core::config::load()
        .map(|saved| saved.appearance)
        .unwrap_or_default();
    let window = WebviewWindowBuilder::from_config(app, config)?
        .initialization_script(crate::distribution::initialization_script())
        .on_page_load(|window, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                // A request to show the window that came earlier shows it now.
                let mut state = state(window.app_handle());
                let requested = !state.loaded && state.show_requested;
                state.loaded = true;
                drop(state);
                if requested {
                    show(window.app_handle());
                }
            }
        })
        .build()?;
    #[cfg(target_os = "windows")]
    crate::windows_window::disable_browser_accelerator_keys(&window);
    // A new window follows the system, so only a saved light or dark
    // appearance is set, before the page loads. (On Linux the window
    // builder's theme does not apply; `set_theme` does everywhere.)
    if let Some(theme) = native_theme(appearance) {
        window.set_theme(Some(theme))?;
    }
    let app = app.handle().clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(window) = handle.get_webview_window(MAIN) {
                    let _ = window.hide();
                }
                set_dock_visibility(&handle, false);
            });
            #[cfg(target_os = "windows")]
            crate::windows_window::explain_close_to_tray(&app);
        }
    });
    Ok(())
}

fn main_config(app: &AppHandle) -> Option<&WindowConfig> {
    app.config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN)
}

/// The native theme of an appearance; `None` follows the system. The webview's
/// `prefers-color-scheme` follows it: macOS WKWebView inherits the window's
/// appearance, WebView2 takes it as its preferred color scheme, and WebKitGTK
/// follows GTK's dark-theme preference, which does not always track the
/// desktop's dark style (see "Appearance" in docs/configuration.md).
pub fn native_theme(appearance: Appearance) -> Option<tauri::Theme> {
    match appearance {
        Appearance::System => None,
        Appearance::Light => Some(tauri::Theme::Light),
        Appearance::Dark => Some(tauri::Theme::Dark),
    }
}

/// The main window's page has loaded, it was asked to show (it shows once
/// both hold), and the page it was last asked for that the renderer has not
/// taken yet.
#[derive(Default)]
pub struct WindowState {
    loaded: bool,
    show_requested: bool,
    navigation: Option<NavigationTarget>,
}

fn state(app: &AppHandle) -> MutexGuard<'_, WindowState> {
    let state = app.state::<Mutex<WindowState>>().inner();
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn show(app: &AppHandle) {
    let mut state = state(app);
    state.show_requested = true;
    if !state.loaded {
        return;
    }
    drop(state);
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(MAIN) else {
            return;
        };
        set_dock_visibility(&handle, true);
        // Focusing skips a minimized window, so restore it first.
        let _ = window.unminimize();
        let _ = window.show();
        // On macOS this also activates the app.
        let _ = window.set_focus();
    });
}

/// A hidden window leaves only the menu bar item, like other tray apps.
#[cfg(target_os = "macos")]
fn set_dock_visibility(app: &AppHandle, visible: bool) {
    let policy = if visible {
        tauri::ActivationPolicy::Regular
    } else {
        tauri::ActivationPolicy::Accessory
    };
    if let Err(error) = app.set_activation_policy(policy) {
        tracing::warn!("Cannot change the Dock presence: {error}");
    }
}

#[cfg(not(target_os = "macos"))]
fn set_dock_visibility(_app: &AppHandle, _visible: bool) {}

/// Returns the main window to its configured size, windowed and centered.
pub fn reset(app: &AppHandle) -> Result<(), String> {
    let (Some(window), Some(defaults)) = (app.get_webview_window(MAIN), main_config(app)) else {
        return Ok(());
    };
    window
        .set_fullscreen(false)
        .map_err(|_| "Could not reset the window")?;
    window
        .unmaximize()
        .map_err(|_| "Could not reset the window")?;
    window
        .set_size(tauri::LogicalSize::new(defaults.width, defaults.height))
        .map_err(|_| "Could not reset the window size")?;
    window
        .center()
        .map_err(|_| "Could not center the window".into())
}

/// Shows the window at a page or dialog. The request waits until the
/// renderer takes it, since one made before the renderer listens (while the
/// app is starting) would otherwise be lost; the event tells a listening
/// renderer to take it now.
pub fn navigate(app: &AppHandle, target: NavigationTarget) {
    show(app);
    state(app).navigation = Some(target);
    let _ = app.emit(NAVIGATE_EVENT, ());
}

/// Takes the window's pending request, so it is handled once; the renderer
/// asks once it listens for `NAVIGATE_EVENT` and again on each event.
#[tauri::command]
pub(crate) fn take_navigation(state: State<'_, Mutex<WindowState>>) -> Option<NavigationTarget> {
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .navigation
        .take()
}
