use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{
    menu::{
        CheckMenuItem, CheckMenuItemBuilder, Menu, MenuBuilder, MenuItem, MenuItemBuilder, Submenu,
    },
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime, State, Wry,
};
use tauri_plugin_clipboard_manager::ClipboardExt;

use desktop_core::agents::{Agent, AgentStatus};
use desktop_core::brand::PRODUCT_NAME as APP_NAME;
use desktop_core::{
    client::Client,
    contracts::{AppState, NavigationTarget, VerificationStatus},
    protection::{ProtectionOperation, ProtectionPhase},
    protocol::rpc,
    ui_api::{LaunchPreference, LAUNCH_PREFERENCES_EVENT, NAVIGATE_EVENT},
};

/// Native menu handles mirror backend state; actions use the same client as the window.
pub struct TrayMenu<R: Runtime = Wry> {
    toggle: MenuItem<R>,
    status: MenuItem<R>,
    autostart: CheckMenuItem<R>,
    endpoint: MenuItem<R>,
    agents: Vec<(Agent, CheckMenuItem<R>)>,
    profiles: Submenu<R>,
    profile_items: Mutex<Option<Vec<ProfileMenuItem<R>>>>,
    /// The backend instance and agents revision the agent items show.
    agents_seen: Mutex<Option<(Option<String>, u64)>>,
    protected_icon: AtomicBool,
    dark_icon: AtomicBool,
}

struct ProfileMenuItem<R: Runtime> {
    id: String,
    name: String,
    item: CheckMenuItem<R>,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let (tray_menu, menu) = build_menu(app, crate::autostart::is_enabled(app).unwrap_or(false))?;
    app.manage(tray_menu);

    let protection = AppState::default().protection();
    let icon = tray_icon(false, false)?;
    TrayIconBuilder::with_id("gateway")
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(tooltip(&protection.title))
        .menu(&menu)
        // Windows opens a notification area app's window on a left click and
        // its menu on a right click; macOS and Linux show the menu.
        .show_menu_on_left_click(!cfg!(target_os = "windows"))
        .on_tray_icon_event(|tray, event| {
            if cfg!(target_os = "windows")
                && matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                )
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    open_at_login: bool,
) -> tauri::Result<(TrayMenu<R>, Menu<R>)> {
    let protection = AppState::default().protection();
    let toggle = MenuItemBuilder::with_id("toggle", &protection.action.label).build(app)?;
    let status = MenuItemBuilder::with_id("status", &protection.title)
        .enabled(false)
        .build(app)?;
    let autostart = CheckMenuItemBuilder::with_id("autostart", "Open at Login")
        .checked(open_at_login)
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
    let tray_menu = TrayMenu {
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
    };
    Ok((tray_menu, menu))
}

/// Runs a tray menu item, or the menu bar's Settings… item, which shares its
/// id; `menu::handle_event` is the one menu event handler.
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        "toggle" => toggle_or_open_settings(app),
        "open" => show_window(app),
        "settings" => navigate(app, NavigationTarget::Settings),
        "agents" => navigate(app, NavigationTarget::Agents),
        "profiles" => navigate(app, NavigationTarget::Profiles),
        "autostart" => sync_autostart(app),
        "quit" => app.exit(0),
        "stop-all-quit" => navigate(app, NavigationTarget::ConfirmStopAll),
        id if matches!(id, "copy-key" | "copy-endpoint")
            || id.starts_with("profile:")
            || id.starts_with("agent:") =>
        {
            perform_action(app, id.to_string())
        }
        _ => {}
    }
}

/// The latest request for the window that it has not taken yet.
#[derive(Default)]
pub struct PendingNavigation(Mutex<Option<NavigationTarget>>);

impl PendingNavigation {
    /// Replaces a request the window has not taken yet.
    fn set(&self, target: NavigationTarget) {
        if let Ok(mut pending) = self.0.lock() {
            *pending = Some(target);
        }
    }

    /// Takes the request, so it is handled once.
    fn take(&self) -> Option<NavigationTarget> {
        self.0.lock().ok()?.take()
    }
}

/// Shows the window at a page or dialog. The request waits here until the
/// renderer takes it, since one made before the renderer listens (while the
/// app is starting) would otherwise be lost; the event tells a listening
/// renderer to take it now.
fn navigate(app: &AppHandle, target: NavigationTarget) {
    show_window(app);
    app.state::<PendingNavigation>().set(target);
    let _ = app.emit(NAVIGATE_EVENT, ());
}

/// Takes the window's pending request; the renderer asks once it listens for
/// `NAVIGATE_EVENT` and again on each event.
#[tauri::command]
pub(crate) fn take_navigation(pending: State<'_, PendingNavigation>) -> Option<NavigationTarget> {
    pending.take()
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
            _ if id.starts_with("agent:") => {
                set_agent_connection(&app, &client, &id["agent:".len()..])
                    .map_err(|error| ("Could not change the agent connection", error))
            }
            _ => Ok(()),
        };
        if let Err((title, error)) = result {
            crate::notifications::show_failure(&app, title, &error);
        }
        // A check item flips when clicked; show what actually applies.
        if id.starts_with("agent:") {
            refresh_agents(&app);
        }
        sync(
            &app,
            &client.state().unwrap_or_else(|_| client.cached_state()),
        );
    });
}

