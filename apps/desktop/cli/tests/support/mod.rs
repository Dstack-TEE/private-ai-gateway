//! Test-owned install trees whose processes cannot outlive the test.
//!
//! Teardown runs in a watchdog process outside the test's process tree, so it
//! also happens when the test process is killed rather than unwound. It starts
//! when the [`Sandbox`] is dropped or the test process dies (the watchdog's
//! stdin closes). It stops the backend through `service stop`, kills anything
//! still running from the sandbox, and removes the sandbox.

use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

/// The ignored `#[test]` that hosts the watchdog; it must call [`watchdog`].
pub const WATCHDOG_TEST: &str = "sandbox_teardown_watchdog";
const ROOT_ENV: &str = "PAP_TEST_SANDBOX_ROOT";
const ROLE_ENV: &str = "PAP_TEST_SANDBOX_ROLE";
const FAILURE: &str = "sandbox teardown failed: ";
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const KILL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Sandbox {
    root: PathBuf,
    /// Closing this pipe starts teardown.
    trigger: Option<ChildStdin>,
    /// Reaches EOF once the watchdog has finished teardown.
    report: Option<ChildStdout>,
}

impl Sandbox {
    pub fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("pap-test-")
            .tempdir()
            .unwrap()
            .keep();
        // The launcher starts the watchdog with the same pipes and exits, so
        // killing this process tree does not reach the watchdog.
        let mut launcher = Command::new(env::current_exe().unwrap())
            .args(["--exact", WATCHDOG_TEST, "--ignored", "--quiet"])
            .env(ROLE_ENV, "launcher")
            .env(ROOT_ENV, &root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let sandbox = Self {
            root,
            trigger: launcher.stdin.take(),
            report: launcher.stdout.take(),
        };
        assert!(launcher.wait().unwrap().success());
        sandbox
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Runs teardown and waits for it; later calls do nothing.
    pub fn close(&mut self) {
        drop(self.trigger.take());
        let Some(mut report) = self.report.take() else {
            return;
        };
        let mut output = String::new();
        let _ = report.read_to_string(&mut output);
        let failures: Vec<_> = output
            .lines()
            .filter_map(|line| line.strip_prefix(FAILURE))
            .collect();
        if failures.is_empty() {
            return;
        }
        let message = format!("{}: {}", self.root.display(), failures.join("; "));
        if std::thread::panicking() {
            eprintln!("{FAILURE}{message}");
        } else {
            panic!("{FAILURE}{message}");
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.close();
    }
}

/// Entry point of [`WATCHDOG_TEST`]; does nothing outside a sandbox.
pub fn watchdog() {
    let Some(root) = env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    match env::var(ROLE_ENV).as_deref() {
        Ok("launcher") => {
            let mut command = Command::new(env::current_exe().unwrap());
            command
                .args(["--exact", WATCHDOG_TEST, "--ignored", "--quiet"])
                .env(ROLE_ENV, "watchdog")
                .stderr(Stdio::null());
            detach(&mut command);
            // Exiting without waiting orphans the watchdog on purpose.
            #[allow(clippy::zombie_processes)]
            let _watchdog = command.spawn().unwrap();
        }
        Ok("watchdog") => {
            let _ = io::stdin().read_to_end(&mut Vec::new());
            let mut report = io::stdout().lock();
            for failure in teardown(&root) {
                let _ = writeln!(report, "{FAILURE}{failure}");
            }
        }
        _ => panic!("Unknown sandbox role"),
    }
}

/// Keeps terminal interrupts aimed at the test from reaching the watchdog.
fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

fn teardown(root: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    let cli = root.join(executable("private-ai-proxy"));
    if cli.is_file() {
        // The product's own stop path, whichever backend owns the endpoint.
        let mut stop = Command::new(cli);
        stop.args(["service", "stop", "--yes", "--json"])
            .env(agent_bridge::agents::HOME_OVERRIDE_ENV, root.join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Err(error) = run_bounded(stop, STOP_TIMEOUT) {
            failures.push(format!("service stop: {error}"));
        }
    }
    // Last resort for a stuck backend or a straggler started after the stop.
    let deadline = Instant::now() + KILL_TIMEOUT;
    loop {
        match processes_under(root) {
            Ok(pids) if pids.is_empty() => break,
            Ok(pids) if Instant::now() < deadline => {
                if let Err(error) = kill(&pids) {
                    failures.push(error);
                    break;
                }
            }
            Ok(pids) => {
                failures.push(format!("processes {pids:?} are still running"));
                break;
            }
            Err(error) => {
                failures.push(error);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::remove_dir_all(root) {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            // Windows keeps a file locked briefly after its process exits.
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                failures.push(format!("cannot remove sandbox: {error}"));
                break;
            }
        }
    }
    failures
}

fn run_bounded(mut command: Command, timeout: Duration) -> Result<(), String> {
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let deadline = Instant::now() + timeout;
    while child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none()
    {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timed out after {}s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn executable(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Live processes whose command starts inside `root`; every sandbox binary is
/// launched by absolute path, so this finds backends however they started.
#[cfg(unix)]
fn processes_under(root: &Path) -> Result<Vec<u32>, String> {
    let roots = sandbox_prefixes(root);
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "command="])
        .output()
        .map_err(|error| format!("cannot list processes: {error}"))?;
    if !output.status.success() {
        return Err(format!("cannot list processes: {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(' ')?;
            let command = command.trim_start();
            if roots.iter().any(|root| command.starts_with(root.as_str())) {
                pid.parse().ok()
            } else {
                None
            }
        })
        .collect())
}

#[cfg(unix)]
fn kill(pids: &[u32]) -> Result<(), String> {
    // A process may exit between listing and signalling; the next scan decides.
    Command::new("kill")
        .arg("-KILL")
        .args(pids.iter().map(u32::to_string))
        .stderr(Stdio::null())
        .status()
        .map(drop)
        .map_err(|error| format!("cannot kill {pids:?}: {error}"))
}

#[cfg(windows)]
fn processes_under(root: &Path) -> Result<Vec<u32>, String> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            // Match the unique sandbox name: TEMP may be an 8.3 short path.
            "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like ('*\\' + $env:PAP_TEST_SANDBOX_ROOT + '\\*') } | ForEach-Object { $_.ProcessId }",
        ])
        .env(ROOT_ENV, root.file_name().unwrap())
        .output()
        .map_err(|error| format!("cannot list processes: {error}"))?;
    if !output.status.success() {
        return Err(format!("cannot list processes: {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect())
}

#[cfg(windows)]
fn kill(pids: &[u32]) -> Result<(), String> {
    let mut command = Command::new("taskkill.exe");
    command.arg("/F");
    for pid in pids {
        command.args(["/PID", &pid.to_string()]);
    }
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(drop)
        .map_err(|error| format!("cannot kill {pids:?}: {error}"))
}

/// `root` as spawned by tests and as canonicalized by the product launcher,
/// each with a trailing separator so a sibling sandbox never matches.
#[cfg(unix)]
fn sandbox_prefixes(root: &Path) -> Vec<String> {
    let mut prefixes = vec![root.to_path_buf()];
    if let Ok(canonical) = fs::canonicalize(root) {
        prefixes.push(canonical);
    }
    prefixes
        .into_iter()
        .map(|root| format!("{}{}", root.display(), std::path::MAIN_SEPARATOR))
        .collect()
}
