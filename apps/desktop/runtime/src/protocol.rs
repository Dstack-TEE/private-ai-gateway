//! Versioned newline-delimited JSON used between native clients and the
//! desktop runtime process.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    GetState,
    Start,
    Stop,
    VerifyConfiguration,
    ActivateProfile,
    DeleteProfile,
    ClearApiKey,
    QueryUsage,
    ExportUsageCsv,
    ClearUsage,
    RefreshCatalog,
    ListAgents,
    PreviewAgent,
    ApplyAgent,
    DisconnectAllAgents,
    GetClientKey,
    RotateClientKey,
    SaveLocalApiConfig,
    Shutdown,
}

impl Method {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "getState" => Self::GetState,
            "start" => Self::Start,
            "stop" => Self::Stop,
            "verifyConfiguration" => Self::VerifyConfiguration,
            "activateProfile" => Self::ActivateProfile,
            "deleteProfile" => Self::DeleteProfile,
            "clearApiKey" => Self::ClearApiKey,
            "queryUsage" => Self::QueryUsage,
            "exportUsageCsv" => Self::ExportUsageCsv,
            "clearUsage" => Self::ClearUsage,
            "refreshCatalog" => Self::RefreshCatalog,
            "listAgents" => Self::ListAgents,
            "previewAgent" => Self::PreviewAgent,
            "applyAgent" => Self::ApplyAgent,
            "disconnectAllAgents" => Self::DisconnectAllAgents,
            "getClientKey" => Self::GetClientKey,
            "rotateClientKey" => Self::RotateClientKey,
            "saveLocalApiConfig" => Self::SaveLocalApiConfig,
            "shutdown" => Self::Shutdown,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GetState => "getState",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::VerifyConfiguration => "verifyConfiguration",
            Self::ActivateProfile => "activateProfile",
            Self::DeleteProfile => "deleteProfile",
            Self::ClearApiKey => "clearApiKey",
            Self::QueryUsage => "queryUsage",
            Self::ExportUsageCsv => "exportUsageCsv",
            Self::ClearUsage => "clearUsage",
            Self::RefreshCatalog => "refreshCatalog",
            Self::ListAgents => "listAgents",
            Self::PreviewAgent => "previewAgent",
            Self::ApplyAgent => "applyAgent",
            Self::DisconnectAllAgents => "disconnectAllAgents",
            Self::GetClientKey => "getClientKey",
            Self::RotateClientKey => "rotateClientKey",
            Self::SaveLocalApiConfig => "saveLocalApiConfig",
            Self::Shutdown => "shutdown",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub schema_version: u16,
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn validate(&self) -> Result<Method, Error> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::new(
                "unsupported_schema",
                format!("Unsupported protocol schema {}", self.schema_version),
            ));
        }
        validate_id(&self.id)?;
        Method::parse(&self.method)
            .ok_or_else(|| Error::new("unknown_method", "Unknown desktop runtime method"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub schema_version: u16,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

impl Response {
    pub fn success(id: String, result: Value) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: String, error: Error) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub schema_version: u16,
    pub event: String,
    pub payload: Value,
}

impl Event {
    pub fn state(payload: Value) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            event: "stateChanged".to_string(),
            payload,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}

impl Error {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub fn read_request(reader: &mut impl BufRead) -> Result<Option<Request>, Error> {
    let mut bytes = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|_| Error::new("io_error", "Cannot read from the desktop client"))?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if bytes.len().saturating_add(consumed) > MAX_MESSAGE_BYTES {
            return Err(Error::new(
                "message_too_large",
                "Desktop runtime message exceeds the 1 MiB limit",
            ));
        }
        bytes.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if bytes.last() == Some(&b'\n') {
            break;
        }
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(Error::new(
            "invalid_json",
            "Desktop runtime message is empty",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Error::new("invalid_json", "Desktop runtime message is invalid JSON"))
}

pub fn write_message(writer: &mut impl Write, value: &impl Serialize) -> Result<(), Error> {
    serde_json::to_writer(&mut *writer, value)
        .map_err(|_| Error::new("io_error", "Cannot encode a desktop runtime message"))?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|_| Error::new("io_error", "Cannot write to the desktop client"))
}

fn validate_id(id: &str) -> Result<(), Error> {
    if id.is_empty()
        || id.len() > MAX_ID_BYTES
        || id.chars().any(char::is_control)
        || !id.is_ascii()
    {
        return Err(Error::new(
            "invalid_request_id",
            "Desktop runtime request id is invalid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_validates_one_request() {
        let input = br#"{"schemaVersion":1,"id":"request-1","method":"getState"}
"#;
        let request = read_request(&mut &input[..]).unwrap().unwrap();
        assert_eq!(request.validate().unwrap(), Method::GetState);
        assert_eq!(request.params, Value::Null);
    }

    #[test]
    fn rejects_unknown_schema_method_and_oversized_messages() {
        let request = Request {
            schema_version: 2,
            id: "request-1".to_string(),
            method: "getState".to_string(),
            params: Value::Null,
        };
        assert_eq!(request.validate().unwrap_err().code, "unsupported_schema");

        let request = Request {
            schema_version: SCHEMA_VERSION,
            method: "notARealMethod".to_string(),
            ..request
        };
        assert_eq!(request.validate().unwrap_err().code, "unknown_method");

        let oversized = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        assert_eq!(
            read_request(&mut &oversized[..]).unwrap_err().code,
            "message_too_large"
        );
    }

    #[test]
    fn responses_have_exactly_one_outcome() {
        let response = Response::success("request-1".to_string(), Value::Bool(true));
        let encoded = serde_json::to_value(response).unwrap();
        assert_eq!(encoded["result"], Value::Bool(true));
        assert!(encoded.get("error").is_none());

        let response = Response::failure(
            "request-2".to_string(),
            Error::new("invalid_request", "Invalid request"),
        );
        let encoded = serde_json::to_value(response).unwrap();
        assert!(encoded.get("result").is_none());
        assert_eq!(encoded["error"]["code"], "invalid_request");
    }

    #[test]
    fn state_events_do_not_contain_profile_credentials() {
        let payload = serde_json::json!({
            "profiles": [{
                "id": "work",
                "name": "Work",
                "provider": "redpill",
                "remoteUrl": "https://tee.redpill.ai",
                "auth": { "kind": "apiKey" }
            }],
            "apiKeySaved": true
        });
        let encoded = serde_json::to_string(&Event::state(payload)).unwrap();
        assert!(!encoded.contains("secretRef"));
        assert!(!encoded.contains("apiKeyValue"));
        assert!(!encoded.contains("credentialValue"));
        assert!(!encoded.contains("Bearer "));
    }

    #[test]
    fn shared_native_v1_fixtures_decode_and_encode_as_ndjson() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../native/protocol-fixtures/v1.json")).unwrap();
        let request: Request = serde_json::from_value(fixtures["request"].clone()).unwrap();
        assert_eq!(request.validate().unwrap(), Method::GetState);

        let success: Response = serde_json::from_value(fixtures["success"].clone()).unwrap();
        assert!(success.result.is_some());
        assert!(success.error.is_none());

        let failure: Response = serde_json::from_value(fixtures["failure"].clone()).unwrap();
        assert!(failure.result.is_none());
        assert_eq!(failure.error.unwrap().code, "operation_failed");

        let event: Event = serde_json::from_value(fixtures["event"].clone()).unwrap();
        assert_eq!(event.event, "stateChanged");

        let mut output = Vec::new();
        write_message(&mut output, &request).unwrap();
        assert_eq!(output.last(), Some(&b'\n'));
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    }
}
