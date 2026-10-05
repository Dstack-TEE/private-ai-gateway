//! The tray icon and its menu, which show the backend state; their actions use
//! the same client as the window.

use std::sync::{Arc, Mutex};

use tauri::{
    image::Image,
    menu::{
        CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItem, MenuItemBuilder,
        PredefinedMenuItem, Submenu, SubmenuBuilder,
    },
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Wry,
};
use tauri_plugin_clipboard_manager::ClipboardExt;

use desktop_core::{
    agents::{Agent, AgentStatus},
    brand::PRODUCT_NAME,
    client::Client,
    contracts::{AppState, NavigationTarget, VerificationStatus},
    protection::{ProtectionOperation, ProtectionPhase},
    protocol::rpc,
    ui_api::{LaunchPreference, LAUNCH_PREFERENCES_EVENT},
};

use crate::{notifications::show_failure, window};

pub(crate) const TRAY_ID: &str = "gateway";

/// The menu items that show the backend state.
pub struct TrayMenu {
    status: MenuItem<Wry>,
    toggle: MenuItem<Wry>,
    endpoint: MenuItem<Wry>,
    profiles: Submenu<Wry>,
    agents: Vec<(Agent, CheckMenuItem<Wry>)>,
    autostart: CheckMenuItem<Wry>,
    shown: Mutex<Shown>,
}

/// What the menu and the icon show, so only a change updates them.
#[derive(Default)]
struct Shown {
    /// Each listed profile's id and name, and its item.
    profiles: Option<Vec<(String, String, CheckMenuItem<Wry>)>>,
    /// The backend instance and agents revision the agent items show.
    agents: Option<(Option<String>, u64)>,
    /// Whether the icon shows protection, and whether it is the dark variant.
    icon: (bool, bool),
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let protection = AppState::default().protection();
    let status = MenuItemBuilder::with_id("status", &protection.title)
        .enabled(false)
        .build(app)?;
    let toggle = MenuItemBuilder::with_id("toggle", &protection.action.label).build(app)?;
    let endpoint = MenuItemBuilder::with_id("copy-endpoint", "Copy Local API Endpoint")
        .enabled(false)
        .build(app)?;
    let profiles = SubmenuBuilder::new(app, "Profiles").build()?;
    let mut agents_menu = SubmenuBuilder::new(app, "Agents");
    let mut agents = Vec::new();
    for agent in Agent::ALL {
        let item = CheckMenuItemBuilder::with_id(format!("agent:{}", agent.id()), agent.name())
            .checked(false)
            .enabled(false)
            .build(app)?;
        agents_menu = agents_menu.item(&item);
        agents.push((agent, item));
    }
    let autostart = CheckMenuItemBuilder::with_id("autostart", "Open at Login")
        .checked(crate::autostart::is_enabled(app).unwrap_or(false))
        .build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&status)
        .item(&toggle)
        .separator()
        .text("open", format!("Open {PRODUCT_NAME}"))
        .text("settings", "Settings…")
        .separator()
        .item(&endpoint)
        .text("copy-key", "Copy Local API Key")
        .separator()
        .item(&profiles)
        .item(
            &agents_menu
                .separator()
                .text("agents", "Manage Agents…")
                .build()?,
        )
        .separator()
        .item(&autostart)
        .separator()
        .text("quit", format!("Quit {PRODUCT_NAME}"))
        .text("stop-all-quit", "Stop All and Quit…")
        .build()?;
    app.manage(TrayMenu {
        status,
        toggle,
        endpoint,
        profiles,
        agents,
        autostart,
        shown: Mutex::default(),
    });
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon_image((false, false))?)
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
                window::show(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// What a tray item, or the menu bar's Settings… item, which shares its id,
/// does.
#[derive(Debug, PartialEq)]
enum Action {
    Toggle,
    Open,
    Navigate(NavigationTarget),
    OpenAtLogin,
    Quit,
    Background(Task),
}

/// An action that runs off the main thread and reports its failure in a
/// notification.
#[derive(Debug, PartialEq)]
enum Task {
    CopyEndpoint,
    CopyKey,
    ActivateProfile(String),
    ToggleAgent(String),
}

impl Action {
    fn of(id: &str) -> Option<Self> {
        Some(match id {
            "toggle" => Self::Toggle,
            "open" => Self::Open,
            "settings" => Self::Navigate(NavigationTarget::Settings),
            "agents" => Self::Navigate(NavigationTarget::Agents),
            "profiles" => Self::Navigate(NavigationTarget::Profiles),
            "stop-all-quit" => Self::Navigate(NavigationTarget::ConfirmStopAll),
            "autostart" => Self::OpenAtLogin,
            "quit" => Self::Quit,
            "copy-endpoint" => Self::Background(Task::CopyEndpoint),
            "copy-key" => Self::Background(Task::CopyKey),
            _ => Self::Background(match id.strip_prefix("profile:") {
                Some(profile_id) => Task::ActivateProfile(profile_id.into()),
                None => Task::ToggleAgent(id.strip_prefix("agent:")?.into()),
            }),
        })
    }
}

impl Task {
    fn failure(&self) -> &'static str {
        match self {
            Self::CopyEndpoint => "Could not copy the Local API endpoint",
            Self::CopyKey => "Could not copy the Local API key",
            Self::ActivateProfile(_) => "Could not switch profiles",
            Self::ToggleAgent(_) => "Could not change the agent connection",
        }
    }

    fn run(self, app: &AppHandle, client: &Client) -> Result<(), String> {
        match self {
            Self::CopyEndpoint => {
                let endpoint = client.state()?.proxy_url;
                copy(app, endpoint.ok_or("The Local API is unavailable")?)
            }
            Self::CopyKey => copy(app, client.call(rpc::GetClientKey)?),
            Self::ActivateProfile(profile_id) => {
                client.call(rpc::ActivateProfile { profile_id })?;
                Ok(())
            }
            Self::ToggleAgent(agent_id) => {
                let result = set_agent_connection(app, client, &agent_id);
                // A check item flips when clicked; show what actually applies.
                refresh_agents(app);
                result
            }
        }
    }
}

