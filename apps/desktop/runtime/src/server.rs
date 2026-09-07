use std::{
    io::BufReader,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use serde::Serialize;
use serde_json::Value;
use tokio::{runtime::Handle, sync::Semaphore, task::JoinSet};

use crate::{
    controller::DesktopRuntime,
    preferences,
    protocol::{self, Command, Hello, Outcome, Preference, Request, Response, RpcError},
    transport::{Listener, Stream},
};

const MAX_CLIENTS: usize = 32;

struct Admission {
    draining: AtomicBool,
    mutations: RwLock<()>,
    watchers: Semaphore,
    exports: Semaphore,
}

pub async fn serve(runtime: Arc<DesktopRuntime>) -> Result<(), String> {
    let listener = Listener::bind(runtime.instance_lock()?)
        .map_err(|_| "Cannot bind the private management endpoint")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "Cannot configure the management endpoint")?;
    let executable = std::env::current_exe().map_err(|_| "Cannot locate backend executable")?;
    let hello = Hello {
        protocol_version: protocol::VERSION,
        product: desktop_gateway::brand::APP_IDENTIFIER.into(),
        version: protocol::BUILD_VERSION.into(),
        instance_id: format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "Invalid system clock")?
                .as_nanos()
        ),
        executable: executable.to_string_lossy().into_owned(),
        process_id: std::process::id(),
    };
    let stopping = Arc::new(AtomicBool::new(false));
    let permits = Arc::new(Semaphore::new(MAX_CLIENTS));
    let admission = Arc::new(Admission {
        draining: AtomicBool::new(false),
        mutations: RwLock::new(()),
        watchers: Semaphore::new(4),
        exports: Semaphore::new(1),
    });
    let mut tasks = JoinSet::new();
    let handle = Handle::current();
    let startup = runtime.clone();
    tasks.spawn_blocking(move || match preferences::load() {
        Ok(saved) if saved.connect_on_launch => {
            let result = startup
                .state()
                .and_then(|state| startup.start(state.config));
            if let Err(error) = result {
                startup.report_error(error);
            }
        }
        Err(error) => startup.report_error(error),
        _ => {}
    });
    let signal = shutdown_signal();
    tokio::pin!(signal);
    while !stopping.load(Ordering::Acquire) {
        tokio::select! {
            result = &mut signal => {
                result?;
                let worker = runtime.clone();
                let executor = handle.clone();
                let gate = admission.clone();
                let result = tokio::task::spawn_blocking(move || shutdown(&worker, &gate, &executor)).await.map_err(|_| "Shutdown task failed")?;
                match result {
                    Ok(()) => { stopping.store(true, Ordering::Release); break; },
                    Err(error) => runtime.report_error(error),
                }
                signal.set(shutdown_signal());
            }
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        match listener.accept() {
            Ok(stream) => {
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let runtime = runtime.clone();
                let hello = hello.clone();
                let stopping = stopping.clone();
                let handle = handle.clone();
                let admission = admission.clone();
                tasks.spawn_blocking(move || {
                    let _permit = permit;
                    // Malformed/disconnected clients cannot tear down the service.
                    let _ = connection(stream, runtime, hello, stopping, admission, handle);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                ) => {}
            Err(_) => return Err("The management listener failed".into()),
        }
        while tasks.try_join_next().is_some() {}
    }
    drop(listener);
    // Idle clients have bounded read deadlines; watch clients observe stopping.
    while tasks.join_next().await.is_some() {}
    Ok(())
}

async fn shutdown_signal() -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| "Cannot register shutdown signal")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|_| "Cannot register interrupt signal")?,
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|_| "Cannot register shutdown signal".into())
    }
}

