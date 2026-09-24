use std::{
    io::BufReader,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use serde_json::Value;
use tokio::{
    runtime::Handle,
    sync::{RwLock, Semaphore},
    task::JoinSet,
};

use crate::controller::DesktopRuntime;

use desktop_core::{
    protocol::{self, rpc, Command, Hello, Outcome, Request, Response, RpcError, ShutdownMode},
    transport::{Listener, Stream},
};

const MAX_CLIENTS: usize = 32;
/// How long shutdown waits for running management commands (a slow account
/// request, an export) and afterwards for open connections, and how long the
/// service waits for leftover blocking tasks, before continuing without them.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// A shutdown that has not returned by then exits the process, as systemd's
/// `TimeoutStopSec` or `docker stop`'s grace period would end it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_IN_PROGRESS: &str = "Shutdown is already in progress";

/// Lifecycle admission shared by every management transport of one service.
pub(crate) struct Admission {
    draining: AtomicBool,
    pub(crate) mutations: RwLock<()>,
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
        let connect_on_launch = match startup.settings() {
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
                tracing::info!("The owning app exited");
                let worker = runtime.clone();
                let executor = handle.clone();
                let gate = admission.clone();
                match tokio::task::spawn_blocking(move || shutdown(&worker, &gate, &executor, ShutdownMode::Quit)).await {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => runtime.report_error(error),
                    Err(error) => tracing::warn!(
                        "Cannot finish backend shutdown: {error}"
                    ),
                }
                stopping.store(true, Ordering::Release);
                break;
            }
            result = &mut signal => {
                result?;
                tracing::info!("Received a shutdown signal");
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
    // A command blocked on the network is not waited for past the bound.
    tracing::info!("Shutdown: closing management connections");
    let drained = tokio::time::timeout(DRAIN_TIMEOUT, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            "Shutdown: {} management connections still open after {} s; closing without them",
            tasks.len(),
            DRAIN_TIMEOUT.as_secs()
        );
    }
    Ok(())
}

async fn owner_exited() {
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    {
        // The owner is this process's parent, so its exit is observed with
        // kqueue's EVFILT_PROC/NOTE_EXIT like `launch::wait_for_exit`; a
        // parent of 1 (launchd) means it already exited. No IPC endpoint is
        // exposed to external agents. The waiting thread is detached: a
        // blocking task would hold up the runtime's shutdown.
        let parent = unsafe { libc::getppid() };
        let (exited, exit) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let result = if parent == 1 {
                Ok(())
            } else {
                desktop_core::launch::wait_for_exit(parent as u32, Duration::MAX)
            };
            let _ = exited.send(result);
        });
        // A sandboxed service must not outlive its owner, so an owner that
        // cannot be observed also stops it.
        if let Ok(Err(error)) = exit.await {
            tracing::error!("Cannot observe the owning app: {error}");
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
        tracing::info!("A client requested shutdown");
        // A refusal's reason, which may name local paths, stays in the service log.
        let result = shutdown(&runtime, &admission, &handle, *mode)
            .map_err(|error| match error.as_str() {
                SHUTDOWN_IN_PROGRESS => RpcError::operation(&error),
                _ => RpcError::new(
                    "shutdown_refused",
                    "The backend did not stop and keeps running; the service log has the reason (`pap doctor` shows where).",
                ),
            })
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
    let _operation = admission.mutations.blocking_read();
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
        return Err(SHUTDOWN_IN_PROGRESS.into());
    }
    tracing::info!("Shutting down ({mode:?})");
    let _watchdog = Watchdog::arm();
    let result = handle.block_on(drain_and_stop(runtime, admission, mode));
    if let Err(error) = &result {
        admission.draining.store(false, Ordering::Release);
        tracing::error!("Shutdown refused; the backend keeps running: {error}");
    }
    result
}

/// New commands are refused while draining; running ones get [`DRAIN_TIMEOUT`]
/// before the runtime stops without them.
pub(crate) async fn drain_and_stop(
    runtime: &DesktopRuntime,
    admission: &Admission,
    mode: ShutdownMode,
) -> Result<(), String> {
    tracing::info!("Shutdown: waiting for running commands");
    let _exclusive = tokio::time::timeout(DRAIN_TIMEOUT, admission.mutations.write())
        .await
        .inspect_err(|_| {
            tracing::warn!(
                "Shutdown: commands still running after {} s; continuing without them",
                DRAIN_TIMEOUT.as_secs()
            )
        });
    runtime.shutdown(mode).await
}

/// Exits the process if a shutdown has not returned within
/// [`SHUTDOWN_TIMEOUT`] (a hung transaction or network call); dropping it
/// disarms it.
struct Watchdog {
    _disarm: mpsc::Sender<()>,
}

impl Watchdog {
    fn arm() -> Self {
        let (disarm, disarmed) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            if disarmed.recv_timeout(SHUTDOWN_TIMEOUT) == Err(mpsc::RecvTimeoutError::Timeout) {
                tracing::error!(
                    "Shutdown did not finish within {} s; exiting",
                    SHUTDOWN_TIMEOUT.as_secs()
                );
                std::process::exit(1);
            }
        });
        Self { _disarm: disarm }
    }
}

fn outcome(result: Result<Value, RpcError>) -> Outcome {
    match result {
        Ok(value) => Outcome::Result(value),
        Err(error) => Outcome::Error(error),
    }
}
