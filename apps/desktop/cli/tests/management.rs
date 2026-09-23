//! Real binaries and isolated user state; no UI, provider calls, or OS secrets.
mod support;

use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
use support::{executable, Sandbox};

/// Readiness is bounded only to fail a hung startup; loaded hosts are slow.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// A backend in its own install tree and home. Dropping it stops every process
/// started from that tree and removes it; see `support`.
struct Backend {
    directory: Sandbox,
    child: Child,
}

#[test]
#[ignore = "subprocess fixture that tears down a test sandbox"]
fn sandbox_teardown_watchdog() {
    support::watchdog();
}

#[test]
fn argument_errors_are_machine_readable_in_json_mode() {
    for args in [
        vec!["--json", "--non-interactive", "unknown-command"],
        vec!["status", "--json", "--no-interactive", "--unknown-option"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
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
    let settings = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
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
        "webUi",
        "webUiPort",
        "webUiListenAddress",
        "webUiAllowNetworkAccess",
        "webUiClientHost",
    ] {
        assert!(settings.contains(key), "missing settings key {key}");
    }

    let usage = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
        .args(["usage", "export", "--help"])
        .output()
        .unwrap();
    assert_success(&usage);
    let usage = String::from_utf8(usage.stdout).unwrap();
    assert!(usage.contains("Unix timestamp in seconds"));
    assert!(!usage.contains("--cursor"));
    assert!(!usage.contains("--limit"));

    let schema = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
        .arg("schema")
        .output()
        .unwrap();
    assert_success(&schema);
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert_eq!(schema["name"], "private-ai-proxy");
    for name in ["verify", "audit", "sessions", "send", "serve"] {
        assert!(schema["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["name"] == name));
        let output = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
            .args([name, "--help"])
            .output()
            .unwrap();
        assert_success(&output);
    }
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

    let completion = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert_success(&completion);
    assert!(String::from_utf8(completion.stdout)
        .unwrap()
        .contains("private-ai-proxy"));

    let conflict = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
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

/// Scripts such as `scripts/live_e2e` run `aci audit --json` and read its
/// streams; the legacy alias must stay byte-for-byte the canonical command.
#[cfg(unix)]
#[test]
fn legacy_aci_alias_output_matches_the_canonical_command() {
    let directory = tempfile::tempdir().unwrap();
    let alias = directory.path().join("aci");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_private-ai-proxy"), &alias).unwrap();
    let missing = directory.path().join("missing-report.json");
    for args in [
        vec!["--version"],
        vec!["audit", "--report", missing.to_str().unwrap(), "--json"],
    ] {
        let run = |program: &Path| {
            Command::new(program)
                .args(&args)
                .env("PRIVATE_AI_PROXY_HOME", directory.path())
                .stdin(Stdio::null())
                .output()
                .unwrap()
        };
        let canonical = run(Path::new(env!("CARGO_BIN_EXE_private-ai-proxy")));
        let legacy = run(&alias);
        assert_eq!(legacy.status.code(), canonical.status.code(), "{args:?}");
        assert_eq!(legacy.stdout, canonical.stdout, "{args:?}");
        assert_eq!(legacy.stderr, canonical.stderr, "{args:?}");
    }
}

#[test]
fn adding_a_profile_requires_consent_before_startup_or_credential_input() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
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
        .env(agent_bridge::agents::HOME_OVERRIDE_ENV, home.path())
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
        let directory = Sandbox::new();
        let binary = |name: &str| directory.path().join(executable(name));
        install(
            env!("CARGO_BIN_EXE_private-ai-proxy"),
            &binary("private-ai-proxy"),
        );
        install(
            env!("CARGO_BIN_EXE_private-ai-proxy-service"),
            &binary("private-ai-proxy-service"),
        );
        // No test requests agent credentials. Keep this unused helper tiny:
        // startup durably stages it, so copying a debug CLI would fsync hundreds
        // of megabytes per backend before its management endpoint becomes ready.
        let helper = binary("private-ai-proxy-helper");
        fs::write(&helper, b"#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home = directory.path().join("home");
        let data = home.join(".private-ai-proxy");
        agent_bridge::tokens::create_private_dir(&data).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        agent_bridge::tokens::write_private(
            &data.join("local-api.json"),
            &format!(r#"{{"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":{port}}}"#),
        )
        .unwrap();
        let child = Command::new(binary("private-ai-proxy-service"))
            .env(agent_bridge::agents::HOME_OVERRIDE_ENV, &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(directory.path().join("backend.log")).unwrap())
            .spawn()
            .unwrap();
        let mut backend = Self { directory, child };
        let deadline = Instant::now() + READY_TIMEOUT;
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
                "Backend exited during startup: {}",
                backend.startup_diagnostics(&output)
            );
            assert!(
                Instant::now() < deadline,
                "Backend readiness timed out: {}",
                backend.startup_diagnostics(&output)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        backend
    }
    fn startup_diagnostics(&self, output: &Output) -> String {
        format!(
            "status: {}; stdout: {}; stderr: {}; backend: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(self.directory.path().join("backend.log")).unwrap()
        )
    }
    fn cli(&self) -> PathBuf {
        self.directory.path().join(executable("private-ai-proxy"))
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(self.cli());
        command.args(args).env(
            agent_bridge::agents::HOME_OVERRIDE_ENV,
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
        self.directory.close();
        // Teardown already killed it; this only reaps the process.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Places a build output in a sandbox. A hard link avoids copying hundreds of
/// megabytes of debug binaries per backend; the launcher still sees a sibling.
fn install(source: &str, destination: &Path) {
    if fs::hard_link(source, destination).is_err() {
        fs::copy(source, destination).unwrap();
    }
}
#[test]
fn web_ui_is_opt_in_and_login_links_work_once() {
    let backend = Backend::start();
    assert_eq!(
        backend.run(&["status"])["gateway"]["webUi"]["enabled"],
        false
    );
    let refused = backend
        .command(&["app", "open", "--web", "--non-interactive"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("pap settings set webUi true"));

    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port().to_string();
    backend.run(&["settings", "set", "webUiPort", &port, "--yes"]);
    // --yes enables the web UI through the same setting; the bind conflict stays in state.
    let failed = backend.web(&["app", "open", "--web", "--yes", "--json"]);
    assert!(!failed.status.success());
    let status = &backend.run(&["settings", "show"])["webUi"];
    assert_eq!(status["enabled"], true);
    let error = status["error"].as_str().unwrap();
    if error.contains("assets are not built") {
        // Development builds without `npm run build:web` report this instead of serving.
        return;
    }
    assert_eq!(error, format!("Port {port} is already in use on 127.0.0.1"));
    drop(occupied);

    let state = backend.run(&["settings", "set", "webUi", "true", "--yes"]);
    assert_eq!(state["webUi"]["url"], format!("http://127.0.0.1:{port}"));
    let login = backend.web(&["app", "open", "--web", "--json"]);
    assert_success(&login);
    let login: Value = serde_json::from_slice(&login.stdout).unwrap();
    let url = login["url"].as_str().unwrap();
    let code = url
        .strip_prefix(&format!("http://127.0.0.1:{port}/#code="))
        .unwrap();
    let body = serde_json::json!({ "code": code }).to_string();
    let authority = format!("127.0.0.1:{port}");
    let (status, session) = http(&authority, "POST", "/api/session", None, &body);
    assert_eq!(status, 200);
    let token = session["token"].as_str().unwrap().to_string();
    assert_eq!(http(&authority, "POST", "/api/session", None, &body).0, 401);
    assert_eq!(
        http(&authority, "GET", "/api/bootstrap", Some(&token), "").0,
        200
    );
    assert_eq!(http(&authority, "GET", "/api/bootstrap", None, "").0, 401);

    backend.run(&["settings", "set", "webUi", "false", "--yes"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
        assert!(Instant::now() < deadline, "the web UI listener stayed open");
        std::thread::sleep(Duration::from_millis(50));
    }
}
#[test]
fn app_open_fails_fast_while_an_update_holds_the_startup_gate() {
    let backend = Backend::start();
    let data = backend.directory.path().join("home/.private-ai-proxy");
    let gate = agent_bridge::lock::startup(&data).unwrap().unwrap();
    let started = Instant::now();
    let blocked = backend
        .command(&["app", "open", "--web", "--yes", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    // Backend startup would wait out a brief gate; app open reports it at once.
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr)
        .contains("Backend startup or an update is already in progress"));
    drop(gate);
    assert_eq!(
        backend.run(&["status"])["gateway"]["webUi"]["enabled"],
        false
    );
}

/// Every 127.0.0.0/8 address is loopback on Linux, so moving the listener needs no confirmation.
#[cfg(target_os = "linux")]
#[test]
fn web_ui_listener_fails_closed_and_rebinds_with_fresh_sessions() {
    let backend = Backend::start();
    let port = {
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        free.local_addr().unwrap().port().to_string()
    };
    backend.run(&["settings", "set", "webUiPort", &port, "--yes"]);
    let set = |key: &str, value: &str| {
        backend
            .command(&["settings", "set", key, value, "--yes", "--json"])
            .output()
            .unwrap()
    };
    let refused = set("webUiListenAddress", "0.0.0.0");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("explicit confirmation"));
    assert_success(&set("webUiAllowNetworkAccess", "true"));
    let refused = set("webUiListenAddress", "0.0.0.0");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Client host is required"));
    assert_success(&set("webUiAllowNetworkAccess", "false"));

    let state = backend.run(&["settings", "set", "webUi", "true", "--yes"]);
    if state["webUi"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("assets are not built"))
    {
        return;
    }
    let first = format!("127.0.0.1:{port}");
    let token = web_session(&backend, &first);
    assert_eq!(
        http(&first, "GET", "/api/bootstrap", Some(&token), "").0,
        200
    );

    let state = backend.run(&[
        "settings",
        "set",
        "webUiListenAddress",
        "127.0.0.2",
        "--yes",
    ]);
    let second = format!("127.0.0.2:{port}");
    assert_eq!(state["webUi"]["url"], format!("http://{second}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(&first).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the old web UI listener stayed open"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Moving the listener revokes every session; a new link signs in on the new address.
    assert_eq!(
        http(&second, "GET", "/api/bootstrap", Some(&token), "").0,
        401
    );
    let token = web_session(&backend, &second);
    assert_eq!(
        http(&second, "GET", "/api/bootstrap", Some(&token), "").0,
        200
    );
}

/// Prints a login link for `authority` and exchanges its code for a session token.
fn web_session(backend: &Backend, authority: &str) -> String {
    let login = backend.web(&["app", "open", "--web", "--json"]);
    assert_success(&login);
    let login: Value = serde_json::from_slice(&login.stdout).unwrap();
    let code = login["url"]
        .as_str()
        .unwrap()
        .strip_prefix(&format!("http://{authority}/#code="))
        .unwrap()
        .to_string();
    let body = serde_json::json!({ "code": code }).to_string();
    let (status, session) = http(authority, "POST", "/api/session", None, &body);
    assert_eq!(status, 200);
    session["token"].as_str().unwrap().to_string()
}

impl Backend {
    /// Runs as a remote shell so no browser is launched.
    fn web(&self, args: &[&str]) -> Output {
        self.command(args)
            .env("SSH_CONNECTION", "test")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}
/// Sends a same-origin request to `authority` (`IP:PORT`).
fn http(
    authority: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let mut stream = TcpStream::connect(authority).unwrap();
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nOrigin: http://{authority}\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response[9..12].parse().unwrap();
    let body = response.split_once("\r\n\r\n").unwrap().1;
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
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
    // Declared first so it is released only after the backend has stopped.
    let _default_port = default_port_lock();
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
    let occupied = TcpListener::bind("127.0.0.1:4180")
        .expect("127.0.0.1:4180 is in use outside the tests; stop the local Private AI Proxy");
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

/// Reset moves the Local API to its fixed default port, which every test
/// process on this host shares; hold this while a test backend may bind it.
fn default_port_lock() -> fs::File {
    let lock =
        fs::File::create(std::env::temp_dir().join("private-ai-proxy-test-4180.lock")).unwrap();
    lock.lock().unwrap();
    lock
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
    fs::write(command_dir.join("private-ai-proxy"), "unrelated command").unwrap();
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
