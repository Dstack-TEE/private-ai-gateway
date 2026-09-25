//! The backend side of the management command table in
//! `desktop_core::protocol`: each command's handler, checked against the
//! response type the table declares for it.

use std::{path::PathBuf, sync::Arc};

use desktop_core::{
    contracts::{AppState, AppStateWire},
    protocol::{self, encode, rpc, Call, Command, ErrorCode, BUILD_VERSION},
};
use serde_json::Value;

use crate::controller::DesktopRuntime;

/// Runs one admitted command; its result must match the declared response.
pub(crate) async fn dispatch(
    runtime: &Arc<DesktopRuntime>,
    command: Command,
) -> Result<Value, protocol::Error> {
    match command {
        Command::GetState {} => respond_state::<rpc::GetState, _>(runtime.state()),
        Command::Start { config } => respond_state::<rpc::Start, _>(runtime.start(config)),
        Command::Stop {} => respond_state::<rpc::Stop, _>(runtime.stop()),
        // Answered by `server::shutdown` under exclusive lifecycle admission.
        Command::Shutdown { .. } => {
            respond::<rpc::Shutdown, _>(Err("Shutdown requires lifecycle admission"))
        }
        Command::Verify {
            profile,
            require_production_os,
            key,
        } => respond_state::<rpc::Verify, _>(
            runtime
                .verify_configuration(profile, require_production_os, key)
                .await,
        ),
        Command::SaveConfiguration {
            profile,
            require_production_os,
            key,
        } => respond_state::<rpc::SaveConfiguration, _>(
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
        Command::BeginAccountSave {
            operation_id,
            id,
            profile,
            require_production_os,
            workspace_id,
        } => respond::<rpc::BeginAccountSave, _>(runtime.begin_account_save(
            operation_id,
            id,
            profile,
            require_production_os,
            workspace_id,
        )),
        Command::AccountSaveResult { operation_id } => {
            respond::<rpc::AccountSaveResult, _>(runtime.account_save_result(&operation_id))
        }
        Command::GetAccountDetails { profile_id } => {
            respond::<rpc::GetAccountDetails, _>(runtime.account_details(profile_id).await)
        }
        Command::GetAccountBalance { target } => {
            respond::<rpc::GetAccountBalance, _>(runtime.account_balance(target).await)
        }
        Command::PollAccountLogin { id } => {
            respond::<rpc::PollAccountLogin, _>(runtime.poll_account_login(id).await)
        }
        Command::CancelAccountLogin { id } => {
            respond::<rpc::CancelAccountLogin, _>(runtime.cancel_account_login(id).await)
        }
        Command::ActivateProfile { profile_id } => {
            respond_state::<rpc::ActivateProfile, _>(runtime.activate_profile(profile_id))
        }
        Command::DeleteProfile { profile_id } => {
            respond_state::<rpc::DeleteProfile, _>(runtime.delete_profile(profile_id).await)
        }
        Command::ClearApiKey {} => {
            respond_state::<rpc::ClearApiKey, _>(runtime.clear_api_key().await)
        }
        Command::ImportProfiles { backup } => {
            respond::<rpc::ImportProfiles, _>(runtime.import_profiles(backup))
        }
        Command::ExportProfiles { path } => {
            respond::<rpc::ExportProfiles, _>(runtime.export_profiles(absolute(path)?))
        }
        Command::ExportProfilesContent {} => {
            respond::<rpc::ExportProfilesContent, _>(runtime.export_profiles_content())
        }
        Command::ExportDiagnostics { path } => respond::<rpc::ExportDiagnostics, _>(
            runtime.export_diagnostics(absolute(path)?, BUILD_VERSION),
        ),
        Command::ExportDiagnosticsContent {} => respond::<rpc::ExportDiagnosticsContent, _>(
            runtime.export_diagnostics_content(BUILD_VERSION),
        ),
        Command::QueryUsage { query } => respond::<rpc::QueryUsage, _>(runtime.query_usage(query)),
        Command::GetUsageRecord { record_id } => match runtime.usage_record(&record_id) {
            Ok(Some(record)) => encode::<rpc::GetUsageRecord>(record),
            Ok(None) => Err(protocol::Error::new(
                ErrorCode::NotFound,
                "Usage record not found",
            )),
            Err(error) => Err(error.into()),
        },
        Command::ExportUsage { query, path } => {
            respond::<rpc::ExportUsage, _>(runtime.export_usage_csv(query, absolute(path)?))
        }
        Command::ClearUsage {} => respond::<rpc::ClearUsage, _>(runtime.clear_usage()),
        Command::GetClientKey {} => respond::<rpc::GetClientKey, _>(runtime.client_key()),
        Command::RotateClientKey {} => {
            respond::<rpc::RotateClientKey, _>(runtime.rotate_client_key())
        }
        Command::SaveLocalApiConfig { config } => {
            respond_state::<rpc::SaveLocalApiConfig, _>(runtime.save_local_api_config(config).await)
        }
        Command::SaveWebUi { config } => {
            respond_state::<rpc::SaveWebUi, _>(runtime.save_web_ui(config).await)
        }
        Command::SetWebUiPassword { password } => {
            respond_state::<rpc::SetWebUiPassword, _>(runtime.set_web_ui_password(password))
        }
        Command::RefreshCatalog {} => {
            respond_state::<rpc::RefreshCatalog, _>(runtime.refresh_catalog().await)
        }
        Command::ListAgents {} => respond::<rpc::ListAgents, _>(runtime.list_agents()),
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
        Command::SetRequireProductionOs { required } => {
            respond_state::<rpc::SetRequireProductionOs, _>(
                runtime.set_require_production_os(required),
            )
        }
        Command::SetAgentConnection { agent_id, connect } => {
            respond::<rpc::SetAgentConnection, _>(runtime.set_agent_connection(agent_id, connect))
        }
        Command::DisconnectAllAgents {} => {
            respond::<rpc::DisconnectAllAgents, _>(runtime.disconnect_all_agents())
        }
        Command::ResetSettings {} => {
            respond_state::<rpc::ResetSettings, _>(runtime.reset_settings().await)
        }
        Command::Settings {} => respond::<rpc::Settings, _>(runtime.settings()),
        Command::SetPreference { change } => {
            respond::<rpc::SetPreference, _>(runtime.set_preference(change))
        }
    }
}

/// A command that answers a state answers it with the protection it presents.
fn respond_state<C: Call<Response = AppStateWire>, E: Into<crate::Error>>(
    result: Result<AppState, E>,
) -> Result<Value, protocol::Error> {
    respond::<C, E>(result.map(AppStateWire::from))
}

fn respond<C: Call, E: Into<crate::Error>>(
    result: Result<C::Response, E>,
) -> Result<Value, protocol::Error> {
    encode::<C>(result.map_err(|error| {
        let error: crate::Error = error.into();
        protocol::Error::from(error)
    })?)
}

fn absolute(path: String) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("Export path must be absolute".into());
    }
    Ok(path)
}
