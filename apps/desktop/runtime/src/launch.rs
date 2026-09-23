//! Explicit, platform-native launch of the persistent PAP service.

use std::{
    io::{self, Read},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const SERVICE_BINARY: &str = "private-ai-proxy-service";
const MAX_STARTUP_DIAGNOSTICS: usize = 16 * 1024;

pub struct BackgroundService {
    child: Child,
    stderr_reader: Option<JoinHandle<()>>,
    diagnostics: Arc<Mutex<Vec<u8>>>,
}

impl BackgroundService {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    pub fn startup_diagnostic(&mut self) -> Option<String> {
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        self.diagnostic_snapshot()
    }

    pub fn diagnostic_snapshot(&self) -> Option<String> {
        let diagnostics = self.diagnostics.lock().ok()?;
        diagnostics
            .split(|byte| *byte == b'\n' || *byte == b'\r')
            .filter_map(|line| {
                let line = String::from_utf8_lossy(line).trim().to_string();
                (!line.is_empty()).then_some(line)
            })
            .next_back()
            .map(|line| line.chars().take(2_048).collect())
    }
}

pub fn service_executable() -> Result<PathBuf, String> {
    sibling_executable(SERVICE_BINARY)
}

fn sibling_executable(name: &str) -> Result<PathBuf, String> {
    let current = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the application executable: {error}"))?;
    let current = std::fs::canonicalize(&current).map_err(|error| {
        format!(
            "Cannot resolve the application executable {}: {error}",
            current.display()
        )
    })?;
    let directory = current
        .parent()
        .ok_or_else(|| "Cannot locate the application directory".to_string())?;
    let file_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let candidate = directory.join(file_name);
    let executable = std::fs::canonicalize(&candidate).map_err(|error| {
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
    let metadata = std::fs::metadata(&executable).map_err(|error| {
        format!(
            "Cannot inspect executable {}: {error}",
            executable.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "Executable is not a file: {}",
            executable.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "Executable is not executable: {}",
                executable.display()
            ));
        }
    }
    Ok(executable)
}

pub fn spawn_background() -> Result<BackgroundService, String> {
    let executable = service_executable()?;
    let working_directory = executable
        .parent()
        .ok_or_else(|| "Cannot locate the service executable directory".to_string())?;
    let mut command = Command::new(&executable);
    command
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // Inherit runtime-selection variables unchanged. A child-only data override
    // would move its socket away from the parent's Linux or macOS endpoint.
    configure_background_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start PAP service: {error}"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Cannot capture PAP service startup diagnostics")?;
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let captured = diagnostics.clone();
    let stderr_reader = std::thread::spawn(move || capture_stderr(stderr, captured));
    Ok(BackgroundService {
        child,
        stderr_reader: Some(stderr_reader),
        diagnostics,
    })
}

fn capture_stderr(mut stderr: impl Read, diagnostics: Arc<Mutex<Vec<u8>>>) {
    let mut buffer = [0_u8; 4 * 1024];
    loop {
        let read = match stderr.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => read,
            Err(_) => return,
        };
        let Ok(mut captured) = diagnostics.lock() else {
            return;
        };
        captured.extend_from_slice(&buffer[..read]);
        if captured.len() > MAX_STARTUP_DIAGNOSTICS {
            let excess = captured.len() - MAX_STARTUP_DIAGNOSTICS;
            captured.drain(..excess);
        }
    }
}

/// Wait until the operating system reports that `pid` has exited. This never
/// sends a signal or otherwise changes the target process.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> Result<(), String> {
    if pid == 0 {
        return Err("Cannot wait for process ID 0".to_string());
    }
    wait_for_exit_native(pid, timeout)
}

#[cfg(all(unix, not(all(target_os = "macos", feature = "mac-app-store"))))]
fn configure_background_command(command: &mut Command) {
    use std::{io, os::unix::process::CommandExt};

    // SAFETY: `setsid` is async-signal-safe and the closure performs no other
    // work between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(windows)]
fn configure_background_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
    };

    command.creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

#[cfg(any(
    all(target_os = "macos", feature = "mac-app-store"),
    not(any(unix, windows))
))]
fn configure_background_command(_: &mut Command) {}

