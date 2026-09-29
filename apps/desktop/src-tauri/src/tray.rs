//! The tray icon and its menu. The menu shows the backend state; its actions
//! use the same client as the window.

use std::sync::{Arc, Mutex};

use tauri::{
    image::Image,
    menu::{
        CheckMenuItem, CheckMenuItemBuilder, IsMenuItem, Menu, MenuBuilder, MenuItem,
        MenuItemBuilder, PredefinedMenuItem, Submenu, SubmenuBuilder,
    },
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime, Wry,
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
pub struct TrayMenu<R: Runtime = Wry> {
    status: MenuItem<R>,
    toggle: MenuItem<R>,
    endpoint: MenuItem<R>,
    profiles: Submenu<R>,
    agents: Vec<(Agent, CheckMenuItem<R>)>,
    autostart: CheckMenuItem<R>,
    shown: Mutex<Shown<R>>,
}

/// What the menu and the icon show, so only a change updates them.
struct Shown<R: Runtime> {
    profiles: Option<Vec<ProfileMenuItem<R>>>,
    /// The backend instance and agents revision the agent items show.
    agents: Option<(Option<String>, u64)>,
    icon: Icon,
}

struct ProfileMenuItem<R: Runtime> {
    id: String,
    name: String,
    item: CheckMenuItem<R>,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Icon {
    protected: bool,
    dark: bool,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let (tray_menu, menu) = build_menu(app, crate::autostart::is_enabled(app).unwrap_or(false))?;
    app.manage(tray_menu);
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon_image(Icon::default())?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(tooltip(&AppState::default().protection().title))
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

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    open_at_login: bool,
) -> tauri::Result<(TrayMenu<R>, Menu<R>)> {
    let protection = AppState::default().protection();
    let status = MenuItemBuilder::with_id("status", &protection.title)
        .enabled(false)
        .build(app)?;
    let toggle = MenuItemBuilder::with_id("toggle", &protection.action.label).build(app)?;
    let endpoint = MenuItemBuilder::with_id("copy-endpoint", "Copy Local API Endpoint")
        .enabled(false)
        .build(app)?;
    let profiles = SubmenuBuilder::new(app, "Profiles").build()?;
    // Check items are built checked; the agent items show the backend's
    // agents once it lists them.
    let agents = Agent::ALL
        .into_iter()
        .map(|agent| {
            let id = format!("agent:{}", agent.id());
            let item = CheckMenuItemBuilder::with_id(id, agent.name())
                .enabled(false)
                .build(app)?;
            Ok((agent, item))
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let agent_items: Vec<&dyn IsMenuItem<R>> = agents.iter().map(|(_, item)| item as _).collect();
    let autostart = CheckMenuItemBuilder::with_id("autostart", "Open at Login")
        .checked(open_at_login)
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
            &SubmenuBuilder::new(app, "Agents")
                .items(&agent_items)
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
    let shown = Mutex::new(Shown {
        profiles: None,
        agents: None,
        icon: Icon::default(),
    });
    let tray_menu = TrayMenu {
        status,
        toggle,
        endpoint,
        profiles,
        agents,
        autostart,
        shown,
    };
    Ok((tray_menu, menu))
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
        let host = crate::ui_api::TauriHost::new(app.clone());
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

/// Shows `state` in the tray, on the main thread.
pub fn sync(app: &AppHandle, state: &AppState) {
    let handle = app.clone();
    let state = state.clone();
    let _ = app.run_on_main_thread(move || update(&handle, &state));
}

/// Keeps the status separate from the action the user can take.
fn update<R: Runtime>(app: &AppHandle<R>, state: &AppState) {
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
        if let Ok(mut shown) = menu.shown.lock() {
            if let Err(error) = menu.show_profiles(app, &mut shown.profiles, state) {
                tracing::warn!("Cannot refresh tray profiles: {error}");
            }
            let agents = (state.backend_instance.clone(), state.agents_revision);
            if state.backend_instance.is_some() && shown.agents.as_ref() != Some(&agents) {
                shown.agents = Some(agents);
                refresh_agents(app);
            }
            let icon = Icon {
                protected: protection.phase == ProtectionPhase::Protected,
                ..shown.icon
            };
            if let Err(error) = show_icon(app, &mut shown.icon, icon) {
                tracing::warn!("Cannot update tray protection state: {error}");
            }
        }
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip(&protection.title)));
    }
}

impl<R: Runtime> TrayMenu<R> {
    /// Lists the profiles, rebuilding the submenu when they changed, and
    /// marks the active one.
    fn show_profiles(
        &self,
        app: &AppHandle<R>,
        listed: &mut Option<Vec<ProfileMenuItem<R>>>,
        state: &AppState,
    ) -> tauri::Result<()> {
        let unchanged = listed.as_ref().is_some_and(|items| {
            items.iter().map(|entry| (&entry.id, &entry.name)).eq(state
                .profiles
                .iter()
                .map(|profile| (&profile.id, &profile.name)))
        });
        if !unchanged {
            while self.profiles.remove_at(0)?.is_some() {}
            let mut items = Vec::new();
            for profile in &state.profiles {
                let item =
                    CheckMenuItemBuilder::with_id(format!("profile:{}", profile.id), &profile.name)
                        .build(app)?;
                self.profiles.append(&item)?;
                items.push(ProfileMenuItem {
                    id: profile.id.clone(),
                    name: profile.name.clone(),
                    item,
                });
            }
            let manage = if items.is_empty() {
                "New Profile…"
            } else {
                self.profiles.append(&PredefinedMenuItem::separator(app)?)?;
                "Manage Profiles…"
            };
            self.profiles
                .append(&MenuItemBuilder::with_id("profiles", manage).build(app)?)?;
            *listed = Some(items);
        }
        for (entry, profile) in listed.iter().flatten().zip(&state.profiles) {
            let _ = entry
                .item
                .set_checked(profile.id == state.active_profile_id);
            let _ = entry.item.set_enabled(
                state.status != VerificationStatus::Verifying && profile.credential_saved,
            );
        }
        Ok(())
    }

    fn show_agents(&self, agents: &[AgentStatus]) {
        for (kind, item) in &self.agents {
            let agent = agents.iter().find(|agent| agent.id == kind.id());
            let _ = item.set_checked(agent.is_some_and(|agent| agent.recorded));
            let _ =
                item.set_enabled(agent.is_some_and(|agent| {
                    agent.recorded || (agent.installed && agent.error.is_none())
                }));
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
                let _ = app.run_on_main_thread(move || {
                    if let Some(menu) = handle.try_state::<TrayMenu<R>>() {
                        menu.show_agents(&agents);
                    }
                });
            }
            Err(error) => tracing::warn!("Cannot refresh tray agents: {error}"),
        }
    });
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
        let icon = Icon { dark, ..shown.icon };
        if let Err(error) = show_icon(&handle, &mut shown.icon, icon) {
            tracing::warn!("Cannot update tray system theme: {error}");
        }
    })
}