/// Connects a disconnected agent, or disconnects a connected one. Codex's
/// background service keeps the previous settings, so while it runs the
/// window asks whether to stop it, as after a change there. The backend
/// never reports it running in the Mac App Store build.
fn set_agent_connection(app: &AppHandle, client: &Client, agent_id: &str) -> Result<(), String> {
    let agent = client
        .call(rpc::ListAgents)?
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .ok_or("The agent is no longer available")?;
    client.call(rpc::SetAgentConnection {
        agent_id: agent.id.clone(),
        connect: !agent.recorded,
    })?;
    if agent.id == Agent::Codex.id()
        && client
            .call(rpc::AgentServiceRunning { agent_id: agent.id })
            .unwrap_or(false)
    {
        navigate(app, NavigationTarget::ConfirmCodexServiceStop);
    }
    Ok(())
}

/// Follows the agents the backend reports; they change with its
/// `agents_revision` and when another backend answers.
fn refresh_agents<R: Runtime>(app: &AppHandle<R>) {
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

fn sync_agents<R: Runtime>(app: &AppHandle<R>, agents: &[AgentStatus]) {
    let Some(menu) = app.try_state::<TrayMenu<R>>() else {
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
            _ => " – Not Detected",
        };
        let _ = item.set_text(format!("{}{suffix}", kind.name()));
    }
}

