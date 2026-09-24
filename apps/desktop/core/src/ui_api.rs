//! Shared renderer-facing management API used by the Tauri and Web transports.

use std::{future::Future, sync::Arc, time::Duration};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    agent_access,
    client::Client,
    contracts::{
        AccountBalanceTarget, AccountSaveResult, AgentStatus, AppState, ConfidentialProfileInput,
        ConnectOptions, ListenConfig, ServiceProvider, StartConfig,
    },
    maintenance::ProfileBackup,
    preferences::{Appearance, NotificationPreferences, Preferences, WebUiConfig},
    protocol::{rpc, Call, Command, Preference, RpcError},
    usage::UsageQuery,
};

macro_rules! methods {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Method { $($variant),+ }

        impl Method {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

// This is the only renderer management allowlist. Both transports resolve
// requests through it; platform capabilities decide how host operations work.
methods! {
    StartBackendService => "startBackendService",
    GetState => "getState",
    Start => "start",
    Stop => "stop",
    ActivateProfile => "activateProfile",
    DeleteProfile => "deleteProfile",
    SaveConfiguration => "saveConfiguration",
    CompleteAccountLogin => "completeAccountLogin",
    BeginAccountLogin => "beginAccountLogin",
    PollAccountLogin => "pollAccountLogin",
    SaveAccountLogin => "saveAccountLogin",
    GetAccountDetails => "getAccountDetails",
    GetAccountBalance => "getAccountBalance",
    GetOrganizationUrl => "getOrganizationUrl",
    GetTopUpUrl => "getTopUpUrl",
    CancelAccountLogin => "cancelAccountLogin",
    GetClientKey => "getClientKey",
    RotateClientKey => "rotateClientKey",
    SaveLocalApiConfig => "saveLocalApiConfig",
    SaveWebUi => "saveWebUi",
    ListListenAddresses => "listListenAddresses",
    ImportProfiles => "importProfiles",
    ExportProfilesContent => "exportProfilesContent",
    ExportDiagnosticsContent => "exportDiagnosticsContent",
    QueryUsage => "queryUsage",
    GetUsageRecord => "getUsageRecord",
    ListAgents => "listAgents",
    GetAgentAccess => "getAgentAccess",
    RequestAgentAccess => "requestAgentAccess",
    PreviewAgent => "previewAgent",
    ApplyAgent => "applyAgent",
    GetAppearance => "getAppearance",
    SetAppearance => "setAppearance",
    GetLaunchPreferences => "getLaunchPreferences",
    SetLaunchPreference => "setLaunchPreference",
    GetNotificationSettings => "getNotificationSettings",
    SaveNotificationSettings => "saveNotificationSettings",
    ResetSettings => "resetSettings",
    GetUpdateNotice => "getUpdateNotice",
}

pub const APPEARANCE_EVENT: &str = "pap://appearance";
pub const LAUNCH_PREFERENCES_EVENT: &str = "pap://launch-preferences";
pub const SETTINGS_RESET_EVENT: &str = "pap://settings-reset";
pub const STATE_EVENT: &str = "pap://state";
pub const CLIENT_KEY_CHANGED_EVENT: &str = "pap://client-key-changed";
pub const AGENTS_CHANGED_EVENT: &str = "pap://agents-changed";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub event: &'static str,
    pub payload: Value,
}

impl Event {
    pub fn new(event: &'static str, payload: Value) -> Self {
        Self { event, payload }
    }

    pub fn serialized(event: &'static str, payload: &impl Serialize) -> Result<Self, String> {
        serde_json::to_value(payload)
            .map(|payload| Self::new(event, payload))
            .map_err(|_| "Could not encode the interface event".to_string())
    }
}

pub struct StateEventProjection {
    client_key_revision: u64,
    backend_instance: Option<String>,
}

impl StateEventProjection {
    pub fn new(state: &AppState) -> Self {
        Self {
            client_key_revision: state.client_key_revision,
            backend_instance: state.backend_instance.clone(),
        }
    }

    pub fn project(&mut self, state: &AppState) -> Vec<Event> {
        let mut events = Vec::with_capacity(2);
        if state.client_key_revision != self.client_key_revision
            || state.backend_instance != self.backend_instance
        {
            self.client_key_revision = state.client_key_revision;
            self.backend_instance = state.backend_instance.clone();
            events.push(Event::new(
                CLIENT_KEY_CHANGED_EVENT,
                json!(state.client_key_available.unwrap_or(true)),
            ));
        }
        events.push(Event::new(
            STATE_EVENT,
            serde_json::to_value(state).unwrap_or(Value::Null),
        ));
        events
    }
}

#[derive(Clone, Debug)]
pub enum Error {
    InvalidRequest,
    Operation(String),
}

impl Error {
    pub fn message(self) -> String {
        match self {
            Self::InvalidRequest => "Invalid management request".into(),
            Self::Operation(message) => message,
        }
    }

    pub fn rpc(&self) -> RpcError {
        match self {
            Self::InvalidRequest => RpcError::new("invalid_request", "Invalid management request"),
            // Client messages were already sanitized by the backend or written locally;
            // keep them identical to the Tauri transport.
            Self::Operation(message) => RpcError::new("operation_failed", message),
        }
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Operation(message)
    }
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPreferences {
    pub open_at_login: bool,
    pub connect_on_launch: bool,
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ListenAddress {
    pub address: String,
    pub name: String,
}

/// Executes protocol commands. The desktop shell reaches the service over the
/// authenticated IPC endpoint; the service-hosted web UI calls the same
/// admission and dispatch path in process.
pub trait Backend: Clone + Send + Sync + 'static {
    fn execute(&self, command: Command) -> impl Future<Output = Result<Value, String>> + Send;

    fn ensure_running(&self) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }

    /// The last known state when the backend cannot be reached.
    fn disconnected_state(&self) -> Option<AppState> {
        None
    }
}

impl Backend for Arc<Client> {
    async fn execute(&self, command: Command) -> Result<Value, String> {
        let client = self.clone();
        blocking(move || client.forward(command)).await
    }

    async fn ensure_running(&self) -> Result<(), String> {
        blocking(Client::ensure_service).await
    }

    fn disconnected_state(&self) -> Option<AppState> {
        let cached = self.cached_state();
        (cached.backend_connected == Some(false)).then_some(cached)
    }
}

/// Sends one typed request and decodes exactly its declared response.
pub async fn call<C: Call>(backend: &impl Backend, request: C) -> Result<C::Response, String> {
    serde_json::from_value(backend.execute(request.into()).await?)
        .map_err(|_| "Management response failed".to_string())
}

pub trait Host: Clone + Send + Sync + 'static {
    fn emit(&self, _event: Event) -> Result<(), String> {
        Ok(())
    }

    fn apply_appearance(&self, _appearance: Appearance) -> Result<(), String> {
        Ok(())
    }

    /// May block; callers run it on the blocking pool.
    fn open_at_login(&self) -> Result<bool, String> {
        Ok(false)
    }

    /// May block; callers run it on the blocking pool.
    fn set_open_at_login(&self, _enabled: bool) -> Result<(), String> {
        Err("Open at Login is unavailable in this interface".into())
    }

    fn present_account_login(&self, _url: &str) {}

    fn sync_agents(&self, _agents: &[AgentStatus]) {}

    fn notification_configuration(
        &self,
        preferences: NotificationPreferences,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        async move {
            Ok(json!({
                "preferences": preferences,
                "permission": "unsupported",
                "alertsEnabled": false
            }))
        }
    }

    fn notification_preferences_saved(
        &self,
        _preferences: NotificationPreferences,
    ) -> Result<(), String> {
        Ok(())
    }

    fn reset_settings(
        &self,
        backend: &impl Backend,
    ) -> impl Future<Output = Result<AppState, String>> + Send {
        call(backend, rpc::ResetSettings)
    }

    fn request_agent_access(&self) -> impl Future<Output = Result<Value, String>> + Send {
        async { value(agent_access::status()) }
    }
}

