use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{
    menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItem, MenuItemBuilder, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Wry,
};
use tauri_plugin_clipboard_manager::ClipboardExt;

use desktop_core::agents::{Agent, AgentStatus};
use desktop_core::brand::PRODUCT_NAME as APP_NAME;
use desktop_core::{
    client::Client,
    contracts::{AppState, VerificationStatus},
    protection::ProtectionPhase,
    protocol::rpc,
    ui_api::{CONFIRM_STOP_ALL_EVENT, LAUNCH_PREFERENCES_EVENT, NAVIGATE_EVENT},
};

/// Native menu handles mirror backend state; actions use the same client as the window.
pub struct TrayMenu {
    toggle: MenuItem<Wry>,
    status: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    endpoint: MenuItem<Wry>,
    agents: Vec<(Agent, CheckMenuItem<Wry>)>,
    profiles: Submenu<Wry>,
    profile_items: Mutex<Option<Vec<ProfileMenuItem>>>,
    /// The backend instance and agents revision the agent items show.
    agents_seen: Mutex<Option<(Option<String>, u64)>>,
    protected_icon: AtomicBool,
    dark_icon: AtomicBool,
}

struct ProfileMenuItem {
    id: String,
    name: String,
    item: CheckMenuItem<Wry>,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let protection = AppState::default().protection;
    let toggle = MenuItemBuilder::with_id("toggle", &protection.action.label).build(app)?;
    let status = MenuItemBuilder::with_id("status", &protection.title)
        .enabled(false)
        .build(app)?;
    let autostart = CheckMenuItemBuilder::with_id("autostart", "Open at Login")
        .checked(crate::autostart::is_enabled(app).unwrap_or(false))
        .build(app)?;
    let endpoint = MenuItemBuilder::with_id("copy-endpoint", "Copy Local API Endpoint")
        .enabled(false)
        .build(app)?;
    let profiles = Submenu::with_items(app, "Profiles", true, &[])?;
    let agents_menu = Submenu::with_items(app, "Agents", true, &[])?;
    let mut agents = Vec::new();
    for agent in Agent::ALL {
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
        .text("stop-all-quit", "Stop All and Quit…")
        .build()?;
    app.manage(TrayMenu {
        toggle,
        status,
        autostart,
        endpoint,
        agents,
        profiles,
        profile_items: Mutex::new(None),
        agents_seen: Mutex::new(None),
        protected_icon: AtomicBool::new(false),
        dark_icon: AtomicBool::new(false),
    });

    let icon = tray_icon(false, false)?;
    TrayIconBuilder::with_id("gateway")
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(tooltip(&protection.title))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => toggle_or_open_settings(app),
            "open" => show_window(app),
            "settings" | "agents" | "profiles" => {
                show_window(app);
                let _ = app.emit(NAVIGATE_EVENT, event.id().as_ref());
            }
            "autostart" => sync_autostart(app),
            "quit" => {
                app.exit(0);
            }
            "stop-all-quit" => {
                show_window(app);
                let _ = app.emit(CONFIRM_STOP_ALL_EVENT, ());
            }
            id if matches!(id, "copy-key" | "copy-endpoint")
                || id.starts_with("profile:")
                || id.starts_with("agent:") =>
            {
                perform_action(app, id.to_string())
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn perform_action(app: &AppHandle, id: String) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = app.state::<Arc<Client>>().inner().clone();
        let result = match id.as_str() {
            "copy-endpoint" => client
                .state()
                .map_err(String::from)
                .and_then(|state| state.proxy_url.ok_or("The Local API is unavailable".into()))
                .and_then(|endpoint| {
                    app.clipboard()
                        .write_text(endpoint)
                        .map_err(|_| "The clipboard is unavailable".into())
                })
                .map_err(|error| ("Could not copy the Local API endpoint", error)),
            "copy-key" => client
                .call(rpc::GetClientKey)
                .map_err(String::from)
                .and_then(|key| {
                    app.clipboard()
                        .write_text(key)
                        .map_err(|_| "The clipboard is unavailable".into())
                })
                .map_err(|error| ("Could not copy the Local API key", error)),
            _ if id.starts_with("profile:") => client
                .call(rpc::ActivateProfile {
                    profile_id: id["profile:".len()..].to_string(),
                })
                .map(|_| ())
                .map_err(|error| ("Could not switch profiles", error.into())),
            _ if id.starts_with("agent:") => set_agent_connection(&client, &id["agent:".len()..])
                .map_err(|error| ("Could not change the agent connection", error)),
            _ => Ok(()),
        };
        if let Err((title, error)) = result {
            crate::notifications::show_failure(&app, title, &error);
        }
        sync(
            &app,
            &client.state().unwrap_or_else(|_| client.cached_state()),
        );
    });
}

