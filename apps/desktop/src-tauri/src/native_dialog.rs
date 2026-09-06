use tauri::{AppHandle, Emitter, Listener, Manager, WebviewUrl, WebviewWindowBuilder};

const PROFILES_LABEL: &str = "profiles";
const PROFILE_EDITOR_LABEL: &str = "profile-editor";
const PRIVACY_LABEL: &str = "privacy";
const LOCAL_API_LABEL: &str = "local-api";
const USAGE_PROOF_LABEL: &str = "usage-proof";
const PROFILE_REPAIR_EVENT: &str = "gateway://profile-repair";
const USAGE_PROOF_EVENT: &str = "gateway://usage-proof";
const PRESENTED_EVENT: &str = "gateway://dialog-presented";
const UPDATE_PROGRESS_LABEL: &str = "update-progress";
const DIALOG_LABELS: [&str; 8] = [
    "notifications",
    "local-api-example",
    UPDATE_PROGRESS_LABEL,
    PROFILES_LABEL,
    PROFILE_EDITOR_LABEL,
    PRIVACY_LABEL,
    LOCAL_API_LABEL,
    USAGE_PROOF_LABEL,
];

struct DialogSpec {
    label: &'static str,
    title: &'static str,
    width: f64,
    height: f64,
    min_width: f64,
    min_height: f64,
    query: String,
}

