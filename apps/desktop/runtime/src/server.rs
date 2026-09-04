use std::{path::PathBuf, sync::Arc};

use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use crate::{
    contracts::{ConfidentialProfileInput, ConnectOptions, LocalApiConfig, StartGatewayConfig},
    controller::DesktopRuntime,
    protocol::{Error, Event, Method, Request, Response, MAX_MESSAGE_BYTES},
    usage::UsageQuery,
};

pub async fn run_stdio(runtime: Arc<DesktopRuntime>) -> Result<(), Error> {
    let (outgoing, mut messages) = mpsc::channel::<Value>(128);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = messages.recv().await {
            let mut bytes = serde_json::to_vec(&message)
                .map_err(|_| Error::new("io_error", "Cannot encode a runtime response"))?;
            bytes.push(b'\n');
            stdout
                .write_all(&bytes)
                .await
                .map_err(|_| Error::new("io_error", "Cannot write to the desktop client"))?;
            stdout
                .flush()
                .await
                .map_err(|_| Error::new("io_error", "Cannot write to the desktop client"))?;
        }
        Ok::<(), Error>(())
    });

    let mut states = runtime.subscribe();
    let state_output = outgoing.clone();
    let state_task = tokio::spawn(async move {
        while states.changed().await.is_ok() {
            let payload = match serde_json::to_value(states.borrow().clone()) {
                Ok(payload) => payload,
                Err(_) => continue,
            };
            let Ok(event) = serde_json::to_value(Event::state(payload)) else {
                continue;
            };
            if state_output.send(event).await.is_err() {
                break;
            }
        }
    });

    let mut stdin = BufReader::new(tokio::io::stdin());
    let result = loop {
        let request = match read_request(&mut stdin).await {
            Ok(Some(request)) => request,
            Ok(None) => break Ok(()),
            Err(error) => {
                let response = Response::failure("protocol".to_string(), error.clone());
                send(&outgoing, response).await?;
                break Err(error);
            }
        };
        let id = request.id.clone();
        let method = match request.validate() {
            Ok(method) => method,
            Err(error) => {
                send(&outgoing, Response::failure(id, error)).await?;
                continue;
            }
        };
        if method == Method::Shutdown {
            let response = match runtime.shutdown().await {
                Ok(()) => Response::success(id, Value::Null),
                Err(message) => Response::failure(id, operation_error(message)),
            };
            send(&outgoing, response).await?;
            break Ok(());
        }
        let response = match dispatch(&runtime, method, request.params).await {
            Ok(result) => Response::success(id, result),
            Err(error) => Response::failure(id, error),
        };
        send(&outgoing, response).await?;
    };

    let _ = runtime.shutdown().await;
    state_task.abort();
    drop(outgoing);
    writer
        .await
        .map_err(|_| Error::new("io_error", "Desktop runtime output task failed"))??;
    result
}

async fn dispatch(
    runtime: &Arc<DesktopRuntime>,
    method: Method,
    params: Value,
) -> Result<Value, Error> {
    let result = match method {
        Method::GetState => serialize(runtime.state()?),
        Method::Start => {
            let params: StartParams = parse(params)?;
            serialize(runtime.start(params.config)?)
        }
        Method::Stop => serialize(runtime.stop()?),
        Method::VerifyConfiguration => {
            let params: VerifyParams = parse(params)?;
            serialize(
                runtime
                    .verify_configuration(params.profile, params.require_production_os, params.key)
                    .await?,
            )
        }
        Method::ActivateProfile => {
            let params: ProfileParams = parse(params)?;
            serialize(runtime.activate_profile(params.profile_id)?)
        }
        Method::DeleteProfile => {
            let params: ProfileParams = parse(params)?;
            serialize(runtime.delete_profile(params.profile_id)?)
        }
        Method::ClearApiKey => serialize(runtime.clear_api_key()?),
        Method::QueryUsage => {
            let params: UsageParams = parse(params)?;
            serialize(runtime.query_usage(params.query)?)
        }
        Method::ExportUsageCsv => {
            let params: ExportUsageParams = parse(params)?;
            serialize(runtime.export_usage_csv(params.query, PathBuf::from(params.path))?)
        }
        Method::ClearUsage => serialize(runtime.clear_usage()?),
        Method::RefreshCatalog => serialize(runtime.refresh_catalog().await?),
        Method::ListAgents => serialize(runtime.list_agents()?),
        Method::PreviewAgent => {
            let params: AgentParams = parse(params)?;
            serialize(runtime.preview_agent(params.agent_id, params.connect, params.options)?)
        }
        Method::ApplyAgent => {
            let params: ApplyAgentParams = parse(params)?;
            serialize(runtime.apply_agent(
                params.agent_id,
                params.connect,
                params.revision,
                params.options,
            )?)
        }
        Method::DisconnectAllAgents => serialize(runtime.disconnect_all_agents()?),
        Method::GetClientKey => serialize(runtime.client_key()?),
        Method::RotateClientKey => serialize(runtime.rotate_client_key()?),
        Method::SaveLocalApiConfig => {
            let params: LocalApiParams = parse(params)?;
            serialize(runtime.save_local_api_config(params.config).await?)
        }
        Method::Shutdown => Value::Null,
    };
    Ok(result)
}

async fn send(output: &mpsc::Sender<Value>, response: Response) -> Result<(), Error> {
    let value = serde_json::to_value(response)
        .map_err(|_| Error::new("io_error", "Cannot encode a runtime response"))?;
    output
        .send(value)
        .await
        .map_err(|_| Error::new("io_error", "Desktop client output is closed"))
}

async fn read_request(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Request>, Error> {
    let mut bytes = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
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

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, Error> {
    serde_json::from_value(params)
        .map_err(|_| Error::new("invalid_params", "Desktop runtime parameters are invalid"))
}

fn serialize(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or_else(|_| json!(null))
}

fn operation_error(message: String) -> Error {
    Error::new("operation_failed", message)
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        operation_error(message)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartParams {
    config: StartGatewayConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifyParams {
    profile: ConfidentialProfileInput,
    require_production_os: bool,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileParams {
    profile_id: String,
}

#[derive(Deserialize)]
struct UsageParams {
    query: UsageQuery,
}

#[derive(Deserialize)]
struct ExportUsageParams {
    query: UsageQuery,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentParams {
    agent_id: String,
    connect: bool,
    #[serde(default)]
    options: ConnectOptions,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyAgentParams {
    agent_id: String,
    connect: bool,
    revision: String,
    #[serde(default)]
    options: ConnectOptions,
}

#[derive(Deserialize)]
struct LocalApiParams {
    config: LocalApiConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_async_messages() {
        let data = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut reader = BufReader::new(&data[..]);
        assert_eq!(
            read_request(&mut reader).await.unwrap_err().code,
            "message_too_large"
        );
    }
}
