//! Observe system appearance independently of the app's selected window theme.

pub use platform::setup;

#[cfg(target_os = "windows")]
mod platform {
    use std::sync::Mutex;
    use tauri::{AppHandle, Manager};
    use windows::{Foundation::TypedEventHandler, UI::ViewManagement::UISettings};

    pub fn setup(app: &AppHandle) -> Result<(), String> {
        let settings = UISettings::new().map_err(|e| e.to_string())?;
        let handle = app.clone();
        let handler =
            TypedEventHandler::<UISettings, windows::core::IInspectable>::new(move |_, _| {
                // Read on the main thread, so queued notifications always observe
                // the current system setting rather than replaying stale colors.
                let app = handle.clone();
                if let Err(error) = handle.run_on_main_thread(move || {
                    if let Err(error) = apply(&app) {
                        tracing::warn!("Cannot refresh system tray appearance: {error}");
                    }
                }) {
                    tracing::warn!("Cannot schedule system tray appearance: {error}");
                }
                Ok(())
            });
        let token = settings
            .ColorValuesChanged(&handler)
            .map_err(|e| e.to_string())?;
        // Subscribe before reading to avoid missing a change during startup.
        if let Err(error) = apply(app) {
            let _ = settings.RemoveColorValuesChanged(token);
            return Err(error);
        }
        // The subscription lasts as long as `settings`, which the app keeps.
        app.manage(Mutex::new(settings));
        Ok(())
    }

    fn apply(app: &AppHandle) -> Result<(), String> {
        let key = windows_registry::CURRENT_USER
            .open("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
            .map_err(|e| e.to_string())?;
        // Windows allows taskbar and application themes to differ.
        let light = key
            .get_u32("SystemUsesLightTheme")
            .map_err(|e| e.to_string())?;
        crate::tray::set_dark(app, light == 0).map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use futures_util::StreamExt;
    use std::collections::HashMap;
    use tauri::AppHandle;
    use zbus::{proxy, zvariant::OwnedValue};

    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";

    #[proxy(
        interface = "org.freedesktop.portal.Settings",
        default_service = "org.freedesktop.portal.Desktop",
        default_path = "/org/freedesktop/portal/desktop"
    )]
    trait Settings {
        // ReadAll works with both portal interface versions, unlike ReadOne.
        fn read_all(
            &self,
            namespaces: &[&str],
        ) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
        #[zbus(signal)]
        fn setting_changed(
            &self,
            namespace: &str,
            key: &str,
            value: OwnedValue,
        ) -> zbus::Result<()>;
    }

    pub fn setup(app: &AppHandle) -> Result<(), String> {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = observe(&handle).await {
                tracing::warn!("System tray appearance observer stopped: {error}");
            }
        });
        Ok(())
    }

    async fn observe(app: &AppHandle) -> Result<(), String> {
        let connection = zbus::Connection::session()
            .await
            .map_err(|e| e.to_string())?;
        let proxy = SettingsProxy::new(&connection)
            .await
            .map_err(|e| e.to_string())?;
        // Queue changes before the initial read so startup cannot miss a signal.
        let mut signals = proxy
            .receive_setting_changed()
            .await
            .map_err(|e| e.to_string())?;
        let settings = proxy
            .read_all(&[NAMESPACE])
            .await
            .map_err(|e| e.to_string())?;
        if let Some(value) = settings.get(NAMESPACE).and_then(|values| values.get(KEY)) {
            apply(app, value)?;
        }
        while let Some(signal) = signals.next().await {
            match signal.args() {
                Ok(args) if args.namespace == NAMESPACE && args.key == KEY => {
                    if let Err(error) = apply(app, &args.value) {
                        tracing::warn!("Cannot refresh system tray appearance: {error}");
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!("Invalid system appearance signal: {error}")
                }
            }
        }
        Err("Desktop portal appearance stream closed".into())
    }

    fn apply(app: &AppHandle, value: &OwnedValue) -> Result<(), String> {
        let preference = u32::try_from(value).map_err(|e| e.to_string())?;
        // XDG: 0 = no preference, 1 = dark, 2 = light; unknown means no preference.
        crate::tray::set_dark(app, preference == 1).map_err(|e| e.to_string())
    }
}
