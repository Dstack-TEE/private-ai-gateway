//! Real binaries and isolated user state; no UI, provider calls, or OS secrets.
use serde_json::Value;
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Backend {
    directory: tempfile::TempDir,
    child: Child,
}

impl Backend {
    fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let binary = |name: &str| {
            directory.path().join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            })
        };
        fs::copy(env!("CARGO_BIN_EXE_pag"), binary("pag")).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_pag-service"), binary("pag-service")).unwrap();
        // These must exist for bundle validation but no test starts inference.
        fs::copy(env!("CARGO_BIN_EXE_pag"), binary("aci")).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_pag"),
            binary("private-ai-gateway-helper"),
        )
        .unwrap();
        let home = directory.path().join("home");
        let data = home.join(".private-ai-gateway");
        desktop_gateway::tokens::create_private_dir(&data).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        desktop_gateway::tokens::write_private(
            &data.join("local-api.json"),
            &format!(r#"{{"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":{port}}}"#),
        )
        .unwrap();
        let child = Command::new(binary("pag-service"))
            .env(desktop_gateway::agents::HOME_OVERRIDE_ENV, &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut backend = Self { directory, child };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let output = backend.command(&["status", "--json"]).output().unwrap();
            if output.status.success() {
                let state: Value = serde_json::from_slice(&output.stdout).unwrap();
                if !state["backend"].is_null() {
                    break;
                }
            }
            assert!(
                backend.child.try_wait().unwrap().is_none(),
                "Backend exited during startup"
            );
            assert!(Instant::now() < deadline, "Backend readiness timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        backend
    }
    fn cli(&self) -> PathBuf {
        self.directory
            .path()
            .join(if cfg!(windows) { "pag.exe" } else { "pag" })
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(self.cli());
        command.args(args).env(
            desktop_gateway::agents::HOME_OVERRIDE_ENV,
            self.directory.path().join("home"),
        );
        command
    }
    fn run(&self, args: &[&str]) -> Value {
        let output = self.command(args).arg("--json").output().unwrap();
        assert_success(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.command(&["service", "stop", "--yes"]).output();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn two_cli_clients_share_state_and_disconnect_does_not_stop_service() {
    let backend = Backend::start();
    let first = backend.run(&["status"]);
    assert_eq!(first["gateway"]["status"], "stopped");
    let second = backend.run(&["service", "start"]);
    assert_eq!(first["backend"]["instanceId"], second["instanceId"]);
    backend.run(&["settings", "set", "appearance", "dark", "--yes"]);
    assert_eq!(
        backend.run(&["settings", "show"])["preferences"]["appearance"],
        "dark"
    );
    assert_eq!(backend.run(&["profiles", "list"]), serde_json::json!([]));
    let rejected = backend
        .command(&["usage", "clear", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("--yes"));
    assert_eq!(
        backend.run(&["usage", "list"])["items"],
        serde_json::json!([])
    );
    assert_eq!(
        backend.run(&["status"])["backend"]["instanceId"],
        first["backend"]["instanceId"]
    );
}

#[test]
fn shutdown_is_explicit_and_read_only_commands_do_not_restart_backend() {
    let backend = Backend::start();
    backend.run(&["service", "stop", "--yes"]);
    let stopped = backend.run(&["status"]);
    assert!(stopped["backend"].is_null());
    assert_eq!(stopped["status"], "not_running");
    // A fresh CLI starts a backend without needing or launching a desktop UI.
    let starter = backend
        .command(&["service", "start", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = backend.run(&["service", "start"]);
    let competing = starter.wait_with_output().unwrap();
    assert_success(&competing);
    let competing: Value = serde_json::from_slice(&competing.stdout).unwrap();
    assert_eq!(started["instanceId"], competing["instanceId"]);
    assert!(started["processId"].as_u64().is_some());
    backend.run(&["service", "stop", "--yes"]);
}

#[cfg(unix)]
#[test]
fn malformed_client_and_watch_disconnect_do_not_stop_backend() {
    use std::io::{BufReader, Write};
    let backend = Backend::start();
    let endpoint = backend.run(&["doctor"])["endpoint"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut stream = std::os::unix::net::UnixStream::connect(&endpoint).unwrap();
    stream
        .write_all(b"{\"version\":1,\"id\":1,\"command\":{\"method\":\"notACommand\"}}\n")
        .unwrap();
    drop(stream);
    {
        use desktop_runtime::protocol::{
            self, Command as RpcCommand, Hello, Outcome, Request, Response,
        };
        let mut subscribers = Vec::new();
        for index in 0..5 {
            let stream = std::os::unix::net::UnixStream::connect(&endpoint).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let _: Hello = protocol::read(&mut reader).unwrap();
            protocol::write(
                reader.get_mut(),
                &Request {
                    version: protocol::VERSION,
                    id: index,
                    command: RpcCommand::Watch,
                },
            )
            .unwrap();
            let response: Response = protocol::read(&mut reader).unwrap();
            if index < 4 {
                assert!(matches!(response.outcome, Outcome::Result(_)));
                subscribers.push(reader);
            } else {
                assert!(matches!(response.outcome, Outcome::Error(_)));
            }
        }
        assert!(backend.run(&["status"])["backend"]["processId"].is_number());
    }
    assert!(backend.run(&["status"])["backend"]["processId"]
        .as_u64()
        .is_some());
}