#[cfg(target_os = "linux")]
fn wait_for_exit_native(pid: u32, timeout: Duration) -> Result<(), String> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    if pid > libc::pid_t::MAX as u32 {
        return Err(format!("Process ID is outside the native range: {pid}"));
    }

    // SAFETY: `pidfd_open` takes scalar arguments and returns a new owned file
    // descriptor on success.
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    if raw_fd == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(format!("Cannot observe PAP service process {pid}: {error}"));
    }
    // SAFETY: the successful syscall returned a new descriptor owned here.
    let pid_fd = unsafe { OwnedFd::from_raw_fd(raw_fd as libc::c_int) };
    let started = Instant::now();

    loop {
        let mut descriptor = libc::pollfd {
            fd: pid_fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let wait_ms = remaining_millis(started, timeout);
        // SAFETY: `descriptor` points to one valid pollfd for the duration of
        // the call, and the pidfd remains open.
        let result = unsafe { libc::poll(&mut descriptor, 1, wait_ms) };
        if result > 0 {
            if descriptor.revents & libc::POLLIN != 0 {
                return Ok(());
            }
            return Err(format!(
                "Unexpected pidfd event {} for PAP service process {pid}",
                descriptor.revents
            ));
        }
        if result == 0 {
            if started.elapsed() >= timeout {
                return Err(exit_timeout(pid, timeout));
            }
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(format!(
                "Cannot wait for PAP service process {pid}: {error}"
            ));
        }
    }
}

#[cfg(target_os = "macos")]
fn wait_for_exit_native(pid: u32, timeout: Duration) -> Result<(), String> {
    use std::{mem::MaybeUninit, os::fd::FromRawFd, os::fd::OwnedFd, ptr};

    if pid > libc::pid_t::MAX as u32 {
        return Err(format!("Process ID is outside the native range: {pid}"));
    }

    // SAFETY: `kqueue` returns a new owned descriptor on success.
    let raw_queue = unsafe { libc::kqueue() };
    if raw_queue == -1 {
        return Err(format!(
            "Cannot create a PAP service process observer: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: the successful call returned a new descriptor owned here.
    let queue = unsafe { OwnedFd::from_raw_fd(raw_queue) };
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: ptr::null_mut(),
    };
    // SAFETY: `queue` is valid and `change` remains live for the call.
    let registered =
        unsafe { libc::kevent(raw_queue, &change, 1, ptr::null_mut(), 0, ptr::null()) };
    if registered == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(format!("Cannot observe PAP service process {pid}: {error}"));
    }

    let started = Instant::now();
    loop {
        let wait = remaining_timespec(started, timeout);
        let mut event = MaybeUninit::<libc::kevent>::uninit();
        // SAFETY: the queue is valid, the output points to one kevent, and the
        // timeout value remains live for the call.
        let result =
            unsafe { libc::kevent(raw_queue, ptr::null(), 0, event.as_mut_ptr(), 1, &wait) };
        if result > 0 {
            drop(queue);
            return Ok(());
        }
        if result == 0 {
            if started.elapsed() >= timeout {
                return Err(exit_timeout(pid, timeout));
            }
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(format!(
                "Cannot wait for PAP service process {pid}: {error}"
            ));
        }
    }
}

#[cfg(windows)]
fn wait_for_exit_native(pid: u32, timeout: Duration) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };

    struct ProcessHandle(HANDLE);
    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by `OpenProcess` and is closed
            // exactly once by this owner.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    // SAFETY: the call receives scalar values and returns an owned handle.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err(format!("Cannot observe PAP service process {pid}: {error}"));
    }
    let handle = ProcessHandle(handle);
    let started = Instant::now();

    loop {
        let wait_ms = remaining_windows_millis(started, timeout);
        // SAFETY: `handle` stays valid for the duration of the wait.
        match unsafe { WaitForSingleObject(handle.0, wait_ms) } {
            WAIT_OBJECT_0 => return Ok(()),
            WAIT_TIMEOUT => {
                if started.elapsed() >= timeout {
                    return Err(exit_timeout(pid, timeout));
                }
            }
            WAIT_FAILED => {
                return Err(format!(
                    "Cannot wait for PAP service process {pid}: {}",
                    io::Error::last_os_error()
                ));
            }
            result => {
                return Err(format!(
                    "Unexpected wait result {result} for PAP service process {pid}"
                ));
            }
        }
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn wait_for_exit_native(pid: u32, timeout: Duration) -> Result<(), String> {
    if pid > libc::pid_t::MAX as u32 {
        return Err(format!("Process ID is outside the native range: {pid}"));
    }
    let started = Instant::now();

    loop {
        // SAFETY: signal zero performs an existence/permission check and does
        // not deliver a signal to the process.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(format!("Cannot observe PAP service process {pid}: {error}"));
            }
        }

        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(exit_timeout(pid, timeout));
        };
        std::thread::sleep(remaining.min(Duration::from_millis(25)));
    }
}

