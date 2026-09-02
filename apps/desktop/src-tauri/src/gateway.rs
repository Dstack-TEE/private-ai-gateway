//! Sidecar lifecycle and the desktop's view of the gateway.
//!
//! The ACI verifier sidecar listens on a private loopback port; the stable
//! local endpoint belongs to the in-process proxy. A session is only opened
//! for requests once the sidecar's verified identity and the catalog read
//! through it are both in, and they are published to the proxy together under
//! one generation; any loss of verification revokes the session and clears
//! the catalog at once. Every state change is published to the window and
//! mirrored into the tray menu.

use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use desktop_gateway::proxy::{ProxyEvent, ProxyState, Session};
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use tokio::sync::mpsc::Receiver;
use url::Url;

use crate::contracts::{
    CatalogSummary, GatewayIdentity, GatewayState, RequestActivity, SourceProvenance,
    StartGatewayConfig, VerificationCheck,
};

/// The stable loopback HTTP endpoint agents are configured with.
pub const LOCAL_ENDPOINT: &str = "http://127.0.0.1:4180";
pub const LOCAL_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4180);
const EVENT_SCHEMA_VERSION: u64 = 1;
const MAX_ACTIVITY: usize = 50;
const MAX_DIAGNOSTIC_BYTES: usize = 4_096;
const MAX_EVENT_BYTES: usize = 1_048_576;

pub struct GatewayManager {
    inner: Mutex<RuntimeState>,
    proxy: Arc<ProxyState>,
}

struct RuntimeState {
    child: Option<CommandChild>,
    /// Bumped on every start and stop; doubles as the proxy session generation.
    generation: u64,
    /// Bumped on every identity report and catalog refresh; only the newest
    /// epoch may publish a session, so a slow older read never wins.
    epoch: u64,
    stdout: Vec<u8>,
    diagnostic: VecDeque<u8>,
    /// Where the sidecar listens once ready; private to this process.
    sidecar_url: Option<String>,
    /// The sidecar reported a verified identity for this generation.
    identity_ready: bool,
    /// Last published catalog, kept across sessions to report removed models.
    last_catalog: Option<CatalogSummary>,
    state: GatewayState,
}

impl GatewayManager {
    pub fn new(proxy: Arc<ProxyState>) -> Self {
        Self {
            inner: Mutex::new(RuntimeState {
                child: None,
                generation: 0,
                epoch: 0,
                stdout: Vec::new(),
                diagnostic: VecDeque::with_capacity(MAX_DIAGNOSTIC_BYTES),
                sidecar_url: None,
                identity_ready: false,
                last_catalog: None,
                state: GatewayState::default(),
            }),
            proxy,
        }
    }

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
        if let Some(error) = &runtime.state.endpoint_error {
            return Err(format!("The local endpoint is unavailable: {error}"));
        }
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
        runtime.sidecar_url = None;
        runtime.identity_ready = false;
        runtime.state = GatewayState {
            status: "verifying".to_string(),
            progress: Some("Starting the verifier".to_string()),
            remote_url: Some(remote_url.clone()),
            config: StartGatewayConfig {
                remote_url,
                require_production_os: config.require_production_os,
            },
            ..Self::carried(&runtime.state)
        };
        let state = runtime.state.clone();
        drop(runtime);

