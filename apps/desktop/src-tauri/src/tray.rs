use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{
    menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItem, MenuItemBuilder, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Wry,
};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_clipboard_manager::ClipboardExt;

use desktop_gateway::agents::{Agent, AgentStatus, ConnectOptions};
use desktop_gateway::brand::PRODUCT_NAME as APP_NAME;
use desktop_runtime::{contracts::GatewayState, controller::DesktopRuntime};

/// Native menu handles mirror runtime state; actions use the same controller as the window.
pub struct TrayMenu {
    toggle: MenuItem<Wry>,
    status: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    endpoint: MenuItem<Wry>,
    agents: Vec<(Agent, CheckMenuItem<Wry>)>,
    profiles: Submenu<Wry>,
    profile_items: Mutex<Option<Vec<ProfileMenuItem>>>,
    protected_icon: AtomicBool,
}

struct ProfileMenuItem {
    id: String,
    name: String,
    item: CheckMenuItem<Wry>,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let state = GatewayState::default();
    let status_line = protection_title(&state);
    let toggle = MenuItemBuilder::with_id("toggle", protection_action(&state)).build(app)?;
    let status = MenuItemBuilder::with_id("status", &status_line)
        .enabled(false)
        .build(app)?;
    let autostart = CheckMenuItemBuilder::with_id("autostart", "Open at Login")
        .checked(app.autolaunch().is_enabled().unwrap_or(false))
        .build(app)?;
    let endpoint = MenuItemBuilder::with_id("copy-endpoint", "Copy Local API Endpoint")
        .enabled(false)
        .build(app)?;
    let profiles = Submenu::with_items(app, "Profiles", true, &[])?;
    let agents_menu = Submenu::with_items(app, "Agents", true, &[])?;
    let mut agents = Vec::new();
    for agent in [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::Hermes,
        Agent::Pi,
        Agent::OpenCode,
    ] {
        let item = CheckMenuItemBuilder::with_id(format!("agent:{}", agent.id()), agent.name())
            .enabled(false)
            .build(app)?;
        agents_menu.append(&item)?;
        agents.push((agent, item));
    }
    agents_menu.append(&tauri::menu::PredefinedMenuItem::separator(app)?)?;
    agents_menu.append(&MenuItemBuilder::with_id("agents", "Manage Agents…").build(app)?)?;
    let menu = MenuBuilder::new(app)
        .item(&status)
        .item(&toggle)
        .separator()
        .text("open", format!("Open {APP_NAME}"))
        .text("settings", "Settings…")
        .separator()
        .item(&endpoint)
        .text("copy-key", "Copy Local API Key")
        .separator()
        .item(&profiles)
        .item(&agents_menu)
        .separator()
        .item(&autostart)
        .separator()
        .text("quit", format!("Quit {APP_NAME}"))
        .build()?;
    app.manage(TrayMenu {
        toggle,
        status,
        autostart,
        endpoint,
        agents,
        profiles,
        profile_items: Mutex::new(None),
        protected_icon: AtomicBool::new(false),
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
            "settings" | "agents" => {
                show_window(app);
                let _ = app.emit(crate::menu::NAVIGATE_EVENT, event.id().as_ref());
            }
            "autostart" => sync_autostart(app),
            "quit" => {
                app.exit(0);
            }
            id if matches!(id, "profiles" | "copy-key" | "copy-endpoint")
                || id.starts_with("profile:")
                || id.starts_with("agent:") =>
            {
                perform_action(app, id.to_string())
            }
            _ => {}
        })
        .build(app)?;
    let handle = app.clone();
    // Keep installation status and elapsed time current while the window is closed.
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let app = handle.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let runtime = app.state::<Arc<DesktopRuntime>>();
                if let Ok(state) = runtime.state() {
                    sync(&app, &state);
                }
                if let Ok(agents) = runtime.list_agents() {
                    sync_agents(&app, &agents);
                }
            })
            .await;
        }
    });
    Ok(())
}

