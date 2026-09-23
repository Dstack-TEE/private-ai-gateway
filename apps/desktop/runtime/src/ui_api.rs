//! Shared renderer-facing management API used by the Tauri and Web transports.

use std::sync::Arc;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    agent_access,
    client::Client,
    contracts::{
        AccountBalanceTarget, AgentStatus, ConfidentialProfileInput, ConnectOptions, GatewayState,
        LocalApiConfig, ServiceProvider, StartGatewayConfig,
    },
    maintenance::ProfileBackup,
    preferences::{Appearance, NotificationPreferences},
    protocol::{Preference, RpcError},
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
}

pub const APPEARANCE_EVENT: &str = "gateway://appearance";
pub const LAUNCH_PREFERENCES_EVENT: &str = "gateway://launch-preferences";
pub const SETTINGS_RESET_EVENT: &str = "gateway://settings-reset";
pub const STATE_EVENT: &str = "gateway://state";
pub const CLIENT_KEY_CHANGED_EVENT: &str = "gateway://client-key-changed";
pub const AGENTS_CHANGED_EVENT: &str = "gateway://agents-changed";

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
    pub fn new(state: &GatewayState) -> Self {
        Self {
            client_key_revision: state.client_key_revision,
            backend_instance: state.backend_instance.clone(),
        }
    }

    pub fn project(&mut self, state: &GatewayState) -> Vec<Event> {
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
    Internal(&'static str),
}

impl Error {
    pub fn message(self) -> String {
        match self {
            Self::InvalidRequest => "Invalid management request".into(),
            Self::Operation(message) => message,
            Self::Internal(message) => message.into(),
        }
    }

    pub fn rpc(&self) -> RpcError {
        match self {
            Self::InvalidRequest => RpcError::new("invalid_request", "Invalid management request"),
            Self::Operation(message) => RpcError::operation(message),
            Self::Internal(message) => RpcError::new("internal_error", message),
        }
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Operation(message)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPreferences {
    pub open_at_login: bool,
    pub connect_on_launch: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenAddress {
    pub address: String,
    pub name: String,
}

#[allow(async_fn_in_trait)]
pub trait Host: Clone + Send + Sync + 'static {
    fn emit(&self, _event: Event) -> Result<(), String> {
        Ok(())
    }

    fn apply_appearance(&self, _appearance: Appearance) -> Result<(), String> {
        Ok(())
    }

    fn open_at_login(&self) -> Result<bool, String> {
        Ok(false)
    }

    fn set_open_at_login(&self, _enabled: bool) -> Result<(), String> {
        Err("Open at Login is unavailable in this interface".into())
    }

    fn present_account_login(&self, _url: &str) {}

    fn sync_agents(&self, _agents: &[AgentStatus]) {}

    async fn notification_configuration(
        &self,
        preferences: NotificationPreferences,
    ) -> Result<Value, String> {
        Ok(json!({
            "preferences": preferences,
            "permission": "unsupported",
            "alertsEnabled": false
        }))
    }

    fn notification_preferences_saved(
        &self,
        _preferences: NotificationPreferences,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn reset_settings(&self, client: Arc<Client>) -> Result<GatewayState, String> {
        blocking(move || client.reset_settings()).await
    }

    async fn request_agent_access(&self, _client: Arc<Client>) -> Result<Value, String> {
        value(agent_access::status())
    }
}

pub async fn invoke<H: Host>(
    client: Arc<Client>,
    host: H,
    method: Method,
    input: Value,
) -> Result<Value, Error> {
    match method {
        Method::StartBackendService => {
            blocking_value(move || {
                Client::ensure_service()?;
                client.state()
            })
            .await
        }
        Method::GetState => blocking_value(move || client.state_or_cached()).await,
        Method::Start => {
            let input: StartParams = params(input)?;
            blocking_value(move || client.start(input.config)).await
        }
        Method::Stop => blocking_value(move || client.stop()).await,
        Method::ActivateProfile => {
            let input: ProfileIdParams = params(input)?;
            blocking_value(move || client.activate_profile(input.profile_id)).await
        }
        Method::DeleteProfile => {
            let input: ProfileIdParams = params(input)?;
            blocking_value(move || client.delete_profile(input.profile_id)).await
        }
        Method::SaveConfiguration => {
            let input: SaveConfigurationParams = params(input)?;
            result_value(
                client
                    .save_configuration(input.profile, input.require_production_os, input.key)
                    .await,
            )
        }
        Method::CompleteAccountLogin => {
            let input: CompleteLoginParams = params(input)?;
            result_value(
                client
                    .complete_account_login(input.id, input.callback_url)
                    .await,
            )
        }
        Method::BeginAccountLogin => {
            let input: BeginLoginParams = params(input)?;
            let login = client
                .begin_account_login(input.profile)
                .await
                .map_err(Error::Operation)?;
            host.present_account_login(&login.url);
            value(login).map_err(Error::Operation)
        }
        Method::PollAccountLogin => {
            let input: LoginIdParams = params(input)?;
            result_value(client.poll_account_login(input.id).await)
        }
        Method::SaveAccountLogin => {
            let input: SaveLoginParams = params(input)?;
            result_value(
                client
                    .save_account_login(
                        input.id,
                        input.profile,
                        input.require_production_os,
                        input.workspace_id,
                    )
                    .await,
            )
        }
        Method::GetAccountDetails => {
            let input: ProfileIdParams = params(input)?;
            result_value(client.account_details(input.profile_id).await)
        }
        Method::GetAccountBalance => {
            let input: BalanceParams = params(input)?;
            result_value(client.account_balance(input.target).await)
        }
        Method::GetOrganizationUrl => {
            let input: OrganizationParams = params(input)?;
            result_value(crate::account_login::organization_url(Some(
                &input.organization_slug,
            )))
        }
        Method::GetTopUpUrl => {
            let input: TopUpParams = params(input)?;
            result_value(crate::account_login::top_up_url(
                &input.provider,
                input.scope_slug.as_deref(),
            ))
        }
        Method::CancelAccountLogin => {
            let input: LoginIdParams = params(input)?;
            result_value(client.cancel_account_login(input.id).await)
        }
        Method::GetClientKey => blocking_value(move || client.client_key()).await,
        Method::RotateClientKey => blocking_value(move || client.rotate_client_key()).await,
        Method::SaveLocalApiConfig => {
            let input: LocalApiParams = params(input)?;
            result_value(client.save_local_api_config(input.config).await)
        }
        Method::ListListenAddresses => blocking_value(list_listen_addresses).await,
        Method::ImportProfiles => {
            let input: ImportParams = params(input)?;
            blocking_value(move || client.import_profiles(input.backup)).await
        }
        Method::ExportProfilesContent => {
            blocking_value(move || client.export_profiles_content()).await
        }
        Method::ExportDiagnosticsContent => {
            blocking_value(move || client.export_diagnostics_content()).await
        }
        Method::QueryUsage => {
            let input: UsageParams = params(input)?;
            blocking_value(move || client.query_usage(input.query)).await
        }
        Method::GetUsageRecord => {
            let input: UsageRecordParams = params(input)?;
            blocking_value(move || {
                client
                    .usage_record(&input.record_id)?
                    .ok_or_else(|| "Usage record not found".to_string())
            })
            .await
        }
        Method::ListAgents => {
            let agents: Vec<AgentStatus> = blocking(move || client.list_agents())
                .await
                .map_err(Error::Operation)?;
            host.sync_agents(&agents);
            value(agents).map_err(Error::Operation)
        }
        Method::GetAgentAccess => value(agent_access::status()).map_err(Error::Operation),
        Method::RequestAgentAccess => host
            .request_agent_access(client)
            .await
            .map_err(Error::Operation),
        Method::PreviewAgent => {
            let input: AgentChangeParams = params(input)?;
            blocking_value(move || {
                client.preview_agent(input.agent_id, input.connect, input.options)
            })
            .await
        }
        Method::ApplyAgent => {
            let input: AgentChangeParams = params(input)?;
            let revision = input.revision.ok_or(Error::InvalidRequest)?;
            blocking_value(move || {
                client.apply_agent(input.agent_id, input.connect, revision, input.options)
            })
            .await
        }
        Method::GetAppearance => blocking_value(move || Ok(client.preferences()?.appearance)).await,
        Method::SetAppearance => {
            let input: AppearanceParams = params(input)?;
            let appearance = input.appearance;
            blocking(move || {
                client.set_preference(Preference::Appearance(appearance))?;
                Ok(())
            })
            .await
            .map_err(Error::Operation)?;
            host.apply_appearance(appearance)
                .map_err(Error::Operation)?;
            host.emit(Event::serialized(APPEARANCE_EVENT, &appearance).map_err(Error::Operation)?)
                .map_err(Error::Operation)?;
            value(()).map_err(Error::Operation)
        }
        Method::GetLaunchPreferences => launch_preferences(client, &host).await,
        Method::SetLaunchPreference => {
            let input: LaunchPreferenceParams = params(input)?;
            match input.name.as_str() {
                "openAtLogin" => host
                    .set_open_at_login(input.enabled)
                    .map_err(Error::Operation)?,
                "connectOnLaunch" => {
                    let writer = client.clone();
                    blocking(move || {
                        writer.set_preference(Preference::ConnectOnLaunch(input.enabled))?;
                        Ok(())
                    })
                    .await
                    .map_err(Error::Operation)?;
                }
                _ => return Err(Error::InvalidRequest),
            }
            let preferences = launch_preferences_value(&client, &host).await?;
            host.emit(
                Event::serialized(LAUNCH_PREFERENCES_EVENT, &preferences)
                    .map_err(Error::Operation)?,
            )
            .map_err(Error::Operation)?;
            value(preferences).map_err(Error::Operation)
        }
        Method::GetNotificationSettings => {
            let preferences = blocking(move || Ok(client.preferences()?.notifications))
                .await
                .map_err(Error::Operation)?;
            host.notification_configuration(preferences)
                .await
                .map_err(Error::Operation)
        }
        Method::SaveNotificationSettings => {
            let input: NotificationsParams = params(input)?;
            let preferences = input.config;
            blocking(move || {
                client.set_preference(Preference::Notifications(preferences))?;
                Ok(())
            })
            .await
            .map_err(Error::Operation)?;
            host.notification_preferences_saved(preferences)
                .map_err(Error::Operation)?;
            value(()).map_err(Error::Operation)
        }
        Method::ResetSettings => {
            let state = host
                .reset_settings(client.clone())
                .await
                .map_err(Error::Operation)?;
            refresh_preferences(client, &host).await?;
            host.emit(Event::new(SETTINGS_RESET_EVENT, Value::Null))
                .map_err(Error::Operation)?;
            value(state).map_err(Error::Operation)
        }
    }
}

pub async fn refresh_preferences<H: Host>(client: Arc<Client>, host: &H) -> Result<(), Error> {
    if client.cached_state().backend_connected == Some(false) {
        return Ok(());
    }
    let preferences = blocking({
        let client = client.clone();
        move || client.preferences()
    })
    .await
    .map_err(Error::Operation)?;
    let launch = launch_preferences_value(&client, host).await?;
    host.notification_preferences_saved(preferences.notifications)
        .map_err(Error::Operation)?;
    host.apply_appearance(preferences.appearance)
        .map_err(Error::Operation)?;
    host.emit(
        Event::serialized(APPEARANCE_EVENT, &preferences.appearance).map_err(Error::Operation)?,
    )
    .map_err(Error::Operation)?;
    host.emit(Event::serialized(LAUNCH_PREFERENCES_EVENT, &launch).map_err(Error::Operation)?)
        .map_err(Error::Operation)
}

async fn launch_preferences<H: Host>(client: Arc<Client>, host: &H) -> Result<Value, Error> {
    let preferences = launch_preferences_value(&client, host).await?;
    value(preferences).map_err(Error::Operation)
}

async fn launch_preferences_value<H: Host>(
    client: &Arc<Client>,
    host: &H,
) -> Result<LaunchPreferences, Error> {
    let open_at_login = host.open_at_login().map_err(Error::Operation)?;
    let client = client.clone();
    let connect_on_launch = blocking(move || Ok(client.preferences()?.connect_on_launch))
        .await
        .map_err(Error::Operation)?;
    Ok(LaunchPreferences {
        open_at_login,
        connect_on_launch,
    })
}

async fn blocking_value<T, F>(operation: F) -> Result<Value, Error>
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let result = blocking(operation).await.map_err(Error::Operation)?;
    value(result).map_err(Error::Operation)
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

fn result_value<T: Serialize>(result: Result<T, String>) -> Result<Value, Error> {
    result
        .map_err(Error::Operation)
        .and_then(|result| value(result).map_err(Error::Operation))
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
    config: StartGatewayConfig,
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
    config: LocalApiConfig,
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
