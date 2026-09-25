//! Real binaries and isolated user state; no UI, provider calls, or OS secrets.
mod support;

use serde_json::{json, Value};
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
fn command_discovery_is_detailed() {
    let settings = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
        .args(["settings", "set", "--help"])
        .output()
        .unwrap();
    assert_success(&settings);
    let settings = String::from_utf8(settings.stdout).unwrap();
    for key in [
        "auto-cli-registration",
        "connect-on-launch",
        "notifications.local-api",
        "local-api.allow-network-access",
        "local-api.client-host",
        "web-ui.enabled",
        "web-ui.port",
        "web-ui.listen-address",
        "web-ui.allow-network-access",
        "web-ui.client-host",
        "web-ui.password",
    ] {
        assert!(settings.contains(key), "missing settings key {key}");
    }
    // The deprecated flat names still parse but are never offered.
    assert!(!settings.contains("webUi"), "{settings}");

    let usage = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
        .args(["usage", "export", "--help"])
        .output()
        .unwrap();
    assert_success(&usage);
    let usage = String::from_utf8(usage.stdout).unwrap();
    assert!(usage.contains("Unix timestamp in seconds"));
    assert!(!usage.contains("--cursor"));
    assert!(!usage.contains("--limit"));

    let help = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
        .arg("--help")
        .output()
        .unwrap();
    assert_success(&help);
    let help = String::from_utf8(help.stdout).unwrap();
    for name in ["verify", "audit", "sessions", "send", "serve", "profiles"] {
        assert!(
            help.contains(&format!("\n  {name} ")),
            "missing command {name}"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
            .args([name, "--help"])
            .output()
            .unwrap();
        assert_success(&output);
    }
    assert!(help.contains("--json"));

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

#[test]
fn send_reads_the_api_key_from_stdin_not_arguments() {
    let send = |args: &[&str], input: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
            .args(["send", "https://127.0.0.1:9"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        write!(child.stdin.take().unwrap(), "{input}").unwrap();
        child.wait_with_output().unwrap()
    };
    // The key is read before any network access.
    let empty = send(&["--api-key-stdin"], "  \n");
    assert_eq!(empty.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&empty.stderr).contains("Enter an API key"));
    let both = send(&["--api-key-stdin", "--api-key", "sk-test"], "");
    assert_eq!(both.status.code(), Some(2));
    let help = Command::new(env!("CARGO_BIN_EXE_private-ai-proxy"))
        .args(["send", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("--api-key-stdin") && !help.contains("--api-key <"));
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
        .env(desktop_core::paths::HOME_OVERRIDE_ENV, home.path())
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
        let settings = data.join("Config");
        desktop_core::private_fs::create_private_dir(&settings).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        desktop_core::private_fs::write_private(
            &settings.join("config.toml"),
            &format!("# Kept by every write.\n\n[local-api]\nport = {port}\n"),
        )
        .unwrap();
        let child = Command::new(binary("private-ai-proxy-service"))
            .env(desktop_core::paths::HOME_OVERRIDE_ENV, &home)
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
            desktop_core::paths::HOME_OVERRIDE_ENV,
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
const WEB_PASSWORD: &str = "correct horse battery staple";

#[test]
fn web_ui_requires_a_password_that_never_leaves_the_service() {
    let backend = Backend::start();
    let status = &backend.run(&["status"])["gateway"]["webUi"];
    assert_eq!(status["enabled"], false);
    assert_eq!(status["passwordSet"], false);
    let refused = backend
        .command(&["app", "open", "--web", "--non-interactive"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("pap settings set web-ui.password"));
    let refused = backend
        .command(&[
            "settings",
            "set",
            "web-ui.enabled",
            "true",
            "--yes",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("pap settings set web-ui.password"));
    // Passwords never travel in arguments.
    let refused = backend
        .command(&[
            "settings",
            "set",
            "web-ui.password",
            WEB_PASSWORD,
            "--yes",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--value-stdin"));
    let short = backend.set_web_ui_password("too short");
    assert!(!short.status.success());
    assert!(String::from_utf8_lossy(&short.stderr).contains("at least 12 characters"));
    assert_success(&backend.set_web_ui_password(WEB_PASSWORD));

    let diagnostics = backend.directory.path().join("web-diagnostics.json");
    backend.run(&["diagnostics", "--output", diagnostics.to_str().unwrap()]);
    let show = backend.run(&["settings", "show"]);
    assert_eq!(show["webUi"]["passwordSet"], true);
    let data = backend
        .directory
        .path()
        .join("home/.private-ai-proxy/Config");
    let saved = fs::read_to_string(data.join("credentials.toml")).unwrap();
    assert!(saved.contains("[web-ui]\npassword-hash = \"$argon2id$"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(data.join("credentials.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
    let config = fs::read_to_string(data.join("config.toml")).unwrap();
    assert!(config.starts_with("# Kept by every write.\n"), "{config}");
    for (name, text) in [
        ("settings show", show.to_string()),
        ("status", backend.run(&["status"]).to_string()),
        ("diagnostics", fs::read_to_string(&diagnostics).unwrap()),
        ("credentials.toml", saved),
        ("config.toml", config),
        (
            "backend log",
            fs::read_to_string(backend.directory.path().join("backend.log")).unwrap(),
        ),
    ] {
        assert!(!text.contains(WEB_PASSWORD), "{name} contains the password");
        if name != "credentials.toml" {
            assert!(
                !text.contains("$argon2"),
                "{name} contains the password hash"
            );
        }
    }

    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port().to_string();
    backend.run(&["settings", "set", "web-ui.port", &port, "--yes"]);
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

    let state = backend.run(&["settings", "set", "web-ui.enabled", "true", "--yes"]);
    let url = format!("http://127.0.0.1:{port}");
    assert_eq!(state["webUi"]["url"], url);
    let opened = backend.web(&["app", "open", "--web", "--json"]);
    assert_success(&opened);
    let opened: Value = serde_json::from_slice(&opened.stdout).unwrap();
    // The address carries no secret.
    assert_eq!(opened["url"], url);
    let authority = format!("127.0.0.1:{port}");
    let wrong = json!({ "password": "wrong horse battery staple" }).to_string();
    assert_eq!(
        http(&authority, "POST", "/api/session", None, &wrong).0,
        401
    );
    let token = web_session(&authority, WEB_PASSWORD);
    let other = web_session(&authority, WEB_PASSWORD);
    assert_eq!(
        http(&authority, "GET", "/api/bootstrap", Some(&token), "").0,
        200
    );
    assert_eq!(http(&authority, "GET", "/api/bootstrap", None, "").0, 401);

    // A new password ends every session; only the new one signs in.
    let next = "another long passphrase";
    assert_success(&backend.set_web_ui_password(next));
    for token in [&token, &other] {
        assert_eq!(
            http(&authority, "GET", "/api/bootstrap", Some(token), "").0,
            401
        );
    }
    let old = json!({ "password": WEB_PASSWORD }).to_string();
    assert_eq!(http(&authority, "POST", "/api/session", None, &old).0, 401);
    web_session(&authority, next);

    // The password stays while the web UI is on; turning it off ends sessions.
    let clear = |backend: &Backend| {
        backend
            .command(&["settings", "set", "web-ui.password", "", "--yes", "--json"])
            .output()
            .unwrap()
    };
    assert!(!clear(&backend).status.success());
    backend.run(&["settings", "set", "web-ui.enabled", "false", "--yes"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(&authority).is_ok() {
        assert!(Instant::now() < deadline, "the web UI listener stayed open");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_success(&clear(&backend));
    assert_eq!(
        backend.run(&["settings", "show"])["webUi"]["passwordSet"],
        false
    );
}
#[test]
fn service_log_keeps_diagnostics_but_never_secrets() {
    const API_KEY: &str = "sk-log-test-0123456789abcdef";
    let backend = Backend::start();
    let mut add = backend
        .command(&[
            "profiles",
            "add",
            "--id",
            "local",
            "--name",
            "Local",
            "--url",
            "https://127.0.0.1:9",
            "--key-stdin",
            "--yes",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write!(add.stdin.take().unwrap(), "{API_KEY}").unwrap();
    // Adding verifies the profile with the key; the unreachable service fails
    // that, and the failure is logged.
    assert!(!add.wait_with_output().unwrap().status.success());
    assert_success(&backend.set_web_ui_password(WEB_PASSWORD));
    backend.run(&["service", "stop", "--yes"]);

    let logs = backend.directory.path().join("home/.private-ai-proxy/logs");
    let mut log = String::new();
    for entry in fs::read_dir(&logs).unwrap() {
        log.push_str(&fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    assert!(log.contains("Private AI Proxy backend"), "{log}");
    assert!(log.contains("Protection error"), "{log}");
    for secret in [API_KEY, WEB_PASSWORD, "$argon2"] {
        assert!(!log.contains(secret), "the service log contains {secret}");
    }
}

#[test]
fn app_open_fails_fast_while_an_update_holds_the_startup_gate() {
    let backend = Backend::start();
    let data = backend.directory.path().join("home/.private-ai-proxy");
    let gate = desktop_core::lock::startup(&data).unwrap().unwrap();
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
    backend.run(&["settings", "set", "web-ui.port", &port, "--yes"]);
    let set = |key: &str, value: &str| {
        backend
            .command(&["settings", "set", key, value, "--yes", "--json"])
            .output()
            .unwrap()
    };
    let refused = set("web-ui.listen-address", "0.0.0.0");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("explicit confirmation"));
    assert_success(&set("web-ui.allow-network-access", "true"));
    let refused = set("web-ui.listen-address", "0.0.0.0");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Client host is required"));
    assert_success(&set("web-ui.allow-network-access", "false"));

    assert_success(&backend.set_web_ui_password(WEB_PASSWORD));
    let state = backend.run(&["settings", "set", "web-ui.enabled", "true", "--yes"]);
    if state["webUi"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("assets are not built"))
    {
        return;
    }
    let first = format!("127.0.0.1:{port}");
    let token = web_session(&first, WEB_PASSWORD);
    assert_eq!(
        http(&first, "GET", "/api/bootstrap", Some(&token), "").0,
        200
    );

    let state = backend.run(&[
        "settings",
        "set",
        "web-ui.listen-address",
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
    // Moving the listener revokes every session; the password signs in on the new address.
    assert_eq!(
        http(&second, "GET", "/api/bootstrap", Some(&token), "").0,
        401
    );
    let token = web_session(&second, WEB_PASSWORD);
    assert_eq!(
        http(&second, "GET", "/api/bootstrap", Some(&token), "").0,
        200
    );
}

/// Saving the web UI settings restarts its listener, which must not wait for
/// the browser request that asked for it.
#[test]
fn a_browser_saves_the_web_ui_settings_without_waiting_on_itself() {
    let backend = Backend::start();
    let port = {
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        free.local_addr().unwrap().port()
    };
    backend.run(&["settings", "set", "web-ui.port", &port.to_string(), "--yes"]);
    assert_success(&backend.set_web_ui_password(WEB_PASSWORD));
    let state = backend.run(&["settings", "set", "web-ui.enabled", "true", "--yes"]);
    if state["webUi"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("assets are not built"))
    {
        return;
    }
    let authority = format!("127.0.0.1:{port}");
    let token = web_session(&authority, WEB_PASSWORD);
    let config = json!({
        "config": { "enabled": true, "listenAddress": "127.0.0.1", "port": port }
    });
    let started = Instant::now();
    let (status, body) = http(
        &authority,
        "POST",
        "/api/rpc/save_web_ui",
        Some(&token),
        &config.to_string(),
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "saving took {:?}",
        started.elapsed()
    );
    assert_eq!(
        body["result"]["webUi"]["url"],
        format!("http://{authority}")
    );
}

/// Signs in to the web UI at `authority` and returns its session cookie.
fn web_session(authority: &str, password: &str) -> String {
    let body = json!({ "password": password }).to_string();
    let (status, headers, _) = exchange(authority, "POST", "/api/session", None, &body);
    assert_eq!(status, 204);
    let cookie = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(": ")?;
            name.eq_ignore_ascii_case("set-cookie").then_some(value)
        })
        .expect("sign-in sets the session cookie");
    assert!(cookie.contains("; HttpOnly; SameSite=Strict; Path=/"));
    cookie.split_once(';').unwrap().0.to_string()
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

    /// Sets the web UI password the way scripts do, through stdin.
    fn set_web_ui_password(&self, password: &str) -> Output {
        let mut child = self
            .command(&[
                "settings",
                "set",
                "web-ui.password",
                "--value-stdin",
                "--yes",
                "--json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.take().unwrap(), "{password}").unwrap();
        child.wait_with_output().unwrap()
    }
}
/// Sends a same-origin request to `authority` (`IP:PORT`).
fn http(
    authority: &str,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let (status, _, body) = exchange(authority, method, path, cookie, body);
    (status, body)
}

/// Like [`http`], also returning the response headers.
fn exchange(
    authority: &str,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: &str,
) -> (u16, String, Value) {
    let mut stream = TcpStream::connect(authority).unwrap();
    let cookie = cookie
        .map(|cookie| format!("Cookie: {cookie}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nOrigin: http://{authority}\r\n{cookie}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response[9..12].parse().unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    (
        status,
        headers.to_string(),
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
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
    backend.run(&["settings", "set", "connect-on-launch", "true", "--yes"]);
    assert_success(&backend.set_web_ui_password(WEB_PASSWORD));
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
    assert_eq!(settings["settings"]["appearance"], "system");
    assert_eq!(settings["settings"]["connect-on-launch"], false);
    assert_eq!(settings["settings"]["notifications"]["enabled"], true);
    assert_eq!(settings["webUi"]["passwordSet"], false);
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
    // The backend's code, not `command_failed`; the message is text only.
    assert_eq!(rejected["error"]["code"], "revision_conflict");
    assert!(!rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("revision_conflict"));
    assert_eq!(first["gateway"]["status"], "stopped");
    let second = backend.run(&["service", "start"]);
    assert_eq!(first["backend"]["instanceId"], second["instanceId"]);
    backend.run(&["settings", "set", "appearance", "dark", "--yes"]);
    assert_eq!(
        backend.run(&["settings", "show"])["settings"]["appearance"],
        "dark"
    );
    backend.run(&["settings", "set", "notifications.enabled", "false", "--yes"]);
    let settings = backend.run(&["settings", "show"]);
    assert_eq!(settings["settings"]["notifications"]["enabled"], false);
    assert_eq!(settings["settings"]["notifications"]["local-api"], true);
    assert_eq!(settings["settings"]["appearance"], "dark");
    // A deprecated flat name still works and says what replaces it.
    let deprecated = backend
        .command(&[
            "settings",
            "set",
            "autoCliRegistration",
            "false",
            "--yes",
            "--json",
        ])
        .output()
        .unwrap();
    assert_success(&deprecated);
    assert!(String::from_utf8_lossy(&deprecated.stderr).contains(
        "`autoCliRegistration` is deprecated and will be removed in 0.3; use `auto-cli-registration`"
    ));
    assert_eq!(
        backend.run(&["settings", "show"])["settings"]["auto-cli-registration"],
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
fn settings_files_are_edited_in_place_and_hand_edits_apply_live() {
    let backend = Backend::start();
    let data = backend
        .directory
        .path()
        .join("home/.private-ai-proxy/Config");
    let config = data.join("config.toml");
    let show = backend.run(&["settings", "show"]);
    assert_eq!(show["files"]["config"], config.to_str().unwrap());
    assert!(show["files"]["error"].is_null());

    backend.run(&["settings", "set", "appearance", "dark", "--yes"]);
    let text = fs::read_to_string(&config).unwrap();
    assert!(
        text.starts_with("# Kept by every write.\n\nappearance = \"dark\"\n"),
        "{text}"
    );
    assert!(text.contains("appearance = \"dark\""), "{text}");

    let wait = |check: &dyn Fn(&Value) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let show = backend.run(&["settings", "show"]);
            if check(&show) {
                return show;
            }
            assert!(Instant::now() < deadline, "not applied: {show}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let edited = |line: &str| {
        text.replacen(
            "appearance = \"dark\"\n",
            &format!("appearance = \"dark\"\n{line}\n"),
            1,
        )
    };
    fs::write(&config, edited("connect-on-launch = true")).unwrap();
    wait(&|show| show["settings"]["connect-on-launch"] == true);

    // An unknown key warns but never rejects the file.
    fs::write(
        &config,
        edited("connect-on-launch = true\nfuture-option = 1"),
    )
    .unwrap();
    let show = wait(&|show| {
        show["files"]["warnings"]
            .as_array()
            .is_some_and(|warnings| !warnings.is_empty())
    });
    assert!(show["files"]["error"].is_null(), "{show}");
    assert_eq!(show["settings"]["connect-on-launch"], true);
    assert!(show["files"]["warnings"][0]
        .as_str()
        .unwrap()
        .contains("future-option: unknown key, ignored"));
    let doctor = backend.command(&["doctor", "--json"]).output().unwrap();
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(
        doctor["warnings"]["settingsFiles"],
        show["files"]["warnings"]
    );
    assert!(doctor["errors"]["settings"].is_null(), "{doctor}");

    // A broken edit keeps the last good settings and names the position.
    fs::write(&config, edited("connect-on-launch = \"yes\"")).unwrap();
    let show = wait(&|show| show["files"]["error"].is_string());
    let error = show["files"]["error"].as_str().unwrap();
    assert!(error.starts_with("config.toml:"), "{error}");
    assert_eq!(show["settings"]["connect-on-launch"], true);
    let doctor = backend.command(&["doctor", "--json"]).output().unwrap();
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["errors"]["settings"], error);
    let refused = backend
        .command(&["settings", "set", "appearance", "light", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("config.toml:"));

    fs::write(&config, &text).unwrap();
    let show = wait(&|show| show["files"]["error"].is_null());
    assert_eq!(show["settings"]["connect-on-launch"], false);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_success(&backend.set_web_ui_password(WEB_PASSWORD));
        let credentials = data.join("credentials.toml");
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o644)).unwrap();
        let doctor = backend.command(&["doctor", "--json"]).output().unwrap();
        let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
        assert!(doctor["warnings"]["credentials"]
            .as_str()
            .unwrap()
            .contains("chmod 600"));
    }

    let schema = backend.command(&["settings", "schema"]).output().unwrap();
    assert_success(&schema);
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert!(schema["properties"]["local-api"].is_object());
    assert_eq!(
        fs::read_to_string(data.join("config.schema.json"))
            .unwrap()
            .trim_end(),
        serde_json::to_string_pretty(&schema).unwrap()
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
fn malformed_requests_and_event_streams_do_not_stop_backend() {
    use std::io::Write;
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
    use std::{io::BufRead, os::unix::net::UnixStream};
    // A pre-0.2 frame and a truncated request are not HTTP; each closes only its connection.
    for garbage in [
        &b"{\"version\":3,\"id\":1,\"command\":{\"method\":\"notACommand\"}}\n"[..],
        b"POST /api/rpc/stop HTTP/1.1\r\nHost: localhost\r\nContent-Length: 99\r\n\r\n{",
    ] {
        let mut stream = UnixStream::connect(&endpoint).unwrap();
        stream.write_all(garbage).unwrap();
        drop(stream);
    }
    // Event streams start with the state; a subscriber may leave at any time.
    let mut subscribers = Vec::new();
    for _ in 0..8 {
        let mut stream = UnixStream::connect(&endpoint).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "GET /api/events HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let snapshot = (0..64)
            .map_while(|_| {
                let mut line = String::new();
                reader.read_line(&mut line).ok().filter(|read| *read > 0)?;
                Some(line)
            })
            .find_map(|line| {
                let event: Value = serde_json::from_str(line.strip_prefix("data: ")?).ok()?;
                (event["event"] == "pap://state").then(|| event["payload"].clone())
            })
            .expect("the event stream starts with the state");
        assert!(snapshot["proxyUrl"]
            .as_str()
            .is_some_and(|url| url.starts_with("http://127.0.0.1:")));
        subscribers.push(reader);
    }
    drop(subscribers.pop());
    assert!(backend.run(&["status"])["backend"]["processId"].is_number());
    drop(subscribers);
    assert!(backend.run(&["status"])["backend"]["processId"]
        .as_u64()
        .is_some());
}

/// A 0.1.4 to 0.2 beta backend speaks only NDJSON on the old endpoint; this
/// client still stops it (`client::legacy`, removed in 0.3).
#[cfg(unix)]
#[test]
fn a_legacy_backend_is_stopped_over_its_own_protocol() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };
    let sandbox = Sandbox::new();
    let cli = sandbox.path().join(executable("private-ai-proxy"));
    install(env!("CARGO_BIN_EXE_private-ai-proxy"), &cli);
    let home = sandbox.path().join("home");
    let runtime = home.join(".private-ai-proxy/runtime");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = UnixListener::bind(runtime.join("backend.sock")).unwrap();
    // Stands in for the old backend process whose exit the client awaits.
    let mut process = Command::new("sleep").arg("60").spawn().unwrap();
    let pid = process.id();
    let server = std::thread::spawn(move || {
        let request = loop {
            let (mut stream, _) = listener.accept().unwrap();
            let hello = json!({
                "protocolVersion": 3,
                "product": desktop_core::brand::APP_IDENTIFIER,
                "version": "0.1.7",
                "instanceId": "legacy-1",
                "processId": pid,
                "executable": "/opt/old/private-ai-proxy-service",
            });
            writeln!(stream, "{hello}").unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut request = String::new();
            let _ = BufReader::new(&stream).read_line(&mut request);
            if !request.is_empty() {
                writeln!(stream, r#"{{"id":1,"outcome":{{"result":null}}}}"#).unwrap();
                break request;
            }
        };
        process.kill().unwrap();
        process.wait().unwrap();
        request
    });
    let command = |args: &[&str]| {
        Command::new(&cli)
            .args(args)
            .env(desktop_core::paths::HOME_OVERRIDE_ENV, &home)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let status = command(&["status", "--json"]);
    assert!(String::from_utf8_lossy(&status.stderr).contains("service start to replace it"));
    assert_success(&command(&["service", "stop", "--yes", "--json"]));
    let request: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert_eq!(
        request,
        json!({
            "version": 3,
            "id": 1,
            "command": {
                "method": "shutdown",
                "params": { "instance_id": "legacy-1", "mode": "quit" },
            },
        })
    );
}