fn connection(
    mut stream: Stream,
    runtime: Arc<DesktopRuntime>,
    hello: Hello,
    stopping: Arc<AtomicBool>,
    admission: Arc<Admission>,
    handle: Handle,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    protocol::write(&mut stream, &hello)?;
    let mut reader = BufReader::new(stream);
    let request: Request = protocol::read(&mut reader)?;
    if request.version != protocol::VERSION {
        return protocol::write(
            reader.get_mut(),
            &Response {
                id: request.id,
                outcome: Outcome::Error(RpcError::new(
                    "incompatible_protocol",
                    "Update PAG clients and backend together.",
                )),
            },
        );
    }
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    if matches!(request.command, Command::Watch) {
        let Ok(_watcher) = admission.watchers.try_acquire() else {
            return protocol::write(
                reader.get_mut(),
                &Response {
                    id: request.id,
                    outcome: Outcome::Error(RpcError::new(
                        "busy",
                        "The state subscription limit has been reached.",
                    )),
                },
            );
        };
        let mut states = runtime.subscribe();
        while !stopping.load(Ordering::Acquire) {
            let state = states.borrow_and_update().clone();
            let result = encode(state);
            protocol::write(
                reader.get_mut(),
                &Response {
                    id: request.id,
                    outcome: outcome(result),
                },
            )?;
            // A full snapshot heartbeat bounds disconnect detection and supports coalescing.
            for _ in 0..10 {
                if stopping.load(Ordering::Acquire) || states.has_changed().unwrap_or(false) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        return Ok(());
    }
    if let Command::Shutdown { instance_id } = &request.command {
        if instance_id != &hello.instance_id {
            return protocol::write(
                reader.get_mut(),
                &Response {
                    id: request.id,
                    outcome: Outcome::Error(RpcError::new(
                        "instance_changed",
                        "The backend instance changed; reconnect before shutting down.",
                    )),
                },
            );
        }
        let result = shutdown(&runtime, &admission, &handle)
            .map(|()| Value::Null)
            .map_err(|error| RpcError::operation(&error));
        if result.is_ok() {
            stopping.store(true, Ordering::Release);
        }
        return protocol::write(
            reader.get_mut(),
            &Response {
                id: request.id,
                outcome: outcome(result),
            },
        );
    }
    if matches!(request.command, Command::State) {
        let result = handle.block_on(dispatch(&runtime, request.command));
        return protocol::write(
            reader.get_mut(),
            &Response {
                id: request.id,
                outcome: outcome(result),
            },
        );
    }
    let _operation = admission
        .mutations
        .read()
        .map_err(|_| std::io::Error::other("Management admission failed"))?;
    if admission.draining.load(Ordering::Acquire) {
        return protocol::write(
            reader.get_mut(),
            &Response {
                id: request.id,
                outcome: Outcome::Error(RpcError::new("busy", "The backend is shutting down.")),
            },
        );
    }
    let _export = if matches!(request.command, Command::ExportUsage { .. }) {
        match admission.exports.try_acquire() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return protocol::write(
                    reader.get_mut(),
                    &Response {
                        id: request.id,
                        outcome: Outcome::Error(RpcError::new(
                            "busy",
                            "Another export is in progress.",
                        )),
                    },
                )
            }
        }
    } else {
        None
    };
    let result = handle.block_on(dispatch(&runtime, request.command));
    protocol::write(
        reader.get_mut(),
        &Response {
            id: request.id,
            outcome: outcome(result),
        },
    )
}

fn shutdown(
    runtime: &Arc<DesktopRuntime>,
    admission: &Admission,
    handle: &Handle,
) -> Result<(), String> {
    if admission.draining.swap(true, Ordering::AcqRel) {
        return Err("Shutdown is already in progress".into());
    }
    let result = (|| {
        let _exclusive = admission
            .mutations
            .write()
            .map_err(|_| "Management admission unavailable")?;
        handle.block_on(runtime.shutdown())
    })();
    if result.is_err() {
        admission.draining.store(false, Ordering::Release);
    }
    result
}

