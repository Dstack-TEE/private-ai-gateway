//! Golden outputs of the command surface: help, human and `--json` output,
//! error messages and exit codes. Each case is `$ pap <args>`, with a nonzero
//! exit code, then stdout, then stderr marked `! `. After an intended change,
//! rewrite them with `SNAPSHOTS=overwrite cargo test --package
//! private-ai-proxy --test golden --bin private-ai-proxy`; the binary's tests
//! hold the human renderings.
mod support;

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
};

use sha2::{Digest, Sha256};
use snapbox::{assert::DEFAULT_ACTION_ENV, Assert, Data, Redactions};
use support::Backend;

const PAP: &str = env!("CARGO_BIN_EXE_private-ai-proxy");

#[test]
#[ignore = "subprocess fixture that tears down a test sandbox"]
fn sandbox_teardown_watchdog() {
    support::watchdog();
}

/// One transcript entry: `command` run with `line`, split as a shell would,
/// and `stdin`.
fn run(mut command: Command, line: &str, stdin: &str) -> String {
    let mut child = command
        .args(shlex::split(line).expect("a well-formed command line"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.as_bytes()).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    let mut entry = format!("$ pap {line}");
    if output.status.code() != Some(0) {
        entry.push_str(&format!("  # exit {:?}", output.status.code()));
    }
    entry.push('\n');
    entry.push_str(&String::from_utf8_lossy(&output.stdout));
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        entry.push_str(&format!("! {line}\n"));
    }
    entry + "\n"
}

/// A path or URL as one shell word.
fn quote(value: &str) -> String {
    shlex::try_quote(value).unwrap().into_owned()
}

/// Compares byte for byte, after replacing run-dependent values with
/// placeholders, against `tests/golden/<name>`.
fn assert_golden(name: &str, actual: String, values: &[(&'static str, &str)]) {
    let mut redactions = Redactions::new();
    for (placeholder, value) in values {
        redactions.insert(placeholder, value.to_string()).unwrap();
    }
    // The expiry check (id-3) names the current time.
    let now = regex::Regex::new(r"now (?<redacted>\d+) < not_after").unwrap();
    redactions.insert("[NOW]", now).unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    Assert::new().action_env(DEFAULT_ACTION_ENV).eq(
        redactions.redact(&actual),
        Data::read_from(&path, None).raw(),
    );
}

/// A plain-HTTP service that answers with `body`, as an ACI service serves
/// its attestation report, except under the paths named for other answers.
fn serve_json(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            // Read the whole request head so closing never resets the reply;
            // a TLS handshake (not `GET`) gets the plain reply at once.
            let mut head = Vec::new();
            let mut byte = [0];
            while !head.ends_with(b"\r\n\r\n")
                && head.first().is_none_or(|first| *first == b'G')
                && stream.read(&mut byte).unwrap_or(0) == 1
            {
                head.push(byte[0]);
            }
            let path = String::from_utf8_lossy(&head)
                .split('/')
                .nth(1)
                .map(str::to_owned);
            let (status, body) = match path.as_deref() {
                Some("busy") => ("503 Service Unavailable", &b""[..]),
                Some("limited") => ("429 Too Many Requests", &b""[..]),
                Some("private") => ("403 Forbidden", &b""[..]),
                Some("missing") => ("404 Not Found", &b""[..]),
                Some("plain") => ("200 OK", &b"not an attestation"[..]),
                _ => ("200 OK", &body[..]),
            };
            let head = format!("HTTP/1.1 {status}\r\nconnection: close\r\ncontent-length");
            let _ = write!(stream, "{head}: {}\r\n\r\n", body.len());
            let _ = stream.write_all(body);
        }
    });
    url
}

/// Usage for the whole command, help for an ACI and a management command, and
/// the completion script, which lists every command, option and value, by
/// digest.
#[test]
fn help() {
    let mut transcript: String = ["", "help verify", "settings set -h"]
        .iter()
        .map(|line| run(Command::new(PAP), line, ""))
        .collect();
    let script = Command::new(PAP)
        .args(["completions", "bash"])
        .output()
        .unwrap()
        .stdout;
    transcript.push_str(&format!(
        "$ pap completions bash\nsha256 {}, {} lines\n",
        hex::encode(Sha256::digest(&script)),
        String::from_utf8_lossy(&script).lines().count()
    ));
    assert_golden("help.txt", transcript, &[]);
}