fn sync_profiles<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    menu: &TrayMenu<R>,
) -> tauri::Result<()> {
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
        let action = state.protection().action;
        if !action.enabled {
            sync(&app, &state);
            return;
        }
        let result = match action.operation {
            ProtectionOperation::SetUpProfile => {
                sync(&app, &state);
                navigate(&app, NavigationTarget::ProfileSetup);
                return;
            }
            ProtectionOperation::Stop => client
                .call(rpc::Stop)
                .map_err(|error| ("Could not stop protection", error)),
            ProtectionOperation::Start => client
                .call(rpc::Start {
                    config: state.config,
                })
                .map_err(|error| ("Could not start protection", error)),
        };
        if let Err((title, error)) = result {
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
        let result = desktop_core::ui_api::set_launch_preference(
            &client,
            &host,
            LaunchPreference::OpenAtLogin,
            checked,
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

fn sync_inner<R: Runtime>(app: &AppHandle<R>, state: &AppState) {
    let protection = state.protection();
    if let Some(menu) = app.try_state::<TrayMenu<R>>() {
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

fn apply_icon<R: Runtime>(
    app: &AppHandle<R>,
    menu: &TrayMenu<R>,
    protected: bool,
    dark: bool,
) -> tauri::Result<()> {
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

impl MainWindowPresentation {
    /// Records a request to show the window; whether it can show now.
    fn request(&self) -> bool {
        self.requested.store(true, Ordering::SeqCst);
        self.ready.load(Ordering::SeqCst)
    }

    /// Records that the page loaded; whether an earlier request shows it now.
    fn loaded(&self) -> bool {
        !self.ready.swap(true, Ordering::SeqCst) && self.requested.load(Ordering::SeqCst)
    }
}

/// The main window's page has loaded; a request to show it that came earlier
/// shows it now.
pub fn main_window_ready(app: &AppHandle) {
    if app.state::<MainWindowPresentation>().loaded() {
        show_window(app);
    }
}

pub fn show_window(app: &AppHandle) {
    if !app.state::<MainWindowPresentation>().request() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::contracts::ConfidentialProfile;

    #[test]
    fn the_window_takes_the_latest_request_once() {
        let pending = PendingNavigation::default();
        assert_eq!(pending.take(), None);
        pending.set(NavigationTarget::Profiles);
        pending.set(NavigationTarget::ConfirmStopAll);
        assert_eq!(pending.take(), Some(NavigationTarget::ConfirmStopAll));
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn the_window_shows_once_its_page_loaded() {
        let early = MainWindowPresentation::default();
        assert!(!early.request());
        assert!(early.loaded());
        assert!(early.request());
        assert!(!early.loaded());

        let unrequested = MainWindowPresentation::default();
        assert!(!unrequested.loaded());
        assert!(unrequested.request());
    }

    fn describe_tray<R: Runtime>(menu: &Menu<R>) -> Vec<String> {
        crate::menu::describe(menu.items().unwrap())
    }

    fn profile(id: &str, name: &str, credential_saved: bool) -> ConfidentialProfile {
        ConfidentialProfile {
            id: id.into(),
            credential_ref: None,
            name: name.into(),
            provider: desktop_core::contracts::ServiceProvider::Custom,
            remote_url: "https://private.example.com".into(),
            auth: Default::default(),
            credential_saved,
            verified_at: None,
        }
    }

    /// The Agents submenu with `agents` as its agent items.
    fn agents_submenu(agents: &[String]) -> Vec<String> {
        let mut lines = vec!["Agents:".to_string()];
        lines.extend(agents.iter().map(|agent| format!("  {agent}")));
        lines.extend(["  ---".into(), "  agents: Manage Agents…".into()]);
        lines
    }

    fn tray_lines(
        status: &str,
        toggle: &str,
        endpoint: &str,
        profiles: &[&str],
        agents: &[String],
        open_at_login: &str,
    ) -> Vec<String> {
        let mut lines: Vec<String> = [
            &format!("status: {status} (disabled)"),
            &format!("toggle: {toggle}"),
            "---",
            "open: Open Private AI Proxy",
            "settings: Settings…",
            "---",
            &format!("copy-endpoint: Copy Local API Endpoint{endpoint}"),
            "copy-key: Copy Local API Key",
            "---",
        ]
        .map(String::from)
        .into();
        lines.extend(profiles.iter().map(|line| line.to_string()));
        lines.extend(agents_submenu(agents));
        lines.extend(
            [
                "---",
                &format!("autostart: [{open_at_login}] Open at Login"),
                "---",
                "quit: Quit Private AI Proxy",
                "stop-all-quit: Stop All and Quit…",
            ]
            .map(String::from),
        );
        lines
    }

    #[test]
    fn the_tray_menu_follows_the_backend_state() {
        let app = tauri::test::mock_app();
        let app = app.handle();
        let (tray, menu) = build_menu(app, true).unwrap();
        app.manage(tray);
        // Until the backend lists them, agent items are check items built
        // checked (the builder's default) and disabled.
        let unlisted: Vec<String> = Agent::ALL
            .iter()
            .map(|agent| format!("agent:{}: [x] {} (disabled)", agent.id(), agent.name()))
            .collect();
        assert_eq!(
            describe_tray(&menu),
            tray_lines(
                "Not protected",
                "Set Up Profile…",
                " (disabled)",
                &["Profiles:"],
                &unlisted,
                "x"
            )
        );

        let mut state = AppState {
            backend_connected: Some(true),
            api_key_saved: true,
            proxy_url: Some("http://127.0.0.1:8080/v1".into()),
            profiles: vec![
                profile("work", "Work", true),
                profile("home", "Home", false),
            ],
            active_profile_id: "work".into(),
            ..AppState::default()
        };
        sync_inner(app, &state);
        let profiles = [
            "Profiles:",
            "  profile:work: [x] Work",
            "  profile:home: [ ] Home (disabled)",
            "  ---",
            "  profiles: Manage Profiles…",
        ];
        assert_eq!(
            describe_tray(&menu),
            tray_lines(
                "Not protected",
                "Start Protection",
                "",
                &profiles,
                &unlisted,
                "x"
            )
        );

        state.status = VerificationStatus::Verifying;
        state.active_profile_id = "home".into();
        sync_inner(app, &state);
        let verifying = [
            "Profiles:",
            "  profile:work: [ ] Work (disabled)",
            "  profile:home: [x] Home (disabled)",
            "  ---",
            "  profiles: Manage Profiles…",
        ];
        assert_eq!(
            describe_tray(&menu),
            tray_lines(
                "Verifying…",
                "Cancel Verification",
                "",
                &verifying,
                &unlisted,
                "x"
            )
        );

        let starting = AppState {
            backend_connected: Some(false),
            ..AppState::default()
        };
        sync_inner(app, &starting);
        assert_eq!(
            describe_tray(&menu),
            tray_lines(
                "Starting…",
                "Set Up Profile… (disabled)",
                " (disabled)",
                &["Profiles: (disabled)", "  profiles: New Profile…"],
                &unlisted,
                "x"
            )
        );
        assert_eq!(tooltip("Starting…"), "Private AI Proxy – Starting…");
    }

    #[test]
    fn tray_agents_show_what_the_backend_reports() {
        let app = tauri::test::mock_app();
        let app = app.handle();
        let (tray, menu) = build_menu(app, false).unwrap();
        app.manage(tray);
        let status = |agent: Agent, installed, recorded, attention: Option<&str>| AgentStatus {
            id: agent.id().into(),
            name: agent.name().into(),
            config_path: String::new(),
            installed,
            connected: recorded,
            recorded,
            authorized: recorded,
            attention: attention.map(Into::into),
            error: None,
            repair_action: None,
        };
        let [connected, installed, attention, ..] = Agent::ALL;
        sync_agents(
            app,
            &[
                status(connected, true, true, None),
                status(installed, true, false, None),
                status(attention, true, false, Some("Removed model")),
            ],
        );
        let mut agents = vec![
            format!("agent:{}: [x] {}", connected.id(), connected.name()),
            format!("agent:{}: [ ] {}", installed.id(), installed.name()),
            format!(
                "agent:{}: [ ] {} – Needs Attention",
                attention.id(),
                attention.name()
            ),
        ];
        agents.extend(Agent::ALL[3..].iter().map(|agent| {
            format!(
                "agent:{}: [ ] {} – Not Detected (disabled)",
                agent.id(),
                agent.name()
            )
        }));
        assert_eq!(
            describe_tray(&menu),
            tray_lines(
                "Not protected",
                "Set Up Profile…",
                " (disabled)",
                &["Profiles:"],
                &agents,
                " "
            )
        );
    }
}
