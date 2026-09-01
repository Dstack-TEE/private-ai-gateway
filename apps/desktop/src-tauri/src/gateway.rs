use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use tokio::sync::mpsc::Receiver;
use url::Url;

use crate::contracts::{
    GatewayIdentity, GatewayState, RequestActivity, SourceProvenance, StartGatewayConfig,
    VerificationCheck,
};

const EVENT_SCHEMA_VERSION: u64 = 1;
const MAX_ACTIVITY: usize = 30;
const MAX_DIAGNOSTIC_BYTES: usize = 4_096;
const MAX_EVENT_BYTES: usize = 1_048_576;

pub struct GatewayManager {
    inner: Mutex<RuntimeState>,
}

struct RuntimeState {
    child: Option<CommandChild>,
    generation: u64,
    stdout: Vec<u8>,
    diagnostic: VecDeque<u8>,
    state: GatewayState,
}

impl Default for GatewayManager {
    fn default() -> Self {
        Self {
            inner: Mutex::new(RuntimeState {
                child: None,
                generation: 0,
                stdout: Vec::new(),
                diagnostic: VecDeque::with_capacity(MAX_DIAGNOSTIC_BYTES),
                state: GatewayState::default(),
            }),
        }
    }
}

impl GatewayManager {
    pub fn snapshot(&self) -> Result<GatewayState, String> {
        Ok(self.lock()?.state.clone())
    }

    pub fn start(
        &self,
        app: &AppHandle,
        config: StartGatewayConfig,
    ) -> Result<GatewayState, String> {
        let remote_url = validate_remote_url(&config.remote_url)?;

        let mut runtime = self.lock()?;
        if runtime.child.is_some() {
            return Err("Gateway is already running".to_string());
        }

        let mut args = Vec::with_capacity(9);
        if config.require_production_os {
            args.push("--require-production-os".to_string());
        }
        args.extend([
            "serve".to_string(),
            remote_url.clone(),
            "--listen".to_string(),
            "127.0.0.1:0".to_string(),
            "--control".to_string(),
            "127.0.0.1:0".to_string(),
            "--json-events".to_string(),
            "--verify-receipts".to_string(),
        ]);

        let command = app
            .shell()
            .sidecar("aci")
            .map_err(|error| format!("Cannot locate bundled ACI executable: {error}"))?
            .args(args)
            .set_raw_out(true);
        let (receiver, child) = command
            .spawn()
            .map_err(|error| format!("Cannot start bundled ACI executable: {error}"))?;

        runtime.generation = runtime.generation.wrapping_add(1);
        let generation = runtime.generation;
        runtime.child = Some(child);
        runtime.stdout.clear();
        runtime.diagnostic.clear();
        runtime.state = GatewayState {
            status: "verifying".to_string(),
            remote_url: Some(remote_url),
            ..GatewayState::default()
        };
        let state = runtime.state.clone();
        drop(runtime);

        publish_state(app, &state);
        spawn_event_reader(app.clone(), generation, receiver);
        Ok(state)
    }

    pub fn stop(&self, app: &AppHandle) -> Result<GatewayState, String> {
        let mut runtime = self.lock()?;
        let child = runtime.child.take();
        runtime.generation = runtime.generation.wrapping_add(1);
        runtime.stdout.clear();
        runtime.diagnostic.clear();
        runtime.state = GatewayState::default();
        let state = runtime.state.clone();
        drop(runtime);

        if let Some(child) = child {
            child
                .kill()
                .map_err(|error| format!("Cannot stop ACI executable: {error}"))?;
        }
        publish_state(app, &state);
        Ok(state)
    }

    fn handle_stdout(&self, app: &AppHandle, generation: u64, bytes: &[u8]) -> Result<(), String> {
        let lines = {
            let mut runtime = self.lock()?;
            if runtime.generation != generation {
                return Ok(());
            }
            if runtime.stdout.len().saturating_add(bytes.len()) > MAX_EVENT_BYTES {
                drop(runtime);
                self.fail(
                    app,
                    generation,
                    "ACI emitted an oversized event".to_string(),
                )?;
                return Ok(());
            }
            runtime.stdout.extend_from_slice(bytes);

            let mut lines = Vec::new();
            while let Some(position) = runtime.stdout.iter().position(|byte| *byte == b'\n') {
                let line = runtime.stdout.drain(..=position).collect::<Vec<_>>();
                lines.push(line);
            }
            lines
        };

        for line in lines {
            let line = String::from_utf8(line)
                .map_err(|_| "ACI emitted non-UTF-8 event data".to_string())?;
            let line = line.trim();
            if !line.is_empty() {
                self.handle_line(app, generation, line)?;
            }
        }
        Ok(())
    }

    fn handle_line(&self, app: &AppHandle, generation: u64, line: &str) -> Result<(), String> {
        let event: Value = serde_json::from_str(line)
            .map_err(|_| "ACI emitted invalid JSON event data".to_string())?;
        let object = event
            .as_object()
            .ok_or_else(|| "ACI emitted an invalid event".to_string())?;
        if object.get("schema_version").and_then(Value::as_u64) != Some(EVENT_SCHEMA_VERSION) {
            return Err("ACI emitted an unsupported event schema".to_string());
        }

        let event_type = required_string(object, "type")?;
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }

