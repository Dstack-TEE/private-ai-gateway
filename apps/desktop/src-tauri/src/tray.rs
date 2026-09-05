use tauri::{
    menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItem, MenuItemBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Wry,
};
use tauri_plugin_autostart::ManagerExt;

use desktop_gateway::brand::PRODUCT_NAME as APP_NAME;
use desktop_runtime::{contracts::GatewayState, controller::DesktopRuntime};

/// Live handles to the two menu rows that mirror gateway state.
pub struct TrayMenu {
    toggle: CheckMenuItem<Wry>,
    status: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let (checked, status_line) = menu_state(&GatewayState::default());
    let toggle = CheckMenuItemBuilder::with_id("toggle", "Protected")
        .checked(checked)
        .build(app)?;
    let status = MenuItemBuilder::with_id("status", status_line)
        .enabled(false)
        .build(app)?;
    let autostart = CheckMenuItemBuilder::with_id("autostart", "Open at Login")
        .checked(app.autolaunch().is_enabled().unwrap_or(false))
        .build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&toggle)
        .item(&status)
        .separator()
        .text("open", format!("Open {APP_NAME}"))
        .text("settings", "Settings…")
        .separator()
        .item(&autostart)
        .separator()
        .text("quit", format!("Quit {APP_NAME}"))
        .build()?;
    app.manage(TrayMenu {
        toggle,
        status,
        autostart,
    });

    let icon = tray_icon(false)?;
    TrayIconBuilder::with_id("gateway")
        .icon(icon)
        .icon_as_template(true)
        .tooltip(format!("{APP_NAME} - {status_line}"))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => toggle_or_open_settings(app),
            "open" => show_window(app),
            "settings" => {
                show_window(app);
                let _ = app.emit(crate::menu::NAVIGATE_EVENT, "settings");
            }
            "autostart" => sync_autostart(app),
            "quit" => {
                let _ = app.state::<std::sync::Arc<DesktopRuntime>>().stop();
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn toggle_or_open_settings(app: &AppHandle) {
    let runtime = app.state::<std::sync::Arc<DesktopRuntime>>();
    let Ok(state) = runtime.state() else {
        return;
    };
    let running = matches!(state.status.as_str(), "verifying" | "verified" | "blocked");
    if !running && !active_profile_ready(&state) {
        sync(app, &state);
        show_window(app);
        if let Err(error) = crate::native_dialog::open_profiles(app, true) {
            runtime.report_error(error);
        }
        return;
    }
    runtime.inner().clone().toggle();
}

fn sync_autostart(app: &AppHandle) {
    let menu = app.state::<TrayMenu>();
    let checked = menu.autostart.is_checked().unwrap_or(false);
    let result = if checked {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    if let Err(error) = result {
        let _ = menu.autostart.set_checked(!checked);
        app.state::<std::sync::Arc<DesktopRuntime>>()
            .report_error(format!("Open at Login could not be changed: {error}"));
    }
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
        if let Ok(icon) = tray_icon(
            state.status == "verified" && !state.configuration_verification && state.api_key_saved,
        ) {
            let _ = tray.set_icon(Some(icon));
        }
    }
}

fn tray_icon(protected: bool) -> tauri::Result<tauri::image::Image<'static>> {
    let bytes = if protected {
        include_bytes!("../../assets/tray/trayTemplateProtected.png").as_slice()
    } else {
        include_bytes!("../../assets/tray/trayTemplate.png").as_slice()
    };
    tauri::image::Image::from_bytes(bytes)
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
    if state.endpoint_error.is_some() {
        return (false, "Local API unavailable");
    }
    if state.status == "stopped" && !active_profile_ready(state) {
        return (false, "Profile required");
    }
    match state.status.as_str() {
        "verifying" if state.configuration_verification => (false, "Verifying configuration"),
        "verifying" => (true, "Starting - verifying service"),
        "verified" if state.configuration_verification => (false, "Configuration verified"),
        "verified" if !state.api_key_saved => (true, "Verified - add your API key"),
        "verified" if !state.config.require_production_os => {
            (true, "Ready - development OS allowed")
        }
        "verified" => (true, "Ready - requests protected"),
        "blocked" => (true, "Blocked - service identity changed"),
        "error" => (false, "Failed - open for details"),
        _ => (false, "Stopped"),
    }
}

fn active_profile_ready(state: &GatewayState) -> bool {
    state.api_key_saved
        && state.profiles.iter().any(|profile| {
            profile.id == state.active_profile_id
                && profile.verified_at.is_some()
                && profile.credential_saved.unwrap_or(true)
        })
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
    use desktop_runtime::contracts::{
        ConfidentialProfile, GatewayState, ProfileAuth, ServiceProvider,
    };

    fn state(status: &str, api_key_saved: bool) -> GatewayState {
        GatewayState {
            status: status.to_string(),
            api_key_saved,
            ..GatewayState::default()
        }
    }

    #[test]
    fn checkmark_tracks_running_states() {
        assert_eq!(
            menu_state(&GatewayState::default()),
            (false, "Profile required")
        );
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

    #[test]
    fn stopped_state_requires_a_verified_active_profile() {
        let mut ready = state("stopped", true);
        ready.active_profile_id = "profile-1".to_string();
        ready.profiles.push(ConfidentialProfile {
            id: "profile-1".to_string(),
            name: "Private AI".to_string(),
            provider: ServiceProvider::Custom,
            remote_url: "https://private.example.com".to_string(),
            auth: ProfileAuth::ApiKey,
            credential_saved: Some(true),
            verified_at: Some(1),
        });
        assert_eq!(menu_state(&ready), (false, "Stopped"));

        ready.profiles[0].credential_saved = Some(false);
        assert_eq!(menu_state(&ready), (false, "Profile required"));
    }
}
