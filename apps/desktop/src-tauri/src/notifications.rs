mod permission;
use desktop_core::{
    client::CallError,
    config::NotificationPreferences,
    contracts::{AppState, VerificationStatus},
};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

#[derive(Default)]
pub struct Settings(Mutex<NotificationPreferences>);

#[derive(serde::Serialize)]
pub struct Configuration {
    preferences: NotificationPreferences,
    #[serde(flatten)]
    system: permission::PermissionStatus,
}

pub async fn configuration(
    app: &AppHandle,
    preferences: NotificationPreferences,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(Configuration {
        preferences,
        system: permission::query(app).await,
    })
    .map_err(|_| "Management response failed".to_string())
}

pub fn set_cached_preferences(
    app: &AppHandle,
    preferences: NotificationPreferences,
) -> Result<(), String> {
    *app.state::<Settings>()
        .0
        .lock()
        .map_err(|_| "Notification settings are unavailable")? = preferences;
    Ok(())
}

#[tauri::command]
pub async fn request_notification_permission(
    app: AppHandle,
) -> Result<permission::PermissionStatus, CallError> {
    permission::request(&app).await?;
    Ok(permission::query(&app).await)
}

#[tauri::command]
pub fn open_notification_settings(app: AppHandle) -> Result<(), CallError> {
    use tauri_plugin_opener::OpenerExt;
    let url = system_notification_settings()
        .ok_or("Open Notifications in your desktop environment's settings.")?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| "Could not open system notification settings".into())
}

/// The system's notification settings page, where the platform has one.
fn system_notification_settings() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
    } else if cfg!(target_os = "windows") {
        Some("ms-settings:notifications")
    } else {
        None
    }
}

/// Reports an action the tray or the menu bar started, which has no window
/// to show its failure in.
pub fn show_failure(app: &AppHandle, title: &str, error: &str) {
    if let Err(notification) = app.notification().builder().title(title).body(error).show() {
        tracing::warn!("{title}: {error}; the notification failed too: {notification}");
    }
}

pub struct Observer {
    faults: [bool; 2],
    session: Option<String>,
    failed_proof: u64,
    last_alert: [Option<Instant>; 3],
}

impl Observer {
    pub fn new(state: &AppState) -> Self {
        Self {
            faults: [false; 2],
            session: state.session_id.clone(),
            failed_proof: state.session_usage.failed_proof,
            last_alert: [None; 3],
        }
    }
    fn next(
        &mut self,
        state: &AppState,
        background: bool,
        config: NotificationPreferences,
        now: Instant,
    ) -> Vec<(&'static str, &'static str)> {
        let faults = [
            !state.reconnecting
                && (matches!(
                    state.status,
                    VerificationStatus::Blocked | VerificationStatus::Error
                ) || state.error.is_some()),
            state.endpoint_error.is_some(),
        ];
        let proof = if self.session == state.session_id {
            state.session_usage.failed_proof > self.failed_proof
        } else {
            state.session_usage.failed_proof > 0
        };
        let changed = [
            faults[0] && !self.faults[0],
            faults[1] && !self.faults[1],
            proof,
        ];
        self.faults = faults;
        self.session = state.session_id.clone();
        self.failed_proof = state.session_usage.failed_proof;
        let allowed = [config.gateway, config.local_api, config.verification];
        let messages = [
            ("Protection needs attention", "Protection or a Private AI Proxy operation encountered a problem. Open Private AI Proxy to review its status."),
            ("Local API unavailable", "The local listener encountered a problem. Open Local API settings to review it."),
            ("Response verification failed", "A response failed proof verification. Open Usage to review the recorded result."),
        ];
        let mut result = Vec::new();
        for index in 0..3 {
            if background
                && config.enabled
                && allowed[index]
                && changed[index]
                && self.last_alert[index]
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(60))
            {
                self.last_alert[index] = Some(now);
                result.push(messages[index]);
            }
        }
        result
    }
    pub fn update(&mut self, app: &AppHandle, state: &AppState) {
        let background = !app
            .webview_windows()
            .values()
            .any(|window| window.is_focused().unwrap_or(true));
        let Ok(config) = app.state::<Settings>().0.lock().map(|config| *config) else {
            return;
        };
        for (title, body) in self.next(state, background, config, Instant::now()) {
            // On Linux the plugin sends from a spawned task, so a delivery
            // failure there never reaches this result.
            if let Err(error) = app.notification().builder().title(title).body(body).show() {
                tracing::warn!("Cannot submit notification: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn categories_deduplicate_and_respect_foreground_and_master_switch() {
        let mut state = AppState::default();
        let mut observer = Observer::new(&state);
        let now = Instant::now();
        let config = NotificationPreferences {
            gateway: false,
            ..Default::default()
        };
        state.status = VerificationStatus::Error;
        assert!(observer.next(&state, true, config, now).is_empty());
        state.endpoint_error = Some("private detail".into());
        assert_eq!(observer.next(&state, true, config, now).len(), 1);
        assert!(observer
            .next(&state, true, config, now + Duration::from_secs(120))
            .is_empty());
        state.session_usage.failed_proof = 1;
        assert!(observer.next(&state, false, config, now).is_empty());
        assert!(observer.next(&state, true, config, now).is_empty());
        state.session_usage.failed_proof = 2;
        assert!(observer
            .next(
                &state,
                true,
                NotificationPreferences {
                    enabled: false,
                    ..config
                },
                now
            )
            .is_empty());
        state.session_usage.failed_proof = 3;
        assert_eq!(observer.next(&state, true, config, now).len(), 1);
        state.session_usage.failed_proof = 4;
        assert!(observer.next(&state, true, config, now).is_empty());
    }
}