        match event_type.as_str() {
            "ready" => apply_identity_event(&mut runtime.state, object, true)?,
            "identity_updated" => apply_identity_event(&mut runtime.state, object, false)?,
            "request_complete" => apply_request_event(&mut runtime.state, object)?,
            "blocked" => {
                runtime.state.status = "blocked".to_string();
                runtime.state.error = Some(
                    optional_string(object, "reason")
                        .unwrap_or_else(|| "ACI blocked forwarding".to_string()),
                );
            }
            "fatal" => {
                let message =
                    optional_string(object, "message").unwrap_or_else(|| "ACI failed".to_string());
                runtime.state.status = "error".to_string();
                runtime.state.error = Some(message);
            }
            _ => return Ok(()),
        }

        let state = runtime.state.clone();
        drop(runtime);
        publish_state(app, &state);
        Ok(())
    }

    fn append_diagnostic(&self, generation: u64, bytes: &[u8]) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        for byte in bytes {
            if runtime.diagnostic.len() == MAX_DIAGNOSTIC_BYTES {
                runtime.diagnostic.pop_front();
            }
            runtime.diagnostic.push_back(*byte);
        }
        Ok(())
    }

    fn terminated(&self, app: &AppHandle, generation: u64) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        runtime.child = None;
        if runtime.state.status != "error" {
            let diagnostic =
                String::from_utf8_lossy(&runtime.diagnostic.iter().copied().collect::<Vec<_>>())
                    .trim()
                    .to_string();
            runtime.state.status = "error".to_string();
            runtime.state.error = Some(if diagnostic.is_empty() {
                "ACI stopped unexpectedly".to_string()
            } else {
                format!("ACI stopped unexpectedly: {diagnostic}")
            });
        }
        let state = runtime.state.clone();
        drop(runtime);
        publish_state(app, &state);
        Ok(())
    }

    fn fail(&self, app: &AppHandle, generation: u64, message: String) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        let child = runtime.child.take();
        runtime.state.status = "error".to_string();
        runtime.state.error = Some(message);
        let state = runtime.state.clone();
        drop(runtime);
        if let Some(child) = child {
            let _ = child.kill();
        }
        publish_state(app, &state);
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, RuntimeState>, String> {
        self.inner
            .lock()
            .map_err(|_| "Gateway runtime state is unavailable".to_string())
    }
}

fn spawn_event_reader(app: AppHandle, generation: u64, mut receiver: Receiver<CommandEvent>) {
    tauri::async_runtime::spawn(async move {
        while let Some(event) = receiver.recv().await {
            let manager = app.state::<GatewayManager>();
            let result = match event {
                CommandEvent::Stdout(bytes) => manager.handle_stdout(&app, generation, &bytes),
                CommandEvent::Stderr(bytes) => manager.append_diagnostic(generation, &bytes),
                CommandEvent::Error(error) => {
                    manager.fail(&app, generation, format!("ACI process error: {error}"))
                }
                CommandEvent::Terminated(_) => manager.terminated(&app, generation),
                _ => Ok(()),
            };
            if let Err(error) = result {
                let _ = manager.fail(&app, generation, error);
            }
        }
    });
}