        self.proxy.publish(Session {
            generation,
            epoch: 0,
            ..Session::default()
        });
        publish_state(app, &state);
        spawn_event_reader(app.clone(), generation, receiver);
        Ok(state)
    }

    /// Stop the sidecar in any state, including while verifying. Requests
    /// already forwarded fail with the sidecar; no new request is accepted.
    pub fn stop(&self, app: &AppHandle) -> Result<GatewayState, String> {
        let mut runtime = self.lock()?;
        let child = runtime.child.take();
        runtime.generation = runtime.generation.wrapping_add(1);
        let generation = runtime.generation;
        runtime.stdout.clear();
        runtime.diagnostic.clear();
        runtime.sidecar_url = None;
        runtime.identity_ready = false;
        runtime.state = Self::carried(&runtime.state);
        let state = runtime.state.clone();
        let epoch = runtime.epoch;
        drop(runtime);

        self.proxy.publish(Session {
            generation,
            epoch,
            ..Session::default()
        });
        if let Some(child) = child {
            child
                .kill()
                .map_err(|error| format!("Cannot stop ACI executable: {error}"))?;
        }
        publish_state(app, &state);
        Ok(state)
    }

    /// What survives a stop or restart of the sidecar: settings, key status,
    /// the local endpoint, and recent activity. The catalog does not: it
    /// belongs to a verified session.
    fn carried(previous: &GatewayState) -> GatewayState {
        GatewayState {
            config: previous.config.clone(),
            api_key_saved: previous.api_key_saved,
            proxy_url: previous.proxy_url.clone(),
            endpoint_error: previous.endpoint_error.clone(),
            activity: previous.activity.clone(),
            ..GatewayState::default()
        }
    }

    /// Tray switch: stop when a sidecar is running, otherwise start with the
    /// last configuration. Failures surface in the state instead of a result.
    pub fn toggle(&self, app: &AppHandle) {
        let (running, config) = match self.lock() {
            Ok(runtime) => (runtime.child.is_some(), runtime.state.config.clone()),
            Err(_) => return,
        };
        let result = if running {
            self.stop(app)
        } else {
            self.start(app, config)
        };
        if let Err(message) = result {
            self.report_error(app, message);
        }
    }

    pub fn set_api_key_saved(&self, app: &AppHandle, saved: bool) {
        self.update(app, |state| state.api_key_saved = saved);
    }

    /// Record whether the stable local endpoint is bound. A failure blocks
    /// starting and connecting until the app is relaunched.
    pub fn set_endpoint(&self, app: &AppHandle, bound: Result<(), String>) {
        self.update(app, |state| match bound {
            Ok(()) => {
                state.proxy_url = Some(LOCAL_ENDPOINT.to_string());
                state.endpoint_error = None;
            }
            Err(error) => {
                state.proxy_url = None;
                state.endpoint_error = Some(error);
            }
        });
    }

    pub fn report_error(&self, app: &AppHandle, message: String) {
        self.update(app, |state| state.error = Some(message));
    }

    /// A request the local proxy answered itself, before any receipt.
    pub fn record_proxy_event(&self, app: &AppHandle, event: ProxyEvent) {
        self.update(app, |state| {
            state.activity.insert(
                0,
                RequestActivity {
                    method: event.method,
                    path: event.path,
                    status: event.status,
                    streamed: false,
                    receipt_id: None,
                    verified: None,
                    detail: event.detail,
                    at: event.at,
                    agent: event.agent,
                    locally_constrained: None,
                    rewritten: None,
                },
            );
            state.activity.truncate(MAX_ACTIVITY);
        });
    }

    /// Re-read the catalog under a new epoch and republish the session. A
    /// result from an older epoch or generation is dropped.
    pub async fn refresh_catalog(&self, app: &AppHandle) -> Result<GatewayState, String> {
        let (generation, epoch) = {
            let mut runtime = self.lock()?;
            if !runtime.identity_ready {
                return Err("Start the gateway and wait for verification first".to_string());
            }
            runtime.epoch += 1;
            let epoch = runtime.epoch;
            // Publishing the new epoch invalidates any read still in flight;
            // the session stays verified with its current catalog meanwhile.
            let current = self.proxy.session();
            self.proxy.publish(Session { epoch, ..current });
            (runtime.generation, epoch)
        };
        self.load_catalog(app, generation, epoch).await?;
        self.snapshot()
    }

    async fn load_catalog(
        &self,
        app: &AppHandle,
        generation: u64,
        epoch: u64,
    ) -> Result<(), String> {
        let result = self.proxy.fetch_catalog(generation, epoch).await;
        let mut runtime = self.lock()?;
        if runtime.generation != generation || runtime.epoch != epoch || !runtime.identity_ready {
            return Ok(());
        }
        let Some(sidecar_url) = runtime.sidecar_url.clone() else {
            return Ok(());
        };
        let outcome = match result {
            Ok(catalog) => {
                let summary = CatalogSummary::from_catalog(&catalog, runtime.last_catalog.as_ref());
                runtime.last_catalog = Some(summary.clone());
                runtime.state.catalog = Some(summary);
                runtime.state.status = "verified".to_string();
                runtime.state.progress = None;
                runtime.state.error = None;
                self.proxy.publish(Session {
                    generation,
                    epoch,
                    base_url: Some(sidecar_url),
                    verified: true,
                    catalog: Some(catalog),
                });
                Ok(())
            }
            Err(error) => {
                let message = format!("Cannot read the verified model list: {error}");
                if runtime.state.status == "verified" {
                    // A failed manual refresh keeps the session it had.
                    runtime.state.error = Some(message.clone());
                    Err(message)
                } else {
                    // The first catalog read failed: never leave a running
                    // sidecar behind a stopped-looking UI. Terminate it and
                    // land in a plain, retryable error state.
                    drop(runtime);
                    let _ = self.fail(app, generation, message.clone());
                    return Err(message);
                }
            }
        };
        let state = runtime.state.clone();
        drop(runtime);
        publish_state(app, &state);
        outcome
    }

    fn update(&self, app: &AppHandle, change: impl FnOnce(&mut GatewayState)) {
        let Ok(mut runtime) = self.lock() else {
            return;
        };
        change(&mut runtime.state);
        let state = runtime.state.clone();
        drop(runtime);
        publish_state(app, &state);
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
            return Err("ACI emitted an unknown event schema".to_string());
        }

        let event_type = required_string(object, "type")?;
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }

        let mut load_catalog = false;
        match event_type.as_str() {
            // Identity in (or rotated): a new epoch; the session stays closed
            // until the catalog read through this identity is in too.
            "ready" | "identity_updated" => {
                apply_identity_event(&mut runtime.state, object)?;
                if event_type == "ready" {
                    runtime.state.remote_url = Some(required_string(object, "remote_url")?);
                    runtime.sidecar_url = Some(required_string(object, "proxy_url")?);
                }
                runtime.identity_ready = true;
                runtime.epoch += 1;
                runtime.state.status = "verifying".to_string();
                runtime.state.progress = Some("Reading the verified model list".to_string());
                runtime.state.catalog = None;
                load_catalog = true;
            }
            "request_complete" => apply_request_event(&mut runtime.state, object)?,
            // Verification lost: one atomic barrier. The epoch moves so a
            // read still in flight can neither publish nor clear this error,
            // and the identity must be reported again before anything opens.
            "blocked" => {
                runtime.epoch += 1;
                runtime.identity_ready = false;
                runtime.state.status = "blocked".to_string();
                runtime.state.progress = None;
                runtime.state.catalog = None;
                runtime.state.error = Some(
                    optional_string(object, "reason")
                        .unwrap_or_else(|| "ACI blocked forwarding".to_string()),
                );
            }
            "fatal" => {
                runtime.epoch += 1;
                runtime.identity_ready = false;
                runtime.state.status = "error".to_string();
                runtime.state.progress = None;
                runtime.state.catalog = None;
                runtime.state.error = Some(
                    optional_string(object, "message").unwrap_or_else(|| "ACI failed".to_string()),
                );
            }
            _ => return Ok(()),
        }

        let state = runtime.state.clone();
        let epoch = runtime.epoch;
        if state.status != "verified" {
            // Any state other than verified revokes the session at once.
            self.proxy.publish(Session {
                generation,
                epoch,
                base_url: runtime.sidecar_url.clone(),
                ..Session::default()
            });
        }
        drop(runtime);
        publish_state(app, &state);
        if load_catalog {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let manager = app.state::<GatewayManager>();
                let _ = manager.load_catalog(&app, generation, epoch).await;
            });
        }
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
        runtime.epoch += 1;
        runtime.identity_ready = false;
        runtime.state.catalog = None;
        runtime.state.progress = None;
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
        let epoch = runtime.epoch;
        drop(runtime);
        self.proxy.publish(Session {
            generation,
            epoch,
            ..Session::default()
        });
        publish_state(app, &state);
        Ok(())
    }

    fn fail(&self, app: &AppHandle, generation: u64, message: String) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        let child = runtime.child.take();
        runtime.epoch += 1;
        runtime.identity_ready = false;
        runtime.state.status = "error".to_string();
        runtime.state.progress = None;
        runtime.state.catalog = None;
        runtime.state.error = Some(message);
        let state = runtime.state.clone();
        let epoch = runtime.epoch;
        drop(runtime);
        self.proxy.publish(Session {
            generation,
            epoch,
            ..Session::default()
        });
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

