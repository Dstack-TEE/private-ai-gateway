use crate::*;

#[tauri::command]
pub(crate) async fn query_usage(
    client: State<'_, Arc<Client>>,
    query: UsageQuery,
) -> Result<UsagePage, String> {
    let client = client.inner().clone();
    run_blocking(move || client.query_usage(query)).await
}

#[tauri::command]
pub(crate) async fn get_usage_record(
    client: State<'_, Arc<Client>>,
    record_id: String,
) -> Result<RequestActivity, String> {
    let client = client.inner().clone();
    run_blocking(move || {
        client
            .usage_record(&record_id)?
            .ok_or_else(|| "Usage record not found".to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn export_usage_csv(
    client: State<'_, Arc<Client>>,
    query: UsageQuery,
    path: String,
) -> Result<usize, String> {
    let client = client.inner().clone();
    run_blocking(move || client.export_usage_csv(query, PathBuf::from(path))).await
}

#[tauri::command]
pub(crate) async fn clear_usage(client: State<'_, Arc<Client>>) -> Result<u64, String> {
    let client = client.inner().clone();
    run_blocking(move || client.clear_usage()).await
}
