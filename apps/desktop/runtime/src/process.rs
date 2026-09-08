//! Platform-neutral process ownership for the ACI sidecar.
//!
//! The desktop process starts a small instance of itself as a supervisor. The
//! supervisor owns ACI and watches a pipe held by the desktop process. Normal
//! shutdown closes the pipe explicitly; an abrupt desktop-process exit closes
//! it in the kernel. In both cases the supervisor terminates and reaps ACI.

use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self as std_mpsc, RecvTimeoutError},
    thread,
    time::Duration,
};

use crate::gateway::{SidecarChild, SidecarEvent, SidecarLauncher};
use tokio::{
    runtime::Handle,
    sync::mpsc::{self, Receiver, Sender},
};

const ACI_BINARY: &str = "pap";
const SUPERVISOR_ARGUMENT: &str = "--pap-internal-aci-supervisor-v1";
const EVENT_BUFFER: usize = 256;
const PIPE_BUFFER: usize = 8 * 1024;
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(10);

pub struct TokioSidecarLauncher {
    runtime: Handle,
    supervisor_executable: PathBuf,
}

impl TokioSidecarLauncher {
    pub fn new(aci_executable: PathBuf) -> Result<Self, String> {
        let expected = sibling_executable(ACI_BINARY)?;
        let supplied = canonicalize_executable(&aci_executable)?;
        if supplied != expected {
            return Err(format!(
                "ACI executable must be the bundled sibling {}",
                expected.display()
            ));
        }

        let runtime = Handle::try_current()
            .map_err(|_| "The ACI launcher requires an active Tokio runtime".to_string())?;
        let supervisor_executable = canonical_current_executable()?;
        Ok(Self {
            runtime,
            supervisor_executable,
        })
    }
}

impl SidecarLauncher for TokioSidecarLauncher {
    fn spawn(
        &self,
        args: Vec<String>,
    ) -> Result<(Receiver<SidecarEvent>, Box<dyn SidecarChild>), String> {
        let mut command = Command::new(&self.supervisor_executable);
        command
            .arg(SUPERVISOR_ARGUMENT)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_supervisor_command(&mut command);

        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start the ACI supervisor: {error}"))?;
        let guard = child
            .stdin
            .take()
            .ok_or_else(|| "Cannot open the ACI supervisor ownership pipe".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Cannot capture ACI stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Cannot capture ACI stderr".to_string())?;

        let (events, receiver) = mpsc::channel(EVENT_BUFFER);
        forward_pipe(
            &self.runtime,
            stdout,
            events.clone(),
            SidecarEvent::Stdout,
            "stdout",
        );
        forward_pipe(
            &self.runtime,
            stderr,
            events.clone(),
            SidecarEvent::Stderr,
            "stderr",
        );

        let (reaped_tx, reaped_rx) = std_mpsc::sync_channel(1);
        drop(self.runtime.spawn_blocking(move || {
            let result = child
                .wait()
                .map(|_| ())
                .map_err(|error| format!("Cannot reap the ACI supervisor: {error}"));
            let event = match &result {
                Ok(()) => SidecarEvent::Terminated,
                Err(error) => SidecarEvent::Error(error.clone()),
            };
            let _ = reaped_tx.send(result);
            let _ = events.blocking_send(event);
        }));

        Ok((
            receiver,
            Box::new(TokioSidecarChild {
                guard: Some(guard),
                reaped: Some(reaped_rx),
            }),
        ))
    }
}

struct TokioSidecarChild {
    guard: Option<ChildStdin>,
    reaped: Option<std_mpsc::Receiver<Result<(), String>>>,
}

impl TokioSidecarChild {
    fn stop_and_reap(&mut self) -> Result<(), String> {
        self.stop_and_reap_with_timeout(CHILD_REAP_TIMEOUT)
    }

    fn stop_and_reap_with_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        self.guard.take();
        let Some(reaped) = self.reaped.take() else {
            return Ok(());
        };
        match reaped.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.reaped = Some(reaped);
                Err(format!(
                    "Timed out after {} ms waiting for the ACI supervisor to exit",
                    timeout.as_millis()
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err("The ACI supervisor exited without a reap result".to_string())
            }
        }
    }
}

impl SidecarChild for TokioSidecarChild {
    fn kill(&mut self) -> Result<(), String> {
        self.stop_and_reap()
    }
}

impl Drop for TokioSidecarChild {
    fn drop(&mut self) {
        let _ = self.stop_and_reap();
    }
}

