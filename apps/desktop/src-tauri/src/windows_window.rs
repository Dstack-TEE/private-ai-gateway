//! The main window on Windows: the Windows 11 Mica material, a webview
//! without browser shortcuts, and the one-time notice that closing the window
//! leaves the app in the notification area.

use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_notification::NotificationExt;

/// Windows 11 (build 22000) is the first with Mica; elsewhere the window stays
/// opaque, since a transparent window without a backdrop shows the desktop.
pub fn mica_supported() -> bool {
    windows_version::OsVersion::current() >= windows_version::OsVersion::new(10, 0, 0, 22000)
}

/// Turns off WebView2's browser accelerator keys (Find, Print, Reload, zoom,
/// caret browsing and the like); text editing and navigation keys keep working.
pub fn disable_browser_accelerator_keys(window: &WebviewWindow) {
    let result = window.with_webview(|webview| {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
        use windows::core::Interface;
        // SAFETY: the controller is live for the duration of the callback,
        // which runs on the thread that owns it.
        let result = unsafe {
            webview
                .controller()
                .CoreWebView2()
                .and_then(|core| core.Settings())
                .and_then(|settings| settings.cast::<ICoreWebView2Settings3>())
                .and_then(|settings| settings.SetAreBrowserAcceleratorKeysEnabled(false))
        };
        if let Err(error) = result {
            tracing::warn!("Cannot turn off browser shortcuts: {error}");
        }
    });
    if let Err(error) = result {
        tracing::warn!("Cannot reach the webview to turn off browser shortcuts: {error}");
    }
}

const TRAY_NOTICE_FILE: &str = "tray-notice-shown";

/// The first time closing the window leaves the app running, says where it
/// went. The notice is recorded in the app's local data, not its settings.
pub fn explain_close_to_tray(app: &AppHandle) {
    if app.tray_by_id("gateway").is_none() {
        return;
    }
    let Ok(directory) = app.path().app_local_data_dir() else {
        return;
    };
    let marker = directory.join(TRAY_NOTICE_FILE);
    if marker.exists() {
        return;
    }
    let shown = app
        .notification()
        .builder()
        .title(format!(
            "{} is still running",
            desktop_core::brand::PRODUCT_NAME
        ))
        .body("Open it again or quit it from its icon in the notification area.")
        .show();
    if let Err(error) = shown {
        tracing::warn!("Cannot show the notification area notice: {error}");
        return;
    }
    if let Err(error) =
        std::fs::create_dir_all(&directory).and_then(|()| std::fs::write(&marker, b""))
    {
        tracing::warn!("Cannot record the notification area notice: {error}");
    }
}
