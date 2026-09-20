use crate::*;

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
pub(crate) static AGENT_ACCESS_REQUEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tauri::command]
pub(crate) async fn get_gateway_state(
    client: State<'_, Arc<Client>>,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.state()).await
}

#[tauri::command]
pub(crate) async fn start_gateway(
    client: State<'_, Arc<Client>>,
    config: StartGatewayConfig,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.start(config)).await
}

#[tauri::command]
pub(crate) async fn start_backend_service(
    client: State<'_, Arc<Client>>,
) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || {
        Client::ensure_service()?;
        client.state()
    })
    .await
}

#[tauri::command]
pub(crate) async fn stop_gateway(client: State<'_, Arc<Client>>) -> Result<GatewayState, String> {
    let client = client.inner().clone();
    run_blocking(move || client.stop()).await
}

#[tauri::command]
pub(crate) async fn get_client_key(client: State<'_, Arc<Client>>) -> Result<String, String> {
    let client = client.inner().clone();
    run_blocking(move || client.client_key()).await
}

#[tauri::command]
pub(crate) async fn rotate_client_key(client: State<'_, Arc<Client>>) -> Result<String, String> {
    let client = client.inner().clone();
    run_blocking(move || client.rotate_client_key()).await
}

#[tauri::command]
pub(crate) async fn save_local_api_config(
    client: State<'_, Arc<Client>>,
    config: LocalApiConfig,
) -> Result<GatewayState, String> {
    client.inner().clone().save_local_api_config(config).await
}

#[tauri::command]
pub(crate) async fn list_listen_addresses() -> Result<Vec<ListenAddress>, String> {
    run_blocking(|| {
        let interfaces = if_addrs::get_if_addrs()
            .map_err(|_| "Could not read network interfaces. Enter an IP address manually.".to_string())?;
        let mut addresses: Vec<_> = interfaces
            .into_iter()
            .filter(|interface| interface.is_oper_up())
            // Link-local IPv6 needs a scope ID, which the listener does not support.
            .filter(|interface| !matches!(interface.ip(), std::net::IpAddr::V6(ip) if ip.is_unicast_link_local()))
            .map(|interface| ListenAddress {
                address: interface.ip().to_string(),
                name: interface.name,
            })
            .collect();
        addresses.sort_by(|a, b| a.address.cmp(&b.address).then(a.name.cmp(&b.name)));
        addresses.dedup_by(|a, b| a.address == b.address);
        Ok(addresses)
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_agents(
    app: AppHandle,
    client: State<'_, Arc<Client>>,
) -> Result<Vec<AgentStatus>, String> {
    let client = client.inner().clone();
    let agents = run_blocking(move || client.list_agents()).await?;
    tray::sync_agents(&app, &agents);
    Ok(agents)
}

#[tauri::command]
pub(crate) async fn preview_agent_connection(
    client: State<'_, Arc<Client>>,
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
) -> Result<AgentPreview, String> {
    let client = client.inner().clone();
    run_blocking(move || client.preview_agent(agent_id, connect, options)).await
}

#[tauri::command]
pub(crate) async fn apply_agent_connection(
    client: State<'_, Arc<Client>>,
    agent_id: String,
    connect: bool,
    revision: String,
    options: ConnectOptions,
) -> Result<AgentStatus, String> {
    let client = client.inner().clone();
    run_blocking(move || client.apply_agent(agent_id, connect, revision, options)).await
}

#[tauri::command]
pub(crate) async fn get_agent_access(
) -> Result<desktop_runtime::agent_access::AgentAccessStatus, String> {
    run_blocking(|| Ok(desktop_runtime::agent_access::status())).await
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
#[tauri::command]
pub(crate) async fn request_agent_access(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<desktop_runtime::agent_access::AgentAccessStatus, String> {
    use tauri::Emitter;
    use tauri_plugin_dialog::DialogExt;

    // Serialize explicit requests across windows; never stack native panels.
    let Ok(_request) = AGENT_ACCESS_REQUEST.try_lock() else {
        return get_agent_access().await;
    };
    let status = get_agent_access().await?;
    if window.label() != "main" || crate::native_dialog::has_active_dialog(window.app_handle()) {
        return Ok(status);
    }
    if status != desktop_runtime::agent_access::AgentAccessStatus::Authorized {
        let home = desktop_runtime::agent_access::expected_home()?;
        let (send, receive) = tokio::sync::oneshot::channel();
        window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Choose Home Folder")
            .set_directory(&home)
            .set_can_create_directories(false)
            .pick_folder(move |selection| {
                let _ = send.send(selection);
            });
        let Some(selection) = receive
            .await
            .map_err(|_| "The Home folder picker could not complete".to_string())?
        else {
            return run_blocking(|| Ok(desktop_runtime::agent_access::status())).await;
        };
        let path = selection
            .into_path()
            .map_err(|_| "The selected Home folder is invalid".to_string())?;
        run_blocking(move || desktop_runtime::agent_access::authorize(&path)).await?;
    }
    let client = client.inner().clone();
    run_blocking(move || client.restart_service()).await?;
    let _ = window.emit("gateway://agents-changed", ());
    run_blocking(|| Ok(desktop_runtime::agent_access::status())).await
}

#[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
#[tauri::command]
pub(crate) async fn request_agent_access(
) -> Result<desktop_runtime::agent_access::AgentAccessStatus, String> {
    get_agent_access().await
}