/// Run the internal ACI supervisor when the current process was launched in
/// supervisor mode. Call this before initializing any desktop framework.
pub fn run_sidecar_supervisor_if_requested() -> Result<Option<ExitStatus>, String> {
    let mut arguments = env::args_os();
    let _ = arguments.next();
    if arguments.next().as_deref() != Some(OsStr::new(SUPERVISOR_ARGUMENT)) {
        return Ok(None);
    }

    let executable = sibling_executable(ACI_BINARY)?;
    supervise(executable, arguments).map(Some)
}

pub(crate) fn sibling_executable(name: &str) -> Result<PathBuf, String> {
    let current = canonical_current_executable()?;
    let directory = current
        .parent()
        .ok_or_else(|| "Cannot locate the application directory".to_string())?;
    let candidate = directory.join(binary_name(name));
    let executable = fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "Cannot locate bundled executable {}: {error}",
            candidate.display()
        )
    })?;

    if executable.parent() != Some(directory) {
        return Err(format!(
            "Bundled executable resolves outside the application directory: {}",
            executable.display()
        ));
    }
    validate_executable(&executable)?;
    Ok(executable)
}

fn canonical_current_executable() -> Result<PathBuf, String> {
    let executable = env::current_exe()
        .map_err(|error| format!("Cannot locate the application executable: {error}"))?;
    fs::canonicalize(&executable).map_err(|error| {
        format!(
            "Cannot resolve the application executable {}: {error}",
            executable.display()
        )
    })
}

fn canonicalize_executable(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "Executable path must be absolute: {}",
            path.display()
        ));
    }
    let executable = fs::canonicalize(path)
        .map_err(|error| format!("Cannot resolve executable {}: {error}", path.display()))?;
    validate_executable(&executable)?;
    Ok(executable)
}

fn binary_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn validate_executable(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Cannot inspect executable {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("Executable is not a file: {}", path.display()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("Executable is not executable: {}", path.display()));
        }
    }

    Ok(())
}

fn forward_pipe<R, F>(
    runtime: &Handle,
    mut pipe: R,
    events: Sender<SidecarEvent>,
    event: F,
    stream_name: &'static str,
) where
    R: Read + Send + 'static,
    F: Fn(Vec<u8>) -> SidecarEvent + Send + 'static,
{
    drop(runtime.spawn_blocking(move || {
        let mut buffer = [0_u8; PIPE_BUFFER];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => return,
                Ok(length) => {
                    if events
                        .blocking_send(event(buffer[..length].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = events.try_send(SidecarEvent::Error(format!(
                        "Cannot read ACI {stream_name}: {error}"
                    )));
                    return;
                }
            }
        }
    }));
}

fn supervise<I>(executable: PathBuf, arguments: I) -> Result<ExitStatus, String>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    let mut command = Command::new(&executable);
    command.args(arguments);
    supervise_command(command)
}

