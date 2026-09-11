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
use tokio::sync::watch;

use crate::{
    contracts::*,
    preferences::Preferences,
    protocol::{self, Command, Hello, Outcome, Preference, Request, Response},
    transport::Stream,
    usage::{UsagePage, UsageQuery},
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
        open().map(|(_, hello)| hello).map_err(connection_error)
    }

    pub fn is_running(&self) -> Result<bool, String> {
        match open() {
            Ok(_) => Ok(true),
            Err(error) if absent(&error) => Ok(false),
            Err(error) => Err(connection_error(error)),
        }
    }

    pub fn ensure_service() -> Result<(), String> {
        match open() {
            Ok(_) => return Ok(()),
            Err(error) if absent(&error) => {}
            Err(error) => return Err(connection_error(error)),
        }
        let data = desktop_gateway::agents::app_data_dir()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let _startup = loop {
            if let Some(lock) = desktop_gateway::lock::startup(&data)
                .map_err(|_| "Cannot acquire backend startup lock")?
            {
                break lock;
            }
            if Instant::now() >= deadline {
                return Err("Backend startup or an update is already in progress.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        match open() {
            Ok(_) => return Ok(()),
            Err(error) if absent(&error) => {}
            Err(error) => return Err(connection_error(error)),
        }
        let mut child = crate::launch::spawn_background()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match open() {
                Ok(_) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    return Ok(());
                }
                Err(error) if absent(&error) => {}
                Err(error) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    return Err(connection_error(error));
                }
            }
            // A competing starter can win the lock but still be initializing.
            // Wait for its handshake even when our own child has already exited.
            let _ = child
                .try_wait()
                .map_err(|_| "Cannot inspect backend startup")?;
            if Instant::now() >= deadline {
                // Never kill an unrelated winner or retry a mutation after an ambiguous timeout.
                let _ = child.kill();
                let _ = child.wait();
                return Err("Backend readiness timed out. Run pap doctor.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn attach(handle: tokio::runtime::Handle) -> Result<Arc<Self>, String> {
        Self::ensure_service()?;
        let client = Arc::new(Self::new());
        client.states.send_replace(client.state()?);
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
        state.status = "error".into();
        state.backend_connected = Some(false);
        state.identity = None;
        state.proxy_url = None;
        state.error = Some(error);
        state.endpoint_error =
            Some("Backend disconnected. Start it with pap service start.".into());
        self.states.send_replace(state);
    }

    pub fn watch_connection(mut receive: impl FnMut(GatewayState) -> bool) -> Result<(), String> {
        let (mut reader, hello) = open().map_err(connection_error)?;
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

    pub fn request<T: DeserializeOwned>(&self, command: Command) -> Result<T, String> {
        let (mut reader, _) = open().map_err(connection_error)?;
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

    pub fn subscribe(&self) -> watch::Receiver<GatewayState> {
        self.states.subscribe()
    }
    pub fn state(&self) -> Result<GatewayState, String> {
        self.request(Command::State)
    }
    pub fn start(&self, config: StartGatewayConfig) -> Result<GatewayState, String> {
        self.request(Command::Start(config))
    }
    pub fn stop(&self) -> Result<GatewayState, String> {
        self.request(Command::Stop)
    }
    pub fn activate_profile(&self, profile_id: String) -> Result<GatewayState, String> {
        self.request(Command::ActivateProfile { profile_id })
    }
    pub fn delete_profile(&self, profile_id: String) -> Result<GatewayState, String> {
        self.request(Command::DeleteProfile { profile_id })
    }
    pub fn clear_api_key(&self) -> Result<GatewayState, String> {
        self.request(Command::ClearApiKey)
    }
    pub fn import_profiles(
        &self,
        backup: crate::maintenance::ProfileBackup,
    ) -> Result<crate::maintenance::ImportResult, String> {
        self.request(Command::ImportProfiles(backup))
    }
    pub fn export_profiles(&self, path: PathBuf) -> Result<(), String> {
        self.request(Command::ExportProfiles {
            path: export_path(&path)?,
        })
    }
    pub fn export_diagnostics(&self, path: PathBuf) -> Result<(), String> {
        self.request(Command::ExportDiagnostics {
            path: export_path(&path)?,
        })
    }
    pub fn query_usage(&self, query: UsageQuery) -> Result<UsagePage, String> {
        self.request(Command::Usage(query))
    }
    pub fn usage_record(&self, record_id: &str) -> Result<Option<RequestActivity>, String> {
        self.request(Command::UsageRecord {
            record_id: record_id.into(),
        })
    }
    pub fn export_usage_csv(&self, query: UsageQuery, path: PathBuf) -> Result<usize, String> {
        self.request(Command::ExportUsage {
            query,
            path: export_path(&path)?,
        })
    }
    pub fn clear_usage(&self) -> Result<u64, String> {
        self.request(Command::ClearUsage)
    }
    pub fn client_key(&self) -> Result<String, String> {
        self.request(Command::ClientKey)
    }
    pub fn rotate_client_key(&self) -> Result<String, String> {
        self.request(Command::RotateClientKey)
    }
    pub fn list_agents(&self) -> Result<Vec<AgentStatus>, String> {
        self.request(Command::Agents)
    }
    pub fn preview_agent(
        &self,
        agent_id: String,
        connect: bool,
        options: ConnectOptions,
    ) -> Result<AgentPreview, String> {
        self.request(Command::PreviewAgent {
            agent_id,
            connect,
            options,
        })
    }
    pub fn apply_agent(
        &self,
        agent_id: String,
        connect: bool,
        revision: String,
        options: ConnectOptions,
    ) -> Result<AgentStatus, String> {
        self.request(Command::ApplyAgent {
            agent_id,
            connect,
            revision,
            options,
        })
    }
    pub fn disconnect_all_agents(&self) -> Result<Vec<AgentStatus>, String> {
        self.request(Command::DisconnectAllAgents)
    }
    pub fn preferences(&self) -> Result<Preferences, String> {
        self.request(Command::Preferences)
    }
    pub fn reset_settings(&self) -> Result<GatewayState, String> {
        self.request(Command::ResetSettings)
    }
    pub fn set_preference(&self, change: Preference) -> Result<Preferences, String> {
        self.request(Command::SetPreference(change))
    }

    pub async fn save_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<GatewayState, String> {
        self.background(Command::SaveConfiguration {
            profile,
            require_production_os,
            key,
        })
        .await
    }
    pub async fn complete_account_login(
        self: &Arc<Self>,
        id: String,
        callback_url: String,
    ) -> Result<(), String> {
        self.background(Command::CompleteAccountLogin { id, callback_url })
            .await
    }
    pub async fn begin_account_login(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
    ) -> Result<crate::account_login::LoginPresentation, String> {
        self.background(Command::BeginAccountLogin { profile })
            .await
    }
    pub async fn poll_account_login(
        self: &Arc<Self>,
        id: String,
    ) -> Result<Option<AccountLoginDetails>, String> {
        self.background(Command::PollAccountLogin { id }).await
    }
    pub async fn save_account_login(
        self: &Arc<Self>,
        id: String,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        workspace_id: Option<i64>,
    ) -> Result<GatewayState, String> {
        let operation_id = uuid::Uuid::new_v4().to_string();
        let initial = self
            .background::<AccountSaveResult>(Command::SaveAccountLogin {
                operation_id: operation_id.clone(),
                id,
                profile,
                require_production_os,
                workspace_id,
            })
            .await;
        let mut outcome = match initial {
            Ok(result) => result,
            Err(_) => {
                self.background(Command::AccountSaveResult {
                    operation_id: operation_id.clone(),
                })
                .await?
            }
        };
        loop {
            match outcome {
                AccountSaveResult::Complete { state } => return Ok(*state),
                AccountSaveResult::Failed { error } => return Err(error),
                AccountSaveResult::Running => tokio::time::sleep(Duration::from_millis(500)).await,
            }
            outcome = self.background(Command::AccountSaveResult { operation_id: operation_id.clone() }).await
                .map_err(|_| "Account: Save outcome is not yet confirmed. Reconnect to the backend and check the profile before retrying.".to_string())?;
        }
    }
    pub async fn account_workspaces(
        self: &Arc<Self>,
        profile_id: String,
    ) -> Result<Vec<AccountWorkspace>, String> {
        self.background(Command::AccountWorkspaces { profile_id })
            .await
    }
    pub async fn account_balance(
        self: &Arc<Self>,
        target: AccountBalanceTarget,
    ) -> Result<AccountBalance, String> {
        self.background(Command::AccountBalance { target }).await
    }
    pub async fn cancel_account_login(self: &Arc<Self>, id: String) -> Result<(), String> {
        self.background(Command::CancelAccountLogin { id }).await
    }

    pub async fn verify_configuration(
        self: &Arc<Self>,
        profile: ConfidentialProfileInput,
        require_production_os: bool,
        key: Option<String>,
    ) -> Result<GatewayState, String> {
        self.background(Command::Verify {
            profile,
            require_production_os,
            key,
        })
        .await
    }
    pub async fn save_local_api_config(
        self: &Arc<Self>,
        config: LocalApiConfig,
    ) -> Result<GatewayState, String> {
        self.background(Command::SaveLocalApi(config)).await
    }
    pub async fn refresh_catalog(self: &Arc<Self>) -> Result<GatewayState, String> {
        self.background(Command::RefreshCatalog).await
    }
    async fn background<T: DeserializeOwned + Send + 'static>(
        self: &Arc<Self>,
        command: Command,
    ) -> Result<T, String> {
        let client = self.clone();
        tokio::task::spawn_blocking(move || client.request(command))
            .await
            .map_err(|_| "Management request task failed")?
    }
    pub fn report_error(&self, error: String) {
        let mut state = self.states.borrow().clone();
        state.error = Some(error);
        self.states.send_replace(state);
    }
    pub fn toggle(&self) {
        let result = self.state().and_then(|state| {
            if matches!(state.status.as_str(), "verifying" | "verified" | "blocked") {
                self.stop()
            } else {
                self.start(state.config)
            }
        });
        if let Err(error) = result {
            self.report_error(error);
        }
    }
    pub fn shutdown(&self) -> Result<(), String> {
        self.shutdown_owned(None)
    }
    fn shutdown_owned(&self, expected: Option<&std::path::Path>) -> Result<(), String> {
        let (mut reader, before) = open().map_err(connection_error)?;
        if let Some(expected) = expected {
            let actual = PathBuf::from(&before.executable)
                .canonicalize()
                .map_err(|_| "Cannot identify the running backend")?;
            if actual != expected {
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
        let data = desktop_gateway::agents::app_data_dir()?;
        let _startup = desktop_gateway::lock::startup(&data)
            .map_err(|_| "Cannot secure update startup gate")?
            .ok_or("Another startup or update is in progress.")?;
        let expected = crate::launch::service_executable()?
            .canonicalize()
            .map_err(|_| "Cannot identify the installed backend")?;
        if self.is_running()? {
            self.shutdown_owned(Some(&expected))?;
        }
        let _ownership = desktop_gateway::lock::instance(&data)
            .map_err(|_| "Cannot secure update ownership")?
            .ok_or("Another backend started before the update. Stop it before retrying.")?;
        install()
    }
}

fn export_path(path: &std::path::Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "Export paths must be valid Unicode".into())
}

fn open() -> io::Result<(BufReader<Stream>, Hello)> {
    let stream = Stream::connect()?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream);
    let hello: Hello = protocol::read(&mut reader)?;
    if hello.protocol_version != protocol::VERSION
        || hello.product != desktop_gateway::brand::APP_IDENTIFIER
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Incompatible PAP backend; update the client and backend together",
        ));
    }
    Ok((reader, hello))
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
            "Backend is not running. Run pap service start.".into()
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
        _ => "Management connection failed. Run pap doctor; do not automatically retry mutations."
            .into(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::export_path;
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

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

    #[test]
    fn export_rejects_paths_that_json_cannot_represent() {
        let path = PathBuf::from(OsString::from_vec(b"/tmp/pap-\xff.csv".to_vec()));
        assert!(export_path(&path).is_err());
        assert_eq!(
            export_path(std::path::Path::new("/tmp/pap.csv")).unwrap(),
            "/tmp/pap.csv"
        );
    }
}