/// Record the sidecar's identity and checks; the status is decided by the
/// caller once the catalog is in.
fn apply_identity_event(
    state: &mut GatewayState,
    event: &Map<String, Value>,
) -> Result<(), String> {
    state.identity = Some(parse_identity(event)?);
    state.checks = parse_checks(event.get("verification"));
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
        at: now_secs(),
        // The local proxy tags each forwarded request with its agent.
        agent: optional_string(event, "tag"),
        locally_constrained: event.get("locally_constrained").and_then(Value::as_bool),
        rewritten: event.get("rewritten").and_then(Value::as_bool),
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    crate::tray::sync(app, state);
    let _ = app.emit("gateway://state", state);
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
    fn identity_alone_does_not_verify_and_requests_are_attributed() {
        let identity = json!({
            "type": "ready",
            "schema_version": 1,
            "remote_url": "https://tee.redpill.ai",
            "proxy_url": "http://127.0.0.1:53211",
            "tee_type": "tdx",
            "trust_level": "hardware_verified",
            "keyset_digest": "sha256:keyset",
            "keyset_not_after": 2_000_000_000,
            "source_provenance": { "repo_commit": "abc123" },
            "verification": { "checks": [{
                "id": "id-1", "section": "9.1(1)", "title": "Hardware quote",
                "status": "pass", "detail": "TDX quote verified"
            }]}
        });
        let mut state = GatewayState::default();
        apply_identity_event(&mut state, identity.as_object().unwrap()).unwrap();
        assert_eq!(
            state.status, "stopped",
            "status is decided once the catalog is in"
        );
        assert!(state.identity.is_some());

        let request = json!({
            "method": "POST", "path": "/v1/messages", "status": 200, "streamed": true,
            "receipt_id": "rcpt-1", "verified": null,
            "detail": "receipt rcpt-1 recorded", "tag": "claude-code"
        });
        apply_request_event(&mut state, request.as_object().unwrap()).unwrap();
        let verdict = json!({
            "method": "POST", "path": "/v1/messages", "status": 200, "streamed": true,
            "receipt_id": "rcpt-1", "verified": true, "rewritten": true,
            "locally_constrained": true,
            "detail": "receipt verified", "tag": "claude-code"
        });
        apply_request_event(&mut state, verdict.as_object().unwrap()).unwrap();

        assert_eq!(state.activity.len(), 1);
        let item = &state.activity[0];
        assert_eq!(item.agent.as_deref(), Some("claude-code"));
        assert_eq!(item.verified, Some(true));
        assert_eq!(item.rewritten, Some(true));
        assert_eq!(item.locally_constrained, Some(true));
    }
}
