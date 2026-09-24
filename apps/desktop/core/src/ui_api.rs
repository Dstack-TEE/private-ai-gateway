//! Shared renderer-facing management API used by the Tauri and Web transports.

use std::{future::Future, sync::Arc, time::Duration};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    agent_access,
    client::{CallError, Client},
    config::{Appearance, Config, NotificationPreferences},
    contracts::{
        AccountSaveResult, AgentStatus, AppState, ConfidentialProfileInput, ServiceProvider,
    },
    protocol::{self, rpc, Call, Command, Preference},
};

const TASK_FAILED: &str = "The background operation could not complete. Please try again.";

macro_rules! methods {
    (
        commands { $($command:ident => $command_name:literal),+ $(,)? }
        host { $($host:ident => $host_name:literal),+ $(,)? }
    ) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Method { $($command,)+ $($host,)+ }

        impl Method {
            pub const ALL: &'static [Self] = &[$(Self::$command,)+ $(Self::$host,)+];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$command => $command_name,)+
                    $(Self::$host => $host_name,)+
                }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($command_name => Some(Self::$command),)+
                    $($host_name => Some(Self::$host),)+
                    _ => None,
                }
            }

            /// Whether the method is the management command of the same name
            /// and parameters; the host may add effects around it.
            pub const fn is_command(self) -> bool {
                matches!(self, $(Self::$command)|+)
            }
        }
    };
}

// This is the only renderer management allowlist: Tauri command names, web
// RPC paths and the methods a web UI session may call. A method either is the
// command of the same name or is composed by the `Host` from commands and
// local data; a host method that shares a command's name takes the same
// parameters and answers the same result.
methods! {
    commands {
        GetState => "get_state",
        Start => "start",
        Stop => "stop",
        ActivateProfile => "activate_profile",
        DeleteProfile => "delete_profile",
        SaveConfiguration => "save_configuration",
        CompleteAccountLogin => "complete_account_login",
        BeginAccountLogin => "begin_account_login",
        PollAccountLogin => "poll_account_login",
        GetAccountDetails => "get_account_details",
        GetAccountBalance => "get_account_balance",
        CancelAccountLogin => "cancel_account_login",
        GetClientKey => "get_client_key",
        RotateClientKey => "rotate_client_key",
        SaveLocalApiConfig => "save_local_api_config",
        SaveWebUi => "save_web_ui",
        SetWebUiPassword => "set_web_ui_password",
        ImportProfiles => "import_profiles",
        ExportProfilesContent => "export_profiles_content",
        ExportDiagnosticsContent => "export_diagnostics_content",
        QueryUsage => "query_usage",
        GetUsageRecord => "get_usage_record",
        ListAgents => "list_agents",
        PreviewAgent => "preview_agent",
        ApplyAgent => "apply_agent",
    }
    host {
        StartBackendService => "start_backend_service",
        SaveAccountLogin => "save_account_login",
        GetOrganizationUrl => "get_organization_url",
        GetTopUpUrl => "get_top_up_url",
        ListListenAddresses => "list_listen_addresses",
        GetAgentAccess => "get_agent_access",
        RequestAgentAccess => "request_agent_access",
        GetAppearance => "get_appearance",
        SetAppearance => "set_appearance",
        GetLaunchPreferences => "get_launch_preferences",
        SetLaunchPreference => "set_launch_preference",
        GetNotificationSettings => "get_notification_settings",
        SaveNotificationSettings => "save_notification_settings",
        ResetSettings => "reset_settings",
        GetUpdateNotice => "get_update_notice",
    }
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
    settings_revision: u64,
}

impl StateEventProjection {
    pub fn new(state: &AppState) -> Self {
        Self {
            client_key_revision: state.client_key_revision,
            backend_instance: state.backend_instance.clone(),
            settings_revision: state.config_files.revision,
        }
    }