pub async fn invoke(
    backend: &impl Backend,
    host: &impl Host,
    method: Method,
    input: Value,
) -> Result<Value, Error> {
    let command = match method {
        Method::StartBackendService => {
            backend.ensure_running().await?;
            Command::State
        }
        Method::GetState => {
            return match backend.execute(Command::State).await {
                Ok(state) => Ok(state),
                Err(error) => match backend.disconnected_state() {
                    Some(cached) => Ok(value(cached)?),
                    None => Err(error.into()),
                },
            };
        }
        Method::Start => Command::Start(params::<StartParams>(input)?.config),
        Method::Stop => Command::Stop,
        Method::ActivateProfile => Command::ActivateProfile {
            profile_id: params::<ProfileIdParams>(input)?.profile_id,
        },
        Method::DeleteProfile => Command::DeleteProfile {
            profile_id: params::<ProfileIdParams>(input)?.profile_id,
        },
        Method::SaveConfiguration => {
            let input: SaveConfigurationParams = params(input)?;
            Command::SaveConfiguration {
                profile: input.profile,
                require_production_os: input.require_production_os,
                key: input.key,
            }
        }
        Method::CompleteAccountLogin => {
            let input: CompleteLoginParams = params(input)?;
            Command::CompleteAccountLogin {
                id: input.id,
                callback_url: input.callback_url,
            }
        }
        Method::BeginAccountLogin => {
            let profile = params::<BeginLoginParams>(input)?.profile;
            let login = call(backend, rpc::BeginAccountLogin { profile }).await?;
            host.present_account_login(&login.url);
            return Ok(value(login)?);
        }
        Method::PollAccountLogin => Command::PollAccountLogin {
            id: params::<LoginIdParams>(input)?.id,
        },
        Method::SaveAccountLogin => {
            let input: SaveLoginParams = params(input)?;
            return Ok(value(save_account_login(backend, input).await?)?);
        }
        Method::GetAccountDetails => Command::AccountDetails {
            profile_id: params::<ProfileIdParams>(input)?.profile_id,
        },
        Method::GetAccountBalance => Command::AccountBalance {
            target: params::<BalanceParams>(input)?.target,
        },
        Method::GetOrganizationUrl => {
            let input: OrganizationParams = params(input)?;
            return Ok(value(crate::account::organization_url(Some(
                &input.organization_slug,
            ))?)?);
        }
        Method::GetTopUpUrl => {
            let input: TopUpParams = params(input)?;
            return Ok(value(crate::account::top_up_url(
                &input.provider,
                input.scope_slug.as_deref(),
            )?)?);
        }
        Method::CancelAccountLogin => Command::CancelAccountLogin {
            id: params::<LoginIdParams>(input)?.id,
        },
        Method::GetClientKey => Command::ClientKey,
        Method::RotateClientKey => Command::RotateClientKey,
        Method::SaveLocalApiConfig => {
            Command::SaveLocalApi(params::<LocalApiParams>(input)?.config)
        }
        Method::SaveWebUi => Command::SaveWebUi(params::<WebUiParams>(input)?.config),
        Method::ListListenAddresses => return Ok(value(blocking(list_listen_addresses).await?)?),
        Method::ImportProfiles => Command::ImportProfiles(params::<ImportParams>(input)?.backup),
        Method::ExportProfilesContent => Command::ExportProfilesContent,
        Method::ExportDiagnosticsContent => Command::ExportDiagnosticsContent,
        Method::QueryUsage => Command::Usage(params::<UsageParams>(input)?.query),
        Method::GetUsageRecord => {
            let record_id = params::<UsageRecordParams>(input)?.record_id;
            let record = call(backend, rpc::UsageRecord { record_id }).await?;
            return Ok(value(
                record.ok_or_else(|| "Usage record not found".to_string())?,
            )?);
        }
        Method::ListAgents => {
            let agents = call(backend, rpc::Agents).await?;
            host.sync_agents(&agents);
            return Ok(value(agents)?);
        }
        Method::GetAgentAccess => return Ok(value(agent_access::status())?),
        Method::RequestAgentAccess => return Ok(host.request_agent_access().await?),
        Method::PreviewAgent => {
            let input: AgentChangeParams = params(input)?;
            Command::PreviewAgent {
                agent_id: input.agent_id,
                connect: input.connect,
                options: input.options,
            }
        }
        Method::ApplyAgent => {
            let input: AgentChangeParams = params(input)?;
            Command::ApplyAgent {
                agent_id: input.agent_id,
                connect: input.connect,
                revision: input.revision.ok_or(Error::InvalidRequest)?,
                options: input.options,
            }
        }
        Method::GetAppearance => {
            return Ok(value(preferences(backend).await?.appearance)?);
        }
        Method::SetAppearance => {
            let appearance = params::<AppearanceParams>(input)?.appearance;
            set_preference(backend, Preference::Appearance(appearance)).await?;
            host.apply_appearance(appearance)?;
            host.emit(Event::serialized(APPEARANCE_EVENT, &appearance)?)
                .map_err(|_| "Could not sync appearance".to_string())?;
            return Ok(Value::Null);
        }
        Method::GetLaunchPreferences => {
            return Ok(value(launch_preferences(backend, host).await?)?);
        }
        Method::GetUpdateNotice => {
            return Ok(value(crate::updates::check_installation().await?)?);
        }
        Method::SetLaunchPreference => {
            let input: LaunchPreferenceParams = params(input)?;
            match input.name.as_str() {
                "openAtLogin" => {
                    let host = host.clone();
                    blocking(move || host.set_open_at_login(input.enabled)).await?;
                }
                "connectOnLaunch" => {
                    set_preference(backend, Preference::ConnectOnLaunch(input.enabled)).await?;
                }
                _ => return Err("Unknown startup preference".to_string().into()),
            }
            let preferences = launch_preferences(backend, host).await?;
            if let Ok(event) = Event::serialized(LAUNCH_PREFERENCES_EVENT, &preferences) {
                let _ = host.emit(event);
            }
            return Ok(value(preferences)?);
        }
        Method::GetNotificationSettings => {
            let preferences = preferences(backend).await?.notifications;
            return Ok(host.notification_configuration(preferences).await?);
        }
        Method::SaveNotificationSettings => {
            let preferences = params::<NotificationsParams>(input)?.config;
            set_preference(backend, Preference::Notifications(preferences)).await?;
            host.notification_preferences_saved(preferences)?;
            return Ok(Value::Null);
        }
        Method::ResetSettings => {
            let result = host.reset_settings(backend).await;
            // A partial reset may still have changed preferences.
            if let Err(error) = refresh_preferences(backend, host).await {
                crate::diagnostic(format_args!(
                    "Cannot refresh preferences after reset: {}",
                    error.message()
                ));
            }
            let state = result?;
            host.emit(Event::new(SETTINGS_RESET_EVENT, Value::Null))?;
            return Ok(value(state)?);
        }
    };
    Ok(backend.execute(command).await?)
}