/// Connects a disconnected agent, or disconnects a connected one.
fn set_agent_connection(client: &Client, agent_id: &str) -> Result<(), String> {
    let agent = client
        .call(rpc::ListAgents)?
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .ok_or("The agent is no longer available")?;
    client.call(rpc::SetAgentConnection {
        agent_id: agent.id,
        connect: !agent.recorded,
    })?;
    Ok(())
}

/// Follows the agents the backend reports; they change with its
/// `agents_revision` and when another backend answers.
fn refresh_agents(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = app.state::<Arc<Client>>().inner().clone();
        match client.call(rpc::ListAgents) {
            Ok(agents) => {
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || sync_agents(&handle, &agents));
            }
            Err(error) => tracing::warn!("Cannot refresh tray agents: {error}"),
        }
    });
}

fn sync_agents(app: &AppHandle, agents: &[AgentStatus]) {
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
                " – Needs Attention"
            }
            Some(agent) if agent.installed => "",
            _ => " – Not Installed",
        };
        let _ = item.set_text(format!("{}{suffix}", kind.name()));
    }
}

fn sync_profiles(app: &AppHandle, state: &AppState, menu: &TrayMenu) -> tauri::Result<()> {
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
        let _ = entry
            .item
            .set_enabled(state.status != VerificationStatus::Verifying && profile.credential_saved);
    }
    Ok(())
}

fn toggle_or_open_settings(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = app.state::<Arc<Client>>();
        let Ok(state) = client.state() else {
            sync(&app, &client.cached_state());
            show_window(&app);
            return;
        };
        let protection = &state.protection;
        if !protection.action.enabled {
            sync(&app, &state);
            return;
        }
        if protection.phase == ProtectionPhase::ProfileRequired {
            sync(&app, &state);
            show_window(&app);
            let _ = app.emit(NAVIGATE_EVENT, "profile-setup");
            return;
        }
        if let Err(error) = client.toggle() {
            let title = if protection.action.stops {
                "Could not stop protection"
            } else {
                "Could not start protection"
            };
            crate::notifications::show_failure(&app, title, &error.to_string());
        }
        sync(
            &app,
            &client.state().unwrap_or_else(|_| client.cached_state()),
        );
    });
}

fn sync_autostart(app: &AppHandle) {
    let menu = app.state::<TrayMenu>();
    let checked = menu.autostart.is_checked().unwrap_or(false);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let client = app.state::<std::sync::Arc<Client>>().inner().clone();
        let host = crate::ui_api::TauriHost::from_app(app.clone());
        let result = desktop_core::ui_api::invoke(
            &client,
            &host,
            desktop_core::ui_api::Method::SetLaunchPreference,
            serde_json::json!({ "name": "openAtLogin", "enabled": checked }),
        )
        .await;
        if let Err(error) = result {
            let menu = app.state::<TrayMenu>();
            let _ = menu.autostart.set_checked(!checked);
            crate::notifications::show_failure(
                &app,
                "Could not change Open at Login",
                &error.to_string(),
            );
            // Keep open windows in sync with the preference that actually applies.
            if let Ok(preferences) = desktop_core::ui_api::launch_preferences(&client, &host).await
            {
                let _ = app.emit(LAUNCH_PREFERENCES_EVENT, preferences);
            }
        }
    });
}

pub fn set_open_at_login(app: &AppHandle, enabled: bool) -> Result<(), String> {
    crate::autostart::set_enabled(app, enabled)
        .map_err(|error| format!("Open at Login could not be changed: {error}"))?;
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.autostart.set_checked(enabled);
    }
    Ok(())
}

