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