fn perform_action(app: &AppHandle, id: String) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let runtime = app.state::<Arc<DesktopRuntime>>().inner().clone();
        let result = (|| -> Result<(), String> {
            match id.as_str() {
                "toggle" => {
                    let state = runtime.state()?;
                    if should_stop(&state) {
                        runtime.stop()?;
                    } else {
                        runtime.start(state.config)?;
                    }
                }
                "copy-endpoint" => {
                    let endpoint = runtime
                        .state()?
                        .proxy_url
                        .ok_or("Local API is unavailable")?;
                    app.clipboard()
                        .write_text(endpoint)
                        .map_err(|_| "Cannot copy the endpoint")?;
                }
                "copy-key" => {
                    app.clipboard()
                        .write_text(runtime.client_key()?)
                        .map_err(|_| "Cannot copy the client key")?;
                }
                "profiles" => {
                    show_window(&app);
                    crate::native_dialog::open_profiles(&app, false)?;
                }
                _ if id.starts_with("profile:") => {
                    runtime.activate_profile(id[8..].to_string())?;
                }
                _ if id.starts_with("agent:") => {
                    let agent_id = &id[6..];
                    let agent = runtime
                        .list_agents()?
                        .into_iter()
                        .find(|agent| agent.id == agent_id)
                        .ok_or("Agent is no longer available")?;
                    if !agent.installed && !agent.recorded {
                        return Err("Agent is not installed".into());
                    }
                    let connect = !agent.recorded;
                    let options = ConnectOptions {
                        default_model: if connect && agent_id == "codex" {
                            runtime.state()?.catalog.and_then(|catalog| {
                                catalog.models.first().map(|model| model.id.clone())
                            })
                        } else {
                            None
                        },
                    };
                    let preview =
                        runtime.preview_agent(agent.id.clone(), connect, options.clone())?;
                    runtime.apply_agent(agent.id, connect, preview.revision, options)?;
                }
                _ => return Ok(()),
            }
            Ok(())
        })();
        if let Err(error) = result {
            runtime.report_error(error);
            show_window(&app);
        }
        if let Ok(state) = runtime.state() {
            sync(&app, &state);
        }
        if id.starts_with("agent:") {
            if let Ok(agents) = runtime.list_agents() {
                sync_agents(&app, &agents);
            }
            let _ = app.emit("gateway://agents-changed", ());
        }
    });
}

pub fn sync_agents(app: &AppHandle, agents: &[AgentStatus]) {
    let handle = app.clone();
    let agents = agents.to_vec();
    let _ = app.run_on_main_thread(move || sync_agents_inner(&handle, &agents));
}

fn sync_agents_inner(app: &AppHandle, agents: &[AgentStatus]) {
    let Some(menu) = app.try_state::<TrayMenu>() else {
        return;
    };
    for (kind, item) in &menu.agents {
        let agent = agents.iter().find(|agent| agent.id == kind.id());
        let _ = item.set_checked(agent.is_some_and(|agent| agent.recorded));
        let _ = item.set_enabled(
            agent.is_some_and(|agent| agent.recorded || (agent.installed && agent.error.is_none())),
        );
        let suffix = match agent {
            Some(agent) if agent.attention.is_some() || agent.error.is_some() => {
                " - Needs attention"
            }
            Some(agent) if agent.installed => "",
            _ => " - Not installed",
        };
        let _ = item.set_text(format!("{}{suffix}", kind.name()));
    }
}

fn sync_profiles(app: &AppHandle, state: &GatewayState, menu: &TrayMenu) -> tauri::Result<()> {
    let Ok(mut cached) = menu.profile_items.lock() else {
        return Ok(());
    };
    let changed = cached.as_ref().is_none_or(|items| {
        items.len() != state.profiles.len()
            || items
                .iter()
                .zip(&state.profiles)
                .any(|(entry, profile)| entry.id != profile.id || entry.name != profile.name)
    });
    if changed {
        while menu.profiles.remove_at(0)?.is_some() {}
        let mut items = Vec::new();
        for profile in &state.profiles {
            let item =
                CheckMenuItemBuilder::with_id(format!("profile:{}", profile.id), &profile.name)
                    .build(app)?;
            menu.profiles.append(&item)?;
            items.push(ProfileMenuItem {
                id: profile.id.clone(),
                name: profile.name.clone(),
                item,
            });
        }
        if !items.is_empty() {
            menu.profiles
                .append(&tauri::menu::PredefinedMenuItem::separator(app)?)?;
        }
        menu.profiles.append(
            &MenuItemBuilder::with_id(
                "profiles",
                if items.is_empty() {
                    "New Profile…"
                } else {
                    "Manage Profiles…"
                },
            )
            .build(app)?,
        )?;
        *cached = Some(items);
    }
    for (entry, profile) in cached.iter().flatten().zip(&state.profiles) {
        let _ = entry
            .item
            .set_checked(profile.id == state.active_profile_id);
        let _ = entry.item.set_enabled(
            state.status != "verifying"
                && profile.verified_at.is_some()
                && profile.credential_saved.unwrap_or(true),
        );
    }
    Ok(())
}