/// Keep the status separate from the action the user can take.
pub fn sync(app: &AppHandle, state: &AppState) {
    let handle = app.clone();
    let state = state.clone();
    let _ = app.run_on_main_thread(move || sync_inner(&handle, &state));
}

fn sync_inner(app: &AppHandle, state: &AppState) {
    let protection = &state.protection;
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.status.set_text(&protection.title);
        let _ = menu.toggle.set_text(&protection.action.label);
        let _ = menu.toggle.set_enabled(protection.action.enabled);
        let _ = menu.endpoint.set_enabled(state.proxy_url.is_some());
        // Profiles are unknown until the backend answers.
        let _ = menu
            .profiles
            .set_enabled(protection.phase != ProtectionPhase::Starting);
        if let Err(error) = sync_profiles(app, state, &menu) {
            tracing::warn!("Cannot refresh tray profiles: {error}");
        }
        let agents = (state.backend_instance.clone(), state.agents_revision);
        if let Ok(mut seen) = menu.agents_seen.lock() {
            if state.backend_instance.is_some() && seen.as_ref() != Some(&agents) {
                *seen = Some(agents);
                refresh_agents(app);
            }
        }
        let protected = protection.phase == ProtectionPhase::Protected;
        if menu.protected_icon.load(Ordering::Relaxed) != protected {
            if let Err(error) = apply_icon(
                app,
                &menu,
                protected,
                menu.dark_icon.load(Ordering::Relaxed),
            ) {
                tracing::warn!("Cannot update tray protection state: {error}");
            }
        }
    }
    if let Some(tray) = app.tray_by_id("gateway") {
        let _ = tray.set_tooltip(Some(tooltip(&protection.title)));
    }
}

fn tooltip(status: &str) -> String {
    format!("{APP_NAME} – {status}")
}

/// System theme observers call this independently of the application's theme.
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_dark(app: &AppHandle, dark: bool) -> tauri::Result<()> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if let Some(menu) = handle.try_state::<TrayMenu>() {
            if menu.dark_icon.load(Ordering::Relaxed) != dark {
                if let Err(error) = apply_icon(
                    &handle,
                    &menu,
                    menu.protected_icon.load(Ordering::Relaxed),
                    dark,
                ) {
                    tracing::warn!("Cannot update tray system theme: {error}");
                }
            }
        }
    })
}

fn apply_icon(app: &AppHandle, menu: &TrayMenu, protected: bool, dark: bool) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("gateway") {
        tray.set_icon_with_as_template(
            Some(tray_icon(protected, dark)?),
            cfg!(target_os = "macos"),
        )?;
        menu.protected_icon.store(protected, Ordering::Relaxed);
        menu.dark_icon.store(dark, Ordering::Relaxed);
    }
    Ok(())
}

/// Black icons are the macOS template and the light-theme icons elsewhere.
fn tray_icon(protected: bool, dark: bool) -> tauri::Result<tauri::image::Image<'static>> {
    let bytes: &'static [u8] = match (protected, dark) {
        (true, false) => include_bytes!("../../assets/tray/protected.png"),
        (false, false) => include_bytes!("../../assets/tray/unprotected.png"),
        (true, true) => include_bytes!("../../assets/tray/protected-dark.png"),
        (false, true) => include_bytes!("../../assets/tray/unprotected-dark.png"),
    };
    tauri::image::Image::from_bytes(bytes)
}

#[derive(Default)]
pub struct MainWindowPresentation {
    ready: AtomicBool,
    requested: AtomicBool,
}

pub fn main_window_ready(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Only the main window can announce its content is ready".into());
    }
    let app = window.app_handle();
    let presentation = app.state::<MainWindowPresentation>();
    if !presentation.ready.swap(true, Ordering::SeqCst)
        && presentation.requested.load(Ordering::SeqCst)
    {
        show_window(app);
    }
    Ok(())
}

pub fn show_window(app: &AppHandle) {
    let presentation = app.state::<MainWindowPresentation>();
    presentation.requested.store(true, Ordering::SeqCst);
    if !presentation.ready.load(Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window("main") else {
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

pub fn hide_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window("main") {
            let _ = window.hide();
        }
        set_dock_visibility(&handle, false);
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
