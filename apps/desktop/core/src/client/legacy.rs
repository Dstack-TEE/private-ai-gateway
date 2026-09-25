//! Stops a 0.1.4 to 0.2 beta backend, which answers only its NDJSON protocol
//! (version 3) on the legacy endpoint, so an update can replace it with a
//! backend of this build. Remove in 0.3; see "Removal in 0.3" in
//! `docs/configuration.md`.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};

use super::{connection_error, executable_matches, OTHER_INSTALLATION, REQUEST_TIMEOUT};
use crate::{protocol::ShutdownMode, transport};

const PROTOCOL_VERSION: u16 = 3;
const MAX_FRAME_BYTES: u64 = 1024 * 1024;
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The first line a legacy backend sends on every connection.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hello {
    protocol_version: u16,
    product: String,
    instance_id: String,
    process_id: u32,
    executable: String,
}

pub(super) async fn is_running() -> bool {
    hello().await.is_ok()
}

/// Asks the legacy backend to shut down and returns its process ID to wait for.
pub(super) async fn shutdown(expected: Option<&Path>, mode: ShutdownMode) -> Result<u32, String> {
    let (mut reader, hello) = hello().await.map_err(connection_error)?;
    if let Some(expected) = expected {
        if !executable_matches(&hello.executable, expected)? {
            return Err(OTHER_INSTALLATION.into());
        }
    }
    let request = json!({
        "version": PROTOCOL_VERSION,
        "id": 1,
        "command": {
            "method": "shutdown",
            "params": { "instance_id": hello.instance_id, "mode": mode },
        },
    });
    let mut frame = request.to_string().into_bytes();
    frame.push(b'\n');
    let response = tokio::time::timeout(REQUEST_TIMEOUT, async {
        reader.get_mut().write_all(&frame).await?;
        line(&mut reader).await
    })
    .await
    .map_err(|_| connection_error(std::io::ErrorKind::TimedOut.into()))?
    .map_err(connection_error)?;
    match serde_json::from_str::<Value>(&response)
        .ok()
        .as_ref()
        .and_then(|response| response.get("outcome"))
    {
        // Busy: its shutdown is already under way, as for the current API.
        Some(outcome)
            if outcome.get("result").is_some()
                || outcome.pointer("/error/code").and_then(Value::as_str) == Some("busy") =>
        {
            Ok(hello.process_id)
        }
        Some(outcome) => Err(
            match outcome.pointer("/error/message").and_then(Value::as_str) {
                Some(message) => format!("The previous backend did not stop: {message}"),
                None => "The previous backend did not stop.".into(),
            },
        ),
        None => Err(connection_error(std::io::ErrorKind::InvalidData.into())),
    }
}

async fn hello() -> std::io::Result<(BufReader<transport::ClientStream>, Hello)> {
    let stream = transport::connect(&transport::legacy_endpoint_path()?).await?;
    let mut reader = BufReader::new(stream);
    let hello = tokio::time::timeout(HELLO_TIMEOUT, line(&mut reader))
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    let hello = serde_json::from_str::<Hello>(&hello)
        .ok()
        .filter(|hello| {
            hello.protocol_version == PROTOCOL_VERSION
                && hello.product == crate::brand::APP_IDENTIFIER
        })
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    Ok((reader, hello))
}

/// One bounded NDJSON frame.
async fn line(reader: &mut BufReader<impl AsyncRead + Unpin>) -> std::io::Result<String> {
    let mut line = String::new();
    reader.take(MAX_FRAME_BYTES).read_line(&mut line).await?;
    if !line.ends_with('\n') {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(line)
}
