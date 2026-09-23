use std::sync::Arc;

use desktop_runtime::{
    agent_access::AgentAccessStatus,
    client::Client,
    contracts::{AgentStatus, GatewayState},
    preferences::{Appearance, NotificationPreferences},
    ui_api::{self as shared, Event, Host, Method},
};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
use crate::native_dialog;
use crate::{apply_appearance, autostart, notifications, run_blocking, tray, updates};

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
static AGENT_ACCESS_REQUEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
pub(crate) struct TauriHost {
    app: AppHandle,
    window: Option<WebviewWindow>,
}

impl TauriHost {
    pub(crate) fn new(window: WebviewWindow) -> Self {
        Self {
            app: window.app_handle().clone(),
            window: Some(window),
        }
    }

    pub(crate) fn from_app(app: AppHandle) -> Self {
        Self { app, window: None }
    }

    fn app(&self) -> &AppHandle {
        &self.app
    }
}

impl Host for TauriHost {
    fn emit(&self, event: Event) -> Result<(), String> {
        if event.event == shared::SETTINGS_RESET_EVENT {
            self.app()
                .emit_to("main", event.event, event.payload)
                .map_err(|_| "Settings reset, but the interface could not refresh".to_string())
        } else {
            self.app()
                .emit(event.event, event.payload)
                .map_err(|_| "Could not sync the interface".to_string())
        }
    }

    fn apply_appearance(&self, appearance: Appearance) -> Result<(), String> {
        apply_appearance(self.app(), appearance);
        Ok(())
    }

    fn open_at_login(&self) -> Result<bool, String> {
        autostart::is_enabled(self.app())
    }

    fn set_open_at_login(&self, enabled: bool) -> Result<(), String> {
        tray::set_open_at_login(self.app(), enabled)
    }

    fn present_account_login(&self, url: &str) {
        if self.app().opener().open_url(url, None::<&str>).is_err() {
            eprintln!("Cannot open the account connection page; use the manual connection link");
        }
    }

    fn sync_agents(&self, agents: &[AgentStatus]) {
        tray::sync_agents(self.app(), agents);
    }

    async fn notification_configuration(
        &self,
        preferences: NotificationPreferences,
    ) -> Result<Value, String> {
        notifications::configuration(self.app(), preferences).await
    }

    fn notification_preferences_saved(
        &self,
        preferences: NotificationPreferences,
    ) -> Result<(), String> {
        notifications::set_cached_preferences(self.app(), preferences)
    }

    async fn reset_settings(&self, client: Arc<Client>) -> Result<GatewayState, String> {
        let prepared = self.app().state::<updates::PreparedUpdate>();
        let mut prepared = prepared
            .0
            .try_lock()
            .map_err(|_| "An update operation is in progress")?;
        let worker_app = self.app().clone();
        let result = run_blocking(move || {
            let state = client.reset_settings()?;
            tray::set_open_at_login(&worker_app, false)?;
            if let (Some(window), Some(defaults)) = (
                worker_app.get_webview_window("main"),
                worker_app
                    .config()
                    .app
                    .windows
                    .iter()
                    .find(|window| window.label == "main"),
            ) {
                window
                    .set_fullscreen(false)
                    .map_err(|_| "Could not reset the window")?;
                window
                    .unmaximize()
                    .map_err(|_| "Could not reset the window")?;
                window
                    .set_size(tauri::LogicalSize::new(defaults.width, defaults.height))
                    .map_err(|_| "Could not reset the window size")?;
                window.center().map_err(|_| "Could not center the window")?;
            }
            Ok(state)
        })
        .await;
        *prepared = None;
        result.map_err(|error| {
            format!("Reset did not finish. Review the error and retry Reset settings. {error}")
        })
    }

    async fn request_agent_access(&self, client: Arc<Client>) -> Result<Value, String> {
        let window = self
            .window
            .clone()
            .ok_or_else(|| "Home access requires the main window".to_string())?;
        request_agent_access(window, client).await?;
        serde_json::to_value(desktop_runtime::agent_access::status())
            .map_err(|_| "Management response failed".to_string())
    }
}

pub(crate) async fn invoke(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    method: Method,
    params: Value,
) -> Result<Value, String> {
    let client = client.inner().clone();
    shared::invoke(client, TauriHost::new(window), method, params)
        .await
        .map_err(shared::Error::message)
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
async fn request_agent_access(window: WebviewWindow, client: Arc<Client>) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;

    let Ok(_request) = AGENT_ACCESS_REQUEST.try_lock() else {
        return Ok(());
    };
    if window.label() != "main" || native_dialog::has_active_dialog(window.app_handle()) {
        return Ok(());
    }
    if desktop_runtime::agent_access::status() != AgentAccessStatus::Authorized {
        let home = desktop_runtime::agent_access::expected_home()?;
        let (send, receive) = tokio::sync::oneshot::channel();
        window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Choose Home Folder")
            .set_directory(&home)
            .set_can_create_directories(false)
            .pick_folder(move |selection| {
                let _ = send.send(selection);
            });
        let Some(selection) = receive
            .await
            .map_err(|_| "The Home folder picker could not complete".to_string())?
        else {
            return Ok(());
        };
        let path = selection
            .into_path()
            .map_err(|_| "The selected Home folder is invalid".to_string())?;
        run_blocking(move || desktop_runtime::agent_access::authorize(&path)).await?;
    }
    run_blocking(desktop_runtime::agent_access::prepare_for_service).await?;
    run_blocking(move || client.restart_service()).await?;
    let _ = window.emit(shared::AGENTS_CHANGED_EVENT, ());
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
async fn request_agent_access(_window: WebviewWindow, _client: Arc<Client>) -> Result<(), String> {
    let _ = AgentAccessStatus::Authorized;
    Ok(())
}
