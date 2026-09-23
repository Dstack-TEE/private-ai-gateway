use std::{path::PathBuf, sync::Arc};

use desktop_runtime::{
    client::Client,
    maintenance::ProfileBackup,
    preferences::Appearance,
    protocol::{rpc, Preference},
    ui_api::Method,
};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::{distribution, run_blocking, run_cli_command, CliRegistration, CliStartup};

#[tauri::command]
pub(crate) async fn get_launch_preferences(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::GetLaunchPreferences, json!({})).await
}

#[tauri::command]
pub(crate) async fn set_launch_preference(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    name: String,
    enabled: bool,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::SetLaunchPreference,
        json!({ "name": name, "enabled": enabled }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn reset_settings(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::ResetSettings, json!({})).await
}

#[tauri::command]
pub(crate) async fn get_appearance(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::GetAppearance, json!({})).await
}

#[tauri::command]
pub(crate) async fn set_appearance(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    appearance: Appearance,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::SetAppearance,
        json!({ "appearance": appearance }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn read_profile_backup(path: PathBuf) -> Result<ProfileBackup, String> {
    run_blocking(move || ProfileBackup::read(&path)).await
}

#[tauri::command]
pub(crate) async fn import_profiles(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    backup: ProfileBackup,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::ImportProfiles,
        json!({ "backup": backup }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn export_profiles(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    path: PathBuf,
) -> Result<(), String> {
    let content =
        crate::ui_api::invoke(window, client, Method::ExportProfilesContent, json!({})).await?;
    let content: String =
        serde_json::from_value(content).map_err(|_| "Management response failed")?;
    // The app writes the file so sandboxed builds keep the picker's file access.
    run_blocking(move || desktop_runtime::maintenance::write_export(&path, &content)).await
}

#[tauri::command]
pub(crate) async fn export_diagnostics(
    app: AppHandle,
    runtime: State<'_, Arc<Client>>,
    path: PathBuf,
) -> Result<(), String> {
    let version = app.package_info().version.to_string();
    let runtime = runtime.inner().clone();
    run_blocking(move || {
        let diagnostics = desktop_runtime::maintenance::diagnostics(&runtime.state()?, &version);
        desktop_runtime::maintenance::write_json(&path, &diagnostics)
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_cli_registration(app: AppHandle) -> Result<CliRegistration, String> {
    distribution::require(
        distribution::CAPABILITIES.cli_registration,
        "Command registration is unavailable in this distribution",
    )?;
    #[cfg(target_os = "macos")]
    crate::register_cli_on_startup(&app).await;
    let registration = run_cli_command(&app, vec!["cli", "status", "--json"]).await?;
    let startup_error = app.state::<CliStartup>().0.lock().await.last_error.clone();
    Ok(CliRegistration {
        registration,
        startup_error,
    })
}

#[tauri::command]
pub(crate) async fn set_cli_registration(
    app: AppHandle,
    installed: bool,
) -> Result<CliRegistration, String> {
    distribution::require(
        distribution::CAPABILITIES.cli_registration,
        "Command registration is unavailable in this distribution",
    )?;
    #[cfg(target_os = "macos")]
    crate::register_cli_on_startup(&app).await;
    let startup = app.state::<CliStartup>();
    let mut state = startup.0.lock().await;
    let client = app.state::<Arc<Client>>().inner().clone();
    if !installed {
        let writer = client.clone();
        run_blocking(move || {
            writer.call(rpc::SetPreference {
                change: Preference::AutoCliRegistration(false),
            })
        })
        .await?;
    }
    let registration = if installed {
        run_cli_command(&app, vec!["cli", "install", "--json"]).await
    } else {
        run_cli_command(&app, vec!["cli", "uninstall", "--json", "--yes"]).await
    }?;
    if installed {
        run_blocking(move || {
            client.call(rpc::SetPreference {
                change: Preference::AutoCliRegistration(true),
            })
        })
        .await?;
    }
    state.last_error = None;
    Ok(CliRegistration {
        registration,
        startup_error: None,
    })
}