    /// Whether applied settings changed since the last call, for example by an
    /// edit of `config.toml`; the host then reapplies preferences.
    pub fn settings_changed(&mut self, state: &AppState) -> bool {
        let changed = state.config_files.revision != self.settings_revision;
        self.settings_revision = state.config_files.revision;
        changed
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

/// Executes management commands. The desktop shell sends them to the
/// service's API; the service answers them through its own admission and
/// dispatch.
pub trait Backend: Clone + Send + Sync + 'static {
    fn execute(&self, command: Command) -> impl Future<Output = Result<Value, CallError>> + Send;

    fn ensure_running(&self) -> impl Future<Output = Result<(), CallError>> + Send {
        async { Ok(()) }
    }

    /// The last known state when the backend cannot be reached.
    fn disconnected_state(&self) -> Option<AppState> {
        None
    }
}

impl Backend for Arc<Client> {
    async fn execute(&self, command: Command) -> Result<Value, CallError> {
        let client = self.clone();
        tokio::task::spawn_blocking(move || Client::execute(&client, command))
            .await
            .map_err(|_| CallError::Local(TASK_FAILED.into()))?
    }

    async fn ensure_running(&self) -> Result<(), CallError> {
        Ok(blocking(Client::ensure_service).await?)
    }

    fn disconnected_state(&self) -> Option<AppState> {
        let cached = self.cached_state();
        (cached.backend_connected == Some(false)).then_some(cached)
    }
}

/// Sends one typed request and decodes exactly its declared response.
pub async fn call<C: Call>(backend: &impl Backend, request: C) -> Result<C::Response, CallError> {
    serde_json::from_value(backend.execute(request.into()).await?)
        .map_err(|_| CallError::Local("Management response failed".into()))
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
    ) -> impl Future<Output = Result<AppState, CallError>> + Send {
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
) -> Result<Value, CallError> {
    match method {
        Method::GetState => match backend.execute(Command::GetState {}).await {
            Ok(state) => Ok(state),
            Err(error) => match backend.disconnected_state() {
                Some(cached) => Ok(value(cached)?),
                None => Err(error),
            },
        },
        Method::BeginAccountLogin => {
            let profile = params::<BeginLoginParams>(input)?.profile;
            let login = call(backend, rpc::BeginAccountLogin { profile }).await?;
            host.present_account_login(&login.url);
            Ok(value(login)?)
        }
        Method::ListAgents => {
            let agents = call(backend, rpc::ListAgents).await?;
            host.sync_agents(&agents);
            Ok(value(agents)?)
        }
        method if method.is_command() => {
            backend
                .execute(Command::decode(method.name(), input).map_err(CallError::Api)?)
                .await
        }
        Method::StartBackendService => {
            backend.ensure_running().await?;
            backend.execute(Command::GetState {}).await
        }
        Method::SaveAccountLogin => {
            let input: SaveLoginParams = params(input)?;
            Ok(value(save_account_login(backend, input).await?)?)
        }
        Method::GetOrganizationUrl => {
            let input: OrganizationParams = params(input)?;
            Ok(value(crate::account::organization_url(Some(
                &input.organization_slug,
            ))?)?)
        }
        Method::GetTopUpUrl => {
            let input: TopUpParams = params(input)?;
            Ok(value(crate::account::top_up_url(
                &input.provider,
                input.scope_slug.as_deref(),
            )?)?)
        }
        Method::ListListenAddresses => Ok(value(blocking(list_listen_addresses).await?)?),
        Method::GetAgentAccess => Ok(value(agent_access::status())?),
        Method::RequestAgentAccess => Ok(host.request_agent_access().await?),
        Method::GetAppearance => Ok(value(preferences(backend).await?.appearance)?),
        Method::SetAppearance => {
            let appearance = params::<AppearanceParams>(input)?.appearance;
            set_preference(backend, Preference::Appearance(appearance)).await?;
            host.apply_appearance(appearance)?;
            host.emit(Event::serialized(APPEARANCE_EVENT, &appearance)?)
                .map_err(|_| "Could not sync appearance".to_string())?;
            Ok(Value::Null)
        }
        Method::GetLaunchPreferences => Ok(value(launch_preferences(backend, host).await?)?),
        Method::GetUpdateNotice => Ok(value(crate::updates::check_installation().await?)?),
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
                _ => return Err("Unknown startup preference".into()),
            }
            let preferences = launch_preferences(backend, host).await?;
            if let Ok(event) = Event::serialized(LAUNCH_PREFERENCES_EVENT, &preferences) {
                let _ = host.emit(event);
            }
            Ok(value(preferences)?)
        }
        Method::GetNotificationSettings => {
            let preferences = preferences(backend).await?.notifications;
            Ok(host.notification_configuration(preferences).await?)
        }
        Method::SaveNotificationSettings => {
            let preferences = params::<NotificationsParams>(input)?.config;
            set_preference(backend, Preference::Notifications(preferences)).await?;
            host.notification_preferences_saved(preferences)?;
            Ok(Value::Null)
        }
        Method::ResetSettings => {
            let result = host.reset_settings(backend).await;
            // A partial reset may still have changed preferences.
            if let Err(error) = refresh_preferences(backend, host).await {
                tracing::warn!("Cannot refresh preferences after reset: {error}");
            }
            let state = result?;
            host.emit(Event::new(SETTINGS_RESET_EVENT, Value::Null))?;
            Ok(value(state)?)
        }
        _ => Err(CallError::Api(protocol::Error::method_not_found())),
    }
}

