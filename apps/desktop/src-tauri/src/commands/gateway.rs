use crate::*;
use desktop_runtime::ui_api::Method;
use serde_json::{json, Value};

#[tauri::command]
pub(crate) async fn get_gateway_state(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::GetState, json!({})).await
}

#[tauri::command]
pub(crate) async fn start_gateway(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    config: StartGatewayConfig,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::Start, json!({ "config": config })).await
}

#[tauri::command]
pub(crate) async fn start_backend_service(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::StartBackendService, json!({})).await
}

#[tauri::command]
pub(crate) async fn stop_gateway(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::Stop, json!({})).await
}

#[tauri::command]
pub(crate) async fn get_client_key(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::GetClientKey, json!({})).await
}

#[tauri::command]
pub(crate) async fn rotate_client_key(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::RotateClientKey, json!({})).await
}

#[tauri::command]
pub(crate) async fn save_local_api_config(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    config: LocalApiConfig,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::SaveLocalApiConfig,
        json!({ "config": config }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn list_listen_addresses(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::ListListenAddresses, json!({})).await
}

#[tauri::command]
pub(crate) async fn list_agents(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::ListAgents, json!({})).await
}

#[tauri::command]
pub(crate) async fn preview_agent_connection(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::PreviewAgent,
        json!({ "agentId": agent_id, "connect": connect, "options": options }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn apply_agent_connection(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
    agent_id: String,
    connect: bool,
    revision: String,
    options: ConnectOptions,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::ApplyAgent,
        json!({
            "agentId": agent_id,
            "connect": connect,
            "revision": revision,
            "options": options
        }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn get_agent_access(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::GetAgentAccess, json!({})).await
}

#[tauri::command]
pub(crate) async fn request_agent_access(
    window: tauri::WebviewWindow,
    client: State<'_, Arc<Client>>,
) -> Result<Value, String> {
    crate::ui_api::invoke(window, client, Method::RequestAgentAccess, json!({})).await
}