/// `menu::handle_event` is the one menu event handler.
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match Action::of(id) {
        Some(Action::Toggle) => toggle_or_set_up(app),
        Some(Action::Open) => window::show(app),
        Some(Action::Navigate(target)) => window::navigate(app, target),
        Some(Action::OpenAtLogin) => sync_autostart(app),
        Some(Action::Quit) => app.exit(0),
        Some(Action::Background(task)) => in_background(app, task),
        None => {}
    }
}

/// Runs `task`, reports its failure, and then shows the state that applies.
fn in_background(app: &AppHandle, task: Task) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = app.state::<Arc<Client>>().inner().clone();
        let failure = task.failure();
        if let Err(error) = task.run(&app, &client) {
            show_failure(&app, failure, &error);
        }
        sync(
            &app,
            &client.state().unwrap_or_else(|_| client.cached_state()),
        );
    });
}

fn copy(app: &AppHandle, text: String) -> Result<(), String> {
    app.clipboard()
        .write_text(text)
        .map_err(|_| "The clipboard is unavailable".into())
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
        window::navigate(app, NavigationTarget::ConfirmCodexServiceStop);
    }
    Ok(())
}

/// Starts or stops protection, or opens profile setup when there is no
/// profile to start with.
fn toggle_or_set_up(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = app.state::<Arc<Client>>();
        let Ok(state) = client.state() else {
            sync(&app, &client.cached_state());
            window::show(&app);
            return;
        };
        let action = state.protection().action;
        if !action.enabled {
            return sync(&app, &state);
        }
        let result = match action.operation {
            ProtectionOperation::SetUpProfile => {
                sync(&app, &state);
                return window::navigate(&app, NavigationTarget::ProfileSetup);
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
            show_failure(&app, title, &error.to_string());
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
        let client = app.state::<Arc<Client>>().inner().clone();
        let host = crate::ui_api::TauriHost(app.clone());
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
            show_failure(&app, "Could not change Open at Login", &error.to_string());
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

/// Shows `state` in the tray, on the main thread; the status stays separate
/// from the action the user can take.
pub fn sync(app: &AppHandle, state: &AppState) {
    let handle = app.clone();
    let state = state.clone();
    let _ = app.run_on_main_thread(move || update(&handle, &state));
}

fn update(app: &AppHandle, state: &AppState) {
    let protection = state.protection();
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.status.set_text(&protection.title);
        let _ = menu.toggle.set_text(&protection.action.label);
        let _ = menu.toggle.set_enabled(protection.action.enabled);
        let _ = menu.endpoint.set_enabled(state.proxy_url.is_some());
        // Profiles are unknown until the backend answers.
        let _ = menu
            .profiles
            .set_enabled(protection.phase != ProtectionPhase::Starting);
        if let Ok(mut shown) = menu.shown.lock() {
            if let Err(error) = show_profiles(app, &menu.profiles, &mut shown.profiles, state) {
                tracing::warn!("Cannot refresh tray profiles: {error}");
            }
            let agents = (state.backend_instance.clone(), state.agents_revision);
            if state.backend_instance.is_some() && shown.agents.as_ref() != Some(&agents) {
                shown.agents = Some(agents);
                refresh_agents(app);
            }
            let icon = (protection.phase == ProtectionPhase::Protected, shown.icon.1);
            if let Err(error) = show_icon(app, &mut shown.icon, icon) {
                tracing::warn!("Cannot update tray protection state: {error}");
            }
        }
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip(&protection.title)));
    }
}