fn outcome(result: Result<Value, RpcError>) -> Outcome {
    match result {
        Ok(value) => Outcome::Result(value),
        Err(error) => Outcome::Error(error),
    }
}

fn encode(value: impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|_| RpcError::new("encoding_failed", "Cannot encode the operation result."))
}

async fn dispatch(runtime: &Arc<DesktopRuntime>, command: Command) -> Result<Value, RpcError> {
    async fn execute(runtime: &Arc<DesktopRuntime>, command: Command) -> Result<Value, String> {
        fn value(input: impl Serialize) -> Result<Value, String> {
            serde_json::to_value(input).map_err(|_| "Cannot encode operation result".into())
        }
        match command {
            Command::State => value(runtime.state()?),
            Command::Start(config) => value(runtime.start(config)?),
            Command::Stop => value(runtime.stop()?),
            Command::Shutdown { .. } => Err("Shutdown requires lifecycle admission".into()),
            Command::Verify {
                profile,
                require_production_os,
                key,
            } => value(
                runtime
                    .verify_configuration(profile, require_production_os, key)
                    .await?,
            ),
            Command::ActivateProfile { profile_id } => value(runtime.activate_profile(profile_id)?),
            Command::DeleteProfile { profile_id } => value(runtime.delete_profile(profile_id)?),
            Command::ClearApiKey => value(runtime.clear_api_key()?),
            Command::ImportProfiles(backup) => value(runtime.import_profiles(backup)?),
            Command::ExportProfiles { path } => {
                let path = std::path::PathBuf::from(path);
                if !path.is_absolute() {
                    return Err("Export path must be absolute".into());
                }
                runtime.export_profiles(path)?;
                value(())
            }
            Command::ExportDiagnostics { path } => {
                let path = std::path::PathBuf::from(path);
                if !path.is_absolute() {
                    return Err("Export path must be absolute".into());
                }
                runtime.export_diagnostics(path, protocol::BUILD_VERSION)?;
                value(())
            }
            Command::Usage(query) => value(runtime.query_usage(query)?),
            Command::UsageRecord { record_id } => value(runtime.usage_record(&record_id)?),
            Command::ExportUsage { query, path } => {
                let path = std::path::PathBuf::from(path);
                if !path.is_absolute() {
                    return Err("Export path must be absolute".into());
                }
                value(runtime.export_usage_csv(query, path)?)
            }
            Command::ClearUsage => value(runtime.clear_usage()?),
            Command::ClientKey => value(runtime.client_key()?),
            Command::RotateClientKey => value(runtime.rotate_client_key()?),
            Command::SaveLocalApi(config) => value(runtime.save_local_api_config(config).await?),
            Command::RefreshCatalog => value(runtime.refresh_catalog().await?),
            Command::Agents => value(runtime.list_agents()?),
            Command::PreviewAgent {
                agent_id,
                connect,
                options,
            } => value(runtime.preview_agent(agent_id, connect, options)?),
            Command::ApplyAgent {
                agent_id,
                connect,
                revision,
                options,
            } => value(runtime.apply_agent(agent_id, connect, revision, options)?),
            Command::DisconnectAllAgents => value(runtime.disconnect_all_agents()?),
            Command::Preferences => value(preferences::load()?),
            Command::SetPreference(change) => {
                preferences::update(|saved| match change {
                    Preference::AutoCliRegistration(enabled) => {
                        saved.auto_cli_registration = Some(enabled)
                    }
                    Preference::Notifications(config) => saved.notifications = config,
                    Preference::ConnectOnLaunch(enabled) => saved.connect_on_launch = enabled,
                    Preference::Appearance(appearance) => saved.appearance = appearance,
                    Preference::UpdateChannel(channel) => saved.update_channel = Some(channel),
                })?;
                value(preferences::load()?)
            }
            Command::Watch => Err("Subscription requires its own connection".into()),
        }
    }
    execute(runtime, command)
        .await
        .map_err(|message| RpcError::operation(&message))
}
