use crate::*;

#[tauri::command]
pub(crate) async fn get_launch_preferences(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<LaunchPreferences, String> {
    let client = client.inner().clone();
    run_blocking(move || load_launch_preferences(&app, &client)).await
}

#[tauri::command]
pub(crate) async fn set_launch_preference(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    name: String,
    enabled: bool,
) -> Result<LaunchPreferences, String> {
    let client = client.inner().clone();
    run_blocking(move || {
        match name.as_str() {
            "openAtLogin" => tray::set_open_at_login(&app, enabled)?,
            "connectOnLaunch" => {
                client.set_preference(Preference::ConnectOnLaunch(enabled))?;
            }
            _ => return Err("Unknown startup preference".to_string()),
        }
        let preferences = load_launch_preferences(&app, &client)?;
        let _ = app.emit("gateway://launch-preferences", &preferences);
        Ok(preferences)
    })
    .await
}

#[tauri::command]
pub(crate) async fn reset_settings(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    pending: State<'_, updates::PendingUpdate>,
) -> Result<GatewayState, String> {
    let mut pending = pending
        .0
        .try_lock()
        .map_err(|_| "An update operation is in progress")?;
    let worker_app = app.clone();
    let client = client.inner().clone();
    let worker = client.clone();
    let result = run_blocking(move || {
        let state = worker.reset_settings()?;
        tray::set_open_at_login(&worker_app, false)?;
        if let (Some(window), Some(defaults)) = (
            worker_app.get_webview_window("main"),
            worker_app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main"),
        ) {
            window
                .set_fullscreen(false)
                .map_err(|_| "Could not reset the window")?;
            window
                .unmaximize()
                .map_err(|_| "Could not reset the window")?;
            window
                .set_size(tauri::LogicalSize::new(defaults.width, defaults.height))
                .map_err(|_| "Could not reset the window size")?;
            window.center().map_err(|_| "Could not center the window")?;
        }
        Ok(state)
    })
    .await;
    *pending = None;
    refresh_preferences(&app, &client);
    let state = result.map_err(|error| {
        format!("Reset did not finish. Review the error and retry Reset settings. {error}")
    })?;
    app.emit_to("main", "gateway://settings-reset", ())
        .map_err(|_| "Settings reset, but the interface could not refresh")?;
    Ok(state)
}

#[tauri::command]
pub(crate) async fn get_appearance(client: State<'_, Arc<Client>>) -> Result<Appearance, String> {
    let client = client.inner().clone();
    run_blocking(move || Ok(client.preferences()?.appearance)).await
}

#[tauri::command]
pub(crate) async fn set_appearance(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
    appearance: Appearance,
) -> Result<(), String> {
    let client = client.inner().clone();
    run_blocking(move || {
        client.set_preference(Preference::Appearance(appearance))?;
        Ok(())
    })
    .await?;
    apply_appearance(&app, appearance);
    app.emit("gateway://appearance", appearance)
        .map_err(|_| "Could not sync appearance".to_string())
}

#[tauri::command]
pub(crate) async fn read_profile_backup(
    path: PathBuf,
) -> Result<desktop_runtime::maintenance::ProfileBackup, String> {
    run_blocking(move || desktop_runtime::maintenance::ProfileBackup::read(&path)).await
}

#[tauri::command]
pub(crate) async fn import_profiles(
    runtime: State<'_, Arc<Client>>,
    backup: desktop_runtime::maintenance::ProfileBackup,
) -> Result<desktop_runtime::maintenance::ImportResult, String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.import_profiles(backup)).await
}

#[tauri::command]
pub(crate) async fn export_profiles(
    runtime: State<'_, Arc<Client>>,
    path: PathBuf,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.export_profiles(path)).await
}

#[tauri::command]
pub(crate) async fn export_diagnostics(
    runtime: State<'_, Arc<Client>>,
    path: PathBuf,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    run_blocking(move || runtime.export_diagnostics(path)).await
}

#[tauri::command]
pub(crate) async fn get_cli_registration(app: AppHandle) -> Result<CliRegistration, String> {
    #[cfg(target_os = "macos")]
    register_cli_on_startup(&app).await;
    let registration = run_pap_cli(&app, vec!["cli", "status", "--json"]).await?;
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
    #[cfg(target_os = "macos")]
    register_cli_on_startup(&app).await;
    let startup = app.state::<CliStartup>();
    let mut state = startup.0.lock().await;
    let client = app.state::<Arc<Client>>().inner().clone();
    if !installed {
        let writer = client.clone();
        run_blocking(move || writer.set_preference(Preference::AutoCliRegistration(false))).await?;
    }
    let registration = if installed {
        run_pap_cli(&app, vec!["cli", "install", "--json"]).await
    } else {
        run_pap_cli(&app, vec!["cli", "uninstall", "--json", "--yes"]).await
    }?;
    if installed {
        run_blocking(move || client.set_preference(Preference::AutoCliRegistration(true))).await?;
    }
    state.last_error = None;
    Ok(CliRegistration {
        registration,
        startup_error: None,
    })
}