/// Shows `icon` in the tray unless it already does.
fn show_icon<R: Runtime>(app: &AppHandle<R>, shown: &mut Icon, icon: Icon) -> tauri::Result<()> {
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
fn icon_image(icon: Icon) -> tauri::Result<Image<'static>> {
    let bytes: &'static [u8] = match (icon.protected, icon.dark) {
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
    use desktop_core::contracts::ConfidentialProfile;

    fn item_ids<R: Runtime>(items: Vec<tauri::menu::MenuItemKind<R>>) -> Vec<String> {
        use tauri::menu::MenuItemKind;
        items
            .into_iter()
            .flat_map(|item| match item {
                MenuItemKind::MenuItem(item) => vec![item.id().0.clone()],
                MenuItemKind::Check(item) => vec![item.id().0.clone()],
                MenuItemKind::Submenu(submenu) => item_ids(submenu.items().unwrap()),
                _ => Vec::new(),
            })
            .collect()
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

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "muda menus need the main thread")]
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
        // Every item but the status line does something.
        for id in item_ids(menu.items().unwrap()) {
            assert_eq!(Action::of(&id).is_none(), id == "status", "{id}");
        }

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
        update(app, &state);
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
        update(app, &state);
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
        update(app, &starting);
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
    #[cfg_attr(target_os = "macos", ignore = "muda menus need the main thread")]
    fn tray_agents_show_what_the_backend_reports() {
        let app = tauri::test::mock_app();
        let app = app.handle();
        let (tray, menu) = build_menu(app, false).unwrap();
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
        tray.show_agents(&[
            status(connected, true, true, None),
            status(installed, true, false, None),
            status(attention, true, false, Some("Removed model")),
        ]);
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
