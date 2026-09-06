//! Local management protocol. It is never exposed through the inference API.
use std::{
    io::{self, BufRead, Write},
    time::{Duration, Instant},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{
    contracts::*,
    preferences::{Appearance, NotificationPreferences, UpdateChannel},
    usage::UsageQuery,
};

pub const VERSION: u16 = 1;
pub const BUILD_VERSION: &str = match option_env!("PAG_BUILD_VERSION") {
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

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum Command {
    State,
    Watch,
    Start(StartGatewayConfig),
    Stop,
    Shutdown {
        instance_id: String,
    },
    Verify {
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    },
    ActivateProfile {
        profile_id: String,
    },
    DeleteProfile {
        profile_id: String,
    },
    ClearApiKey,
    Usage(UsageQuery),
    UsageRecord {
        record_id: String,
    },
    ExportUsage {
        query: UsageQuery,
        path: String,
    },
    ClearUsage,
    ClientKey,
    RotateClientKey,
    SaveLocalApi(LocalApiConfig),
    RefreshCatalog,
    Agents,
    PreviewAgent {
        agent_id: String,
        connect: bool,
        options: ConnectOptions,
    },
    ApplyAgent {
        agent_id: String,
        connect: bool,
        revision: String,
        options: ConnectOptions,
    },
    DisconnectAllAgents,
    Preferences,
    SetPreference(Preference),
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "name",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum Preference {
    Notifications(NotificationPreferences),
    ConnectOnLaunch(bool),
    Appearance(Appearance),
    UpdateChannel(UpdateChannel),
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
        // Return actionable product errors, never arbitrary OS/SQL/provider details.
        if message.contains("in progress") || message.contains("busy") {
            return Self::new(
                "busy",
                "Another operation is in progress. Retry after it completes.",
            );
        }
        if message.contains("changed since the preview") {
            return Self::new(
                "revision_conflict",
                "The agent configuration changed. Obtain a new preview before applying.",
            );
        }
        for prefix in [
            "Stop protection before",
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
}
