use std::{
    io::BufReader,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use serde_json::Value;
use tokio::{runtime::Handle, sync::Semaphore, task::JoinSet};

use crate::controller::DesktopRuntime;

use desktop_core::{
    preferences,
    protocol::{self, rpc, Command, Hello, Outcome, Request, Response, RpcError, ShutdownMode},
    transport::{Listener, Stream},
};

const MAX_CLIENTS: usize = 32;

/// Lifecycle admission shared by every management transport of one service.
pub(crate) struct Admission {
    draining: AtomicBool,
    mutations: RwLock<()>,
    watchers: Semaphore,
    exports: Semaphore,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            draining: AtomicBool::new(false),
            mutations: RwLock::new(()),
            watchers: Semaphore::new(4),
            exports: Semaphore::new(1),
        }
    }
}

pub async fn serve(runtime: Arc<DesktopRuntime>) -> Result<(), String> {
    let listener = Listener::bind(runtime.instance_lock()?)
        .map_err(|error| format!("Cannot bind the private management endpoint: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Cannot configure the management endpoint: {error}"))?;
    let executable = std::env::current_exe().map_err(|_| "Cannot locate backend executable")?;
    let hello = Hello {
        protocol_version: protocol::VERSION,
        product: desktop_core::brand::APP_IDENTIFIER.into(),
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
    let admission = runtime.admission();
    let mut tasks = JoinSet::new();
    let handle = Handle::current();
    let startup = runtime.clone();
    tasks.spawn_blocking(move || {
        let connect_on_launch = match preferences::load() {
            Ok(saved) => saved.connect_on_launch,
            Err(error) => {
                startup.report_error(error);
                false
            }
        };
        let resume_session = startup.state().is_ok_and(|state| state.reconnecting);
        if connect_on_launch || resume_session {
            let result = startup.start_on_launch();
            if let Err(error) = result {
                startup.report_error(error);
            }
        }
    });
    let owner = owner_exited();
    tokio::pin!(owner);
    let signal = shutdown_signal();
    tokio::pin!(signal);
    while !stopping.load(Ordering::Acquire) {
        tokio::select! {
            _ = &mut owner => {
                // A sandboxed service belongs to its GUI parent. Restoration
                // errors remain retryable on next launch, never keep it alive.
                let worker = runtime.clone();
                let executor = handle.clone();
                let gate = admission.clone();
                match tokio::task::spawn_blocking(move || shutdown(&worker, &gate, &executor, ShutdownMode::Quit)).await {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => runtime.report_error(error),
                    Err(error) => crate::diagnostic(format_args!(
                        "Cannot finish backend shutdown: {error}"
                    )),
                }
                stopping.store(true, Ordering::Release);
                break;
            }
            result = &mut signal => {
                result?;
                let worker = runtime.clone();
                let executor = handle.clone();
                let gate = admission.clone();
                let result = tokio::task::spawn_blocking(move || shutdown(&worker, &gate, &executor, ShutdownMode::Quit)).await.map_err(|_| "Shutdown task failed")?;
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
            Err(error) => return Err(format!("The management listener failed: {error}")),
        }
        while tasks.try_join_next().is_some() {}
    }
    drop(listener);
    // Idle clients have bounded read deadlines; watch clients observe stopping.
    while tasks.join_next().await.is_some() {}
    Ok(())
}

async fn owner_exited() {
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    {
        // A direct child is reparented to launchd when its owner exits, even
        // after a crash. No PID lookup, polling of unrelated processes or IPC
        // endpoint exposed to external agents is needed.
        while unsafe { libc::getppid() } != 1 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    #[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
    std::future::pending::<()>().await;
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
                    "Update PAP clients and backend together.",
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
            let result = protocol::encode::<rpc::Watch>(states.borrow_and_update().clone());
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
    if let Command::Shutdown { instance_id, mode } = &request.command {
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
        let result = shutdown(&runtime, &admission, &handle, *mode)
            .map_err(|error| RpcError::operation(&error))
            .and_then(protocol::encode::<rpc::Shutdown>);
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
    let result = execute(&runtime, &admission, &handle, request.command);
    protocol::write(
        reader.get_mut(),
        &Response {
            id: request.id,
            outcome: outcome(result),
        },
    )
}

/// Admits and runs one management command. Both the IPC endpoint and the
/// service-hosted web UI use this path.
pub(crate) fn execute(
    runtime: &Arc<DesktopRuntime>,
    admission: &Admission,
    handle: &Handle,
    command: Command,
) -> Result<Value, RpcError> {
    if matches!(command, Command::State) {
        return handle.block_on(crate::dispatch::dispatch(runtime, command));
    }
    let _operation = admission
        .mutations
        .read()
        .map_err(|_| RpcError::new("busy", "Management admission failed."))?;
    if admission.draining.load(Ordering::Acquire) {
        return Err(RpcError::new("busy", "The backend is shutting down."));
    }
    let _export = if matches!(command, Command::ExportUsage { .. }) {
        Some(
            admission
                .exports
                .try_acquire()
                .map_err(|_| RpcError::new("busy", "Another export is in progress."))?,
        )
    } else {
        None
    };
    handle.block_on(crate::dispatch::dispatch(runtime, command))
}

fn shutdown(
    runtime: &Arc<DesktopRuntime>,
    admission: &Admission,
    handle: &Handle,
    mode: ShutdownMode,
) -> Result<(), String> {
    if admission.draining.swap(true, Ordering::AcqRel) {
        return Err("Shutdown is already in progress".into());
    }
    let result = (|| {
        let _exclusive = admission
            .mutations
            .write()
            .map_err(|_| "Management admission unavailable")?;
        handle.block_on(runtime.shutdown(mode))
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