pub fn open(
    app: &AppHandle,
    kind: &str,
    repair: bool,
    record_id: Option<&str>,
    profile_id: Option<&str>,
) -> Result<(), String> {
    if profile_id.is_some_and(|id| id.len() > 128 || id.chars().any(char::is_control)) {
        return Err("Invalid profile identifier".to_string());
    }
    let spec = match kind {
        "notifications" => DialogSpec {
            label: "notifications",
            title: "Notifications",
            width: 580.0,
            height: 580.0,
            min_width: 500.0,
            min_height: 500.0,
            query: "index.html?native-dialog=notifications".to_string(),
        },
        "local-api-example" => DialogSpec {
            label: "local-api-example",
            title: "Local API examples",
            width: 720.0,
            height: 560.0,
            min_width: 560.0,
            min_height: 440.0,
            query: "index.html?native-dialog=local-api-example".to_string(),
        },
        "update-progress" => DialogSpec {
            label: UPDATE_PROGRESS_LABEL,
            title: "Software Update",
            width: 480.0,
            height: 240.0,
            min_width: 480.0,
            min_height: 240.0,
            query: "index.html?native-dialog=update-progress".to_string(),
        },
        "profile-editor" => DialogSpec {
            label: PROFILE_EDITOR_LABEL,
            title: if profile_id.is_some() {
                "Edit Profile"
            } else {
                "New Profile"
            },
            width: 580.0,
            height: 510.0,
            min_width: 520.0,
            min_height: 460.0,
            query: format!(
                "index.html?native-dialog=profile-editor&profile={}",
                encode_query_component(profile_id.unwrap_or_default())
            ),
        },
        "profiles" => DialogSpec {
            label: PROFILES_LABEL,
            title: "Profiles",
            width: 620.0,
            height: 560.0,
            min_width: 520.0,
            min_height: 460.0,
            query: if repair {
                "index.html?native-dialog=profiles&repair=1".to_string()
            } else {
                "index.html?native-dialog=profiles".to_string()
            },
        },
        "privacy" => DialogSpec {
            label: PRIVACY_LABEL,
            title: "Privacy Verification",
            width: 700.0,
            height: 680.0,
            min_width: 600.0,
            min_height: 520.0,
            query: "index.html?native-dialog=privacy".to_string(),
        },
        "local-api" => DialogSpec {
            label: LOCAL_API_LABEL,
            title: "Local API Settings",
            width: 600.0,
            height: 680.0,
            min_width: 540.0,
            min_height: 580.0,
            query: "index.html?native-dialog=local-api".to_string(),
        },
        "usage-proof" => {
            let record_id = record_id
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .ok_or_else(|| "A usage record is required".to_string())?;
            if record_id.chars().any(char::is_control) {
                return Err("Invalid usage record".to_string());
            }
            DialogSpec {
                label: USAGE_PROOF_LABEL,
                title: "Usage Proof",
                width: 560.0,
                height: 500.0,
                min_width: 500.0,
                min_height: 420.0,
                query: format!(
                    "index.html?native-dialog=usage-proof&record={}",
                    encode_query_component(record_id)
                ),
            }
        }
        _ => return Err("Unknown native dialog".to_string()),
    };

    // A document can present one modal sheet at a time. Keep a second tray or
    // menu action from creating an invisible queued dialog.
    for label in DIALOG_LABELS.into_iter().filter(|label| {
        *label != spec.label && !(spec.label == PROFILE_EDITOR_LABEL && *label == PROFILES_LABEL)
    }) {
        if let Some(window) = app.get_webview_window(label) {
            if spec.label == UPDATE_PROGRESS_LABEL {
                focus_if_visible(&window)?;
                return Err("Close the open dialog before installing an update".to_string());
            }
            return focus_if_visible(&window);
        }
    }

    if let Some(window) = app.get_webview_window(spec.label) {
        if spec.label == UPDATE_PROGRESS_LABEL {
            focus_if_visible(&window)?;
            return Err("An update dialog is already open".to_string());
        }
        if spec.label == PROFILES_LABEL && repair {
            window
                .emit(PROFILE_REPAIR_EVENT, ())
                .map_err(window_error)?;
        } else if let Some(record_id) = record_id.filter(|_| spec.label == USAGE_PROOF_LABEL) {
            window
                .emit(USAGE_PROOF_EVENT, record_id)
                .map_err(window_error)?;
        }
        return focus_if_visible(&window);
    }

    let main = app
        .get_webview_window(
            if spec.label == PROFILE_EDITOR_LABEL
                && app.get_webview_window(PROFILES_LABEL).is_some()
            {
                PROFILES_LABEL
            } else {
                "main"
            },
        )
        .ok_or_else(|| "The main window is unavailable".to_string())?;
    let state = app
        .state::<std::sync::Arc<desktop_runtime::controller::DesktopRuntime>>()
        .state()?;
    let initial_state = serde_json::to_string(&state).map_err(window_error)?;
    let theme = main.theme().map_err(window_error)?;
    if spec.label == UPDATE_PROGRESS_LABEL {
        crate::updates::reset_progress(app);
    }
    let mut builder =
        WebviewWindowBuilder::new(app, spec.label, WebviewUrl::App(spec.query.into()))
            .initialization_script(format!(
                "window.__GATEWAY_INITIAL_STATE__ = {initial_state};document.documentElement?.setAttribute('data-theme','{}');",
                if theme == tauri::Theme::Dark { "dark" } else { "light" }
            ))
            .background_color(if theme == tauri::Theme::Dark { tauri::webview::Color(10, 10, 10, 255) } else { tauri::webview::Color(255, 255, 255, 255) })
            .title(spec.title)
            .inner_size(spec.width, spec.height)
            .min_inner_size(spec.min_width, spec.min_height)
            .prevent_overflow()
            .resizable(true)
            .maximizable(false)
            .minimizable(false)
            .skip_taskbar(true)
            .visible(false);
    builder = match centered_position(&main, spec.width, spec.height) {
        Some((x, y)) => builder.position(x, y),
        None => builder.center(),
    };
    #[cfg(target_os = "macos")]
    let window = builder
        .closable(false)
        .hidden_title(true)
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .build()
        .map_err(window_error)?;
    #[cfg(not(target_os = "macos"))]
    let window = builder
        .parent(&main)
        .map_err(window_error)?
        .build()
        .map_err(window_error)?;
    let dialog = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(error) = request_close(&dialog) {
                eprintln!("Cannot request dialog close: {error}");
            }
        }
        #[cfg(not(target_os = "macos"))]
        if matches!(event, tauri::WindowEvent::Destroyed) {
            if let Err(error) = main.set_enabled(true).and_then(|_| main.set_focus()) {
                eprintln!("Cannot restore the dialog parent: {error}");
            }
        }
    });
    let presented = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let did_present = presented.clone();
    let listener = window.once(PRESENTED_EVENT, move |_| {
        did_present.store(true, std::sync::atomic::Ordering::Release);
    });
    let pending = window.clone();
    tauri::async_runtime::spawn(async move {
        // A deadline for a failed renderer handshake, not a presentation delay.
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        pending.unlisten(listener);
        if !presented.load(std::sync::atomic::Ordering::Acquire)
            && matches!(pending.is_visible(), Ok(false))
        {
            let app = pending.app_handle().clone();
            if pending.destroy().is_ok() {
                app.state::<std::sync::Arc<desktop_runtime::controller::DesktopRuntime>>()
                    .report_error(
                        "The dialog could not finish loading. Please try opening it again."
                            .to_string(),
                    );
                crate::tray::show_window(&app);
            }
        }
    });
    Ok(())
}

