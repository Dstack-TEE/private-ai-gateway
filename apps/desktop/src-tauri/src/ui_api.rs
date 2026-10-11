use std::sync::{Arc, Mutex};

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

/// The desktop shell's `Host`: the app's window, tray and native services.
#[derive(Clone)]
pub(crate) struct TauriHost(pub(crate) AppHandle);

impl Host for TauriHost {
    fn emit(&self, event: Event) -> Result<(), String> {
        if event.event == shared::SETTINGS_RESET_EVENT {
            self.0
                .emit_to(window::MAIN, event.event, event.payload)
                .map_err(|_| "Settings reset, but the interface could not refresh".to_string())
        } else {
            self.0
                .emit(event.event, event.payload)
                .map_err(|_| "Could not sync the interface".to_string())
        }
    }

    fn apply_appearance(&self, appearance: Appearance) -> Result<(), String> {
        self.0.set_theme(window::native_theme(appearance));
        Ok(())
    }

    fn open_at_login(&self) -> Result<bool, String> {
        autostart::is_enabled(&self.0)
    }

    fn set_open_at_login(&self, enabled: bool) -> Result<(), String> {
        tray::set_open_at_login(&self.0, enabled)
    }

    fn present_account_login(&self, url: &str) {
        if self.0.opener().open_url(url, None::<&str>).is_err() {
            tracing::warn!(
                "Cannot open the account connection page; use the manual connection link"
            );
        }
    }

    async fn notification_configuration(
        &self,
        preferences: NotificationPreferences,
    ) -> NotificationConfiguration {
        NotificationConfiguration {
            preferences,
            system: notifications::permission::query(&self.0).await,
        }
    }

    fn notification_preferences_saved(
        &self,
        preferences: NotificationPreferences,
    ) -> Result<(), String> {
        let cached = self.0.state::<Mutex<NotificationPreferences>>();
        *cached
            .lock()
            .map_err(|_| "Notification settings are unavailable")? = preferences;
        Ok(())
    }

    async fn reset_settings(&self, backend: &impl Backend) -> Result<AppStateWire, CallError> {
        let prepared = self.0.state::<updates::PreparedUpdate>();
        let mut prepared = prepared
            .try_lock()
            .map_err(|_| "An update operation is in progress")?;
        let worker_app = self.0.clone();
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
            .0
            .get_webview_window(window::MAIN)
            .ok_or_else(|| "Home access requires the main window".to_string())?;
        let client = self.0.state::<Arc<Client>>().inner().clone();
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
    shared::invoke(client.inner(), &TauriHost(app), method, params).await
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
async fn request_agent_access(window: WebviewWindow, client: Arc<Client>) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;

    let Ok(_request) = AGENT_ACCESS_REQUEST.try_lock() else {
        return Ok(());
    };
    if desktop_core::agent_access::status() != AgentAccessStatus::Authorized {
        let home = desktop_core::agent_access::expected_home()?;
        let dialog = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Choose Home Folder")
            .set_directory(&home)
            .set_can_create_directories(false);
        let Some(selection) = run_blocking(move || Ok(dialog.blocking_pick_folder())).await? else {
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
