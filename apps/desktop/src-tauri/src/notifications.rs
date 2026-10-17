pub(crate) mod permission;
use desktop_core::{
    client::{CallError, Client},
    config::NotificationPreferences,
    contracts::{
        AppState, NotificationPermission, NotificationPermissionStatus, VerificationStatus,
    },
    protocol::rpc,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// Asks for the system permission at startup while the backend's preferences
/// have notifications on and the user has not decided yet, so alerts can show
/// without opening Settings. Where the system has no prompt, it never opens
/// anything. Returns whether the preferences could be read, which decides.
pub async fn request_startup_permission(app: &AppHandle, client: &Arc<Client>) -> bool {
    let Ok(settings) = desktop_core::ui_api::call(client, rpc::Settings).await else {
        return false;
    };
    if settings.notifications.enabled
        && permission::query(app).await.permission == NotificationPermission::NotDetermined
    {
        if let Err(error) = permission::request(app).await {
            tracing::warn!("Cannot request notification permission: {error}");
        }
    }
    true
}

#[tauri::command]
pub async fn request_notification_permission(
    app: AppHandle,
) -> Result<NotificationPermissionStatus, CallError> {
    permission::request(&app).await?;
    Ok(permission::query(&app).await)
}

#[tauri::command]
pub fn open_notification_settings(app: AppHandle) -> Result<(), CallError> {
    use tauri_plugin_opener::OpenerExt;
    let url = if cfg!(target_os = "macos") {
        "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
    } else if cfg!(target_os = "windows") {
        "ms-settings:notifications"
    } else {
        return Err("Open Notifications in your desktop environment's settings.".into());
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| "Could not open system notification settings".into())
}

/// Reports an action the tray or the menu bar started, which has no window
/// to show its failure in.
pub fn show_failure(app: &AppHandle, title: &str, error: &str) {
    if let Err(notification) = app.notification().builder().title(title).body(error).show() {
        tracing::warn!("{title}: {error}; the notification failed too: {notification}");
    }
}

#[derive(Default)]
pub struct Observer {
    faults: [bool; 2],
    /// The session and its failed proof count last seen; `None` until a
    /// backend answers, since the count it starts with is history, not news.
    proof: Option<(Option<String>, u64)>,
    last_alert: [Option<Instant>; 3],
}

impl Observer {
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
        let failed_proof = state.session_usage.failed_proof;
        let proof = match &self.proof {
            Some((session, seen)) if *session == state.session_id => failed_proof > *seen,
            Some(_) => failed_proof > 0,
            None => false,
        };
        let changed = [
            faults[0] && !self.faults[0],
            faults[1] && !self.faults[1],
            proof,
        ];
        self.faults = faults;
        if self.proof.is_some()
            || (state.backend_instance.is_some() && state.backend_connected != Some(false))
        {
            self.proof = Some((state.session_id.clone(), failed_proof));
        }
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
        let preferences = app.state::<Mutex<NotificationPreferences>>();
        let Ok(config) = preferences.lock().map(|config| *config) else {
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
        let mut state = AppState {
            backend_instance: Some("first".into()),
            ..AppState::default()
        };
        let mut observer = Observer::default();
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

    #[test]
    fn failed_proofs_before_the_backend_answers_are_history() {
        let mut observer = Observer::default();
        let config = NotificationPreferences::default();
        let now = Instant::now();
        let mut state = AppState {
            backend_instance: Some("first".into()),
            backend_connected: Some(false),
            session_id: Some("session".into()),
            ..AppState::default()
        };
        state.session_usage.failed_proof = 3;
        assert!(observer.next(&state, true, config, now).is_empty());
        // The first state of a backend that answered sets the baseline.
        state.backend_connected = None;
        assert!(observer.next(&state, true, config, now).is_empty());
        state.session_usage.failed_proof = 4;
        assert_eq!(observer.next(&state, true, config, now).len(), 1);
    }
}