pub fn request_close(window: &tauri::WebviewWindow) -> Result<(), String> {
    if !DIALOG_LABELS.contains(&window.label()) {
        return window.close().map_err(window_error);
    }
    if window.label() == PROFILES_LABEL {
        if let Some(child) = window.app_handle().get_webview_window(PROFILE_EDITOR_LABEL) {
            return focus_if_visible(&child);
        }
    }
    window
        .emit("gateway://dialog-close-requested", ())
        .map_err(window_error)
}

pub fn open_profiles(app: &AppHandle, repair: bool) -> Result<(), String> {
    open(app, "profiles", repair, None, None)
}

fn focus_if_visible(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.is_visible().map_err(window_error)? {
        window.set_focus().map_err(window_error)?;
    }
    Ok(())
}

pub async fn ready(window: &tauri::WebviewWindow) -> Result<(), String> {
    if !DIALOG_LABELS.contains(&window.label()) {
        return Err("Only native dialogs can present themselves".to_string());
    }
    let app = window.app_handle();
    let parent = if window.label() == PROFILE_EDITOR_LABEL {
        app.get_webview_window(PROFILES_LABEL)
            .or_else(|| app.get_webview_window("main"))
    } else {
        app.get_webview_window("main")
    }
    .ok_or("The parent window is unavailable")?;
    #[cfg(target_os = "macos")]
    {
        if let Err(error) = macos::render(window).await {
            let _ = window.destroy();
            app.state::<std::sync::Arc<desktop_runtime::controller::DesktopRuntime>>()
                .report_error(error.clone());
            crate::tray::show_window(app);
            return Err(error);
        }
        macos::present(parent, window.clone())?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.show().map_err(window_error)?;
        window.set_focus().map_err(window_error)?;
        parent.set_enabled(false).map_err(window_error)?;
    }
    window
        .emit_to(window.label(), PRESENTED_EVENT, ())
        .map_err(window_error)
}