#[test]
fn aci_commands() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let wire: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixtures.join("aci_wire_fixtures.json")).unwrap())
            .unwrap();
    let file = |name: &str, contents: &[u8]| {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        quote(path.to_str().unwrap())
    };
    let receipt = file("receipt.json", wire["receipt"].to_string().as_bytes());
    let session = file("session.json", wire["session"].to_string().as_bytes());
    let request = br#"{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}"#;
    let request = file("request.json", request);
    let response = file("response.json", br#"{"choices":[],"id":"chatcmpl-123"}"#);
    let invalid = file("invalid.json", b"{");
    let missing = quote(root.join("missing.json").to_str().unwrap());
    let report = fixtures.join("aci_report_fixture.json");
    // The fixture report binds this nonce, so its checks reach explain material.
    let service = serve_json(fs::read(&report).unwrap());
    let tls = service.replace("http:", "https:");
    let nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
    let report = format!("audit --report {}", quote(report.to_str().unwrap()));
    let refused = format!(
        "https://{}",
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    );
    let pin = "a".repeat(64);
    let kms = "--accept-dstack-kms-root-public-key";
    let root_key = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    let mut transcript: String = [
        format!("{report} --nonce {nonce}"),
        format!("{report} --receipt {receipt} --session {session} --request-body {request} --response-body {response} --require-claim tee_attested=hardware_proven --pin {pin}"),
        format!("audit --report {missing}"),
        format!("audit --report {missing} --json"),
        format!("audit --report {invalid}"),
        format!("{report} --receipt {invalid}"),
        format!("{report} --pin not-hex --json"),
        format!("{report} --require-claim a=b"),
        format!("{report} --accept-subject 0xab"),
        format!("{report} --accept-subject app-id:0xab"),
        format!("{report} {kms} {root_key}"),
        format!("{report} --accept-subject app-id:0xab {kms} 02zz"),
        format!("verify {service} --nonce {nonce} --explain"),
        format!("verify {service} --nonce {nonce} --explain --json"),
        format!("verify {service}/busy"),
        format!("verify {service}/limited"),
        format!("verify {service}/private"),
        format!("verify {service}/missing"),
        format!("verify {service}/plain"),
        format!("verify {tls}"),
        format!("verify {refused}"),
        format!("verify {refused} --json"),
        "verify ''".to_string(),
        "verify 'not a url'".to_string(),
        format!("sessions {refused} --model demo"),
        format!("send {refused} --api-key sk-test --no-stream"),
        format!("send {refused} --api-key-stdin --api-key sk-test"),
        format!("send {refused} --allow-unverified --session {pin}"),
        format!("send {refused} --session short"),
        "curl http://example.com/v1/models".to_string(),
        "curl https://user@example.com/v1/models --json".to_string(),
        "curl 'https://example.com/v1/models#frag'".to_string(),
        "curl https://example.com/v1/models -- -L".to_string(),
        "curl https://example.com/v1/models -- --header".to_string(),
        format!("curl {refused}/v1 -- --silent -H 'a: b'"),
        "curl".to_string(),
        format!("serve {refused}"),
        format!("serve {refused} --json-events"),
        format!("--json serve {refused}"),
        format!("serve {refused} --session {pin} --allow-unverified"),
        "unknown-command".to_string(),
        "--json --non-interactive unknown-command".to_string(),
        format!("verify {refused} --json --unknown-option"),
    ]
    .iter()
    .map(|line| run(Command::new(PAP), line, ""))
    .collect();
    let line = format!("send {refused} --api-key-stdin");
    transcript.push_str(&run(Command::new(PAP), &line, "  \n"));
    assert_golden(
        "aci.txt",
        transcript,
        &[
            ("[ROOT]", root.to_str().unwrap()),
            ("[FIXTURES]", fixtures.to_str().unwrap()),
            ("[REFUSED]", &refused),
            ("[SERVICE]", &service),
            ("[TLS_SERVICE]", &tls),
        ],
    );
}

