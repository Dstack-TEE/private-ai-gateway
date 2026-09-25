//! The management API: one HTTP API the service answers on its private local
//! socket (a named pipe on Windows) and, for the web UI, on TCP, as Docker
//! Engine serves one API on `unix://` and `tcp://`. It is never exposed
//! through the inference API.
//!
//! - `GET /api/version` answers [`Version`]; clients refuse another build.
//! - `POST /api/rpc/{command}` runs one command of the table below: the body
//!   is its parameters as a JSON object, the answer `{"result": …}` or
//!   `{"error": Error}` with the status of its [`ErrorCode`].
//! - `GET /api/events` streams server-sent `ui_api::Event`s, starting with a
//!   full state snapshot.
use std::{fmt, path::Path};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    account::LoginPresentation,
    config::{Appearance, Config, NotificationPreferences, UpdateChannel, WebUiConfig},
    contracts::*,
    maintenance::{ImportResult, ProfileBackup},
    usage::{UsagePage, UsageQuery},
};

/// The HTTP API's version, reported by `GET /api/version` and in the
/// `Api-Version` header of every answer, as Docker Engine reports its own.
pub const API_VERSION: u16 = 1;
pub const API_VERSION_HEADER: &str = "api-version";
pub const VERSION_PATH: &str = "/api/version";
pub const RPC_PATH: &str = "/api/rpc/";
pub const EVENTS_PATH: &str = "/api/events";
/// The committed app version; Mac App Store builds append their build number
/// (`runtimeBuildVersion` in `scripts/distribution.mjs`).
pub const BUILD_VERSION: &str = match option_env!("PAP_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// `GET /api/version`: which backend answers, like Docker's `/version`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub api_version: u16,
    pub product: String,
    pub version: String,
    pub instance_id: String,
    pub process_id: u32,
    pub executable: String,
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

/// Expands the command table below into the `Command` enum and one typed
/// `rpc::*` request per command. The backend answers each command with its
/// declared response in an exhaustive match over `Command`. Every command
/// takes a JSON object, so commands without parameters are empty structs.
macro_rules! commands {
    (@parse [$($variant:tt)*] [$($request:tt)*] [$($name:ident)*]) => {
        /// A command as `{"command": name, "params": {…}}`: the name is the
        /// snake_case variant and the Tauri command of the renderer method
        /// that forwards it; parameters are camelCase like the renderer's.
        #[derive(Serialize, Deserialize)]
        #[serde(
            tag = "command",
            content = "params",
            rename_all = "snake_case",
            rename_all_fields = "camelCase",
            deny_unknown_fields
        )]
        pub enum Command {
            $($variant)*
        }

        /// Only the command names, to tell an unknown command from invalid parameters.
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Name { $($name),* }

        /// Typed requests: `client.call(rpc::X { .. })` sends `Command::X`
        /// and decodes exactly its declared response.
        pub mod rpc {
            use super::*;
            $($request)*
        }
    };
    (@parse [$($variant:tt)*] [$($request:tt)*] [$($names:ident)*]
        $(#[$meta:meta])* $name:ident -> $response:ty; $($rest:tt)*) => {
        impl From<rpc::$name> for Command {
            fn from(_: rpc::$name) -> Self {
                Self::$name {}
            }
        }
        impl Call for rpc::$name {
            type Response = $response;
        }
        commands!(@parse
            [$($variant)* $(#[$meta])* $name {},]
            [$($request)* $(#[$meta])* pub struct $name;]
            [$($names)* $name]
            $($rest)*);
    };
    (@parse [$($variant:tt)*] [$($request:tt)*] [$($names:ident)*]
        $(#[$meta:meta])* $name:ident { $($field:ident: $type:ty),* $(,)? } -> $response:ty;
        $($rest:tt)*) => {
        impl From<rpc::$name> for Command {
            fn from(request: rpc::$name) -> Self {
                Self::$name { $($field: request.$field),* }
            }
        }
        impl Call for rpc::$name {
            type Response = $response;
        }
        commands!(@parse
            [$($variant)* $(#[$meta])* $name { $($field: $type),* },]
            [$($request)* $(#[$meta])* pub struct $name { $(pub $field: $type),* }]
            [$($names)* $name]
            $($rest)*);
    };
    ($($entries:tt)*) => {
        commands!(@parse [] [] [] $($entries)*);
    };
}

// The only list of management commands: parameters and response. The backend
// (`desktop_runtime::dispatch`) handles each; admission is decided in
// `desktop_runtime::server::execute`. Browsers may call only the commands the
// renderer method table (`ui_api::Method`) forwards.
commands! {
    GetState -> AppStateWire;
    Start { config: StartConfig } -> AppStateWire;
    Stop -> AppStateWire;
    /// Answered under exclusive lifecycle admission; the service then exits.
    Shutdown { instance_id: String, mode: ShutdownMode } -> ();
    Verify {
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    } -> AppStateWire;
    SaveConfiguration {
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    } -> AppStateWire;
    CompleteAccountLogin { id: String, callback_url: String } -> ();
    BeginAccountLogin { profile: ConfidentialProfileInput } -> LoginPresentation;
    /// Starts saving a signed-in account; poll `AccountSaveResult`.
    BeginAccountSave {
        operation_id: String,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    } -> AccountSaveResult;
    AccountSaveResult { operation_id: String } -> AccountSaveResult;
    GetAccountDetails { profile_id: String } -> AccountLoginDetails;
    GetAccountBalance { target: AccountBalanceTarget } -> Option<AccountBalance>;
    PollAccountLogin { id: String } -> Option<AccountLoginDetails>;
    CancelAccountLogin { id: String } -> ();
    ActivateProfile { profile_id: String } -> AppStateWire;
    DeleteProfile { profile_id: String } -> AppStateWire;
    ClearApiKey -> AppStateWire;
    ImportProfiles { backup: ProfileBackup } -> ImportResult;
    ExportProfiles { path: String } -> ();
    ExportProfilesContent -> String;
    ExportDiagnostics { path: String } -> ();
    ExportDiagnosticsContent -> String;
    QueryUsage { query: UsageQuery } -> UsagePage;
    GetUsageRecord { record_id: String } -> RequestActivity;
    ExportUsage { query: UsageQuery, path: String } -> usize;
    ClearUsage -> u64;
    GetClientKey -> String;
    RotateClientKey -> String;
    SaveLocalApiConfig { config: ListenConfig } -> AppStateWire;
    SaveWebUi { config: WebUiConfig } -> AppStateWire;
    /// Set or clear the web UI sign-in password; every browser session ends.
    SetWebUiPassword { password: Option<String> } -> AppStateWire;
    RefreshCatalog -> AppStateWire;
    ListAgents -> Vec<AgentStatus>;
    PreviewAgent { agent_id: String, connect: bool, options: ConnectOptions } -> AgentPreview;
    ApplyAgent {
        agent_id: String,
        connect: bool,
        revision: String,
        options: ConnectOptions,
    } -> AgentStatus;
    /// Stops protection and saves whether it requires a production OS image;
    /// the next start uses the saved policy.
    SetRequireProductionOs { required: bool } -> AppStateWire;
    /// Connects or disconnects an agent with the configuration a preview
    /// would show, in one step.
    SetAgentConnection { agent_id: String, connect: bool } -> AgentStatus;
    DisconnectAllAgents -> Vec<AgentStatus>;
    ResetSettings -> AppStateWire;
    /// The settings in effect (`config.toml`); never includes a secret.
    Settings -> Config;
    SetPreference { change: Preference } -> Config;
}

impl Command {
    /// The command `POST /api/rpc/{name}` names, with its JSON body.
    pub fn decode(name: &str, params: Value) -> Result<Self, Error> {
        serde_json::from_value::<Name>(Value::String(name.into()))
            .map_err(|_| Error::method_not_found())?;
        serde_json::from_value(json!({ "command": name, "params": params }))
            .map_err(|_| Error::invalid_request())
    }

    /// The command's name and parameters, as `POST /api/rpc/{name}` sends them.
    pub fn encode(&self) -> Result<(String, Value), Error> {
        let Ok(Value::Object(mut encoded)) = serde_json::to_value(self) else {
            return Err(Error::internal());
        };
        match (encoded.remove("command"), encoded.remove("params")) {
            (Some(Value::String(name)), Some(params)) => Ok((name, params)),
            _ => Err(Error::internal()),
        }
    }
}

/// Encodes a response the service produces for `C` itself.
pub fn encode<C: Call>(response: C::Response) -> Result<Value, Error> {
    serde_json::to_value(response).map_err(|_| Error::internal())
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
    /// One notification switch, applied to the saved preferences by the
    /// backend so concurrent changes of the others are kept.
    Notification {
        kind: NotificationKind,
        enabled: bool,
    },
    ConnectOnLaunch(bool),
    Appearance(Appearance),
    UpdateChannel(UpdateChannel),
}

/// A field of [`NotificationPreferences`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationKind {
    Enabled,
    Gateway,
    LocalApi,
    Verification,
}

impl Preference {
    pub fn apply(self, saved: &mut Config) {
        match self {
            Self::AutoCliRegistration(enabled) => saved.auto_cli_registration = Some(enabled),
            Self::Notifications(config) => saved.notifications = config,
            Self::Notification { kind, enabled } => {
                let notifications = &mut saved.notifications;
                *match kind {
                    NotificationKind::Enabled => &mut notifications.enabled,
                    NotificationKind::Gateway => &mut notifications.gateway,
                    NotificationKind::LocalApi => &mut notifications.local_api,
                    NotificationKind::Verification => &mut notifications.verification,
                } = enabled;
            }
            Self::ConnectOnLaunch(enabled) => saved.connect_on_launch = enabled,
            Self::Appearance(appearance) => saved.appearance = appearance,
            Self::UpdateChannel(channel) => saved.update_channel = Some(channel),
        }
    }
}

/// Stable error codes. Each has one HTTP status, following Docker Engine's
/// `errdefs` classes and the gRPC status codes they correspond to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Malformed parameters (errdefs InvalidParameter, INVALID_ARGUMENT).
    InvalidRequest,
    /// No web UI session (errdefs Unauthorized, UNAUTHENTICATED).
    Unauthorized,
    /// A request the listener refuses (errdefs Forbidden, PERMISSION_DENIED).
    Forbidden,
    /// No such page or endpoint (errdefs NotFound, NOT_FOUND).
    NotFound,
    /// No such command, or none this caller may run (NOT_FOUND).
    MethodNotFound,
    MethodNotAllowed,
    UnsupportedMediaType,
    /// The sign-in budget is spent (RESOURCE_EXHAUSTED).
    TooManyRequests,
    /// The current state does not allow it (errdefs Conflict, FAILED_PRECONDITION).
    InvalidState,
    /// The backend instance named by a shutdown is gone (FAILED_PRECONDITION).
    InstanceChanged,
    /// Another operation holds what this one needs; retry (errdefs Unavailable, UNAVAILABLE).
    Busy,
    /// The account service, or signing in to it, failed; the message is authored locally.
    AccountError,
    /// The backend kept running; its log has the reason (errdefs System, INTERNAL).
    ShutdownRefused,
    /// An unexpected failure whose details stay in the service (errdefs System, INTERNAL).
    OperationFailed,
    // Agent configuration (`agent_bridge::agents::AgentError`).
    ConfigurationReadFailed,
    InvalidConfiguration,
    ConfigurationConflict,
    AuthenticationConflict,
    CredentialStoreUnavailable,
    ConfigurationWriteFailed,
    ConfigurationLockFailed,
    ConnectionRecordUnavailable,
    ConfigurationRestoreFailed,
    HelperUnavailable,
    CodexMetadataUnavailable,
    NoCompatibleModels,
    IncompatibleModel,
    RevisionConflict,
}

impl ErrorCode {
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound | Self::MethodNotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::InvalidState
            | Self::InstanceChanged
            | Self::InvalidConfiguration
            | Self::ConfigurationConflict
            | Self::AuthenticationConflict
            | Self::NoCompatibleModels
            | Self::IncompatibleModel
            | Self::RevisionConflict => 409,
            Self::UnsupportedMediaType => 415,
            Self::TooManyRequests => 429,
            Self::AccountError => 502,
            Self::Busy => 503,
            Self::ShutdownRefused
            | Self::OperationFailed
            | Self::ConfigurationReadFailed
            | Self::CredentialStoreUnavailable
            | Self::ConfigurationWriteFailed
            | Self::ConfigurationLockFailed
            | Self::ConnectionRecordUnavailable
            | Self::ConfigurationRestoreFailed
            | Self::HelperUnavailable
            | Self::CodexMetadataUnavailable => 500,
        }
    }
}

const OPERATION_FAILED: &str = "The operation could not complete. Check the protection status and supplied configuration before retrying.";
const BUSY: &str = "Another operation is in progress. Retry after it completes.";

/// An API error: a stable code and a message authored for the user. Other
/// failures convert from their text into [`ErrorCode::OperationFailed`], which
/// never carries OS, SQL or provider details.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_request() -> Self {
        Self::new(ErrorCode::InvalidRequest, "Invalid management request")
    }

    pub fn method_not_found() -> Self {
        Self::new(ErrorCode::MethodNotFound, "Unknown management method")
    }

    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidState, message)
    }

    pub fn account(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::AccountError, message)
    }

    /// Another operation holds what this one needs.
    pub fn busy() -> Self {
        Self::new(ErrorCode::Busy, BUSY)
    }

    pub fn internal() -> Self {
        Self::new(ErrorCode::OperationFailed, OPERATION_FAILED)
    }
}

