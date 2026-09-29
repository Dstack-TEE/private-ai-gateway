use std::sync::Arc;

use desktop_core::{
    agent_access::AgentAccessStatus,
    client::{CallError, Client},
    config::{Appearance, NotificationPreferences},
    contracts::{AppStateWire, NotificationConfiguration},
    protocol::rpc,
    ui_api::{self as shared, Backend, Event, Host, Method},
};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

use crate::{autostart, notifications, run_blocking, tray, updates, window};

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
static AGENT_ACCESS_REQUEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
pub(crate) struct TauriHost {
    app: AppHandle,
}

impl TauriHost {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Host for TauriHost {
    fn emit(&self, event: Event) -> Result<(), String> {
        if event.event == shared::SETTINGS_RESET_EVENT {
            self.app
                .emit_to(window::MAIN, event.event, event.payload)
                .map_err(|_| "Settings reset, but the interface could not refresh".to_string())
        } else {
            self.app
                .emit(event.event, event.payload)
                .map_err(|_| "Could not sync the interface".to_string())
        }
    }

    fn apply_appearance(&self, appearance: Appearance) -> Result<(), String> {
        self.app.set_theme(window::native_theme(appearance));
        Ok(())
    }

    fn open_at_login(&self) -> Result<bool, String> {
        autostart::is_enabled(&self.app)
    }

    fn set_open_at_login(&self, enabled: bool) -> Result<(), String> {
        tray::set_open_at_login(&self.app, enabled)
    }

    fn present_account_login(&self, url: &str) {
        if self.app.opener().open_url(url, None::<&str>).is_err() {
            tracing::warn!(
                "Cannot open the account connection page; use the manual connection link"
            );
        }
    }

    async fn notification_configuration(
        &self,
        preferences: NotificationPreferences,
    ) -> NotificationConfiguration {
        notifications::configuration(&self.app, preferences).await
    }

    fn notification_preferences_saved(
        &self,
        preferences: NotificationPreferences,
    ) -> Result<(), String> {
        notifications::set_cached_preferences(&self.app, preferences)
    }

    async fn reset_settings(&self, backend: &impl Backend) -> Result<AppStateWire, CallError> {
        let prepared = self.app.state::<updates::PreparedUpdate>();
        let mut prepared = prepared
            .0
            .try_lock()
            .map_err(|_| "An update operation is in progress")?;
        let worker_app = self.app.clone();
        let reset = shared::call(backend, rpc::ResetSettings).await;
        let result = run_blocking(move || {
            let state = reset?;
            tray::set_open_at_login(&worker_app, false)?;
            window::reset(&worker_app)?;
            Ok(state)
        })
        .await;
        *prepared = None;
        result.map_err(|error| {
            CallError::Local(format!(
                "Reset did not finish. Review the error and retry Reset Settings. {error}"
            ))
        })
    }

    async fn request_agent_access(&self) -> Result<AgentAccessStatus, String> {
        let window = self
            .app
            .get_webview_window(window::MAIN)
            .ok_or_else(|| "Home access requires the main window".to_string())?;
        let client = self.app.state::<Arc<Client>>().inner().clone();
        request_agent_access(window, client).await?;
        Ok(desktop_core::agent_access::status())
    }
}

pub(crate) async fn invoke(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    method: Method,
    params: Value,
) -> Result<Value, CallError> {
    shared::invoke(client.inner(), &TauriHost::new(app), method, params).await
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
async fn request_agent_access(window: WebviewWindow, client: Arc<Client>) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;

    let Ok(_request) = AGENT_ACCESS_REQUEST.try_lock() else {
        return Ok(());
    };
    if desktop_core::agent_access::status() != AgentAccessStatus::Authorized {
        let home = desktop_core::agent_access::expected_home()?;
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
        run_blocking(move || desktop_core::agent_access::authorize(&path)).await?;
    }
    run_blocking(desktop_core::agent_access::prepare_for_service).await?;
    run_blocking(move || client.restart_service()).await?;
    let _ = window.emit(shared::AGENTS_CHANGED_EVENT, ());
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
async fn request_agent_access(_window: WebviewWindow, _client: Arc<Client>) -> Result<(), String> {
    Ok(())
}
