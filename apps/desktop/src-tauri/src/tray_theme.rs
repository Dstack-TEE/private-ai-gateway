//! Observe system appearance independently of the app's selected window theme.

use std::sync::Mutex;
use tauri::{AppHandle, Manager};

pub use platform::{setup, shutdown};

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use windows::{Foundation::TypedEventHandler, UI::ViewManagement::UISettings};

    struct Watcher {
        settings: UISettings,
        token: i64,
    }

    impl Drop for Watcher {
        fn drop(&mut self) {
            if let Err(error) = self.settings.RemoveColorValuesChanged(self.token) {
                eprintln!("Cannot remove system appearance observer: {error}");
            }
        }
    }

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
                        eprintln!("Cannot refresh system tray appearance: {error}");
                    }
                }) {
                    eprintln!("Cannot schedule system tray appearance: {error}");
                }
                Ok(())
            });
        let token = settings
            .ColorValuesChanged(&handler)
            .map_err(|e| e.to_string())?;
        let watcher = Watcher { settings, token };
        // Subscribe before reading to avoid missing a change during startup.
        apply(app)?;
        if !app.manage(Mutex::new(Some(watcher))) {
            return Err("System appearance observer is already running".into());
        }
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

    pub fn shutdown(app: &AppHandle) {
        if let Some(watcher) = app.try_state::<Mutex<Option<Watcher>>>() {
            match watcher.lock() {
                Ok(mut guard) => {
                    guard.take();
                }
                Err(error) => eprintln!("Cannot stop system appearance observer: {error}"),
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use futures_util::StreamExt;
    use std::collections::HashMap;
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

    struct Watcher(tauri::async_runtime::JoinHandle<()>);

    impl Drop for Watcher {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    pub fn setup(app: &AppHandle) -> Result<(), String> {
        let handle = app.clone();
        let task = tauri::async_runtime::spawn(async move {
            if let Err(error) = observe(&handle).await {
                eprintln!("System tray appearance observer stopped: {error}");
            }
        });
        if !app.manage(Mutex::new(Some(Watcher(task)))) {
            return Err("System appearance observer is already running".into());
        }
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
                        eprintln!("Cannot refresh system tray appearance: {error}");
                    }
                }
                Ok(_) => {}
                Err(error) => eprintln!("Invalid system appearance signal: {error}"),
            }
        }
        Err("Desktop portal appearance stream closed".into())
    }

    fn apply(app: &AppHandle, value: &OwnedValue) -> Result<(), String> {
        let preference = u32::try_from(value).map_err(|e| e.to_string())?;
        // XDG: 0 = no preference, 1 = dark, 2 = light; unknown means no preference.
        crate::tray::set_dark(app, preference == 1).map_err(|e| e.to_string())
    }

    pub fn shutdown(app: &AppHandle) {
        if let Some(watcher) = app.try_state::<Mutex<Option<Watcher>>>() {
            match watcher.lock() {
                Ok(mut guard) => {
                    guard.take();
                }
                Err(error) => eprintln!("Cannot stop system appearance observer: {error}"),
            }
        }
    }
}
