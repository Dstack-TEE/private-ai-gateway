//! Golden outputs of the command surface: help, human and `--json` output,
//! error messages and exit codes, each case as `$ pap <args>` with its exit
//! code and both streams. After an intended change, rewrite them with
//! `SNAPSHOTS=overwrite cargo test --package private-ai-proxy --test golden
//! --bin private-ai-proxy`; the binary's tests hold the human renderings.
mod support;

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use serde_json::Value;
use snapbox::{assert::DEFAULT_ACTION_ENV, Assert, Data, Redactions};
use support::Backend;

const PAP: &str = env!("CARGO_BIN_EXE_private-ai-proxy");

#[test]
#[ignore = "subprocess fixture that tears down a test sandbox"]
fn sandbox_teardown_watchdog() {
    support::watchdog();
}

/// Every command path, as `--help` and `help` list them.
const COMMANDS: &[&str] = &[
    "",
    "status",
    "start",
    "stop",
    "service",
    "service start",
    "service stop",
    "service status",
    "profiles",
    "profiles list",
    "profiles show",
    "profiles import",
    "profiles export",
    "profiles login",
    "profiles add",
    "profiles verify",
    "profiles edit",
    "profiles use",
    "profiles remove",
    "agents",
    "agents list",
    "agents connect",
    "agents disconnect",
    "agents disconnect-all",
    "models",
    "models list",
    "usage",
    "usage list",
    "usage show",
    "usage export",
    "usage clear",
    "settings",
    "settings reset",
    "settings show",
    "settings schema",
    "settings set",
    "token",
    "token rotate",
    "token show",
    "token clear-credential",
    "web-ui",
    "web-ui password",
    "web-ui password show",
    "web-ui password rotate",
    "cli",
    "cli status",
    "cli install",
    "cli uninstall",
    "app",
    "app open",
    "doctor",
    "diagnostics",
    "completions",
    "verify",
    "audit",
    "sessions",
    "send",
    "curl",
    "serve",
];

/// One invocation as a transcript entry: the command line, exit code, stdout
/// and stderr.
struct Case {
    command: Command,
    label: String,
    stdin: Option<String>,
}

impl Case {
    fn new(args: &[&str]) -> Self {
        Self::with(Command::new(PAP), args)
    }

    fn with(mut command: Command, args: &[&str]) -> Self {
        command.args(args).stdin(Stdio::null());
        Self {
            command,
            label: args.join(" "),
            stdin: None,
        }
    }