/// Save can outlive one request; poll its operation until it settles.
async fn save_account_login(
    backend: &impl Backend,
    input: SaveLoginParams,
) -> Result<AppState, String> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let result = |operation_id: String| rpc::AccountSaveResult { operation_id };
    let initial = call(
        backend,
        rpc::SaveAccountLogin {
            operation_id: operation_id.clone(),
            id: input.id,
            profile: input.profile,
            require_production_os: input.require_production_os,
            workspace_id: input.workspace_id,
        },
    )
    .await;
    let mut outcome = match initial {
        Ok(outcome) => outcome,
        Err(_) => call(backend, result(operation_id.clone())).await?,
    };
    loop {
        match outcome {
            AccountSaveResult::Complete { state } => return Ok(*state),
            AccountSaveResult::Failed { error } => return Err(error),
            AccountSaveResult::Running => tokio::time::sleep(Duration::from_millis(500)).await,
        }
        outcome = call(backend, result(operation_id.clone())).await.map_err(|_| {
            "Account: Save outcome is not yet confirmed. Reconnect to the backend and check the profile before retrying.".to_string()
        })?;
    }
}

async fn preferences(backend: &impl Backend) -> Result<Preferences, String> {
    call(backend, rpc::Preferences).await
}

async fn set_preference(backend: &impl Backend, change: Preference) -> Result<(), String> {
    call(backend, rpc::SetPreference { change })
        .await
        .map(|_| ())
}

