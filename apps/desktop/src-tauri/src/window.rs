//! The main window: created hidden and shown once its page has loaded, hidden
//! to the tray rather than closed (minimized while no tray can show the app),
//! and shown at the page the tray or the menu bar asks for. It shows only the
//! app's own pages; links to any other open in the default browser.

use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use desktop_core::{config::Appearance, contracts::NavigationTarget, ui_api::NAVIGATE_EVENT};
use tauri::{
    utils::config::WindowConfig,
    webview::{NewWindowResponse, PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Emitter, Manager, State, Url, WindowEvent,
};

pub(crate) const MAIN: &str = "main";

/// Creates the main window from its configuration. It is created hidden
/// (`visible: false`) in the saved `appearance`, so the page paints in it from
/// the start, and shown once the page has loaded. The appearance comes from
/// the settings file: the backend may still be starting, and it reapplies the
/// appearance on connecting.
pub fn create(app: &tauri::App, appearance: Appearance) -> Result<(), Box<dyn std::error::Error>> {
    let config = main_config(app.handle()).ok_or("Main window configuration is missing")?;
    let origin = AppOrigin {
        https: config.use_https_scheme,
        dev_url: app.config().build.dev_url.clone().filter(|_| cfg!(dev)),
    };
    let navigation = (app.handle().clone(), origin.clone());
    let new_window = (app.handle().clone(), origin);
    let window = WebviewWindowBuilder::from_config(app, config)?
        .initialization_script(crate::distribution::initialization_script())
        .on_navigation(move |url| {
            let (app, origin) = &navigation;
            let own = origin.contains(url);
            if !own {
                open_in_browser(app, url);
            }
            own
        })
        .on_new_window(move |url, _| {
            let (app, origin) = &new_window;
            if !origin.contains(&url) {
                open_in_browser(app, &url);
            }
            NewWindowResponse::Deny
        })
        .on_page_load(|window, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                // A request to show the window that came earlier shows it now.
                let mut state = state(window.app_handle());
                let requested = !state.loaded && state.show_requested;
                let presentation = state.presentation;
                state.loaded = true;
                drop(state);
                if requested {
                    reveal(window.app_handle(), presentation);
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
            let presented = state(&app).presented;
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                close(&app, presented, crate::tray::usable(&app).await);
            });
        }
    });
    Ok(())
}

/// Hides the window to the tray. Without a usable tray it minimizes the
/// window instead, so the taskbar or the Dock can still open it. A request to
/// show the window made since the close (`presented` counts them) wins: its
/// change runs on the main thread after this check.
fn close(app: &AppHandle, presented: u64, to_tray: bool) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if state(&handle).presented != presented {
            return;
        }
        let Some(window) = handle.get_webview_window(MAIN) else {
            return;
        };
        if to_tray {
            let _ = window.hide();
            set_dock_visibility(&handle, false);
            #[cfg(target_os = "windows")]
            crate::windows_window::explain_close_to_tray(&handle);
        } else {
            tracing::info!("No usable tray: minimizing the window instead of hiding it");
            let _ = window.minimize();
        }
    });
}

/// How long a launch at login waits for a tray host, and how often it asks:
/// panels such as XFCE's start alongside the apps opened at login.
const LOGIN_TRAY_WAIT: Duration = Duration::from_secs(10);
const LOGIN_TRAY_POLL: Duration = Duration::from_millis(500);

/// An app opened at login starts in the tray, without a Dock icon on macOS.
/// Without a usable tray it shows minimized instead, so the taskbar or the
/// Dock can open it.
pub fn launch_hidden(app: &AppHandle) {
    // Setup created the icon, if it could, before this runs.
    let icon = app.tray_by_id(crate::tray::TRAY_ID).is_some();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let host = async {
            while !crate::tray::host_registered().await {
                tokio::time::sleep(LOGIN_TRAY_POLL).await;
            }
        };
        if icon && tokio::time::timeout(LOGIN_TRAY_WAIT, host).await.is_ok() {
            // A window opened meanwhile keeps its Dock icon. Checked on the
            // main thread, which runs each show's Dock change in turn.
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                if !state(&handle).show_requested {
                    set_dock_visibility(&handle, false);
                }
            });
        } else {
            present(&app, Presentation::Minimized);
        }
    });
}

