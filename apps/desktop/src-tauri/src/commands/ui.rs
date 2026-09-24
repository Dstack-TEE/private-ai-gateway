//! Tauri commands served by the shared UI API. Each command is its method's
//! name in snake_case, so capabilities grant it individually; its arguments are
//! passed through unchanged as the method parameters the web UI sends.

use std::sync::Arc;

use desktop_core::{client::Client, ui_api::Method};
use serde_json::Value;
use tauri::{
    ipc::{InvokeBody, Request},
    State, WebviewWindow,
};

macro_rules! ui_commands {
    ($($command:ident => $method:ident),+ $(,)?) => {$(
        #[tauri::command]
        pub(crate) async fn $command(
            window: WebviewWindow,
            client: State<'_, Arc<Client>>,
            request: Request<'_>,
        ) -> Result<Value, String> {
            crate::ui_api::invoke(window, client, Method::$method, params(&request)?).await
        }
    )+};
}

ui_commands! {
    start_backend_service => StartBackendService,
    get_state => GetState,
    start => Start,
    stop => Stop,
    activate_profile => ActivateProfile,
    delete_profile => DeleteProfile,
    save_configuration => SaveConfiguration,
    complete_account_login => CompleteAccountLogin,
    begin_account_login => BeginAccountLogin,
    poll_account_login => PollAccountLogin,
    save_account_login => SaveAccountLogin,
    get_account_details => GetAccountDetails,
    get_account_balance => GetAccountBalance,
    cancel_account_login => CancelAccountLogin,
    get_client_key => GetClientKey,
    rotate_client_key => RotateClientKey,
    save_local_api_config => SaveLocalApiConfig,
    save_web_ui => SaveWebUi,
    set_web_ui_password => SetWebUiPassword,
    list_listen_addresses => ListListenAddresses,
    import_profiles => ImportProfiles,
    query_usage => QueryUsage,
    get_usage_record => GetUsageRecord,
    list_agents => ListAgents,
    get_agent_access => GetAgentAccess,
    request_agent_access => RequestAgentAccess,
    preview_agent => PreviewAgent,
    apply_agent => ApplyAgent,
    get_appearance => GetAppearance,
    set_appearance => SetAppearance,
    get_launch_preferences => GetLaunchPreferences,
    set_launch_preference => SetLaunchPreference,
    get_notification_settings => GetNotificationSettings,
    save_notification_settings => SaveNotificationSettings,
    reset_settings => ResetSettings,
}

fn params(request: &Request<'_>) -> Result<Value, String> {
    match request.body() {
        InvokeBody::Json(params) => Ok(params.clone()),
        InvokeBody::Raw(_) => Err("Invalid management request".into()),
    }
}
