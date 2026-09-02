use tauri::{
    menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItem, MenuItemBuilder},
    tray::TrayIconBuilder,
    AppHandle, Manager, Wry,
};

use crate::{contracts::GatewayState, gateway::GatewayManager};

const APP_NAME: &str = "Private AI Gateway";

/// Live handles to the two menu rows that mirror gateway state.
pub struct TrayMenu {
    toggle: CheckMenuItem<Wry>,
    status: MenuItem<Wry>,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let (checked, status_line) = menu_state(&GatewayState::default());
    let toggle = CheckMenuItemBuilder::with_id("toggle", APP_NAME)
        .checked(checked)
        .build(app)?;
    let status = MenuItemBuilder::with_id("status", status_line)
        .enabled(false)
        .build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&toggle)
        .item(&status)
        .separator()
        .text("open", format!("Open {APP_NAME}"))
        .separator()
        .text("quit", format!("Quit {APP_NAME}"))
        .build()?;
    app.manage(TrayMenu { toggle, status });

    let icon =
        tauri::image::Image::from_bytes(include_bytes!("../../assets/tray/trayTemplate@2x.png"))?;
    TrayIconBuilder::with_id("gateway")
        .icon(icon)
        .icon_as_template(true)
        .tooltip(format!("{APP_NAME} - {status_line}"))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => app.state::<GatewayManager>().toggle(app),
            "open" => show_window(app),
            "quit" => {
                let _ = app.state::<GatewayManager>().stop(app);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Reflect the gateway state in the checkmark, the status row, and the tooltip.
pub fn sync(app: &AppHandle, state: &GatewayState) {
    let (checked, status_line) = menu_state(state);
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.toggle.set_checked(checked);
        let _ = menu.status.set_text(status_line);
    }
    if let Some(tray) = app.tray_by_id("gateway") {
        let _ = tray.set_tooltip(Some(format!("{APP_NAME} - {status_line}")));
    }
}

pub fn show_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.show();
    activate_app();
    let _ = window.set_focus();
}

/// Checkmark state and the plain-language status row for the gateway state.
fn menu_state(state: &GatewayState) -> (bool, &'static str) {
    match state.status.as_str() {
        "verifying" => (true, "Starting - verifying service"),
        "verified" if !state.api_key_saved => (true, "Verified - add your API key"),
        "verified" => (true, "Ready - requests protected"),
        "blocked" => (true, "Blocked - service identity changed"),
        "error" => (false, "Failed - open for details"),
        _ => (false, "Stopped"),
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn activate_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    if let Some(marker) = MainThreadMarker::new() {
        NSApplication::sharedApplication(marker).activateIgnoringOtherApps(true);
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_app() {}

#[cfg(test)]
mod tests {
    use super::menu_state;
    use crate::contracts::GatewayState;

    fn state(status: &str, api_key_saved: bool) -> GatewayState {
        GatewayState {
            status: status.to_string(),
            api_key_saved,
            ..GatewayState::default()
        }
    }

    #[test]
    fn checkmark_tracks_running_states() {
        assert_eq!(menu_state(&state("stopped", false)), (false, "Stopped"));
        assert_eq!(
            menu_state(&state("verified", true)),
            (true, "Ready - requests protected")
        );
        assert_eq!(
            menu_state(&state("verified", false)),
            (true, "Verified - add your API key")
        );
        assert!(menu_state(&state("verifying", false)).0);
        assert!(menu_state(&state("blocked", true)).0);
        assert!(!menu_state(&state("error", true)).0);
    }
}