/// Save can outlive one request; poll its operation until it settles.
async fn save_account_login(
    backend: &impl Backend,
    input: SaveLoginParams,
) -> Result<AppState, CallError> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let result = |operation_id: String| rpc::AccountSaveResult { operation_id };
    let initial = call(
        backend,
        rpc::BeginAccountSave {
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
            AccountSaveResult::Failed { error } => return Err(CallError::Local(error)),
            AccountSaveResult::Running => tokio::time::sleep(Duration::from_millis(500)).await,
        }
        outcome = call(backend, result(operation_id.clone())).await.map_err(|_| {
            "Account: Save outcome is not yet confirmed. Reconnect to the backend and check the profile before retrying.".to_string()
        })?;
    }
}

async fn preferences(backend: &impl Backend) -> Result<Config, CallError> {
    call(backend, rpc::Settings).await
}

async fn set_preference(backend: &impl Backend, change: Preference) -> Result<(), CallError> {
    call(backend, rpc::SetPreference { change })
        .await
        .map(|_| ())
}

/// Appearance and startup preference events for a (re)connected interface.
pub async fn preference_events(
    backend: &impl Backend,
    host: &impl Host,
) -> Result<(Config, Vec<Event>), CallError> {
    let preferences = preferences(backend).await?;
    let launch = launch_preferences(backend, host).await?;
    let events = vec![
        Event::serialized(APPEARANCE_EVENT, &preferences.appearance)?,
        Event::serialized(LAUNCH_PREFERENCES_EVENT, &launch)?,
    ];
    Ok((preferences, events))
}

pub async fn refresh_preferences(
    backend: &impl Backend,
    host: &impl Host,
) -> Result<(), CallError> {
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
) -> Result<LaunchPreferences, CallError> {
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
        .map_err(|_| TASK_FAILED.to_string())?
}

fn value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "Management response failed".to_string())
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T, CallError> {
    serde_json::from_value(value).map_err(|_| CallError::Api(protocol::Error::invalid_request()))
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
struct NotificationsParams {
    config: NotificationPreferences,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_methods_name_commands_and_names_are_unique() {
        let mut names = std::collections::HashSet::new();
        for method in Method::ALL {
            assert!(names.insert(method.name()), "{}", method.name());
            assert_eq!(Method::from_name(method.name()), Some(*method));
            let decoded = Command::decode(method.name(), json!({}));
            let unknown =
                decoded.err().map(|error| error.code) == Some(protocol::ErrorCode::MethodNotFound);
            // Host methods may share a command's name (`reset_settings`); command methods must.
            if method.is_command() {
                assert!(!unknown, "{} is not a command", method.name());
            }
        }
    }
}
