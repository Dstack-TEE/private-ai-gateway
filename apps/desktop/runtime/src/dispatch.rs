//! The backend side of the management command table in
//! `desktop_core::protocol`: each command's handler, checked against the
//! response type the table declares for it.

use std::{path::PathBuf, sync::Arc};

use desktop_core::{
    preferences,
    protocol::{encode, rpc, Call, Command, RpcError, BUILD_VERSION},
};
use serde_json::Value;

use crate::controller::DesktopRuntime;

/// Runs one admitted command; its result must match the declared response.
pub(crate) async fn dispatch(
    runtime: &Arc<DesktopRuntime>,
    command: Command,
) -> Result<Value, RpcError> {
    match command {
        Command::State => respond::<rpc::State, _>(runtime.state()),
        // Streams run on their own connection in `server`.
        Command::Watch => respond::<rpc::Watch, _>(Err("Subscription requires its own connection")),
        Command::Start(config) => respond::<rpc::Start, _>(runtime.start(config)),
        Command::Stop => respond::<rpc::Stop, _>(runtime.stop()),
        // Answered by the connection under exclusive lifecycle admission.
        Command::Shutdown { .. } => {
            respond::<rpc::Shutdown, _>(Err("Shutdown requires lifecycle admission"))
        }
        Command::Verify {
            profile,
            require_production_os,
            key,
        } => respond::<rpc::Verify, _>(
            runtime
                .verify_configuration(profile, require_production_os, key)
                .await,
        ),
        Command::SaveConfiguration {
            profile,
            require_production_os,
            key,
        } => respond::<rpc::SaveConfiguration, _>(
            runtime
                .save_configuration(profile, require_production_os, key)
                .await,
        ),
        Command::CompleteAccountLogin { id, callback_url } => {
            respond::<rpc::CompleteAccountLogin, _>(
                runtime.complete_account_login(id, callback_url).await,
            )
        }
        Command::BeginAccountLogin { profile } => {
            respond::<rpc::BeginAccountLogin, _>(runtime.begin_account_login(profile).await)
        }
        Command::SaveAccountLogin {
            operation_id,
            id,
            profile,
            require_production_os,
            workspace_id,
        } => respond::<rpc::SaveAccountLogin, _>(runtime.begin_account_save(
            operation_id,
            id,
            profile,
            require_production_os,
            workspace_id,
        )),
        Command::AccountSaveResult { operation_id } => {
            respond::<rpc::AccountSaveResult, _>(runtime.account_save_result(&operation_id))
        }
        Command::AccountDetails { profile_id } => {
            respond::<rpc::AccountDetails, _>(runtime.account_details(profile_id).await)
        }
        Command::AccountBalance { target } => {
            respond::<rpc::AccountBalance, _>(runtime.account_balance(target).await)
        }
        Command::PollAccountLogin { id } => {
            respond::<rpc::PollAccountLogin, _>(runtime.poll_account_login(id).await)
        }
        Command::CancelAccountLogin { id } => {
            respond::<rpc::CancelAccountLogin, _>(runtime.cancel_account_login(id).await)
        }
        Command::ActivateProfile { profile_id } => {
            respond::<rpc::ActivateProfile, _>(runtime.activate_profile(profile_id))
        }
        Command::DeleteProfile { profile_id } => {
            respond::<rpc::DeleteProfile, _>(runtime.delete_profile(profile_id).await)
        }
        Command::ClearApiKey => respond::<rpc::ClearApiKey, _>(runtime.clear_api_key().await),
        Command::ImportProfiles(backup) => {
            respond::<rpc::ImportProfiles, _>(runtime.import_profiles(backup))
        }
        Command::ExportProfiles { path } => {
            respond::<rpc::ExportProfiles, _>(runtime.export_profiles(absolute(path)?))
        }
        Command::ExportProfilesContent => {
            respond::<rpc::ExportProfilesContent, _>(runtime.export_profiles_content())
        }
        Command::ExportDiagnostics { path } => respond::<rpc::ExportDiagnostics, _>(
            runtime.export_diagnostics(absolute(path)?, BUILD_VERSION),
        ),
        Command::ExportDiagnosticsContent => respond::<rpc::ExportDiagnosticsContent, _>(
            runtime.export_diagnostics_content(BUILD_VERSION),
        ),
        Command::Usage(query) => respond::<rpc::Usage, _>(runtime.query_usage(query)),
        Command::UsageRecord { record_id } => {
            respond::<rpc::UsageRecord, _>(runtime.usage_record(&record_id))
        }
        Command::ExportUsage { query, path } => {
            respond::<rpc::ExportUsage, _>(runtime.export_usage_csv(query, absolute(path)?))
        }
        Command::ClearUsage => respond::<rpc::ClearUsage, _>(runtime.clear_usage()),
        Command::ClientKey => respond::<rpc::ClientKey, _>(runtime.client_key()),
        Command::RotateClientKey => respond::<rpc::RotateClientKey, _>(runtime.rotate_client_key()),
        Command::SaveLocalApi(config) => {
            respond::<rpc::SaveLocalApi, _>(runtime.save_local_api_config(config).await)
        }
        Command::SaveWebUi(config) => respond::<rpc::SaveWebUi, _>(runtime.save_web_ui(config)),
        Command::WebUiLogin => respond::<rpc::WebUiLogin, _>(runtime.web_ui_login()),
        Command::RefreshCatalog => {
            respond::<rpc::RefreshCatalog, _>(runtime.refresh_catalog().await)
        }
        Command::Agents => respond::<rpc::Agents, _>(runtime.list_agents()),
        Command::PreviewAgent {
            agent_id,
            connect,
            options,
        } => respond::<rpc::PreviewAgent, _>(runtime.preview_agent(agent_id, connect, options)),
        Command::ApplyAgent {
            agent_id,
            connect,
            revision,
            options,
        } => {
            respond::<rpc::ApplyAgent, _>(runtime.apply_agent(agent_id, connect, revision, options))
        }
        Command::DisconnectAllAgents => {
            respond::<rpc::DisconnectAllAgents, _>(runtime.disconnect_all_agents())
        }
        Command::ResetSettings => respond::<rpc::ResetSettings, _>(runtime.reset_settings().await),
        Command::Preferences => respond::<rpc::Preferences, _>(preferences::load()),
        Command::SetPreference(change) => respond::<rpc::SetPreference, _>(
            preferences::update(|saved| change.apply(saved)).and_then(|()| preferences::load()),
        ),
    }
}

fn respond<C: Call, E: Into<RpcError>>(result: Result<C::Response, E>) -> Result<Value, RpcError> {
    encode::<C>(result.map_err(Into::into)?)
}

fn absolute(path: String) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("Export path must be absolute".into());
    }
    Ok(path)
}
