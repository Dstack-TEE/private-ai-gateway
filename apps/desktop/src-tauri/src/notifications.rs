mod permission;
use desktop_runtime::{
    client::Client, contracts::GatewayState, preferences::NotificationPreferences,
    protocol::Preference,
};
use std::{
    sync::{Arc, Mutex},
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

pub fn initialize(app: &AppHandle) {
    match app.state::<Arc<Client>>().preferences() {
        Ok(preferences) => {
            if let Ok(mut current) = app.state::<Settings>().0.lock() {
                *current = preferences.notifications;
            }
        }
        Err(error) => eprintln!("Cannot load notification preferences: {error}"),
    }
}

#[tauri::command]
pub async fn get_notification_settings(app: AppHandle) -> Result<Configuration, String> {
    let client = app.state::<Arc<Client>>().inner().clone();
    let preferences = crate::run_blocking(move || Ok(client.preferences()?.notifications)).await?;
    Ok(Configuration {
        preferences,
        system: permission::query(&app).await,
    })
}

#[tauri::command]
pub async fn save_notification_settings(
    app: AppHandle,
    config: NotificationPreferences,
) -> Result<(), String> {
    let client = app.state::<Arc<Client>>().inner().clone();
    crate::run_blocking(move || client.set_preference(Preference::Notifications(config))).await?;
    *app.state::<Settings>()
        .0
        .lock()
        .map_err(|_| "Notification settings are unavailable")? = config;
    Ok(())
}

#[tauri::command]
pub async fn request_notification_permission(
    app: AppHandle,
) -> Result<permission::PermissionStatus, String> {
    permission::request(&app).await?;
    Ok(permission::query(&app).await)
}

#[tauri::command]
pub fn open_notification_settings(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    #[cfg(target_os = "macos")]
    let url = "x-apple.systempreferences:com.apple.Notifications-Settings.extension";
    #[cfg(target_os = "windows")]
    let url = "ms-settings:notifications";
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    return app
        .opener()
        .open_url(url, None::<&str>)
        .map_err(|_| "Could not open system notification settings".into());
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app.opener();
        Err("Open Notifications in your desktop environment's settings.".into())
    }
}

pub struct Observer {
    faults: [bool; 2],
    session: Option<String>,
    failed_proof: u64,
    last_alert: [Option<Instant>; 3],
}

impl Observer {
    pub fn new(state: &GatewayState) -> Self {
        Self {
            faults: [false; 2],
            session: state.session_id.clone(),
            failed_proof: state.session_usage.failed_proof,
            last_alert: [None; 3],
        }
    }
    fn next(
        &mut self,
        state: &GatewayState,
        background: bool,
        config: NotificationPreferences,
        now: Instant,
    ) -> Vec<(&'static str, &'static str)> {
        let faults = [
            matches!(state.status.as_str(), "blocked" | "error") || state.error.is_some(),
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
            ("Gateway needs attention", "Protection or a gateway operation encountered a problem. Open Private AI Gateway to review its status."),
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
    pub fn update(&mut self, app: &AppHandle, state: &GatewayState) {
        let background = !app
            .webview_windows()
            .values()
            .any(|window| window.is_focused().unwrap_or(true));
        let Ok(config) = app.state::<Settings>().0.lock().map(|config| *config) else {
            return;
        };
        for (title, body) in self.next(state, background, config, Instant::now()) {
            if let Err(error) = app.notification().builder().title(title).body(body).show() {
                eprintln!("Cannot submit notification: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn categories_deduplicate_and_respect_foreground_and_master_switch() {
        let mut state = GatewayState::default();
        let mut observer = Observer::new(&state);
        let now = Instant::now();
        let config = NotificationPreferences {
            gateway: false,
            ..Default::default()
        };
        state.status = "error".into();
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