fn supervise_command(mut command: Command) -> Result<ExitStatus, String> {
    let (owner_gone_tx, owner_gone_rx) = std_mpsc::sync_channel(1);
    thread::Builder::new()
        .name("aci-owner-watch".to_string())
        .spawn(move || {
            let mut input = io::stdin().lock();
            let mut byte = [0_u8; 1];
            loop {
                match input.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = owner_gone_tx.send(());
        })
        .map_err(|error| format!("Cannot start the ACI ownership monitor: {error}"))?;

    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    configure_aci_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start bundled ACI executable: {error}"))?;

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Cannot inspect ACI process state: {error}"))?
        {
            return Ok(status);
        }

        match owner_gone_rx.recv_timeout(SUPERVISOR_POLL_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Err(kill_error) = child.kill() {
                    if let Some(status) = child
                        .try_wait()
                        .map_err(|error| format!("Cannot inspect ACI after owner exit: {error}"))?
                    {
                        return Ok(status);
                    }
                    return Err(format!("Cannot stop ACI after owner exit: {kill_error}"));
                }
                return child
                    .wait()
                    .map_err(|error| format!("Cannot reap ACI after owner exit: {error}"));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(windows)]
fn configure_supervisor_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_supervisor_command(_: &mut Command) {}

#[cfg(windows)]
fn configure_aci_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_aci_command(_: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        process::{Child, ChildStdin},
        time::Instant,
    };

    const FIXTURE_TEST: &str = "process::tests::supervisor_process_fixture";
    const FIXTURE_ROLE: &str = "PAP_PROCESS_FIXTURE_ROLE";
    const FIXTURE_PID_FILE: &str = "PAP_PROCESS_FIXTURE_PID_FILE";
    const FIXTURE_TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn supervisor_reaps_aci_when_owner_pipe_closes() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let pid_file = directory.path().join("aci.pid");
        let (mut supervisor, owner) = spawn_supervisor_fixture(&pid_file);
        let aci_pid = wait_for_fixture_pid(&mut supervisor, &pid_file);

        drop(owner);
        let status = wait_for_child(&mut supervisor, FIXTURE_TIMEOUT)
            .expect("supervisor should exit after ownership pipe closure");
        assert!(status.success(), "supervisor fixture failed: {status}");
        crate::launch::wait_for_exit(aci_pid, FIXTURE_TIMEOUT)
            .expect("fake ACI should be terminated and reaped");
    }

    #[test]
    fn normal_stop_reaps_supervisor_and_finishes_waiter_thread() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let pid_file = directory.path().join("aci.pid");
        let (mut supervisor, owner) = spawn_supervisor_fixture(&pid_file);
        let aci_pid = wait_for_fixture_pid(&mut supervisor, &pid_file);
        let (reaped_tx, reaped_rx) = std_mpsc::sync_channel(1);
        let waiter = thread::Builder::new()
            .name("supervisor-test-reaper".to_string())
            .spawn(move || {
                let result = supervisor
                    .wait()
                    .map(|_| ())
                    .map_err(|error| format!("Cannot reap supervisor fixture: {error}"));
                let _ = reaped_tx.send(result);
            })
            .expect("supervisor waiter thread");
        let mut child = TokioSidecarChild {
            guard: Some(owner),
            reaped: Some(reaped_rx),
        };

        child
            .kill()
            .expect("normal stop should reap the supervisor");
        child.kill().expect("normal stop should be idempotent");
        waiter.join().expect("supervisor waiter should finish");
        crate::launch::wait_for_exit(aci_pid, FIXTURE_TIMEOUT)
            .expect("normal stop should terminate and reap fake ACI");
    }

    #[test]
    fn reap_timeout_keeps_completion_for_retry() {
        let (reaped_tx, reaped_rx) = std_mpsc::sync_channel(1);
        let mut child = TokioSidecarChild {
            guard: None,
            reaped: Some(reaped_rx),
        };

        let error = child
            .stop_and_reap_with_timeout(Duration::ZERO)
            .expect_err("an incomplete reap should time out");
        assert!(error.contains("Timed out"));
        assert!(child.reaped.is_some(), "timeout must retain reap ownership");

        reaped_tx.send(Ok(())).expect("send reap completion");
        child
            .kill()
            .expect("retained reap completion should be retryable");
    }

    #[test]
    #[ignore = "subprocess fixture invoked by the supervisor ownership tests"]
    fn supervisor_process_fixture() {
        match env::var(FIXTURE_ROLE).as_deref() {
            Ok("supervisor") => {
                let mut command = Command::new(env::current_exe().expect("test executable"));
                command
                    .args(["--ignored", "--exact", FIXTURE_TEST])
                    .env(FIXTURE_ROLE, "aci");
                supervise_command(command).expect("fixture supervisor");
            }
            Ok("aci") => {
                let path = env::var_os(FIXTURE_PID_FILE).expect("fixture PID file");
                let mut file = fs::File::create(path).expect("create fixture PID file");
                writeln!(file, "{}", std::process::id()).expect("write fixture PID");
                file.sync_all().expect("flush fixture PID");
                thread::sleep(Duration::from_secs(30));
            }
            _ => {}
        }
    }

    fn spawn_supervisor_fixture(pid_file: &Path) -> (Child, ChildStdin) {
        let mut command = Command::new(env::current_exe().expect("test executable"));
        command
            .args(["--ignored", "--exact", FIXTURE_TEST])
            .env(FIXTURE_ROLE, "supervisor")
            .env(FIXTURE_PID_FILE, pid_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        configure_supervisor_command(&mut command);
        let mut child = command.spawn().expect("spawn supervisor fixture");
        let owner = child.stdin.take().expect("supervisor ownership pipe");
        (child, owner)
    }

    fn wait_for_fixture_pid(supervisor: &mut Child, pid_file: &Path) -> u32 {
        let started = Instant::now();
        loop {
            if let Ok(value) = fs::read_to_string(pid_file) {
                if let Ok(pid) = value.trim().parse() {
                    return pid;
                }
            }
            if let Some(status) = supervisor.try_wait().expect("inspect supervisor fixture") {
                panic!("supervisor fixture exited before starting ACI: {status}");
            }
            assert!(
                started.elapsed() < FIXTURE_TIMEOUT,
                "timed out waiting for fake ACI to start"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_child(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("inspect child fixture") {
                return Some(status);
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
