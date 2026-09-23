use std::sync::Arc;

use desktop_runtime::{client::Client, ui_api::Method, usage::UsageQuery};
use serde_json::{json, Value};
use tauri::{State, WebviewWindow};

#[tauri::command]
pub(crate) async fn query_usage(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    query: UsageQuery,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::QueryUsage,
        json!({ "query": query }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn get_usage_record(
    window: WebviewWindow,
    client: State<'_, Arc<Client>>,
    record_id: String,
) -> Result<Value, String> {
    crate::ui_api::invoke(
        window,
        client,
        Method::GetUsageRecord,
        json!({ "recordId": record_id }),
    )
    .await
}
