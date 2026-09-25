//! The service's lifecycle around the management API: it serves the API on
//! the private local endpoint until a client, a signal or the owning app's
//! exit shuts it down, and admits each command.

use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use serde_json::Value;
use tokio::{
    runtime::Handle,
    sync::{Notify, RwLock, Semaphore},
};
use tokio_util::sync::CancellationToken;
use tower::limit::GlobalConcurrencyLimitLayer;

use crate::{
    api::{self, Api, ServiceBackend, ServiceHost},
    controller::DesktopRuntime,
};

use desktop_core::{
    protocol::{self, rpc, Command, ErrorCode, ShutdownMode},
    transport,
};

/// How long shutdown waits for running management commands (a slow account
/// request, an export) before stopping without them.
pub(crate) const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the exit then waits for open connections, and the service for
/// leftover blocking tasks, before continuing without them.
pub const EXIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// A process still running this long after its shutdown began exits, as
/// systemd's `TimeoutStopSec` or `docker stop`'s grace period would end it.
/// It covers both drains, the stop steps and the final exit drains.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_IN_PROGRESS: &str = "Shutdown is already in progress";
/// Requests the local endpoint runs at once; later ones wait for a slot.
const MAX_REQUESTS: usize = 64;
/// Connections the local endpoint holds open at once, event streams included;
/// later ones wait to be accepted.
const MAX_CONNECTIONS: usize = 64;

/// Lifecycle admission shared by every listener of one service.
pub(crate) struct Admission {
    draining: AtomicBool,
    pub(crate) mutations: RwLock<()>,
    exports: Semaphore,
    /// Cancelled once a shutdown succeeded; the service then exits.
    stopped: CancellationToken,
    /// Notified when a client's shutdown was refused and the backend keeps running.
    refused: Notify,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            draining: AtomicBool::new(false),
            mutations: RwLock::new(()),
            exports: Semaphore::new(1),
            stopped: CancellationToken::new(),
            refused: Notify::new(),
        }
    }
}

