//! Real binaries and isolated user state; no UI, provider calls, or OS secrets.
#![cfg(feature = "desktop-client")]
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

#[test]
fn argument_errors_are_machine_readable_in_json_mode() {
    for args in [
        vec!["--json", "--non-interactive", "unknown-command"],
        vec!["status", "--json", "--no-interactive", "--unknown-option"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_pap"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
    }
}

#[test]
fn command_discovery_is_detailed_and_machine_readable() {
    let settings = Command::new(env!("CARGO_BIN_EXE_pap"))
        .args(["settings", "set", "--help"])
        .output()
        .unwrap();
    assert_success(&settings);
    let settings = String::from_utf8(settings.stdout).unwrap();
    for key in [
        "autoCliRegistration",
        "connectOnLaunch",
        "allowNetworkAccess",
        "clientHost",
    ] {
        assert!(settings.contains(key), "missing settings key {key}");
    }

    let usage = Command::new(env!("CARGO_BIN_EXE_pap"))
        .args(["usage", "export", "--help"])
        .output()
        .unwrap();
    assert_success(&usage);
    let usage = String::from_utf8(usage.stdout).unwrap();
    assert!(usage.contains("Unix timestamp in seconds"));
    assert!(!usage.contains("--cursor"));
    assert!(!usage.contains("--limit"));

    let schema = Command::new(env!("CARGO_BIN_EXE_pap"))
        .arg("schema")
        .output()
        .unwrap();
    assert_success(&schema);
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert_eq!(schema["name"], "pap");
    let json_flag = schema["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|arg| arg["long"] == "json")
        .unwrap();
    assert_eq!(json_flag["takesValue"], false);
    assert_eq!(json_flag["numArgs"]["max"], 0);
    assert_eq!(json_flag["possibleValues"], serde_json::json!([]));
    assert!(schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|command| command["name"] == "profiles"));

    let completion = Command::new(env!("CARGO_BIN_EXE_pap"))
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert_success(&completion);
    assert!(String::from_utf8(completion.stdout)
        .unwrap()
        .contains("pap"));

    let conflict = Command::new(env!("CARGO_BIN_EXE_pap"))
        .args([
            "agents",
            "connect",
            "codex",
            "--dry-run",
            "--revision",
            "preview-revision",
        ])
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(2));
}