    fn env(mut self, key: &str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    fn stdin(mut self, input: &str) -> Self {
        self.command.stdin(Stdio::piped());
        self.stdin = Some(input.to_string());
        self
    }

    fn run(mut self) -> String {
        let mut child = self
            .command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = &self.stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        format!(
            "$ pap {}\nexit: {:?}\n--- stdout\n{}--- stderr\n{}\n",
            self.label,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    }
}

/// Compares byte for byte, after replacing run-dependent values with
/// placeholders, against `tests/golden/<name>`.
fn assert_golden(name: &str, actual: String, redactions: Redactions) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    Assert::new().action_env(DEFAULT_ACTION_ENV).eq(
        redactions.redact(&actual),
        Data::read_from(&path, None).raw(),
    );
}

fn redactions(values: &[(&'static str, &str)]) -> Redactions {
    let mut redactions = Redactions::new();
    redactions
        .insert("[VERSION]", env!("CARGO_PKG_VERSION"))
        .unwrap();
    for (placeholder, value) in values {
        redactions.insert(placeholder, value.to_string()).unwrap();
    }
    redactions
}

/// A port nothing listens on, so every connection to it is refused.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn help() {
    let mut transcript = Case::new(&["-h"]).run();
    transcript.push_str(&Case::new(&["--version"]).run());
    for command in COMMANDS {
        let mut args: Vec<&str> = command.split_whitespace().collect();
        args.push("--help");
        transcript.push_str(&Case::new(&args).run());
    }
    transcript.push_str(&Case::new(&[]).run());
    transcript.push_str(&Case::new(&["help", "verify"]).run());
    assert_golden("help.txt", transcript, redactions(&[]));
}

#[test]
fn completions() {
    let transcript = Case::new(&["completions", "bash"]).run();
    assert_golden("completions-bash.txt", transcript, redactions(&[]));
}

/// A plain-HTTP service that answers every request with `body`, as an ACI
/// service serves its attestation report.
fn serve_json(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            // Read the whole request head so closing never resets the reply.
            let mut head = Vec::new();
            let mut byte = [0];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    url
}

#[test]
fn aci_commands() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let report = fixtures.join("aci_report_fixture.json");
    let wire: Value =
        serde_json::from_str(&fs::read_to_string(fixtures.join("aci_wire_fixtures.json")).unwrap())
            .unwrap();
    let file = |name: &str, contents: &[u8]| {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        path.to_str().unwrap().to_string()
    };
    let receipt = file("receipt.json", wire["receipt"].to_string().as_bytes());
    let session = file("session.json", wire["session"].to_string().as_bytes());
    let request = file(
        "request.json",
        br#"{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}"#,
    );
    let response = file("response.json", br#"{"choices":[],"id":"chatcmpl-123"}"#);
    let invalid = file("invalid.json", b"{");
    let missing = root.join("missing.json");
    // The fixture report binds this nonce, so its checks reach explain material.
    let service = serve_json(fs::read(&report).unwrap());
    let nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
    let report = report.to_str().unwrap();
    let missing = missing.to_str().unwrap();
    let refused = format!("https://127.0.0.1:{}", closed_port());
    let session_id = "a".repeat(64);
    let root_key = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    let cases: Vec<Case> = vec![
        Case::new(&["audit", "--report", report]),
        Case::new(&["audit", "--report", report, "--json"]),
        Case::new(&["--json", "audit", "--report", report]),
        Case::new(&[
            "audit",
            "--report",
            report,
            "--accept-compose",
            "abcd",
            "--accept-subject",
            "app-id:0xab",
            "--accept-dstack-kms-root-public-key",
            root_key,
            "--require-production-os",
            "--skip-expiry",
        ]),
        Case::new(&[
            "audit",
            "--report",
            report,
            "--receipt",
            &receipt,
            "--session",
            &session,
            "--request-body",
            &request,
            "--response-body",
            &response,
            "--require-claim",
            "tee_attested=hardware_proven",
            "--pin",
            &session_id,
        ]),
        Case::new(&[
            "audit",
            "--report",
            report,
            "--receipt",
            &receipt,
            "--require-verified",
            "--json",
        ]),
        Case::new(&["audit", "--report", missing]),
        Case::new(&["audit", "--report", missing, "--json"]),
        Case::new(&["audit", "--report", &invalid]),
        Case::new(&["audit", "--report", report, "--receipt", &invalid]),
        Case::new(&["audit", "--report", report, "--pin", "not-hex"]),
        Case::new(&["audit", "--report", report, "--pin", "not-hex", "--json"]),
        Case::new(&["audit", "--report", report, "--require-claim", "a=b"]),
        Case::new(&["audit", "--report", report, "--accept-subject", "0xab"]),
        Case::new(&[
            "audit",
            "--report",
            report,
            "--accept-dstack-kms-root-public-key",
            root_key,
        ]),
        Case::new(&["audit"]),
        Case::new(&["verify", &service, "--nonce", nonce, "--explain"]),
        Case::new(&["verify", &service, "--nonce", nonce, "--explain", "--json"]),
        Case::new(&["verify", &service, "--nonce", nonce, "--json"]),
        Case::new(&["verify", &refused]),
        Case::new(&["verify", &refused, "--json", "--explain"]),
        Case::new(&["verify", ""]),
        Case::new(&["verify", "not a url"]),
        Case::new(&["verify", &refused, "--accept-subject", "app-id:0xab"]),
        Case::new(&["sessions", &refused, "--model", "demo"]),
        Case::new(&["sessions", &refused, "--json"]),
        Case::new(&["send", &refused]).env("ACI_API_KEY", "sk-test"),
        Case::new(&["send", &refused, "--json", "--no-stream"]),
        Case::new(&["send", &refused, "--api-key-stdin"]).stdin("  \n"),
        Case::new(&["send", &refused, "--api-key-stdin"]).stdin("sk-test\n"),
        Case::new(&["send", &refused, "--api-key", "sk-test", "--json"]),
        Case::new(&["send", &refused, "--api-key-stdin", "--api-key", "sk-test"]),
        Case::new(&[
            "send",
            &refused,
            "--allow-unverified",
            "--session",
            &session_id,
        ]),
        Case::new(&["send", &refused, "--session", "short"]),
        Case::new(&["curl", "http://example.com/v1/models"]),
        Case::new(&["curl", "https://user@example.com/v1/models", "--json"]),
        Case::new(&["curl", "https://example.com/v1/models#frag"]),
        Case::new(&["curl", "https://example.com/v1/models", "--", "-L"]),
        Case::new(&["curl", "https://example.com/v1/models", "--", "--header"]),
        Case::new(&[
            "curl",
            &format!("{refused}/v1/models"),
            "--",
            "--silent",
            "--header",
            "accept: */*",
        ]),
        Case::new(&["curl"]),
        Case::new(&["serve", &refused]),
        Case::new(&["serve", &refused, "--json-events"]),
        Case::new(&["--json", "serve", &refused]),
        Case::new(&[
            "serve",
            &refused,
            "--session",
            &session_id,
            "--allow-unverified",
        ]),
        Case::new(&[
            "serve",
            &refused,
            "--accept-subject",
            "0xab",
            "--json-events",
        ]),
        Case::new(&["unknown-command"]),
        Case::new(&["--json", "--non-interactive", "unknown-command"]),
        Case::new(&["verify", "--unknown-option"]),
        Case::new(&["verify", &refused, "--json", "--unknown-option"]),
    ];
    let transcript: String = cases.into_iter().map(Case::run).collect();
    let mut redactions = redactions(&[
        ("[ROOT]", root.to_str().unwrap()),
        ("[FIXTURES]", fixtures.to_str().unwrap()),
        ("[REFUSED]", &refused),
        ("[SERVICE]", &service),
    ]);
    // The expiry check (id-3) names the current time.
    let now = regex::Regex::new(r"now (?<redacted>\d+) < not_after").unwrap();
    redactions.insert("[NOW]", now).unwrap();
    assert_golden("aci.txt", transcript, redactions);
}

#[test]
fn management_without_a_backend() {
    let home = tempfile::tempdir().unwrap();
    let case =
        |args: &[&str]| Case::new(args).env(desktop_core::paths::HOME_OVERRIDE_ENV, home.path());
    let cases = vec![
        case(&["status"]),
        case(&["status", "--json"]),
        case(&["status", "--watch"]),
        case(&["service", "status"]),
        case(&["profiles", "list"]),
        case(&["--json", "profiles", "list"]),
        case(&["stop"]),
        case(&["token", "show"]),
        case(&["token", "show", "--yes", "--json"]),
        case(&[
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
        ]),
        case(&[
            "profiles",
            "add",
            "--id",
            "test",
            "--name",
            "Test",
            "--url",
            "not a url",
            "--yes",
        ]),
        case(&["profiles", "add", "--id", "test"]),
        case(&[
            "profiles",
            "edit",
            "work",
            "--allow-development-os",
            "--require-production-os",
        ]),
        case(&["agents", "connect", "codex", "--dry-run", "--revision", "r"]),
        case(&["start", "--timeout", "0"]),
        case(&["usage", "list", "--limit", "101"]),
        case(&["usage", "export", "--output", "out.csv", "--format", "json"]),
        case(&["settings", "set", "appearance"]),
        case(&["settings", "set", "no-such-key", "true"]),
        case(&["settings", "set", "web-ui.password", "secret", "--yes"]),
        case(&["completions", "no-such-shell"]),
    ];
    let transcript: String = cases.into_iter().map(Case::run).collect();
    assert_golden(
        "management-offline.txt",
        transcript,
        redactions(&[("[HOME]", home.path().to_str().unwrap())]),
    );
}

/// The first state `status --watch` prints, through its models line, which
/// ends the rendering of a backend with no protection session. The watch
/// streams until stopped, so the command is killed once it has been read.
fn first_watch_snapshot(mut command: Command) -> String {
    let mut child = command
        .args(["status", "--watch"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (lines, received) = mpsc::channel();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in stdout.lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut snapshot = String::from("$ pap status --watch (first state)\n--- stdout\n");
    loop {
        let line = received
            .recv_timeout(Duration::from_secs(30))
            .expect("status --watch prints the current state");
        snapshot.push_str(&line);
        snapshot.push('\n');
        if line.starts_with("Models: ") {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    snapshot + "\n"
}

#[test]
fn management_with_a_backend() {
    let backend = Backend::start();
    let root = backend.directory.path();
    let backup = root.join("profiles.json");
    fs::write(
        &backup,
        r#"{"version":1,"profiles":[{"name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com"}]}"#,
    )
    .unwrap();
    let backup = backup.to_str().unwrap();
    let state = backend.run(&["status"]);
    let pid = state["backend"]["processId"].to_string();
    let port = state["gateway"]["localApi"]["port"].to_string();
    let case = |args: &[&str]| Case::with(backend.command(&[]), args);
    let mut transcript: String = [
        case(&["status"]),
        case(&["service", "start"]),
        case(&["settings", "show"]),
        case(&["settings", "show", "--json"]),
        case(&["profiles", "list"]),
        case(&["profiles", "list", "--json"]),
        case(&["profiles", "import", backup]),
        case(&["profiles", "import", backup, "--yes"]),
    ]
    .into_iter()
    .map(Case::run)
    .collect();
    transcript.push_str(&first_watch_snapshot(backend.command(&[])));
    let profile = backend.run(&["profiles", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let exported = root.join("exported.json");
    let diagnostics = root.join("diagnostics.json");
    let usage = root.join("usage.csv");
    transcript.extend(
        [
            case(&["profiles", "list"]),
            case(&["profiles", "show", &profile]),
            case(&["profiles", "show", "missing"]),
            case(&["profiles", "export", "--output", exported.to_str().unwrap()]),
            case(&["profiles", "export", "--output", exported.to_str().unwrap()]),
            case(&["diagnostics", "--output", diagnostics.to_str().unwrap()]),
            case(&["profiles", "use", "missing", "--yes"]),
            case(&["profiles", "use", "missing", "--yes", "--json"]),
            case(&["profiles", "remove", &profile, "--yes"]),
            case(&["profiles", "list"]),
            case(&["settings", "set", "appearance", "dark", "--yes"]),
            case(&["settings", "set", "appearance", "purple", "--yes"]),
            case(&["settings", "set", "update-channel", "nightly", "--yes"]),
            case(&["settings", "set", "local-api.port", "port", "--yes"]),
            case(&["settings", "set", "web-ui.port", "port", "--yes"]),
            case(&["settings", "set", "connect-on-launch", "maybe", "--yes"]),
            case(&["settings", "set", "notifications", "[]", "--yes"]),
            case(&["settings", "set", "appearance", "--value-stdin", "--yes"]),
            case(&["settings", "set", "autoCliRegistration", "false", "--yes"]),
            case(&[
                "settings",
                "set",
                "notifications",
                "{\"gateway\":false}",
                "--yes",
            ]),
            case(&[
                "settings",
                "set",
                "notifications.local-api",
                "false",
                "--yes",
            ]),
            case(&[
                "settings",
                "set",
                "web-ui.password",
                "--value-stdin",
                "--yes",
            ])
            .stdin("a new password\r\n"),
            case(&["settings", "set", "appearance", "light", "--yes", "--json"]),
            case(&["usage", "list"]),
            case(&[
                "usage", "list", "--json", "--agent", "codex", "--limit", "5",
            ]),
            case(&["usage", "show", "missing"]),
            case(&["usage", "show", "missing", "--receipt", "--json"]),
            case(&["usage", "export", "--output", usage.to_str().unwrap()]),
            case(&["usage", "clear"]),
            case(&["usage", "clear", "--yes"]),
            case(&["usage", "clear", "--yes", "--json"]),
            case(&["models", "list"]),
            case(&["models", "list", "--json"]),
            case(&["token", "rotate", "--yes"]),
            case(&["token", "rotate", "--yes", "--json"]),
            case(&["token", "clear-credential", "--yes"]),
            case(&["web-ui", "password", "rotate", "--yes"]),
            case(&["web-ui", "password", "rotate", "--yes", "--json"]),
            case(&[
                "agents",
                "disconnect",
                "codex",
                "--revision",
                "stale",
                "--yes",
            ]),
            case(&[
                "--json",
                "--yes",
                "agents",
                "disconnect",
                "codex",
                "--revision",
                "stale",
            ]),
            case(&["agents", "connect", "no-such-agent", "--dry-run"]),
            case(&["stop"]),
            case(&["stop", "--offline"]),
            case(&["status"]),
            case(&["service", "stop"]),
            case(&["service", "stop", "--yes"]),
            case(&["service", "status"]),
        ]
        .into_iter()
        .map(Case::run),
    );
    assert_golden(
        "management.txt",
        transcript,
        redactions(&[
            ("[ROOT]", root.to_str().unwrap()),
            ("[PROFILE]", &profile),
            ("[PORT]", &port),
            ("[PID]", &pid),
        ]),
    );
}