/// Lists the profiles, rebuilding the submenu when they changed, and marks
/// the active one.
fn show_profiles(
    app: &AppHandle,
    submenu: &Submenu<Wry>,
    listed: &mut Option<Vec<(String, String, CheckMenuItem<Wry>)>>,
    state: &AppState,
) -> tauri::Result<()> {
    let unchanged = listed.as_ref().is_some_and(|items| {
        let listed = items.iter().map(|(id, name, _)| (id, name));
        listed.eq(state
            .profiles
            .iter()
            .map(|profile| (&profile.id, &profile.name)))
    });
    if !unchanged {
        while submenu.remove_at(0)?.is_some() {}
        let mut items = Vec::new();
        for profile in &state.profiles {
            let item =
                CheckMenuItemBuilder::with_id(format!("profile:{}", profile.id), &profile.name)
                    .build(app)?;
            submenu.append(&item)?;
            items.push((profile.id.clone(), profile.name.clone(), item));
        }
        let manage = if items.is_empty() {
            "New Profile…"
        } else {
            submenu.append(&PredefinedMenuItem::separator(app)?)?;
            "Manage Profiles…"
        };
        submenu.append(&MenuItemBuilder::with_id("profiles", manage).build(app)?)?;
        *listed = Some(items);
    }
    for ((_, _, item), profile) in listed.iter().flatten().zip(&state.profiles) {
        let _ = item.set_checked(profile.id == state.active_profile_id);
        let _ = item
            .set_enabled(state.status != VerificationStatus::Verifying && profile.credential_saved);
    }
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
                let _ = app.run_on_main_thread(move || show_agents(&handle, &agents));
            }
            Err(error) => tracing::warn!("Cannot refresh tray agents: {error}"),
        }
    });
}

fn show_agents(app: &AppHandle, agents: &[AgentStatus]) {
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
            _ => " – Not Detected",
        };
        let _ = item.set_text(format!("{}{suffix}", kind.name()));
    }
}

fn tooltip(status: &str) -> String {
    format!("{PRODUCT_NAME} – {status}")
}

/// System theme observers call this independently of the application's theme.
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_dark(app: &AppHandle, dark: bool) -> tauri::Result<()> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let Some(menu) = handle.try_state::<TrayMenu>() else {
            return;
        };
        let Ok(mut shown) = menu.shown.lock() else {
            return;
        };
        let icon = (shown.icon.0, dark);
        if let Err(error) = show_icon(&handle, &mut shown.icon, icon) {
            tracing::warn!("Cannot update tray system theme: {error}");
        }
    })
}

/// Shows the `(protected, dark)` icon unless the tray already does.
fn show_icon(app: &AppHandle, shown: &mut (bool, bool), icon: (bool, bool)) -> tauri::Result<()> {
    if *shown == icon {
        return Ok(());
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_icon_with_as_template(Some(icon_image(icon)?), cfg!(target_os = "macos"))?;
        *shown = icon;
    }
    Ok(())
}

/// Black icons are the macOS template and the light-theme icons elsewhere.
fn icon_image((protected, dark): (bool, bool)) -> tauri::Result<Image<'static>> {
    let bytes: &'static [u8] = match (protected, dark) {
        (true, false) => include_bytes!("../../assets/tray/protected.png"),
        (false, false) => include_bytes!("../../assets/tray/unprotected.png"),
        (true, true) => include_bytes!("../../assets/tray/protected-dark.png"),
        (false, true) => include_bytes!("../../assets/tray/unprotected-dark.png"),
    };
    Image::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_items_route_to_their_actions() {
        let background = |task: Task| Some(Action::Background(task));
        for (id, action) in [
            ("toggle", Some(Action::Toggle)),
            ("open", Some(Action::Open)),
            (
                "settings",
                Some(Action::Navigate(NavigationTarget::Settings)),
            ),
            ("agents", Some(Action::Navigate(NavigationTarget::Agents))),
            (
                "profiles",
                Some(Action::Navigate(NavigationTarget::Profiles)),
            ),
            (
                "stop-all-quit",
                Some(Action::Navigate(NavigationTarget::ConfirmStopAll)),
            ),
            ("autostart", Some(Action::OpenAtLogin)),
            ("quit", Some(Action::Quit)),
            ("copy-endpoint", background(Task::CopyEndpoint)),
            ("copy-key", background(Task::CopyKey)),
            (
                "profile:work",
                background(Task::ActivateProfile("work".into())),
            ),
            ("agent:codex", background(Task::ToggleAgent("codex".into()))),
            ("status", None),
            ("documentation", None),
        ] {
            assert_eq!(Action::of(id), action, "{id}");
        }
        for (task, failure) in [
            (Task::CopyEndpoint, "Could not copy the Local API endpoint"),
            (Task::CopyKey, "Could not copy the Local API key"),
            (
                Task::ActivateProfile("work".into()),
                "Could not switch profiles",
            ),
            (
                Task::ToggleAgent("codex".into()),
                "Could not change the agent connection",
            ),
        ] {
            assert_eq!(task.failure(), failure);
        }
    }
}
