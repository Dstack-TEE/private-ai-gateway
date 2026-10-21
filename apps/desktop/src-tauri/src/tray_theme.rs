//! Observe the Windows taskbar appearance independently of the app's selected
//! window theme. Linux panels do not report theirs, so the Linux tray icons
//! carry their own contrast instead.

use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use windows::{Foundation::TypedEventHandler, UI::ViewManagement::UISettings};

pub fn setup(app: &AppHandle) -> Result<(), String> {
    let settings = UISettings::new().map_err(|e| e.to_string())?;
    let handle = app.clone();
    let handler = TypedEventHandler::<UISettings, windows::core::IInspectable>::new(move |_, _| {
        // Read on the main thread, so queued notifications always observe
        // the current system setting rather than replaying stale colors.
        let app = handle.clone();
        if let Err(error) = handle.run_on_main_thread(move || {
            if let Err(error) = apply(&app) {
                tracing::warn!("Cannot refresh system tray appearance: {error}");
            }
        }) {
            tracing::warn!("Cannot schedule system tray appearance: {error}");
        }
        Ok(())
    });
    let token = settings
        .ColorValuesChanged(&handler)
        .map_err(|e| e.to_string())?;
    // Subscribe before reading to avoid missing a change during startup.
    if let Err(error) = apply(app) {
        let _ = settings.RemoveColorValuesChanged(token);
        return Err(error);
    }
    // The subscription lasts as long as `settings`, which the app keeps.
    app.manage(Mutex::new(settings));
    Ok(())
}

fn apply(app: &AppHandle) -> Result<(), String> {
    let key = windows_registry::CURRENT_USER
        .open("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
        .map_err(|e| e.to_string())?;
    // Windows allows taskbar and application themes to differ.
    let light = key
        .get_u32("SystemUsesLightTheme")
        .map_err(|e| e.to_string())?;
    crate::tray::set_dark(app, light == 0).map_err(|e| e.to_string())
}