fn toggle_or_open_settings(app: &AppHandle) {
    let runtime = app.state::<std::sync::Arc<DesktopRuntime>>();
    let Ok(state) = runtime.state() else {
        return;
    };
    if !should_stop(&state) && !active_profile_ready(&state) {
        sync(app, &state);
        show_window(app);
        if let Err(error) = crate::native_dialog::open_profiles(app, true) {
            runtime.report_error(error);
        }
        return;
    }
    perform_action(app, "toggle".to_string());
}

fn sync_autostart(app: &AppHandle) {
    let menu = app.state::<TrayMenu>();
    let checked = menu.autostart.is_checked().unwrap_or(false);
    let result = set_open_at_login(app, checked);
    if let Err(error) = result {
        let _ = menu.autostart.set_checked(!checked);
        app.state::<std::sync::Arc<DesktopRuntime>>()
            .report_error(format!("Open at Login could not be changed: {error}"));
    }
    if let Ok(preferences) = crate::get_launch_preferences(app.clone()) {
        let _ = app.emit("gateway://launch-preferences", preferences);
    }
}

pub fn set_open_at_login(app: &AppHandle, enabled: bool) -> Result<(), String> {
    if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    }
    .map_err(|error| format!("Open at Login could not be changed: {error}"))?;
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.autostart.set_checked(enabled);
    }
    Ok(())
}

/// Keep the status separate from the action the user can take.
pub fn sync(app: &AppHandle, state: &GatewayState) {
    let handle = app.clone();
    let state = state.clone();
    let _ = app.run_on_main_thread(move || sync_inner(&handle, &state));
}

fn sync_inner(app: &AppHandle, state: &GatewayState) {
    let status_line = protection_title(state);
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.status.set_text(&status_line);
        let _ = menu.toggle.set_text(protection_action(state));
        let _ = menu
            .toggle
            .set_enabled(should_stop(state) || state.endpoint_error.is_none());
        let _ = menu.endpoint.set_enabled(state.proxy_url.is_some());
        if let Err(error) = sync_profiles(app, state, &menu) {
            eprintln!("Cannot refresh tray profiles: {error}");
        }
        let protected = is_protected(state);
        if menu.protected_icon.load(Ordering::Relaxed) != protected {
            if let Some(tray) = app.tray_by_id("gateway") {
                if let Ok(icon) = tray_icon(protected) {
                    // Only replace NSImage on a state transition, preserving its
                    // template flag so macOS controls light/dark menu-bar tint.
                    if tray.set_icon_with_as_template(Some(icon), true).is_ok() {
                        menu.protected_icon.store(protected, Ordering::Relaxed);
                    }
                }
            }
        }
    }
    if let Some(tray) = app.tray_by_id("gateway") {
        let _ = tray.set_tooltip(Some(format!("{APP_NAME} - {status_line}")));
    }
}

fn tray_icon(protected: bool) -> tauri::Result<tauri::image::Image<'static>> {
    let image =
        tauri::image::Image::from_bytes(include_bytes!("../../assets/tray/trayTemplate@2x.png"))?;
    let mut rgba = image.rgba().to_vec();
    if !protected {
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = (u16::from(pixel[3]) * 45 / 100) as u8;
        }
    }
    Ok(tauri::image::Image::new_owned(
        rgba,
        image.width(),
        image.height(),
    ))
}

fn is_protected(state: &GatewayState) -> bool {
    state.status == "verified"
        && !state.configuration_verification
        && state.api_key_saved
        && state.endpoint_error.is_none()
}

