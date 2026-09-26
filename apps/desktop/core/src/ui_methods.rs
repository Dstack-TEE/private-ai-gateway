// The renderer method list, the one source for `ui_api::Method`, the Tauri
// command functions, their invoke handler and the Tauri app manifest
// (`src-tauri/build.rs` includes this file). Each entry is `name => Variant`:
// the name is the Tauri command and the web RPC path. `commands` are the
// management commands of the same name; `host` methods are composed by a
// `ui_api::Host`. Which methods a desktop window may call is granted in
// `src-tauri/capabilities`.

/// Invokes `$callback! { commands { name => Variant, … } host { … } }`.
#[macro_export]
macro_rules! renderer_methods {
    ($callback:ident) => {
        $callback! {
            commands {
                get_state => GetState,
                start => Start,
                stop => Stop,
                set_require_production_os => SetRequireProductionOs,
                activate_profile => ActivateProfile,
                delete_profile => DeleteProfile,
                save_configuration => SaveConfiguration,
                complete_account_login => CompleteAccountLogin,
                begin_account_login => BeginAccountLogin,
                poll_account_login => PollAccountLogin,
                get_account_details => GetAccountDetails,
                get_account_balance => GetAccountBalance,
                cancel_account_login => CancelAccountLogin,
                get_client_key => GetClientKey,
                rotate_client_key => RotateClientKey,
                save_local_api_config => SaveLocalApiConfig,
                save_web_ui => SaveWebUi,
                get_web_ui_password => GetWebUiPassword,
                rotate_web_ui_password => RotateWebUiPassword,
                set_web_ui_password => SetWebUiPassword,
                import_profiles => ImportProfiles,
                export_profiles_content => ExportProfilesContent,
                export_diagnostics_content => ExportDiagnosticsContent,
                query_usage => QueryUsage,
                get_usage_record => GetUsageRecord,
                get_usage_receipt => GetUsageReceipt,
                list_agents => ListAgents,
                set_agent_connection => SetAgentConnection,
            }
            host {
                start_backend_service => StartBackendService,
                save_account_login => SaveAccountLogin,
                get_organization_url => GetOrganizationUrl,
                get_top_up_url => GetTopUpUrl,
                list_listen_addresses => ListListenAddresses,
                get_agent_access => GetAgentAccess,
                request_agent_access => RequestAgentAccess,
                get_appearance => GetAppearance,
                set_appearance => SetAppearance,
                get_launch_preferences => GetLaunchPreferences,
                set_launch_preference => SetLaunchPreference,
                get_notification_settings => GetNotificationSettings,
                save_notification_settings => SaveNotificationSettings,
                reset_settings => ResetSettings,
                get_update_notice => GetUpdateNotice,
            }
        }
    };
}
