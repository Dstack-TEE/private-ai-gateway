pub(crate) mod accounts;
pub(crate) mod desktop;
pub(crate) mod settings;
pub(crate) mod ui;

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// Opens `url` in the default browser, reporting `failure` if it does not open.
pub(crate) async fn open_url(
    app: AppHandle,
    url: String,
    failure: &'static str,
) -> Result<(), String> {
    crate::run_blocking(move || {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|_| failure.to_string())
    })
    .await
}