/// Appearance and startup preference events for a (re)connected interface.
pub async fn preference_events(
    backend: &impl Backend,
    host: &impl Host,
) -> Result<(Preferences, Vec<Event>), String> {
    let preferences = preferences(backend).await?;
    let launch = launch_preferences(backend, host).await?;
    let events = vec![
        Event::serialized(APPEARANCE_EVENT, &preferences.appearance)?,
        Event::serialized(LAUNCH_PREFERENCES_EVENT, &launch)?,
    ];
    Ok((preferences, events))
}

pub async fn refresh_preferences(backend: &impl Backend, host: &impl Host) -> Result<(), Error> {
    if backend.disconnected_state().is_some() {
        return Ok(());
    }
    let (preferences, events) = preference_events(backend, host).await?;
    host.notification_preferences_saved(preferences.notifications)?;
    host.apply_appearance(preferences.appearance)?;
    for event in events {
        let _ = host.emit(event);
    }
    Ok(())
}

pub async fn launch_preferences(
    backend: &impl Backend,
    host: &impl Host,
) -> Result<LaunchPreferences, String> {
    let reader = host.clone();
    let open_at_login = blocking(move || reader.open_at_login()).await?;
    Ok(LaunchPreferences {
        open_at_login,
        connect_on_launch: preferences(backend).await?.connect_on_launch,
    })
}