/// Where the app's own pages come from: Tauri's `tauri://localhost`, which
/// Windows serves as `http://tauri.localhost` (`https` with
/// `useHttpsScheme`), and the development server in a development build.
#[derive(Clone)]
struct AppOrigin {
    https: bool,
    dev_url: Option<Url>,
}

impl AppOrigin {
    fn contains(&self, url: &Url) -> bool {
        let (scheme, host) = if cfg!(windows) {
            (if self.https { "https" } else { "http" }, "tauri.localhost")
        } else {
            ("tauri", "localhost")
        };
        (url.scheme() == scheme && url.host_str() == Some(host) && url.port().is_none())
            || self
                .dev_url
                .as_ref()
                .is_some_and(|dev| dev.origin() == url.origin())
    }
}

/// Opens a web page that is not the app's in the default browser; other
/// schemes are not opened.
fn open_in_browser(app: &AppHandle, url: &Url) {
    if matches!(url.scheme(), "http" | "https") {
        crate::menu::open_link(app, url.as_str());
    } else {
        tracing::warn!("Blocked the window from opening a {} URL", url.scheme());
    }
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
    /// How the requested show presents the window.
    presentation: Presentation,
    /// How many shows were requested, so a close they overtook is dropped.
    presented: u64,
    navigation: Option<NavigationTarget>,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Presentation {
    #[default]
    Focused,
    /// In the taskbar or the Dock: a launch at login without a usable tray.
    Minimized,
}

fn state(app: &AppHandle) -> MutexGuard<'_, WindowState> {
    let state = app.state::<Mutex<WindowState>>().inner();
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn show(app: &AppHandle) {
    present(app, Presentation::Focused);
}

fn present(app: &AppHandle, presentation: Presentation) {
    let mut state = state(app);
    // A window the user opened while the app was starting stays open.
    if presentation == Presentation::Minimized {
        if state.show_requested {
            return;
        }
        tracing::info!("No usable tray: showing the window minimized");
    }
    state.show_requested = true;
    state.presentation = presentation;
    state.presented += 1;
    if !state.loaded {
        return;
    }
    drop(state);
    reveal(app, presentation);
}

fn reveal(app: &AppHandle, presentation: Presentation) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(MAIN) else {
            return;
        };
        set_dock_visibility(&handle, true);
        if presentation == Presentation::Minimized {
            // Minimizing a window that was never shown does not work everywhere.
            let _ = window.show();
            let _ = window.minimize();
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_app_origin_stays_in_the_window() {
        let origin = AppOrigin {
            https: false,
            dev_url: Some(Url::parse("http://127.0.0.1:1420").unwrap()),
        };
        let own = if cfg!(windows) {
            "http://tauri.localhost/settings"
        } else {
            "tauri://localhost/settings"
        };
        for (url, contained) in [
            (own, true),
            ("http://127.0.0.1:1420/agents", true),
            ("http://127.0.0.1:1421/", false),
            ("https://127.0.0.1:1420/", false),
            ("https://redpill.ai/", false),
            ("http://tauri.localhost.example.com/", false),
            ("https://tauri.localhost/", false),
            ("http://localhost/", false),
            ("file:///etc/passwd", false),
        ] {
            assert_eq!(
                origin.contains(&Url::parse(url).unwrap()),
                contained,
                "{url}"
            );
        }
        let release = AppOrigin {
            dev_url: None,
            ..origin
        };
        assert!(!release.contains(&Url::parse("http://127.0.0.1:1420/").unwrap()));
    }
}
