//! The management API client. It speaks HTTP/1.1 to the service's local
//! endpoint with hyper's client connection API over the verified Unix socket
//! or named pipe (what `hyperlocal` does for Unix sockets), one connection per
//! call like `docker` over `docker.sock`: `GET /api/version` first, so a
//! client refuses another build, then the call.
//!
//! Calls block: each runs a current-thread Tokio runtime of its own, so they
//! are made from threads that run no async tasks (the CLI, blocking pools).

use std::{
    fmt,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    body::{Bytes, Incoming},
    client::conn::http1::SendRequest,
    header, Method, Request, StatusCode,
};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use crate::{
    contracts::AppState,
    protocol::{self, rpc, Call, Command, ErrorCode, ShutdownMode, Version},
    sse::DataLines,
    transport,
};

mod legacy;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// The service sends an SSE comment every 15 s; silence past this means it hung.
const EVENTS_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a client waits to retry an event stream the backend has no room for.
const EVENTS_BUSY_RETRY: Duration = Duration::from_secs(1);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Bounds a live but slow backend start, such as the first launch after an
/// update while the OS scans the new binaries on a busy machine.
const BACKEND_START_TIMEOUT: Duration = Duration::from_secs(60);
/// Bounds waiting for a replaced backend to exit before starting its successor.
const BACKEND_EXIT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long an installer or updater may hold the startup gate before a client
/// reports it instead of waiting.
const STARTUP_GATE_WAIT: Duration = Duration::from_secs(5);
const STARTUP_IN_PROGRESS: &str = "Backend startup or an update is already in progress.";
const OTHER_BUILD: &str = "A backend of another Private AI Proxy version is running. Run private-ai-proxy service start to replace it.";
const OTHER_INSTALLATION: &str = "The running backend belongs to another installation. Update it through its owning package manager.";

/// Why a management call failed.
#[derive(Clone, Debug)]
pub enum CallError {
    /// The backend's typed answer.
    Api(protocol::Error),
    /// Reported locally: the backend could not be reached, or the shell failed.
    Local(String),
}

impl CallError {
    /// The API error a web client receives; local messages are authored for users.
    pub fn into_api(self) -> protocol::Error {
        match self {
            Self::Api(error) => error,
            Self::Local(message) => protocol::Error::new(ErrorCode::OperationFailed, message),
        }
    }
}