pub async fn blocking<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| "The background operation could not complete. Please try again.".to_string())?
}

fn value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "Management response failed".to_string())
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::InvalidRequest)
}

fn list_listen_addresses() -> Result<Vec<ListenAddress>, String> {
    let interfaces = if_addrs::get_if_addrs().map_err(|_| {
        "Could not read network interfaces. Enter an IP address manually.".to_string()
    })?;
    let mut addresses: Vec<_> = interfaces
        .into_iter()
        .filter(|interface| interface.is_oper_up())
        .filter(|interface| {
            !matches!(interface.ip(), std::net::IpAddr::V6(ip) if ip.is_unicast_link_local())
        })
        .map(|interface| ListenAddress {
            address: interface.ip().to_string(),
            name: interface.name,
        })
        .collect();
    addresses.sort_by(|left, right| {
        left.address
            .cmp(&right.address)
            .then(left.name.cmp(&right.name))
    });
    addresses.dedup_by(|left, right| left.address == right.address);
    Ok(addresses)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppearanceParams {
    appearance: Appearance,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LaunchPreferenceParams {
    name: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartParams {
    config: StartConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProfileIdParams {
    profile_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveConfigurationParams {
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoginIdParams {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteLoginParams {
    id: String,
    callback_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginLoginParams {
    profile: ConfidentialProfileInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveLoginParams {
    id: String,
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    workspace_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BalanceParams {
    target: AccountBalanceTarget,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrganizationParams {
    organization_slug: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TopUpParams {
    provider: ServiceProvider,
    scope_slug: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalApiParams {
    config: ListenConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebUiParams {
    config: WebUiConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportParams {
    backup: ProfileBackup,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotificationsParams {
    config: NotificationPreferences,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UsageParams {
    query: UsageQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UsageRecordParams {
    record_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentChangeParams {
    agent_id: String,
    connect: bool,
    options: ConnectOptions,
    revision: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transports_report_the_same_operation_message() {
        let message = "invalid_state: Stop protection before deleting the active profile";
        let error = Error::Operation(message.into());
        assert_eq!(error.rpc().message, message);
        assert_eq!(error.message(), message);
        assert_eq!(
            Error::InvalidRequest.rpc().message,
            Error::InvalidRequest.message()
        );
    }
}
