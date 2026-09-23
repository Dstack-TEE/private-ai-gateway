use std::{
    io::{self, BufReader},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::watch;

use crate::{
    contracts::GatewayState,
    protocol::{self, rpc, Call, Command, Hello, Outcome, Request, Response, ShutdownMode},
    transport::Stream,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Client {
    states: watch::Sender<GatewayState>,
    expected_shutdown: Mutex<Option<String>>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        let (states, _) = watch::channel(GatewayState::default());
        Self {
            states,
            expected_shutdown: Mutex::new(None),
        }
    }

    pub fn hello(&self) -> Result<Hello, String> {
        open_current()
            .map(|(_, hello)| hello)
            .map_err(connection_error)
    }

    pub fn is_running(&self) -> Result<bool, String> {
        match open() {
            Ok(_) => Ok(true),
            Err(error) if absent(&error) => Ok(false),
            Err(error) => Err(connection_error(error)),
        }
    }

    pub fn ensure_service() -> Result<(), String> {
        if let Ok((_, hello)) = open() {
            if hello.version == protocol::BUILD_VERSION {
                return Ok(());
            }
        }
        let data = crate::paths::app_data_dir()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let _startup = loop {
            if let Some(lock) = crate::lock::startup(&data)
                .map_err(|error| format!("Cannot acquire backend startup lock: {error}"))?
            {
                break lock;
            }
            if Instant::now() >= deadline {
                return Err("Backend startup or an update is already in progress.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        match open() {
            Ok((_, hello)) if hello.version == protocol::BUILD_VERSION => return Ok(()),
            Ok((_, hello)) => {
                let expected = crate::launch::service_executable()?
                    .canonicalize()
                    .map_err(|_| "Cannot identify the installed backend")?;
                if !executable_matches(&hello.executable, &expected)? {
                    return Err("The running backend belongs to another installation. Update it through its owning package manager.".into());
                }
                Client::new().shutdown_owned(Some(&expected), ShutdownMode::UpdateRestart)?;
            }
            Err(error) if absent(&error) => {}
            Err(error) => return Err(connection_error(error)),
        }
        let mut child = crate::launch::spawn_background()?;
        // A backend that exits is reported immediately below; this only bounds a
        // live but slow start, such as the first launch after an update while
        // the OS scans the new binaries on a busy machine.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match open() {
                Ok((_, hello)) if hello.version == protocol::BUILD_VERSION => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    return Ok(());
                }
                Ok(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("The bundled backend started with a different build. Reinstall the app and try again.".into());
                }
                Err(error) if absent(&error) => {}
                Err(error) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    return Err(connection_error(error));
                }
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("Cannot inspect backend startup: {error}"))?
            {
                let diagnostic = child.startup_diagnostic();
                return Err(match diagnostic {
                    Some(diagnostic) => {
                        format!("Backend exited during startup ({status}): {diagnostic}")
                    }
                    None => format!("Backend exited during startup ({status})"),
                });
            }
            if Instant::now() >= deadline {
                // Never kill an unrelated winner or retry a mutation after an ambiguous timeout.
                let diagnostic = child.diagnostic_snapshot();
                let _ = child.kill();
                let _ = child.wait();
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

    pub fn attach(handle: tokio::runtime::Handle) -> Result<Arc<Self>, String> {
        let client = Arc::new(Self::new());
        match Self::ensure_service().and_then(|()| client.state()) {
            Ok(state) => {
                client.states.send_replace(state);
            }
            Err(error) => {
                crate::diagnostic(format_args!("Cannot start the PAP backend: {error}"));
                client.report_disconnect(error);
            }
        }
        let weak = Arc::downgrade(&client);
        handle.spawn_blocking(move || loop {
            if weak.strong_count() == 0 {
                break;
            }
            let result = Self::watch_connection(|state| {
                let Some(client) = weak.upgrade() else {
                    return false;
                };
                client.states.send_replace(state);
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
        });
        Ok(client)
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
        state.status = "error".into();
        state.backend_connected = Some(false);
        state.identity = None;
        state.proxy_url = None;
        state.error = Some(error);
        state.endpoint_error =
            Some("Backend disconnected. Start it with private-ai-proxy service start.".into());
        self.states.send_replace(state);
    }

    pub fn watch_connection(mut receive: impl FnMut(GatewayState) -> bool) -> Result<(), String> {
        let (mut reader, hello) = open_current().map_err(connection_error)?;
        reader
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(connection_error)?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        protocol::write(
            reader.get_mut(),
            &Request {
                version: protocol::VERSION,
                id,
                command: Command::Watch,
            },
        )
        .map_err(connection_error)?;
        loop {
            let mut state: GatewayState = decode(&mut reader, id)?;
            state.backend_instance = Some(hello.instance_id.clone());
            if !receive(state) {
                return Ok(());
            }
        }
    }

    /// Sends one typed request and decodes exactly its declared response.
    pub fn call<C: Call>(&self, request: C) -> Result<C::Response, String> {
        self.request(request.into())
    }

    /// Forwards a command whose result is passed through unchanged.
    pub fn forward(&self, command: Command) -> Result<Value, String> {
        self.request(command)
    }

    fn request<T: DeserializeOwned>(&self, command: Command) -> Result<T, String> {
        let (mut reader, _) = open_current().map_err(connection_error)?;
        reader
            .get_mut()
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(connection_error)?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        protocol::write(
            reader.get_mut(),
            &Request {
                version: protocol::VERSION,
                id,
                command,
            },
        )
        .map_err(connection_error)?;
        decode(&mut reader, id)
    }

    pub fn cached_state(&self) -> GatewayState {
        self.states.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<GatewayState> {
        self.states.subscribe()
    }

    pub fn state(&self) -> Result<GatewayState, String> {
        self.call(rpc::State)
    }

    pub fn state_or_cached(&self) -> Result<GatewayState, String> {
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

    pub fn toggle(&self) -> Result<GatewayState, String> {
        self.state().and_then(|state| {
            if state.should_stop_protection() {
                self.call(rpc::Stop)
            } else {
                self.call(rpc::Start {
                    config: state.config,
                })
            }
        })
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
    fn shutdown_owned(
        &self,
        expected: Option<&std::path::Path>,
        mode: ShutdownMode,
    ) -> Result<(), String> {
        let (mut reader, before) = open().map_err(connection_error)?;
        if let Some(expected) = expected {
            if !executable_matches(&before.executable, expected)? {
                return Err("The running backend belongs to another installation. Update it through its owning package manager.".into());
            }
        }
        reader
            .get_mut()
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(connection_error)?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        *self
            .expected_shutdown
            .lock()
            .map_err(|_| "Shutdown state unavailable")? = Some(before.instance_id.clone());
        let shutdown = (|| {
            protocol::write(
                reader.get_mut(),
                &Request {
                    version: protocol::VERSION,
                    id,
                    command: Command::Shutdown {
                        instance_id: before.instance_id.clone(),
                        mode,
                    },
                },
            )
            .map_err(connection_error)?;
            decode::<()>(&mut reader, id)?;
            drop(reader);
            crate::launch::wait_for_exit(before.process_id, Duration::from_secs(15))?;
            match open() {
                Err(error) if absent(&error) => Ok(()),
                Err(error) => Err(connection_error(error)),
                Ok(_) => Err("Another backend started during shutdown".into()),
            }
        })();
        if shutdown.is_err() {
            *self
                .expected_shutdown
                .lock()
                .map_err(|_| "Shutdown state unavailable")? = None;
        }
        shutdown
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
                    if let Ok(mut expected) = self.expected_shutdown.lock() {
                        *expected = None;
                    }
                    self.report_disconnect(restart_error.clone());
                    Err(format!(
                        "{error} The background service could not restart: {restart_error}"
                    ))
                }
            },
        }
    }
}

fn open() -> io::Result<(BufReader<Stream>, Hello)> {
    let stream = Stream::connect()?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream);
    let hello: Hello = protocol::read(&mut reader)?;
    if hello.protocol_version != protocol::VERSION || hello.product != crate::brand::APP_IDENTIFIER
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Incompatible Private AI Proxy backend; update the client and backend together",
        ));
    }
    Ok((reader, hello))
}

