use crate::*;

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
pub(crate) async fn refresh_catalog(
    client: State<'_, Arc<Client>>,
) -> Result<GatewayState, String> {
    client.inner().clone().refresh_catalog().await
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
pub(crate) async fn disconnect_all_agents(
    client: State<'_, Arc<Client>>,
) -> Result<Vec<AgentStatus>, String> {
    let client = client.inner().clone();
    run_blocking(move || client.disconnect_all_agents()).await
}