fn protection_title(state: &GatewayState) -> String {
    if !is_protected(state) {
        return menu_state(state).into();
    }
    let mode = if state.config.require_production_os {
        "Protected"
    } else {
        "Protected (Dev mode)"
    };
    let Some(since) = state.protected_since else {
        return mode.into();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let minutes = now.saturating_sub(since) / 60;
    format!("{mode} - {}h {:02}m", minutes / 60, minutes % 60)
}

pub fn show_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window("main") else {
            return;
        };
        let _ = window.show();
        activate_app();
        let _ = window.set_focus();
    });
}

fn should_stop(state: &GatewayState) -> bool {
    matches!(state.status.as_str(), "verifying" | "blocked")
        || (state.status == "verified" && !state.configuration_verification)
}

fn protection_action(state: &GatewayState) -> &'static str {
    if state.status == "verifying" {
        "Cancel verification"
    } else if should_stop(state) {
        "Stop protection"
    } else if !active_profile_ready(state) {
        "Set Up Profile…"
    } else {
        "Start protection"
    }
}

fn menu_state(state: &GatewayState) -> &'static str {
    if state.endpoint_error.is_some() {
        return "Local API unavailable";
    }
    if state.status == "stopped" && !active_profile_ready(state) {
        return "Not protected - profile required";
    }
    match state.status.as_str() {
        "verifying" if state.configuration_verification => "Verifying configuration",
        "verifying" => "Verifying service",
        "verified" if state.configuration_verification => "Not protected - configuration verified",
        "verified" if !state.api_key_saved => "Not protected - API key required",
        "verified" => "Protected",
        "blocked" => "Protection blocked - service identity changed",
        "error" => "Not protected - verification failed",
        _ => "Not protected",
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
    use super::{menu_state, protection_action, should_stop, tray_icon};
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
    fn tray_assets_are_retina_sized_without_excessive_padding() {
        for protected in [false, true] {
            let icon = tray_icon(protected).unwrap();
            assert_eq!((icon.width(), icon.height()), (36, 36));
            let mut bounds = (36, 36, 0, 0);
            for (index, pixel) in icon.rgba().chunks_exact(4).enumerate() {
                if pixel[3] > 0 {
                    let (x, y) = (index % 36, index / 36);
                    bounds = (
                        bounds.0.min(x),
                        bounds.1.min(y),
                        bounds.2.max(x),
                        bounds.3.max(y),
                    );
                }
            }
            assert!(bounds.2 - bounds.0 >= 29 && bounds.3 - bounds.1 >= 29);
        }
        assert_ne!(
            tray_icon(false).unwrap().rgba(),
            tray_icon(true).unwrap().rgba()
        );
        let active = tray_icon(true).unwrap();
        let inactive = tray_icon(false).unwrap();
        for (on, off) in active
            .rgba()
            .chunks_exact(4)
            .zip(inactive.rgba().chunks_exact(4))
        {
            assert_eq!(&on[..3], &off[..3]);
            assert_eq!(u16::from(off[3]), u16::from(on[3]) * 45 / 100);
        }
    }

    #[test]
    fn protection_action_matches_the_runtime_operation() {
        assert_eq!(
            menu_state(&GatewayState::default()),
            "Not protected - profile required"
        );
        assert_eq!(menu_state(&state("verified", true)), "Protected");
        assert_eq!(
            menu_state(&state("verified", false)),
            "Not protected - API key required"
        );
        assert_eq!(
            protection_action(&state("verified", true)),
            "Stop protection"
        );
        assert_eq!(
            protection_action(&state("verifying", false)),
            "Cancel verification"
        );
        assert!(should_stop(&state("verifying", false)));
        assert!(should_stop(&state("blocked", true)));
        assert!(!should_stop(&state("error", true)));
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
        assert_eq!(menu_state(&ready), "Not protected");
        assert_eq!(protection_action(&ready), "Start protection");

        ready.status = "verified".into();
        ready.configuration_verification = true;
        assert!(!should_stop(&ready));
        assert_eq!(protection_action(&ready), "Start protection");
        assert_eq!(menu_state(&ready), "Not protected - configuration verified");

        ready.status = "stopped".into();
        ready.configuration_verification = false;

        ready.profiles[0].credential_saved = Some(false);
        assert_eq!(menu_state(&ready), "Not protected - profile required");
        assert_eq!(protection_action(&ready), "Set Up Profile…");
    }
}
