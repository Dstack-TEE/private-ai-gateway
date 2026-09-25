//! Local management protocol. It is never exposed through the inference API.
use std::{
    io::{self, BufRead, Write},
    path::Path,
    time::{Duration, Instant},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{
    account::LoginPresentation,
    config::{Appearance, Config, NotificationPreferences, UpdateChannel, WebUiConfig},
    contracts::*,
    maintenance::{ImportResult, ProfileBackup},
    usage::{UsagePage, UsageQuery},
};

pub const VERSION: u16 = 3;
/// The committed app version; Mac App Store builds append their build number
/// (`runtimeBuildVersion` in `scripts/distribution.mjs`).
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

/// Expands the command table below into the wire `Command` enum and one typed
/// `rpc::*` request per command. Unit, newtype and struct entries keep their
/// variant shape, so the wire format is unchanged. The backend answers each
/// command with its declared response in an exhaustive match over `Command`.
macro_rules! commands {
    (@parse [$($variant:tt)*] [$($request:tt)*]) => {
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
    };
    (@parse [$($variant:tt)*] [$($request:tt)*]
        $(#[$meta:meta])* $name:ident -> $response:ty; $($rest:tt)*) => {
        impl From<rpc::$name> for Command {
            fn from(_: rpc::$name) -> Self {
                Self::$name
            }
        }
        impl Call for rpc::$name {
            type Response = $response;
        }
        commands!(@parse
            [$($variant)* $(#[$meta])* $name,]
            [$($request)* $(#[$meta])* pub struct $name;]
            $($rest)*);
    };
    (@parse [$($variant:tt)*] [$($request:tt)*]
        $(#[$meta:meta])* $name:ident($field:ident: $type:ty) -> $response:ty; $($rest:tt)*) => {
        impl From<rpc::$name> for Command {
            fn from(request: rpc::$name) -> Self {
                Self::$name(request.$field)
            }
        }
        impl Call for rpc::$name {
            type Response = $response;
        }
        commands!(@parse
            [$($variant)* $(#[$meta])* $name($type),]
            [$($request)* $(#[$meta])* pub struct $name { pub $field: $type }]
            $($rest)*);
    };
    (@parse [$($variant:tt)*] [$($request:tt)*]
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
            $($rest)*);
    };
    ($($entries:tt)*) => {
        commands!(@parse [] [] $($entries)*);
    };
}

// The only list of management commands: wire parameters and response. The
// backend (`desktop_runtime::dispatch`) handles each; admission is decided in
// `desktop_runtime::server::execute`.
commands! {
    State -> AppState;
    /// Streams state snapshots on a dedicated connection.
    Watch -> AppState;
    Start(config: StartConfig) -> AppState;
    Stop -> AppState;
    /// Answered by the connection under exclusive lifecycle admission.
    Shutdown { instance_id: String, mode: ShutdownMode } -> ();
    Verify {
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    } -> AppState;
    SaveConfiguration {
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    } -> AppState;
    CompleteAccountLogin { id: String, callback_url: String } -> ();
    BeginAccountLogin { profile: ConfidentialProfileInput } -> LoginPresentation;
    SaveAccountLogin {
        operation_id: String,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    } -> AccountSaveResult;
    AccountSaveResult { operation_id: String } -> AccountSaveResult;
    AccountDetails { profile_id: String } -> AccountLoginDetails;
    AccountBalance { target: AccountBalanceTarget } -> Option<AccountBalance>;
    PollAccountLogin { id: String } -> Option<AccountLoginDetails>;
    CancelAccountLogin { id: String } -> ();
    ActivateProfile { profile_id: String } -> AppState;
    DeleteProfile { profile_id: String } -> AppState;
    ClearApiKey -> AppState;
    ImportProfiles(backup: ProfileBackup) -> ImportResult;
    ExportProfiles { path: String } -> ();
    ExportProfilesContent -> String;
    ExportDiagnostics { path: String } -> ();
    ExportDiagnosticsContent -> String;
    Usage(query: UsageQuery) -> UsagePage;
    UsageRecord { record_id: String } -> Option<RequestActivity>;
    ExportUsage { query: UsageQuery, path: String } -> usize;
    ClearUsage -> u64;
    ClientKey -> String;
    RotateClientKey -> String;
    SaveLocalApi(config: ListenConfig) -> AppState;
    SaveWebUi(config: WebUiConfig) -> AppState;
    /// Set or clear the web UI sign-in password; every browser session ends.
    SetWebUiPassword { password: Option<String> } -> AppState;
    RefreshCatalog -> AppState;
    Agents -> Vec<AgentStatus>;
    PreviewAgent { agent_id: String, connect: bool, options: ConnectOptions } -> AgentPreview;
    ApplyAgent {
        agent_id: String,
        connect: bool,
        revision: String,
        options: ConnectOptions,
    } -> AgentStatus;
    DisconnectAllAgents -> Vec<AgentStatus>;
    ResetSettings -> AppState;
    /// The settings in effect (`config.toml`); never includes a secret.
    Settings -> Config;
    SetPreference(change: Preference) -> Config;
}

/// Encodes a response the connection handler produces for `C` itself.
pub fn encode<C: Call>(response: C::Response) -> Result<Value, RpcError> {
    serde_json::to_value(response)
        .map_err(|_| RpcError::new("encoding_failed", "Cannot encode the operation result."))
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
            "Protection is already running",
            "Create a Confidential AI profile",
            "Add a credential",
            "Enter an API key",
            "Start protection and wait",
            "Disconnect managed agents",
            "At least one",
            "Confidential AI profile not found",
            "Select or verify",
            "No verified",
            "The connection preview",
            "config.toml",
            "credentials.toml",
            "Settings from 0.1",
        ] {
            if message.starts_with(prefix) && message.len() < 512 {
                return Self::new("invalid_state", message);
            }
        }
        Self::new("operation_failed", "The operation could not complete. Check the protection status and supplied configuration before retrying.")
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
                Command::Start(StartConfig {
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
        let path = std::path::PathBuf::from(OsString::from_vec(b"/tmp/pap-\xff.csv".to_vec()));
        assert!(export_path(&path).is_err());
        assert_eq!(
            export_path(Path::new("/tmp/pap.csv")).unwrap(),
            "/tmp/pap.csv"
        );
    }

    #[test]
    fn unclassified_failures_hide_internal_details() {
        let unclassified = RpcError::operation("PRIVATE_OS_DETAIL secret=sk-hidden");
        assert_eq!(unclassified.code, "operation_failed");
        assert!(!unclassified.message.contains("PRIVATE_OS_DETAIL"));
        assert!(!unclassified.message.contains("sk-hidden"));
    }
}