fn validate_remote_url(value: &str) -> Result<String, String> {
    let mut url = Url::parse(value.trim())
        .map_err(|_| "Gateway URL must be a valid HTTP or HTTPS URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Gateway URL must use HTTP or HTTPS".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Gateway URL must not contain credentials".to_string());
    }
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn apply_identity_event(
    state: &mut GatewayState,
    event: &Map<String, Value>,
    ready: bool,
) -> Result<(), String> {
    let identity = parse_identity(event)?;
    if ready {
        state.remote_url = Some(required_string(event, "remote_url")?);
        state.proxy_url = Some(required_string(event, "proxy_url")?);
        state.control_url = Some(required_string(event, "control_url")?);
    }
    state.identity = Some(identity);
    state.checks = parse_checks(event.get("verification"));
    state.status = "verified".to_string();
    state.error = None;
    Ok(())
}

fn parse_identity(event: &Map<String, Value>) -> Result<GatewayIdentity, String> {
    let source = event.get("source_provenance").and_then(Value::as_object);
    let capabilities = event.get("service_capabilities").and_then(Value::as_object);

    Ok(GatewayIdentity {
        tee_type: required_string(event, "tee_type")?,
        trust_level: required_string(event, "trust_level")?,
        keyset_digest: required_string(event, "keyset_digest")?,
        keyset_not_after: event
            .get("keyset_not_after")
            .and_then(Value::as_u64)
            .ok_or_else(|| "ACI emitted an invalid identity event".to_string())?,
        tls_spki: optional_string(event, "tls_spki"),
        source: SourceProvenance {
            repo_url: source.and_then(|value| optional_string(value, "repo_url")),
            repo_commit: source.and_then(|value| optional_string(value, "repo_commit")),
            image_digest: source.and_then(|value| optional_string(value, "image_digest")),
        },
        serving: capabilities
            .and_then(|value| optional_string(value, "serving"))
            .unwrap_or_else(|| "aggregator".to_string()),
        supported_e2ee_versions: capabilities
            .and_then(|value| value.get("supported_e2ee_versions"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_checks(value: Option<&Value>) -> Vec<VerificationCheck> {
    value
        .and_then(Value::as_object)
        .and_then(|verification| verification.get("checks"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let item = item.as_object()?;
            let status = optional_string(item, "status")?;
            if !matches!(status.as_str(), "pass" | "fail" | "skip" | "info") {
                return None;
            }
            Some(VerificationCheck {
                id: optional_string(item, "id")?,
                section: optional_string(item, "section")?,
                title: optional_string(item, "title")?,
                status,
                detail: optional_string(item, "detail").unwrap_or_default(),
            })
        })
        .collect()
}

fn apply_request_event(state: &mut GatewayState, event: &Map<String, Value>) -> Result<(), String> {
    let status = event
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| "ACI emitted an invalid request event".to_string())?;
    let receipt_id = optional_string(event, "receipt_id");
    let activity = RequestActivity {
        method: required_string(event, "method")?,
        path: required_string(event, "path")?,
        status,
        streamed: event
            .get("streamed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        receipt_id: receipt_id.clone(),
        verified: event.get("verified").and_then(Value::as_bool),
        detail: optional_string(event, "detail").unwrap_or_default(),
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    if let Some(existing) = receipt_id.and_then(|id| {
        state
            .activity
            .iter_mut()
            .find(|item| item.receipt_id.as_deref() == Some(id.as_str()))
    }) {
        let at = existing.at;
        *existing = RequestActivity { at, ..activity };
    } else {
        state.activity.insert(0, activity);
    }
    state.activity.truncate(MAX_ACTIVITY);
    Ok(())
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    optional_string(object, key).ok_or_else(|| format!("ACI event is missing {key}"))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn publish_state(app: &AppHandle, state: &GatewayState) {
    if let Some(tray) = app.tray_by_id("gateway") {
        let _ = tray.set_tooltip(Some(format!(
            "Private AI Gateway - {}",
            status_label(&state.status)
        )));
    }
    let _ = app.emit("gateway://state", state);
}

fn status_label(status: &str) -> &str {
    match status {
        "verifying" => "Verifying",
        "verified" => "Verified",
        "blocked" => "Blocked",
        "error" => "Error",
        _ => "Stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_remote_url_and_rejects_credentials() {
        assert_eq!(
            validate_remote_url(" https://tee.redpill.ai/ ").unwrap(),
            "https://tee.redpill.ai"
        );
        assert!(validate_remote_url("https://token@tee.redpill.ai").is_err());
        assert!(validate_remote_url("file:///tmp/gateway").is_err());
    }

    #[test]
    fn applies_identity_and_request_events() {
        let identity = json!({
            "type": "ready",
            "schema_version": 1,
            "remote_url": "https://tee.redpill.ai",
            "proxy_url": "http://127.0.0.1:4180",
            "control_url": "http://127.0.0.1:4181",
            "tee_type": "tdx",
            "trust_level": "hardware_verified",
            "keyset_digest": "sha256:keyset",
            "keyset_not_after": 2_000_000_000,
            "tls_spki": "sha256:spki",
            "source_provenance": { "repo_commit": "abc123" },
            "service_capabilities": {
                "serving": "aggregator",
                "supported_e2ee_versions": ["2"]
            },
            "verification": {
                "checks": [{
                    "id": "id-1",
                    "section": "9.1(1)",
                    "title": "Hardware quote",
                    "status": "pass",
                    "detail": "TDX quote verified"
                }]
            }
        });
        let mut state = GatewayState::default();
        apply_identity_event(&mut state, identity.as_object().unwrap(), true).unwrap();

        let request = json!({
            "method": "POST",
            "path": "/v1/chat/completions",
            "status": 200,
            "streamed": true,
            "receipt_id": "rcpt-1",
            "verified": true,
            "detail": "receipt verified"
        });
        apply_request_event(&mut state, request.as_object().unwrap()).unwrap();

        let updated_request = json!({
            "method": "POST",
            "path": "/v1/chat/completions",
            "status": 200,
            "streamed": true,
            "receipt_id": "rcpt-1",
            "verified": false,
            "detail": "receipt failed"
        });
        apply_request_event(&mut state, updated_request.as_object().unwrap()).unwrap();

        assert_eq!(state.status, "verified");
        assert_eq!(state.identity.unwrap().tee_type, "tdx");
        assert_eq!(state.checks.len(), 1);
        assert_eq!(state.activity.len(), 1);
        assert_eq!(state.activity[0].receipt_id.as_deref(), Some("rcpt-1"));
        assert_eq!(state.activity[0].verified, Some(false));
    }
}