#[test]
fn management_without_a_backend() {
    let home = tempfile::tempdir().unwrap();
    let transcript: String = [
        "status",
        "status --json",
        "status --watch",
        "profiles list",
        "--json profiles list",
        "token show",
        "--json profiles add --id test --name Test --url https://example.com --key-stdin",
        "profiles add --id t --name T --url 'not a url' --yes",
        "profiles add --id test",
        "profiles edit work --allow-development-os --require-production-os",
        "agents connect codex --dry-run --revision r",
        "start --timeout 0",
        "settings set appearance",
        "settings set no-such-key true",
        "settings set web-ui.password secret --yes",
    ]
    .iter()
    .map(|line| {
        let mut command = Command::new(PAP);
        command.env(desktop_core::paths::HOME_OVERRIDE_ENV, home.path());
        run(command, line, "")
    })
    .collect();
    assert_golden("management-offline.txt", transcript, &[]);
    // Nothing started a backend or saved a credential, even the refused `profiles add`.
    assert!(fs::read_dir(home.path()).unwrap().next().is_none());
}

/// The first state `status --watch` prints, through its models line, which
/// ends the rendering of a backend with no protection session. The watch
/// streams until stopped, so the command is killed once it has been read.
fn first_watch_snapshot(mut command: Command) -> String {
    let mut child = command
        .args(["status", "--watch"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut snapshot = String::from("$ pap status --watch  # first state\n");
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
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
    let profiles = r#"{"version":1,"profiles":[{"name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com"}]}"#;
    fs::write(&backup, profiles).unwrap();
    let backup = quote(backup.to_str().unwrap());
    let state = backend.run(&["status"]);
    let pid = state["backend"]["processId"].to_string();
    let port = state["gateway"]["localApi"]["port"].to_string();
    let case = |line: &str| run(backend.command(&[]), line, "");
    let mut transcript: String = [
        "status",
        "service start",
        "settings show",
        "settings show --json",
        "profiles list --json",
        &format!("profiles import {backup}"),
        &format!("profiles import {backup} --yes"),
    ]
    .iter()
    .map(|line| case(line))
    .collect();
    transcript.push_str(&first_watch_snapshot(backend.command(&[])));
    let profile = backend.run(&["profiles", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let password = "settings set web-ui.password --value-stdin --yes";
    transcript.push_str(&run(backend.command(&[]), password, "a new password\r\n"));
    let exported = quote(root.join("exported.json").to_str().unwrap());
    let usage = quote(root.join("usage.csv").to_str().unwrap());
    transcript.extend(
        [
            "profiles list",
            &format!("profiles show {profile}"),
            "profiles show missing",
            &format!("profiles export --output {exported}"),
            &format!("profiles export --output {exported}"),
            "profiles use missing --yes",
            "profiles use missing --yes --json",
            &format!("profiles remove {profile} --yes"),
            "settings set appearance dark --yes",
            "settings set appearance purple --yes",
            "settings set update-channel nightly --yes",
            "settings set local-api.port port --yes",
            "settings set connect-on-launch maybe --yes",
            r#"settings set notifications '{"gateway":false}' --yes"#,
            "settings set autoCliRegistration false --yes",
            "settings set appearance --value-stdin --yes",
            "settings set appearance light --yes --json",
            "usage list",
            "usage list --json --agent codex --limit 5",
            "usage show missing",
            "usage show missing --receipt --json",
            &format!("usage export --output {usage}"),
            "usage clear",
            "usage clear --yes --json",
            "models list",
            "models list --json",
            "token rotate --yes",
            "token clear-credential --yes",
            "web-ui password rotate --yes",
            "agents disconnect codex --revision stale --yes",
            "--json --yes agents disconnect codex --revision x",
            "agents connect no-such-agent --dry-run",
            "stop",
            "stop --offline",
            "service stop",
            "service stop --yes",
            "service status",
        ]
        .iter()
        .map(|line| case(line)),
    );
    assert_golden(
        "management.txt",
        transcript,
        &[
            ("[ROOT]", root.to_str().unwrap()),
            ("[PROFILE]", &profile),
            ("[PORT]", &port),
            ("[PID]", &pid),
            ("[VERSION]", env!("CARGO_PKG_VERSION")),
        ],
    );
}
