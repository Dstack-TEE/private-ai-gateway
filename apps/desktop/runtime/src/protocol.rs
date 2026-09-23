//! Local management protocol. It is never exposed through the inference API.
use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{
    account_login::LoginPresentation,
    contracts::*,
    controller::DesktopRuntime,
    maintenance::{ImportResult, ProfileBackup},
    preferences::{
        self, Appearance, NotificationPreferences, Preferences, UpdateChannel, WebUiConfig,
    },
    usage::{UsagePage, UsageQuery},
    web_ui::WebUiLogin,
};

pub const VERSION: u16 = 3;
pub const BUILD_VERSION: &str = match option_env!("PAP_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    pub protocol_version: u16,
    pub product: String,
    pub version: String,
    pub instance_id: String,
    pub process_id: u32,
    pub executable: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub id: u64,
    pub command: Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShutdownMode {
    Quit,
    UpdateRestart,
}

/// A management request bound to the response the service answers it with.
pub trait Call: Into<Command> {
    type Response: Serialize + DeserializeOwned;
}

/// Expands the command table below into the wire `Command` enum, one typed
/// `rpc::*` request per command, and the service dispatch. Unit, newtype and
/// struct entries keep their variant shape, so the wire format is unchanged.
macro_rules! commands {
    ($runtime:ident => $($entries:tt)*) => {
        commands!(@parse $runtime [] [] [] $($entries)*);
    };
    (@parse $runtime:ident [$($variant:tt)*] [$($request:tt)*] [$($arm:tt)*]) => {
        #[derive(Serialize, Deserialize)]
        #[serde(
            tag = "method",
            content = "params",
            rename_all = "camelCase",
            deny_unknown_fields
        )]
        pub enum Command {
            $($variant)*
        }

        /// Typed requests: `client.call(rpc::X { .. })` sends `Command::X`
        /// and decodes exactly its declared response.
        pub mod rpc {
            use super::*;
            $($request)*
        }

        /// Runs one admitted command; its result must match the declared response.
        pub(crate) async fn dispatch(
            $runtime: &Arc<DesktopRuntime>,
            command: Command,
        ) -> Result<Value, RpcError> {
            match command {
                $($arm)*
            }
        }
    };
    (@parse $runtime:ident [$($variant:tt)*] [$($request:tt)*] [$($arm:tt)*]
        $(#[$meta:meta])* $name:ident -> $response:ty = $handler:expr; $($rest:tt)*) => {
        impl From<rpc::$name> for Command {
            fn from(_: rpc::$name) -> Self {
                Self::$name
            }
        }
        impl Call for rpc::$name {
            type Response = $response;
        }
        commands!(@parse $runtime
            [$($variant)* $(#[$meta])* $name,]
            [$($request)* $(#[$meta])* pub struct $name;]
            [$($arm)* Command::$name => respond::<rpc::$name, _>($handler),]
            $($rest)*);
    };
    (@parse $runtime:ident [$($variant:tt)*] [$($request:tt)*] [$($arm:tt)*]
        $(#[$meta:meta])* $name:ident($field:ident: $type:ty) -> $response:ty = $handler:expr;
        $($rest:tt)*) => {
        impl From<rpc::$name> for Command {
            fn from(request: rpc::$name) -> Self {
                Self::$name(request.$field)
            }
        }
        impl Call for rpc::$name {
            type Response = $response;
        }
        commands!(@parse $runtime
            [$($variant)* $(#[$meta])* $name($type),]
            [$($request)* $(#[$meta])* pub struct $name { pub $field: $type }]
            [$($arm)* Command::$name($field) => respond::<rpc::$name, _>($handler),]
            $($rest)*);
    };
    (@parse $runtime:ident [$($variant:tt)*] [$($request:tt)*] [$($arm:tt)*]
        $(#[$meta:meta])* $name:ident { $($field:ident: $type:ty),* $(,)? }
        -> $response:ty = $handler:expr; $($rest:tt)*) => {
        impl From<rpc::$name> for Command {
            fn from(request: rpc::$name) -> Self {
                Self::$name { $($field: request.$field),* }
            }
        }
        impl Call for rpc::$name {
            type Response = $response;
        }
        commands!(@parse $runtime
            [$($variant)* $(#[$meta])* $name { $($field: $type),* },]
            [$($request)* $(#[$meta])* pub struct $name { $(pub $field: $type),* }]
            [$($arm)* Command::$name { $($field),* } => respond::<rpc::$name, _>($handler),]
            $($rest)*);
    };
}

// The only list of management commands: wire parameters, response and the
// service handler. Admission is decided in `server::execute`.
commands! { runtime =>
    State -> GatewayState = runtime.state();
    /// Streams state snapshots on a dedicated connection.
    Watch -> GatewayState = Err("Subscription requires its own connection");
    Start(config: StartGatewayConfig) -> GatewayState = runtime.start(config);
    Stop -> GatewayState = runtime.stop();
    /// Answered by the connection under exclusive lifecycle admission.
    Shutdown { instance_id: String, mode: ShutdownMode } -> () = {
        let _ = (instance_id, mode);
        Err("Shutdown requires lifecycle admission")
    };
    Verify {
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    } -> GatewayState = runtime
        .verify_configuration(profile, require_production_os, key)
        .await;
    SaveConfiguration {
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    } -> GatewayState = runtime
        .save_configuration(profile, require_production_os, key)
        .await;
    CompleteAccountLogin { id: String, callback_url: String } -> () =
        runtime.complete_account_login(id, callback_url).await;
    BeginAccountLogin { profile: ConfidentialProfileInput } -> LoginPresentation =
        runtime.begin_account_login(profile).await;
    SaveAccountLogin {
        operation_id: String,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    } -> AccountSaveResult = runtime.begin_account_save(
        operation_id,
        id,
        profile,
        require_production_os,
        workspace_id,
    );
    AccountSaveResult { operation_id: String } -> AccountSaveResult =
        runtime.account_save_result(&operation_id);
    AccountDetails { profile_id: String } -> AccountLoginDetails =
        runtime.account_details(profile_id).await;
    AccountBalance { target: AccountBalanceTarget } -> Option<AccountBalance> =
        runtime.account_balance(target).await;
    PollAccountLogin { id: String } -> Option<AccountLoginDetails> =
        runtime.poll_account_login(id).await;
    CancelAccountLogin { id: String } -> () = runtime.cancel_account_login(id).await;
    ActivateProfile { profile_id: String } -> GatewayState = runtime.activate_profile(profile_id);
    DeleteProfile { profile_id: String } -> GatewayState =
        runtime.delete_profile(profile_id).await;
    ClearApiKey -> GatewayState = runtime.clear_api_key().await;
    ImportProfiles(backup: ProfileBackup) -> ImportResult = runtime.import_profiles(backup);
    ExportProfiles { path: String } -> () = runtime.export_profiles(absolute(path)?);
    ExportProfilesContent -> String = runtime.export_profiles_content();
    ExportDiagnostics { path: String } -> () =
        runtime.export_diagnostics(absolute(path)?, BUILD_VERSION);
    ExportDiagnosticsContent -> String = runtime.export_diagnostics_content(BUILD_VERSION);
    Usage(query: UsageQuery) -> UsagePage = runtime.query_usage(query);
    UsageRecord { record_id: String } -> Option<RequestActivity> =
        runtime.usage_record(&record_id);
    ExportUsage { query: UsageQuery, path: String } -> usize =
        runtime.export_usage_csv(query, absolute(path)?);
    ClearUsage -> u64 = runtime.clear_usage();
    ClientKey -> String = runtime.client_key();
    RotateClientKey -> String = runtime.rotate_client_key();
    SaveLocalApi(config: LocalApiConfig) -> GatewayState =
        runtime.save_local_api_config(config).await;
    SaveWebUi(config: WebUiConfig) -> GatewayState = runtime.save_web_ui(config);
    /// Mint a one-time web UI login link. Only the authenticated IPC endpoint can ask.
    WebUiLogin -> WebUiLogin = runtime.web_ui_login();
    RefreshCatalog -> GatewayState = runtime.refresh_catalog().await;
    Agents -> Vec<AgentStatus> = runtime.list_agents();
    PreviewAgent { agent_id: String, connect: bool, options: ConnectOptions } -> AgentPreview =
        runtime.preview_agent(agent_id, connect, options);
    ApplyAgent {
        agent_id: String,
        connect: bool,
        revision: String,
        options: ConnectOptions,
    } -> AgentStatus = runtime.apply_agent(agent_id, connect, revision, options);
    DisconnectAllAgents -> Vec<AgentStatus> = runtime.disconnect_all_agents();
    ResetSettings -> GatewayState = runtime.reset_settings().await;
    Preferences -> Preferences = preferences::load();
    SetPreference(change: Preference) -> Preferences =
        preferences::update(|saved| change.apply(saved)).and_then(|()| preferences::load());
}

fn respond<C: Call, E: Into<RpcError>>(result: Result<C::Response, E>) -> Result<Value, RpcError> {
    encode::<C>(result.map_err(Into::into)?)
}

/// Encodes a response the connection handler produces for `C` itself.
pub(crate) fn encode<C: Call>(response: C::Response) -> Result<Value, RpcError> {
    serde_json::to_value(response)
        .map_err(|_| RpcError::new("encoding_failed", "Cannot encode the operation result."))
}

fn absolute(path: String) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("Export path must be absolute".into());
    }
    Ok(path)
}

/// Export paths travel as JSON strings.
pub fn export_path(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "Export paths must be valid Unicode".into())
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "name",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum Preference {
    AutoCliRegistration(bool),
    Notifications(NotificationPreferences),
    ConnectOnLaunch(bool),
    Appearance(Appearance),
    UpdateChannel(UpdateChannel),
}

impl Preference {
    fn apply(self, saved: &mut Preferences) {
        match self {
            Self::AutoCliRegistration(enabled) => saved.auto_cli_registration = Some(enabled),
            Self::Notifications(config) => saved.notifications = config,
            Self::ConnectOnLaunch(enabled) => saved.connect_on_launch = enabled,
            Self::Appearance(appearance) => saved.appearance = appearance,
            Self::UpdateChannel(channel) => saved.update_channel = Some(channel),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: u64,
    pub outcome: Outcome,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Result(Value),
    Error(RpcError),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn operation(message: &str) -> Self {
        // Only locally mapped account errors carry this prefix; never raw provider bodies.
        if message.starts_with("Account: ") && message.len() < 512 {
            return Self::new("account_error", message);
        }
        // Return actionable product errors, never arbitrary OS/SQL/provider details.
        if message.contains("in progress") || message.contains("busy") {
            return Self::new(
                "busy",
                "Another operation is in progress. Retry after it completes.",
            );
        }
        for prefix in [
            "Web UI",
            "Stop protection before",
            "Gateway is already running",
            "Create a Confidential AI profile",
            "Add a credential",
            "Enter an API key",
            "Start the gateway and wait",
            "Disconnect managed agents",
            "At least one",
            "Confidential AI profile not found",
            "Select or verify",
            "No verified",
            "The connection preview",
        ] {
            if message.starts_with(prefix) && message.len() < 512 {
                return Self::new("invalid_state", message);
            }
        }
        if message.contains("credential store") {
            return Self::new("credential_store_unavailable", "The OS credential store is unavailable or locked. Unlock it in your user session and retry.");
        }
        Self::new("operation_failed", "The operation could not complete. Check the gateway state and supplied configuration before retrying.")
    }
}

impl From<desktop_gateway::agents::AgentError> for RpcError {
    fn from(error: desktop_gateway::agents::AgentError) -> Self {
        Self::new(error.code(), &error.to_string())
    }
}

impl From<crate::controller::AgentOperationError> for RpcError {
    fn from(error: crate::controller::AgentOperationError) -> Self {
        match error {
            crate::controller::AgentOperationError::Agent(error) => error.into(),
            crate::controller::AgentOperationError::Runtime(message) => Self::operation(&message),
        }
    }
}

impl From<String> for RpcError {
    fn from(message: String) -> Self {
        Self::operation(&message)
    }
}

impl From<&str> for RpcError {
    fn from(message: &str) -> Self {
        Self::operation(message)
    }
}

pub fn read<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<T> {
    read_with_timeout(reader, Duration::from_secs(5))
}

pub fn read_with_timeout<T: DeserializeOwned>(
    reader: &mut impl BufRead,
    timeout: Duration,
) -> io::Result<T> {
    let mut bytes = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Management frame deadline exceeded",
            ));
        }
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Management connection closed",
            ));
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |at| at + 1);
        if bytes.len() + count > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Management frame exceeds limit",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            break;
        }
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid management frame"))
}

pub fn write(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "Cannot encode management frame")
    })?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Management frame exceeds limit",
        ));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_is_bounded_and_truncation_is_not_a_request() {
        let mut input = io::Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            read::<Request>(&mut input).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(read::<Value>(&mut io::Cursor::new(b"{}" as &[u8])).is_err());
        let mut bytes = Vec::new();
        let response = Response {
            id: 1,
            outcome: Outcome::Error(RpcError::new("busy", "Busy")),
        };
        write(&mut bytes, &response).unwrap();
        let decoded: Response = read(&mut io::Cursor::new(bytes)).unwrap();
        assert!(matches!(decoded.outcome, Outcome::Error(_)));
    }

    #[test]
    fn wire_format_is_stable_across_builds() {
        // Shutdown is sent to backends of other builds during updates, so the
        // envelope and every command shape must keep these exact bytes.
        let frames = [
            (Command::State, r#"{"method":"state"}"#),
            (
                Command::Start(StartGatewayConfig {
                    remote_url: "https://tee.example".into(),
                    require_production_os: true,
                }),
                r#"{"method":"start","params":{"remoteUrl":"https://tee.example","requireProductionOs":true}}"#,
            ),
            (
                Command::Shutdown {
                    instance_id: "1-2".into(),
                    mode: ShutdownMode::UpdateRestart,
                },
                r#"{"method":"shutdown","params":{"instance_id":"1-2","mode":"updateRestart"}}"#,
            ),
            (
                Command::ActivateProfile {
                    profile_id: "p".into(),
                },
                r#"{"method":"activateProfile","params":{"profile_id":"p"}}"#,
            ),
            (
                Command::SetPreference(Preference::Appearance(Appearance::Dark)),
                r#"{"method":"setPreference","params":{"name":"appearance","value":"dark"}}"#,
            ),
        ];
        for (command, expected) in frames {
            let request = Request {
                version: VERSION,
                id: 7,
                command,
            };
            let encoded = serde_json::to_string(&request).unwrap();
            assert_eq!(
                encoded,
                format!(r#"{{"version":{VERSION},"id":7,"command":{expected}}}"#)
            );
            let decoded: Request = serde_json::from_str(&encoded).unwrap();
            assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
        }
        let response = Response {
            id: 7,
            outcome: Outcome::Result(Value::Null),
        };
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"id":7,"outcome":{"result":null}}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_paths_that_json_cannot_represent() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let path = PathBuf::from(OsString::from_vec(b"/tmp/pap-\xff.csv".to_vec()));
        assert!(export_path(&path).is_err());
        assert_eq!(
            export_path(Path::new("/tmp/pap.csv")).unwrap(),
            "/tmp/pap.csv"
        );
    }

    #[test]
    fn agent_failures_keep_actionable_causes_without_internal_details() {
        use desktop_gateway::agents::AgentError;
        for (error, code) in [
            (AgentError::NoCompatibleModels, "no_compatible_models"),
            (AgentError::IncompatibleModel, "incompatible_model"),
            (AgentError::ConfigurationRead, "configuration_read_failed"),
            (AgentError::ConfigurationWrite, "configuration_write_failed"),
            (AgentError::Internal, "operation_failed"),
        ] {
            let public = RpcError::from(error);
            assert_eq!(public.code, code);
            assert!(!public.message.contains("PRIVATE_OS_DETAIL"));
            assert!(!public.message.contains("sk-hidden"));
        }
        let diagnostic =
            "The app-owned Codex model catalog is invalid. Reinstall Private AI Proxy.";
        assert_eq!(
            RpcError::from(AgentError::MetadataUnavailable(diagnostic.to_string())).message,
            diagnostic
        );
        let unclassified = RpcError::operation("PRIVATE_OS_DETAIL secret=sk-hidden");
        assert_eq!(unclassified.code, "operation_failed");
        assert!(!unclassified.message.contains("PRIVATE_OS_DETAIL"));
        assert!(!unclassified.message.contains("sk-hidden"));
    }
}