pub async fn serve(runtime: Arc<DesktopRuntime>) -> Result<(), String> {
    let listener = transport::Listener::bind(runtime.instance_lock()?)
        .map_err(|error| format!("Cannot bind the private management endpoint: {error}"))?;
    let admission = runtime.admission();
    let stopped = admission.stopped.clone();
    let handle = Handle::current();
    let startup = runtime.clone();
    let startup = tokio::task::spawn_blocking(move || {
        // Before connecting, which may need an imported key.
        startup.import_legacy_secrets();
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
    // Bounds the requests the local endpoint runs at once, as the removed
    // NDJSON server bounded its connections; an event stream holds a slot
    // only until its response starts.
    let api = api::router(Api {
        backend: ServiceBackend(runtime.clone()),
        host: ServiceHost::default(),
        states: runtime.subscribe(),
        shutdown: stopped.clone(),
        listener: api::Listener::Local,
    })
    .layer(GlobalConcurrencyLimitLayer::new(MAX_REQUESTS));
    let signal = stopped.clone().cancelled_owned();
    let server = tokio::spawn(async move {
        desktop_core::serve::serve(LocalListener(listener), api, MAX_CONNECTIONS, signal)
            .await
            .await
    });
    let owner = owner_exited();
    let signal = shutdown_signal();
    // A signal (systemd, launchd, `docker stop`) or the owning app's exit
    // always ends the service; only a client's `shutdown` can be refused. A
    // step that fails, such as restoring an agent configuration, is logged
    // and retried on the next launch.
    let reason = tokio::select! {
        () = stopped.cancelled() => None,
        () = owner => Some("The owning app exited"),
        result = signal => {
            result?;
            Some("Received a shutdown signal")
        }
    };
    if let Some(reason) = reason {
        tracing::info!("{reason}");
        loop {
            // Registered before trying, so a refusal in between is not missed.
            let refused = admission.refused.notified();
            let (worker, gate, executor) = (runtime.clone(), admission.clone(), handle.clone());
            match tokio::task::spawn_blocking(move || {
                shutdown(&worker, &gate, &executor, ShutdownMode::Quit, false)
            })
            .await
            {
                // A client's shutdown is running, bounded by its watchdog: it
                // stops the backend unless it is refused, and then this one runs.
                Ok(Err(error)) if error == SHUTDOWN_IN_PROGRESS => tokio::select! {
                    () = stopped.cancelled() => {}
                    () = refused => continue,
                },
                Ok(Ok(())) => {}
                Ok(Err(error)) => runtime.report_error(error),
                Err(error) => tracing::warn!("Cannot finish backend shutdown: {error}"),
            }
            break;
        }
    }
    stopped.cancel();
    drop(startup);
    // Open connections finish their answer and close; event streams end at
    // `stopped`. A command blocked on the network is not waited for past the bound.
    tracing::info!("Shutdown: closing management connections");
    if tokio::time::timeout(EXIT_DRAIN_TIMEOUT, server)
        .await
        .is_err()
    {
        tracing::warn!(
            "Shutdown: management connections still open after {} s; closing without them",
            EXIT_DRAIN_TIMEOUT.as_secs()
        );
    }
    Ok(())
}

/// The local endpoint as an axum listener. Accept errors never end the
/// service: a peer of another user is refused, and a failure such as `EMFILE`
/// waits a second before accepting again, as axum's own listeners (after
/// hyper 0.14's `sleep_on_errors`) handle them.
struct LocalListener(transport::Listener);

impl axum::serve::Listener for LocalListener {
    type Io = transport::Stream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok(stream) => return (stream, ()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::BrokenPipe
                    ) =>
                {
                    tracing::debug!("Refused a management connection: {error}");
                }
                Err(error) => {
                    tracing::error!("Management endpoint accept error: {error}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
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

/// Admits and runs one management command, for every listener.
pub(crate) fn execute(
    runtime: &Arc<DesktopRuntime>,
    admission: &Admission,
    handle: &Handle,
    command: Command,
) -> Result<Value, protocol::Error> {
    match command {
        Command::GetState {} => {
            return handle.block_on(crate::dispatch::dispatch(runtime, command))
        }
        Command::Shutdown { instance_id, mode } => {
            if instance_id != api::version().instance_id {
                return Err(protocol::Error::new(
                    ErrorCode::InstanceChanged,
                    "The backend instance changed; reconnect before shutting down.",
                ));
            }
            tracing::info!("A client requested shutdown");
            // A refusal's reason, which may name local paths, stays in the service log.
            return shutdown(runtime, admission, handle, mode, true)
                .map_err(|error| match error.as_str() {
                    SHUTDOWN_IN_PROGRESS => protocol::Error::busy(),
                    _ => protocol::Error::new(
                        ErrorCode::ShutdownRefused,
                        "The backend did not stop and keeps running; the service log has the reason (`pap doctor` shows where).",
                    ),
                })
                .and_then(|()| protocol::encode::<rpc::Shutdown>(()));
        }
        _ => {}
    }
    let _operation = admission.mutations.blocking_read();
    if admission.draining.load(Ordering::Acquire) {
        return Err(protocol::Error::new(
            ErrorCode::Busy,
            "The backend is shutting down.",
        ));
    }
    let _export =
        if matches!(command, Command::ExportUsage { .. }) {
            Some(admission.exports.try_acquire().map_err(|_| {
                protocol::Error::new(ErrorCode::Busy, "Another export is in progress.")
            })?)
        } else {
            None
        };
    handle.block_on(crate::dispatch::dispatch(runtime, command))
}

/// Stops the backend. A failure keeps it running when `refusable`; a signal or
/// the owning app's exit (not refusable) ends it regardless.
fn shutdown(
    runtime: &Arc<DesktopRuntime>,
    admission: &Admission,
    handle: &Handle,
    mode: ShutdownMode,
    refusable: bool,
) -> Result<(), String> {
    if admission.draining.swap(true, Ordering::AcqRel) {
        return Err(SHUTDOWN_IN_PROGRESS.into());
    }
    tracing::info!("Shutting down ({mode:?})");
    let watchdog = Watchdog::arm();
    let result = handle.block_on(drain_and_stop(runtime, admission, mode, refusable));
    match &result {
        Err(error) if refusable => {
            admission.draining.store(false, Ordering::Release);
            admission.refused.notify_waiters();
            tracing::error!("Shutdown refused; the backend keeps running: {error}");
        }
        Err(error) => {
            tracing::error!("Shutdown failed; exiting anyway: {error}");
            watchdog.keep_until_exit();
            admission.stopped.cancel();
        }
        Ok(()) => {
            watchdog.keep_until_exit();
            admission.stopped.cancel();
        }
    }
    result
}

/// New commands are refused while draining; running ones get [`DRAIN_TIMEOUT`]
/// before the runtime stops without them.
pub(crate) async fn drain_and_stop(
    runtime: &DesktopRuntime,
    admission: &Admission,
    mode: ShutdownMode,
    refusable: bool,
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
    runtime
        .shutdown(mode, refusable)
        .await
        .map_err(|error| error.to_string())
}

/// Exits the process if it is still running [`SHUTDOWN_TIMEOUT`] after
/// shutdown began (a hung transaction, network call or drain). Dropping it
/// disarms it, for a refused shutdown.
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

    /// Never disarm: the bound holds through the exit drains until the
    /// process exits.
    fn keep_until_exit(self) {
        std::mem::forget(self);
    }
}
