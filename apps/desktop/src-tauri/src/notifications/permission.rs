use tauri::AppHandle;

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Granted,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Denied,
    #[cfg(target_os = "macos")]
    NotDetermined,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Unknown,
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Unsupported,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatus {
    pub permission: Permission,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alerts_enabled: Option<bool>,
}

impl From<Permission> for PermissionStatus {
    fn from(permission: Permission) -> Self {
        Self {
            permission,
            alerts_enabled: None,
        }
    }
}

pub async fn query(app: &AppHandle) -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        return macos::query(app).await;
    }
    #[cfg(target_os = "windows")]
    {
        let id = app.config().identifier.clone();
        return crate::run_blocking(move || {
            use windows::{
                core::HSTRING,
                UI::Notifications::{NotificationSetting, ToastNotificationManager},
            };
            let setting = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(id))
                .and_then(|notifier| notifier.Setting());
            Ok(match setting {
                Ok(NotificationSetting::Enabled) => Permission::Granted,
                Ok(_) => Permission::Denied,
                Err(_) => Permission::Unknown,
            })
        })
        .await
        .map(PermissionStatus::from)
        .unwrap_or_else(|_| Permission::Unknown.into());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Permission::Unsupported.into()
    }
}

pub async fn request(app: &AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return macos::request(app).await;
    }
    #[cfg(not(target_os = "macos"))]
    {
        super::open_notification_settings(app.clone())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::NSError;
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSetting,
        UNNotificationSettings, UNUserNotificationCenter,
    };
    use std::{ptr::NonNull, sync::Mutex, time::Duration};

    pub async fn query(app: &AppHandle) -> PermissionStatus {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        if app
            .run_on_main_thread(move || {
                let sender = Mutex::new(Some(sender));
                let callback = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
                    // Apple guarantees the settings object is valid during this callback.
                    let settings = unsafe { settings.as_ref() };
                    let permission = match settings.authorizationStatus() {
                        UNAuthorizationStatus::NotDetermined => Permission::NotDetermined,
                        UNAuthorizationStatus::Denied => Permission::Denied,
                        UNAuthorizationStatus::Authorized
                        | UNAuthorizationStatus::Provisional
                        | UNAuthorizationStatus::Ephemeral => Permission::Granted,
                        _ => Permission::Unknown,
                    };
                    let alerts_enabled = match settings.alertSetting() {
                        UNNotificationSetting::Enabled => Some(true),
                        UNNotificationSetting::Disabled => Some(false),
                        _ => None,
                    };
                    if let Ok(mut sender) = sender.lock() {
                        if let Some(sender) = sender.take() {
                            let _ = sender.send(PermissionStatus {
                                permission,
                                alerts_enabled,
                            });
                        }
                    }
                });
                UNUserNotificationCenter::currentNotificationCenter()
                    .getNotificationSettingsWithCompletionHandler(&callback);
            })
            .is_err()
        {
            return Permission::Unknown.into();
        }
        match tokio::time::timeout(Duration::from_secs(10), receiver).await {
            Ok(Ok(permission)) => permission,
            _ => Permission::Unknown.into(),
        }
    }

    pub async fn request(app: &AppHandle) -> Result<(), String> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let sender = Mutex::new(Some(sender));
            let callback = RcBlock::new(move |_granted: Bool, error: *mut NSError| {
                if let Ok(mut sender) = sender.lock() {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(error.is_null());
                    }
                }
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .requestAuthorizationWithOptions_completionHandler(
                    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                    &callback,
                );
        })
        .map_err(|_| "Could not request notification permission")?;
        match receiver.await {
            Ok(true) => Ok(()),
            _ => Err("Could not request notification permission".into()),
        }
    }
}