/// Serialized as the API error `{"code", "message"}`, so every transport
/// (the HTTP API, Tauri commands, `--json`) reports failures in one shape.
impl Serialize for CallError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.clone().into_api().serialize(serializer)
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(error) => error.fmt(formatter),
            Self::Local(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for CallError {
    fn from(message: String) -> Self {
        Self::Local(message)
    }
}

impl From<&str> for CallError {
    fn from(message: &str) -> Self {
        Self::Local(message.into())
    }
}

impl From<CallError> for String {
    fn from(error: CallError) -> Self {
        error.to_string()
    }
}

pub struct Client {
    states: watch::Sender<AppState>,
    expected_shutdown: Mutex<Option<String>>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`Client::watch_connection`] reports.
pub enum Watched {
    /// The backend's state, and after that each change.
    State(Box<AppState>),
    /// Every event stream of the backend is taken; the watch retries until
    /// another client closes one.
    Busy,
}

impl Client {
    pub fn new() -> Self {
        let (states, _) = watch::channel(AppState::default());
        Self {
            states,
            expected_shutdown: Mutex::new(None),
        }
    }

    pub fn version(&self) -> Result<Version, String> {
        block_on(open_current())?.map(|connection| connection.version)
    }

    pub fn is_running(&self) -> Result<bool, String> {
        block_on(async {
            match open().await {
                Ok(_) => Ok(true),
                Err(error) if absent(&error) => Ok(legacy::is_running().await),
                Err(error) => Err(connection_error(error)),
            }
        })?
    }

    pub fn ensure_service() -> Result<(), String> {
        let current = |version: &Version| version.version == protocol::BUILD_VERSION;
        if block_on(open())?.is_ok_and(|connection| current(&connection.version)) {
            return Ok(());
        }
        let data = crate::paths::app_data_dir()?;
        let _startup = acquire_startup(&data, STARTUP_GATE_WAIT)?;
        let expected = || {
            crate::launch::service_executable()?
                .canonicalize()
                .map_err(|_| "Cannot identify the installed backend".to_string())
        };
        match block_on(open())? {
            Ok(connection) if current(&connection.version) => return Ok(()),
            Ok(connection) => {
                let expected = expected()?;
                if !executable_matches(&connection.version.executable, &expected)? {
                    return Err(OTHER_INSTALLATION.into());
                }
                drop(connection);
                Client::new().shutdown_owned(Some(&expected), ShutdownMode::UpdateRestart)?;
            }
            Err(error) if absent(&error) => {
                if block_on(legacy::is_running())? {
                    Client::new()
                        .shutdown_owned(Some(&expected()?), ShutdownMode::UpdateRestart)?;
                }
            }
            Err(error) => return Err(connection_error(error)),
        }
        let mut child = Some(crate::launch::spawn_background()?);
        // A backend that exits is reported immediately below; this only bounds a
        // live but slow start.
        let deadline = Instant::now() + BACKEND_START_TIMEOUT;
        loop {
            match block_on(open())? {
                Ok(connection) if current(&connection.version) => {
                    if let Some(mut child) = child {
                        std::thread::spawn(move || {
                            let _ = child.wait();
                        });
                    }
                    return Ok(());
                }
                Ok(_) => {
                    if let Some(mut child) = child {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    return Err("The bundled backend started with a different build. Reinstall the app and try again.".into());
                }
                Err(error) if absent(&error) => {}
                Err(error) => {
                    if let Some(mut child) = child {
                        std::thread::spawn(move || {
                            let _ = child.wait();
                        });
                    }
                    return Err(connection_error(error));
                }
            }
            if let Some(running) = child.as_mut() {
                if let Some(status) = running
                    .try_wait()
                    .map_err(|error| format!("Cannot inspect backend startup: {error}"))?
                {
                    // Another client started a backend at the same time and it
                    // holds the instance lock: wait for that one instead.
                    if status.code() == Some(crate::launch::EXIT_ALREADY_RUNNING) {
                        child = None;
                    } else {
                        let diagnostic = running.startup_diagnostic();
                        return Err(match diagnostic {
                            Some(diagnostic) => {
                                format!("Backend exited during startup ({status}): {diagnostic}")
                            }
                            None => format!("Backend exited during startup ({status})"),
                        });
                    }
                }
            }
            if Instant::now() >= deadline {
                // Never kill an unrelated winner or retry a mutation after an ambiguous timeout.
                let diagnostic = child.as_ref().and_then(|child| child.diagnostic_snapshot());
                if let Some(mut child) = child {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(match diagnostic {
                    Some(diagnostic) => format!(
                        "Backend readiness timed out: {diagnostic}. Run private-ai-proxy doctor."
                    ),
                    None => "Backend readiness timed out. Run private-ai-proxy doctor.".into(),
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A client that starts the backend when needed and then follows its
    /// state, both on `handle`'s blocking pool, so it returns at once. Until
    /// the backend answers, the state reports it disconnected without an
    /// error: starting.
    pub fn attach(handle: tokio::runtime::Handle) -> Arc<Self> {
        let client = Arc::new(Self::new());
        client
            .states
            .send_modify(|state| state.backend_connected = Some(false));
        let weak = Arc::downgrade(&client);
        handle.spawn_blocking(move || {
            let Some(client) = weak.upgrade() else {
                return;
            };
            match Self::ensure_service().and_then(|()| client.state().map_err(String::from)) {
                Ok(state) => {
                    client.states.send_replace(state);
                }
                Err(error) => {
                    tracing::error!("Cannot start the PAP backend: {error}");
                    client.report_disconnect(error);
                }
            }
            drop(client);
            Self::follow(weak);
        });
        client
    }

    /// Mirrors the backend's state until the client is dropped, reconnecting
    /// after each disconnection.
    fn follow(weak: std::sync::Weak<Self>) {
        loop {
            if weak.strong_count() == 0 {
                break;
            }
            let result = Self::watch_connection(|watched| {
                let Some(client) = weak.upgrade() else {
                    return false;
                };
                if let Watched::State(state) = watched {
                    client.states.send_replace(*state);
                }
                true
            });
            let Some(client) = weak.upgrade() else {
                break;
            };
            if let Err(error) = result {
                client.report_disconnect(error);
            }
            drop(client);
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn report_disconnect(&self, error: String) {
        let mut state = self.states.borrow().clone();
        let expected = self
            .expected_shutdown
            .lock()
            .ok()
            .and_then(|value| value.clone());
        // Only a shutdown initiated by this client for this exact instance is expected.
        if expected.is_some() && expected == state.backend_instance {
            return;
        }
        if state.backend_connected == Some(false) && state.error.is_some() {
            return;
        }
        state.disconnect(error);
        self.states.send_replace(state);
    }

    /// Streams the backend's state from `GET /api/events` until `receive`
    /// returns `false`; each state names the backend instance it came from.
    pub fn watch_connection(mut receive: impl FnMut(Watched) -> bool) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Event {
            event: String,
            payload: Value,
        }
        block_on(async {
            // The connection stays open while its event stream is read.
            let (connection, response) = loop {
                let mut connection = open_current().await?;
                let response = send(
                    &mut connection.sender,
                    Method::GET,
                    protocol::EVENTS_PATH,
                    None,
                )
                .await
                .map_err(connection_error)?;
                if response.status() == StatusCode::SERVICE_UNAVAILABLE {
                    if !receive(Watched::Busy) {
                        return Ok(());
                    }
                    tokio::time::sleep(EVENTS_BUSY_RETRY).await;
                    continue;
                }
                break (connection, response);
            };
            let instance = connection.version.instance_id.clone();
            if response.status() != StatusCode::OK {
                return Err(connection_error(io::ErrorKind::InvalidData.into()));
            }
            let mut body = response.into_body();
            let mut lines = DataLines::new();
            loop {
                let frame = match tokio::time::timeout(EVENTS_IDLE_TIMEOUT, body.frame()).await {
                    Err(_) => return Err(connection_error(io::ErrorKind::TimedOut.into())),
                    Ok(None) => return Err(connection_error(io::ErrorKind::UnexpectedEof.into())),
                    Ok(Some(frame)) => {
                        frame.map_err(|error| connection_error(http_error(error)))?
                    }
                };
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                let mut states = Vec::new();
                lines.push(&data, |value| {
                    if let Ok(event) = serde_json::from_str::<Event>(value) {
                        if event.event == crate::ui_api::STATE_EVENT {
                            states.extend(serde_json::from_value::<AppState>(event.payload).ok());
                        }
                    }
                });
                for mut state in states {
                    state.backend_instance = Some(instance.clone());
                    if !receive(Watched::State(Box::new(state))) {
                        return Ok(());
                    }
                }
            }
        })?
    }

    /// Sends one typed request and decodes exactly its declared response.
    pub fn call<C: Call>(&self, request: C) -> Result<C::Response, CallError> {
        let value = self.execute(request.into())?;
        serde_json::from_value(value)
            .map_err(|_| CallError::Local("Unexpected management response shape".into()))
    }

    /// Runs one command and returns its result as sent.
    pub fn execute(&self, command: Command) -> Result<Value, CallError> {
        block_on(async {
            let mut connection = open_current().await?;
            connection.call(&command).await
        })?
    }

    pub fn cached_state(&self) -> AppState {
        self.states.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<AppState> {
        self.states.subscribe()
    }

    pub fn state(&self) -> Result<AppState, CallError> {
        self.call(rpc::GetState)
    }

    pub fn state_or_cached(&self) -> Result<AppState, CallError> {
        match self.state() {
            Ok(state) => Ok(state),
            Err(error) => {
                let cached = self.cached_state();
                if cached.backend_connected == Some(false) {
                    Ok(cached)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub fn shutdown(&self) -> Result<(), String> {
        self.shutdown_owned(None, ShutdownMode::Quit)
    }

    pub fn restart_service(&self) -> Result<(), String> {
        let expected = crate::launch::service_executable()?
            .canonicalize()
            .map_err(|_| "Cannot identify the installed backend")?;
        if self.is_running()? {
            self.shutdown_owned(Some(&expected), ShutdownMode::UpdateRestart)?;
        }
        Self::ensure_service()
    }

    /// Stops the running backend, of this build or another, and waits for it
    /// to exit. With `expected`, only a backend of that installation.
    fn shutdown_owned(&self, expected: Option<&Path>, mode: ShutdownMode) -> Result<(), String> {
        let process_id = block_on(async {
            let mut connection = match open().await {
                Ok(connection) => connection,
                Err(error) if absent(&error) => return legacy::shutdown(expected, mode).await,
                Err(error) => return Err(connection_error(error)),
            };
            let before = connection.version.clone();
            if let Some(expected) = expected {
                if !executable_matches(&before.executable, expected)? {
                    return Err(OTHER_INSTALLATION.into());
                }
            }
            self.expect_shutdown(Some(before.instance_id.clone()))?;
            let shutdown = Command::Shutdown {
                instance_id: before.instance_id,
                mode,
            };
            match connection.call(&shutdown).await {
                // Busy: another client's shutdown of this instance is under way.
                Ok(_) => Ok(before.process_id),
                Err(CallError::Api(error)) if error.code == ErrorCode::Busy => {
                    Ok(before.process_id)
                }
                Err(error) => {
                    self.expect_shutdown(None)?;
                    Err(error.to_string())
                }
            }
        })??;
        let stopped =
            crate::launch::wait_for_exit(process_id, BACKEND_EXIT_TIMEOUT).and_then(|()| {
                match block_on(open())? {
                    Err(error) if absent(&error) => Ok(()),
                    Err(error) => Err(connection_error(error)),
                    Ok(_) => Err("Another backend started during shutdown".into()),
                }
            });
        if stopped.is_err() {
            self.expect_shutdown(None)?;
        }
        stopped
    }

    fn expect_shutdown(&self, instance: Option<String>) -> Result<(), String> {
        *self
            .expected_shutdown
            .lock()
            .map_err(|_| "Shutdown state unavailable")? = instance;
        Ok(())
    }

    pub fn install_update(
        &self,
        install: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let data = crate::paths::app_data_dir()?;
        let startup = crate::lock::startup(&data)
            .map_err(|_| "Cannot secure update startup gate")?
            .ok_or("Another startup or update is in progress.")?;
        let expected = crate::launch::service_executable()?
            .canonicalize()
            .map_err(|_| "Cannot identify the installed backend")?;
        let was_running = self.is_running()?;
        if was_running {
            self.shutdown_owned(Some(&expected), ShutdownMode::UpdateRestart)?;
        }
        let ownership = crate::lock::instance(&data)
            .map_err(|_| "Cannot secure update ownership")?
            .ok_or("Another backend started before the update. Stop it before retrying.")?;
        let result = install();
        drop(ownership);
        drop(startup);

        match result {
            Ok(()) => Ok(()),
            Err(error) if !was_running => Err(format!("{error} Retry the update.")),
            Err(error) => match Self::ensure_service() {
                Ok(()) => Err(format!(
                    "{error} The background service restarted; retry the update."
                )),
                Err(restart_error) => {
                    let _ = self.expect_shutdown(None);
                    self.report_disconnect(restart_error.clone());
                    Err(format!(
                        "{error} The background service could not restart: {restart_error}"
                    ))
                }
            },
        }
    }
}

/// Take the startup gate to start the backend. Other clients share it; an
/// installer or updater holding it is reported after `gate_wait`.
fn acquire_startup(data: &Path, gate_wait: Duration) -> Result<crate::lock::StartupLock, String> {
    let deadline = Instant::now() + gate_wait;
    loop {
        if let Some(startup) = crate::lock::startup_shared(data)
            .map_err(|error| format!("Cannot acquire backend startup lock: {error}"))?
        {
            return Ok(startup);
        }
        if Instant::now() >= deadline {
            return Err(STARTUP_IN_PROGRESS.into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One HTTP connection to the backend, which answered `GET /api/version`.
struct Connection {
    sender: SendRequest<Full<Bytes>>,
    version: Version,
}

impl Connection {
    async fn call(&mut self, command: &Command) -> Result<Value, CallError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        enum Answer {
            Result(Value),
            Error(protocol::Error),
        }
        let (name, params) = command.encode().map_err(CallError::Api)?;
        let body = serde_json::to_vec(&params)
            .map_err(|_| CallError::Local("Cannot encode the management request".into()))?;
        let path = format!("{}{name}", protocol::RPC_PATH);
        let exchange = async {
            let response = send(&mut self.sender, Method::POST, &path, Some(body)).await?;
            read(response).await
        };
        let (status, bytes) = tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))
            .and_then(|result| result)
            .map_err(connection_error)?;
        match serde_json::from_slice(&bytes) {
            Ok(Answer::Result(value)) if status.is_success() => Ok(value),
            Ok(Answer::Error(error)) if !status.is_success() => Err(CallError::Api(error)),
            _ => Err(CallError::Local(
                "Unexpected management response shape".into(),
            )),
        }
    }
}

async fn open() -> io::Result<Connection> {
    let stream = transport::connect(&transport::endpoint_path()?).await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(http_error)?;
    tokio::spawn(connection);
    let response = send(&mut sender, Method::GET, protocol::VERSION_PATH, None).await?;
    let (status, body) = read(response).await?;
    let version = serde_json::from_slice::<Version>(&body)
        .ok()
        .filter(|version| {
            status == StatusCode::OK
                && version.api_version == protocol::API_VERSION
                && version.product == crate::brand::APP_IDENTIFIER
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Incompatible Private AI Proxy backend; update the client and backend together",
            )
        })?;
    Ok(Connection { sender, version })
}

/// A connection to a backend of this build. A backend of another build,
/// including one that speaks only the legacy protocol, is reported with the
/// command that replaces it.
async fn open_current() -> Result<Connection, String> {
    match open().await {
        Ok(connection) if connection.version.version == protocol::BUILD_VERSION => Ok(connection),
        Ok(_) => Err(OTHER_BUILD.into()),
        Err(error) if absent(&error) && legacy::is_running().await => Err(OTHER_BUILD.into()),
        Err(error) => Err(connection_error(error)),
    }
}

async fn send(
    sender: &mut SendRequest<Full<Bytes>>,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> io::Result<hyper::Response<Incoming>> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        // The endpoint has no host name; HTTP/1.1 requires the header.
        .header(header::HOST, "localhost");
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let request = request
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    sender.ready().await.map_err(http_error)?;
    sender.send_request(request).await.map_err(http_error)
}

async fn read(response: hyper::Response<Incoming>) -> io::Result<(StatusCode, Bytes)> {
    let status = response.status();
    let body = Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
        .collect()
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok((status, body.to_bytes()))
}

/// The I/O error behind a hyper error, or what it says about the protocol.
fn http_error(error: hyper::Error) -> io::Error {
    if error.is_parse() || error.is_parse_status() {
        return io::ErrorKind::InvalidData.into();
    }
    if error.is_timeout() {
        return io::ErrorKind::TimedOut.into();
    }
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<io::Error>() {
            return io.kind().into();
        }
        source = cause.source();
    }
    io::ErrorKind::ConnectionAborted.into()
}

fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start the management client".to_string())?;
    Ok(runtime.block_on(future))
}

fn executable_matches(actual: &str, expected: &Path) -> Result<bool, String> {
    let actual = PathBuf::from(actual);
    if actual == expected {
        return Ok(true);
    }
    actual
        .canonicalize()
        .map(|path| path == expected)
        .map_err(|_| "Cannot identify the running backend".to_string())
}

fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn connection_error(error: io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            "Backend is not running. Run private-ai-proxy service start.".into()
        }
        io::ErrorKind::PermissionDenied => {
            "Management endpoint access denied. Use the same OS user as the backend.".into()
        }
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            "Management request timed out; its outcome may be unknown. Check state before retrying."
                .into()
        }
        io::ErrorKind::InvalidData => {
            "Incompatible or invalid management protocol. Update the client and backend together."
                .into()
        }
        _ => "Management connection failed. Run private-ai-proxy doctor; do not automatically retry mutations."
            .into(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use super::{acquire_startup, STARTUP_IN_PROGRESS};

    #[test]
    fn startup_reports_an_installer_gate() {
        let dir = tempfile::tempdir().unwrap();
        let gate = crate::lock::startup(dir.path()).unwrap().unwrap();
        let started = Instant::now();
        let result = acquire_startup(dir.path(), Duration::from_millis(200));
        assert_eq!(result.err().as_deref(), Some(STARTUP_IN_PROGRESS));
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(gate);
    }

    #[test]
    fn clients_start_the_backend_without_waiting_for_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire_startup(dir.path(), Duration::ZERO).unwrap();
        let second = acquire_startup(dir.path(), Duration::ZERO).unwrap();
        drop((first, second));
    }

    #[test]
    fn attach_keeps_a_disconnected_client_when_backend_startup_fails() {
        const CHILD: &str = "PAP_TEST_ATTACH_FAILURE";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "client::tests::attach_keeps_a_disconnected_client_when_backend_startup_fails",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env(crate::paths::HOME_OVERRIDE_ENV, home.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(crate::launch::service_executable().is_err());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = super::Client::attach(runtime.handle().clone());
        let mut states = client.subscribe();
        // Starting: disconnected without an error, until startup fails.
        assert_eq!(states.borrow().backend_connected, Some(false));
        runtime
            .block_on(states.wait_for(|state| state.error.is_some()))
            .unwrap();
        assert_eq!(client.cached_state().backend_connected, Some(false));
        assert!(client.state_or_cached().unwrap().error.is_some());
        drop(client);
        runtime.shutdown_timeout(std::time::Duration::from_secs(3));
    }

    #[test]
    fn only_the_intentionally_stopped_instance_disconnects_without_a_fault() {
        let client = super::Client::new();
        let mut state = crate::contracts::AppState {
            backend_instance: Some("old-instance".into()),
            ..Default::default()
        };
        client.states.send_replace(state.clone());
        *client.expected_shutdown.lock().unwrap() = Some("old-instance".into());
        client.report_disconnect("connection closed".into());
        assert!(client.states.borrow().error.is_none());

        state.backend_instance = Some("replacement-instance".into());
        client.states.send_replace(state);
        client.report_disconnect("unexpected disconnection".into());
        assert_eq!(
            client.states.borrow().status,
            crate::contracts::VerificationStatus::Error
        );
        assert_eq!(client.states.borrow().backend_connected, Some(false));
        assert_eq!(
            client.states.borrow().error.as_deref(),
            Some("unexpected disconnection")
        );
    }
}