/// The message; the code travels beside it in every serialized error.
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(_: String) -> Self {
        Self::internal()
    }
}

impl From<&str> for Error {
    fn from(_: &str) -> Self {
        Self::internal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_have_stable_names_and_parameters() {
        // The names are Tauri command names and web RPC paths; the
        // parameters are what the renderer sends.
        let commands = [
            (Command::GetState {}, "get_state", json!({})),
            (
                Command::Start {
                    config: StartConfig {
                        remote_url: "https://tee.example".into(),
                        require_production_os: true,
                    },
                },
                "start",
                json!({"config": {"remoteUrl": "https://tee.example", "requireProductionOs": true}}),
            ),
            (
                Command::Shutdown {
                    instance_id: "1-2".into(),
                    mode: ShutdownMode::UpdateRestart,
                },
                "shutdown",
                json!({"instanceId": "1-2", "mode": "updateRestart"}),
            ),
            (
                Command::ActivateProfile {
                    profile_id: "p".into(),
                },
                "activate_profile",
                json!({"profileId": "p"}),
            ),
            (
                Command::SetPreference {
                    change: Preference::Appearance(Appearance::Dark),
                },
                "set_preference",
                json!({"change": {"name": "appearance", "value": "dark"}}),
            ),
        ];
        for (command, name, params) in commands {
            let (encoded, body) = command.encode().unwrap();
            assert_eq!((encoded.as_str(), &body), (name, &params));
            let decoded = Command::decode(name, params).unwrap();
            assert_eq!(decoded.encode().unwrap(), (encoded, body));
        }
    }

    #[test]
    fn unknown_commands_and_invalid_parameters_differ() {
        let code = |name, params| Command::decode(name, params).err().map(|error| error.code);
        assert_eq!(
            code("notACommand", json!({})),
            Some(ErrorCode::MethodNotFound)
        );
        assert_eq!(
            code("activate_profile", json!({"profile_id": "p"})),
            Some(ErrorCode::InvalidRequest)
        );
        assert_eq!(
            code("get_state", json!({"extra": 1})),
            Some(ErrorCode::InvalidRequest)
        );
    }

    #[test]
    fn errors_serialize_their_code_and_hide_unauthored_details() {
        let error = Error::invalid_state("Stop protection before deleting a profile");
        assert_eq!(
            error.to_string(),
            "Stop protection before deleting a profile"
        );
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"code": "invalid_state", "message": "Stop protection before deleting a profile"})
        );
        assert_eq!(error.code.status(), 409);
        let unclassified = Error::from("PRIVATE_OS_DETAIL secret=sk-hidden".to_string());
        assert_eq!(unclassified.code, ErrorCode::OperationFailed);
        assert!(!unclassified.message.contains("PRIVATE_OS_DETAIL"));
        assert!(!unclassified.message.contains("sk-hidden"));
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_paths_that_json_cannot_represent() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let path = std::path::PathBuf::from(OsString::from_vec(b"/tmp/pap-\xff.csv".to_vec()));
        assert!(export_path(&path).is_err());
        assert_eq!(
            export_path(Path::new("/tmp/pap.csv")).unwrap(),
            "/tmp/pap.csv"
        );
    }
}