#[cfg(not(any(unix, windows)))]
fn wait_for_exit_native(_: u32, _: Duration) -> Result<(), String> {
    Err("Waiting for process exit is unsupported on this platform".to_string())
}

#[cfg(target_os = "linux")]
fn remaining_millis(started: Instant, timeout: Duration) -> libc::c_int {
    let remaining = timeout.checked_sub(started.elapsed()).unwrap_or_default();
    if remaining.is_zero() {
        0
    } else {
        remaining.as_millis().max(1).min(libc::c_int::MAX as u128) as libc::c_int
    }
}

#[cfg(target_os = "macos")]
fn remaining_timespec(started: Instant, timeout: Duration) -> libc::timespec {
    let remaining = timeout.checked_sub(started.elapsed()).unwrap_or_default();
    libc::timespec {
        tv_sec: remaining.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
        tv_nsec: remaining.subsec_nanos() as libc::c_long,
    }
}

#[cfg(windows)]
fn remaining_windows_millis(started: Instant, timeout: Duration) -> u32 {
    const MAX_FINITE_WAIT: u128 = u32::MAX as u128 - 1;
    let remaining = timeout.checked_sub(started.elapsed()).unwrap_or_default();
    if remaining.is_zero() {
        0
    } else {
        remaining.as_millis().clamp(1, MAX_FINITE_WAIT) as u32
    }
}

#[cfg(any(unix, windows))]
fn exit_timeout(pid: u32, timeout: Duration) -> String {
    format!(
        "Timed out after {} ms waiting for PAP service process {pid} to exit",
        timeout.as_millis()
    )
}

// MAS resolves IPC from its app container, not inherited native runtime variables.
#[cfg(all(test, not(all(target_os = "macos", feature = "mac-app-store"))))]
mod tests {
    use super::*;

    const ENDPOINT_FIXTURE_ROLE: &str = "PAP_TEST_ENDPOINT_FIXTURE_ROLE";
    const ENDPOINT_FIXTURE_OUTPUT: &str = "PAP_TEST_ENDPOINT_FIXTURE_OUTPUT";
    const ENDPOINT_FIXTURE_TEST: &str = "launch::tests::endpoint_resolution_fixture";

    #[test]
    fn parent_and_child_resolve_the_same_backend_endpoint() {
        for configured in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let runtime = fixture.path().join("runtime");
            std::fs::create_dir(&runtime).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let output = fixture.path().join("endpoint");
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--ignored", "--exact", ENDPOINT_FIXTURE_TEST, "--nocapture"])
                .env(ENDPOINT_FIXTURE_ROLE, "parent")
                .env(ENDPOINT_FIXTURE_OUTPUT, &output)
                .env_remove(agent_bridge::agents::HOME_OVERRIDE_ENV);
            if configured {
                command
                    .env(
                        agent_bridge::agents::APP_DATA_OVERRIDE_ENV,
                        fixture.path().join("data"),
                    )
                    .env("XDG_RUNTIME_DIR", &runtime);
            } else {
                command
                    .env_remove(agent_bridge::agents::APP_DATA_OVERRIDE_ENV)
                    .env_remove("XDG_RUNTIME_DIR");
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "configured={configured}\n{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }

    #[test]
    #[ignore = "subprocess fixture for endpoint inheritance"]
    fn endpoint_resolution_fixture() {
        match std::env::var(ENDPOINT_FIXTURE_ROLE).as_deref() {
            Ok("parent") => {
                let parent = crate::transport::endpoint_path().unwrap();
                let result = Command::new(std::env::current_exe().unwrap())
                    .args(["--ignored", "--exact", ENDPOINT_FIXTURE_TEST, "--nocapture"])
                    .env(ENDPOINT_FIXTURE_ROLE, "child")
                    .output()
                    .unwrap();
                assert!(result.status.success());
                let output = std::env::var_os(ENDPOINT_FIXTURE_OUTPUT).unwrap();
                assert_eq!(
                    std::fs::read_to_string(output).unwrap(),
                    parent.to_string_lossy()
                );
            }
            Ok("child") => {
                let endpoint = crate::transport::endpoint_path().unwrap();
                let output = std::env::var_os(ENDPOINT_FIXTURE_OUTPUT).unwrap();
                std::fs::write(output, endpoint.to_string_lossy().as_bytes()).unwrap();
            }
            _ => {}
        }
    }
}