#[test]
fn adding_a_profile_requires_consent_before_startup_or_credential_input() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pap"))
        .args([
            "--json",
            "profiles",
            "add",
            "--id",
            "test",
            "--name",
            "Test",
            "--url",
            "https://example.com",
            "--key-stdin",
        ])
        .env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--yes"));
    assert!(fs::read_dir(home.path()).unwrap().next().is_none());
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
        fs::copy(env!("CARGO_BIN_EXE_pap"), binary("pap")).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_pap-service"), binary("pap-service")).unwrap();
        // The helper must exist for bundle validation; no test starts inference.
        fs::copy(env!("CARGO_BIN_EXE_pap"), binary("private-ai-proxy-helper")).unwrap();
        let home = directory.path().join("home");
        let data = home.join(".private-ai-proxy");
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
        let child = Command::new(binary("pap-service"))
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
            .join(if cfg!(windows) { "pap.exe" } else { "pap" })
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
fn reset_settings_preserves_user_data_and_requires_explicit_consent() {
    let backend = Backend::start();
    let backup = backend.directory.path().join("profiles-reset.json");
    fs::write(&backup, r#"{"version":1,"profiles":[{"name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com"}]}"#).unwrap();
    backend.run(&["profiles", "import", backup.to_str().unwrap(), "--yes"]);
    backend.run(&["settings", "set", "appearance", "dark", "--yes"]);
    backend.run(&["settings", "set", "connectOnLaunch", "true", "--yes"]);
    backend.run(&["agents", "connect", "codex", "--yes"]);
    let key = backend.run(&["token", "show", "--yes"]);
    let profiles = backend.run(&["profiles", "list"]);
    let before = backend.run(&["settings", "show"]);
    let refused = backend
        .command(&["settings", "reset", "--json"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(backend.run(&["settings", "show"]), before);
    let occupied = TcpListener::bind("127.0.0.1:4180").unwrap();
    let failed = backend
        .command(&["settings", "reset", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(backend.run(&["settings", "show"]), before);
    assert_eq!(backend.run(&["token", "show", "--yes"]), key);
    drop(occupied);
    let state = backend.run(&["settings", "reset", "--yes"]);
    assert_eq!(state["status"], "stopped");
    assert_eq!(state["sessionActive"], false);
    assert_eq!(state["localApi"]["listenAddress"], "127.0.0.1");
    assert_eq!(state["localApi"]["port"], 4180);
    assert_eq!(state["config"]["requireProductionOs"], true);
    assert_eq!(backend.run(&["token", "show", "--yes"]), key);
    assert_eq!(backend.run(&["profiles", "list"]), profiles);
    assert!(backend
        .run(&["agents", "list"])
        .as_array()
        .unwrap()
        .iter()
        .all(|agent| agent["recorded"] == false));
    let settings = backend.run(&["settings", "show"]);
    assert_eq!(settings["preferences"]["appearance"], "system");
    assert_eq!(settings["preferences"]["connectOnLaunch"], false);
    assert_eq!(settings["preferences"]["notifications"]["enabled"], true);
}

#[test]
fn two_cli_clients_share_state_and_disconnect_does_not_stop_service() {
    let backend = Backend::start();
    #[cfg(unix)]
    assert!(backend
        .directory
        .path()
        .join("home/.private-ai-proxy/helpers/private-ai-proxy-helper")
        .is_file());
    let first = backend.run(&["status"]);
    let rejected = backend
        .command(&[
            "--json",
            "--yes",
            "agents",
            "disconnect",
            "codex",
            "--revision",
            "stale-revision",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1));
    let rejected: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("revision_conflict"));
    assert_eq!(first["gateway"]["status"], "stopped");
    let second = backend.run(&["service", "start"]);
    assert_eq!(first["backend"]["instanceId"], second["instanceId"]);
    backend.run(&["settings", "set", "appearance", "dark", "--yes"]);
    assert_eq!(
        backend.run(&["settings", "show"])["preferences"]["appearance"],
        "dark"
    );
    backend.run(&[
        "settings",
        "set",
        "notifications",
        r#"{"enabled":false}"#,
        "--yes",
    ]);
    let settings = backend.run(&["settings", "show"]);
    assert_eq!(settings["preferences"]["notifications"]["enabled"], false);
    assert_eq!(settings["preferences"]["appearance"], "dark");
    backend.run(&["settings", "set", "autoCliRegistration", "false", "--yes"]);
    assert_eq!(
        backend.run(&["settings", "show"])["preferences"]["autoCliRegistration"],
        false
    );
    backend.run(&["token", "rotate", "--yes"]);
    let state = backend.run(&["status"]);
    assert_eq!(state["gateway"]["clientKeyRevision"], 1);
    assert_eq!(state["gateway"]["clientKeyAvailable"], true);
    assert_eq!(backend.run(&["profiles", "list"]), serde_json::json!([]));
    let backup = backend.directory.path().join("profiles.json");
    fs::write(&backup, r#"{"version":1,"profiles":[{"name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com"}]}"#).unwrap();
    assert_eq!(
        backend.run(&["profiles", "import", backup.to_str().unwrap(), "--yes"])["imported"],
        1
    );
    let profiles = backend.run(&["profiles", "list"]);
    let profile_id = profiles[0]["id"].as_str().unwrap();
    assert_eq!(
        backend.run(&["profiles", "show", profile_id])["name"],
        "Work"
    );
    let exported = backend.directory.path().join("exported.json");
    backend.run(&["profiles", "export", "--output", exported.to_str().unwrap()]);
    let text = fs::read_to_string(exported).unwrap();
    assert!(!text.contains("credential"));
    assert!(!text.contains("verified"));
    let diagnostics = backend.directory.path().join("diagnostics.json");
    backend.run(&["diagnostics", "--output", diagnostics.to_str().unwrap()]);
    let report: Value = serde_json::from_str(&fs::read_to_string(diagnostics).unwrap()).unwrap();
    assert_eq!(report["profiles"]["count"], 1);
    assert_eq!(report["gateway"]["status"], "stopped");
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
    let home = backend.directory.path().join("diagnostic-home");
    let command_dir = home.join(".local/bin");
    fs::create_dir_all(&command_dir).unwrap();
    fs::write(command_dir.join("pap"), "unrelated command").unwrap();
    let diagnostic = backend
        .command(&["doctor", "--json"])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_eq!(diagnostic.status.code(), Some(1));
    let diagnostic: Value = serde_json::from_slice(&diagnostic.stdout).unwrap();
    assert_eq!(diagnostic["backendRunning"], true);
    assert!(diagnostic["errors"]["cli"].is_string());
    assert!(diagnostic["endpoint"].is_string());
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
                let Outcome::Result(snapshot) = response.outcome else {
                    panic!("Subscription failed");
                };
                assert!(snapshot["proxyUrl"]
                    .as_str()
                    .is_some_and(|url| url.starts_with("http://127.0.0.1:")));
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