fn open_current() -> io::Result<(BufReader<Stream>, Hello)> {
    let result = open()?;
    if result.1.version != protocol::BUILD_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Incompatible Private AI Proxy backend build; update the client and backend together",
        ));
    }
    Ok(result)
}

fn executable_matches(actual: &str, expected: &std::path::Path) -> Result<bool, String> {
    let actual = PathBuf::from(actual);
    if actual == expected {
        return Ok(true);
    }
    actual
        .canonicalize()
        .map(|path| path == expected)
        .map_err(|_| "Cannot identify the running backend".to_string())
}

fn decode<T: DeserializeOwned>(reader: &mut BufReader<Stream>, id: u64) -> Result<T, String> {
    let response: Response =
        protocol::read_with_timeout(reader, REQUEST_TIMEOUT).map_err(connection_error)?;
    if response.id != id {
        return Err("Management response ID mismatch".into());
    }
    match response.outcome {
        Outcome::Result(value) => {
            serde_json::from_value(value).map_err(|_| "Unexpected management response shape".into())
        }
        Outcome::Error(error) => Err(format!("{}: {}", error.code, error.message)),
    }
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
        let client = super::Client::attach(runtime.handle().clone()).unwrap();
        assert_eq!(client.cached_state().backend_connected, Some(false));
        assert!(client.cached_state().error.is_some());
        assert!(client.state_or_cached().unwrap().error.is_some());
        drop(client);
        runtime.shutdown_timeout(std::time::Duration::from_secs(3));
    }

    #[test]
    fn only_the_intentionally_stopped_instance_disconnects_without_a_fault() {
        let client = super::Client::new();
        let mut state = crate::contracts::GatewayState {
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
        assert_eq!(client.states.borrow().status, "error");
        assert_eq!(client.states.borrow().backend_connected, Some(false));
        assert_eq!(
            client.states.borrow().error.as_deref(),
            Some("unexpected disconnection")
        );
    }
}