pub fn close(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == PROFILES_LABEL {
        if let Some(child) = window.app_handle().get_webview_window(PROFILE_EDITOR_LABEL) {
            focus_if_visible(&child)?;
            return Err("Close the profile editor first".to_string());
        }
    }
    if window.label() == UPDATE_PROGRESS_LABEL
        && !crate::updates::can_close_progress(window.app_handle())
    {
        return Err("Wait for the update to finish".to_string());
    }
    if !DIALOG_LABELS.contains(&window.label()) {
        return Err("Only native dialog windows can close themselves".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        macos::dismiss(window.clone())
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.destroy().map_err(window_error)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::window_error;
    use objc2::{rc::Retained, MainThreadMarker};
    use objc2_app_kit::{NSWindow, NSWindowButton};
    use tauri::WebviewWindow;

    pub async fn render(window: &WebviewWindow) -> Result<(), String> {
        use block2::RcBlock;
        use objc2_app_kit::NSImage;
        use objc2_foundation::NSError;
        use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};
        let (sender, receiver) = tokio::sync::oneshot::channel();
        window
            .with_webview(move |webview| {
                let Some(mtm) = MainThreadMarker::new() else {
                    let _ = sender.send(false);
                    return;
                };
                // Tauri owns this WKWebView; the callback receives a transient image
                // after WebKit incorporates screen updates. No image leaves memory.
                let view = unsafe { Retained::retain(webview.inner().cast::<WKWebView>()) };
                let Some(view) = view else {
                    let _ = sender.send(false);
                    return;
                };
                let sender = std::sync::Mutex::new(Some(sender));
                let completed = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                    if let Ok(mut sender) = sender.lock() {
                        if let Some(sender) = sender.take() {
                            let _ = sender.send(!image.is_null() && error.is_null());
                        }
                    }
                });
                // Public WebKit API, called on its owning main thread with valid objects.
                unsafe {
                    let config = WKSnapshotConfiguration::new(mtm);
                    config.setAfterScreenUpdates(true);
                    view.takeSnapshotWithConfiguration_completionHandler(Some(&config), &completed);
                }
            })
            .map_err(window_error)?;
        match tokio::time::timeout(std::time::Duration::from_secs(10), receiver).await {
            Ok(Ok(true)) => Ok(()),
            _ => Err("The dialog could not render. Please try opening it again.".into()),
        }
    }

    pub fn present(parent: WebviewWindow, window: WebviewWindow) -> Result<(), String> {
        let dispatcher = window.clone();
        on_main(&dispatcher, move || {
            let parent = native(&parent)?;
            let sheet = native(&window)?;
            if sheet.sheetParent().is_some() {
                return Ok(());
            }
            for kind in [
                NSWindowButton::CloseButton,
                NSWindowButton::MiniaturizeButton,
                NSWindowButton::ZoomButton,
            ] {
                if let Some(button) = sheet.standardWindowButton(kind) {
                    button.setHidden(true);
                }
            }
            sheet.setMovable(false);
            // Lay out and display AppKit content before the sheet animation starts.
            if let Some(content) = sheet.contentView() {
                content.layoutSubtreeIfNeeded();
                content.displayIfNeeded();
            }
            parent.beginSheet_completionHandler(&sheet, None);
            Ok(())
        })
    }

    pub fn dismiss(window: WebviewWindow) -> Result<(), String> {
        let dispatcher = window.clone();
        on_main(&dispatcher, move || {
            let sheet = native(&window)?;
            if let Some(parent) = sheet.sheetParent() {
                parent.endSheet(&sheet);
            }
            window.destroy().map_err(window_error)
        })
    }

    fn native(window: &WebviewWindow) -> Result<Retained<NSWindow>, String> {
        MainThreadMarker::new().ok_or("Native sheets require the main thread")?;
        let ptr = window.ns_window().map_err(window_error)?.cast::<NSWindow>();
        // SAFETY: Tauri owns this live NSWindow. Retaining it on the main thread
        // keeps it alive for the entire AppKit operation, including dismissal.
        unsafe { Retained::retain(ptr) }
            .ok_or_else(|| "The native window is unavailable".to_string())
    }

    fn on_main(
        window: &WebviewWindow,
        action: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<(), String> {
        if MainThreadMarker::new().is_some() {
            return action();
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        window
            .run_on_main_thread(move || {
                let _ = sender.send(action());
            })
            .map_err(window_error)?;
        receiver.recv().map_err(window_error)?
    }
}

fn centered_position(parent: &tauri::WebviewWindow, width: f64, height: f64) -> Option<(f64, f64)> {
    let Ok(scale) = parent.scale_factor() else {
        return None;
    };
    let (Ok(position), Ok(size)) = (parent.outer_position(), parent.outer_size()) else {
        return None;
    };
    Some((
        position.x as f64 / scale + (size.width as f64 / scale - width) / 2.0,
        position.y as f64 / scale + (size.height as f64 / scale - height) / 2.0,
    ))
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn window_error(error: impl std::fmt::Display) -> String {
    format!("Cannot manage the native dialog: {error}")
}

#[cfg(test)]
mod tests {
    use super::encode_query_component;

    #[test]
    fn usage_record_ids_are_url_encoded() {
        assert_eq!(
            encode_query_component("tag:legacy/agent@example"),
            "tag%3Alegacy%2Fagent%40example"
        );
    }
}
