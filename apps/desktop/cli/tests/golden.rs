//! The command-line contract scripts and users rely on: `--json` shapes,
//! exit codes, error codes and messages, help for the entry points and the
//! completion script. Rewrite the expectations after an intended change by
//! running, from `apps/desktop`, `SNAPSHOTS=overwrite
//! CARGO_RUSTC_CURRENT_DIR=$PWD cargo test --package private-ai-proxy --test
//! golden`; the variable tells snapbox which workspace holds this file.
mod support;

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
};

use sha2::{Digest, Sha256};
use snapbox::{
    assert::DEFAULT_ACTION_ENV,
    cmd::{cargo_bin, Command},
    str, Assert, Redactions,
};
use support::Backend;

#[test]
#[ignore = "subprocess fixture that tears down a test sandbox"]
fn sandbox_teardown_watchdog() {
    support::watchdog();
}

/// An assertion that reads run-dependent `values` as their placeholders.
fn redacting(values: &[(&'static str, &str)]) -> Assert {
    let mut redactions = Redactions::new();
    for (placeholder, value) in values {
        redactions.insert(placeholder, value.to_string()).unwrap();
    }
    // The expiry check (id-3) names the current time.
    let now = regex::Regex::new(r"now (?<redacted>\d+) < not_after").unwrap();
    redactions.insert("[NOW]", now).unwrap();
    // Escapes in JSON output are not Windows path separators.
    Assert::new()
        .action_env(DEFAULT_ACTION_ENV)
        .normalize_paths(false)
        .redact_with(redactions)
}

fn pap() -> Command {
    Command::new(cargo_bin!("private-ai-proxy"))
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
                Some("private") => ("403 Forbidden", &b""[..]),
                Some("missing") => ("404 Not Found", &b""[..]),
                _ => ("200 OK", &body[..]),
            };
            let head = format!("HTTP/1.1 {status}\r\nconnection: close\r\ncontent-length");
            let _ = write!(stream, "{head}: {}\r\n\r\n", body.len());
            let _ = stream.write_all(body);
        }
    });
    url
}

#[test]
fn help() {
    pap().assert().code(2).stdout_eq("").stderr_eq(str![[r#"
Private AI Proxy: manage local protection and verify confidential AI services

Usage: private-ai-proxy [OPTIONS] <COMMAND>

Commands:
  status       Show backend and protection state. This does not start the backend
  start        Start protection and wait for a verified connection
  stop         Stop protection and restore managed agent configurations. The backend keeps running; with --offline, no backend is needed
  service      Manage the local backend process. Starting it may start protection when Protect on launch (`connect-on-launch`) is on
  profiles     Sign in, list, inspect, verify, import, export, select, or delete service profiles
  agents       Inspect or change supported coding-agent configurations
  models       Inspect models from the last verified catalog
  usage        Query or export local usage records
  settings     Inspect or change settings (config.toml; API keys and passwords are in credentials.toml)
  token        Manage the Local API token and active profile credential
  web-ui       Manage the browser UI hosted by the backend
  cli          Manage installation of the private-ai-proxy command
  app          Open the installed desktop app. Its backend may start protection when Protect on launch (`connect-on-launch`) is on
  doctor       Report independent local installation and backend diagnostics without starting services
  diagnostics  Export a redacted diagnostics report to a new file
  completions  Generate shell completions from the current command definition
  verify       Fetch /v1/aci/attestation with a fresh nonce, run the spec 9.1 checks under the dstack tdx verifier policy (spec 1.3), and print a verification transcript. Exits 0 only if the verdict is VERIFIED.
  audit        Offline verification of saved artifacts using the same transcript engine as verify.
  sessions     Verify the service (fail closed), list its current attested sessions, and run the spec 9.2 audit on each — the ids that pass are what a client pins (spec 5.3).
  send         Verify the service (fail closed), send one chat completion over an SPKI-pinned connection, then fetch and verify its receipt. The API key is read from stdin with --api-key-stdin or from the ACI_API_KEY environment variable.
  curl         Verify the target's ACI service (fail closed), then run the system curl with the attested TLS key pinned. Exits 125 when pap refuses the request, otherwise with curl's exit code.
  serve        Local verifying proxy (default 127.0.0.1:4180, plain HTTP on localhost). Verifies the service on startup and refuses to start unless VERIFIED, accepts plaintext API requests only, forwards them over the pinned attested TLS channel, and verifies each POST response's receipt after the fact.
  help         Print this message or the help of the given subcommand(s)

Options:
      --json                   Emit compact JSON instead of human-readable output
      --non-interactive        Never prompt. Mutations require --yes; credential inputs use stdin flags [alias: --no-interactive]
      --yes                    Approve a command's documented mutation without prompting
      --require-production-os  Require an attested production OS image
  -h, --help                   Print help (see more with '--help')
  -V, --version                Print version

"#]]);
    pap()
        .args(["help", "verify"])
        .assert()
        .success()
        .stdout_eq(str![[r#"
Fetch /v1/aci/attestation with a fresh nonce, run the spec 9.1 checks under the dstack tdx verifier policy (spec 1.3), and print a verification transcript. Exits 0 only if the verdict is VERIFIED.

Usage: private-ai-proxy verify [OPTIONS] <BASE_URL>

Arguments:
  <BASE_URL>  Base URL of the ACI service to verify.

Options:
      --accept-compose <HEX>
          Compose hash to accept (spec 1.3 verifier policy); repeatable. Without it the compose measurement is verified and reported, and you appraise the provenance yourself.
      --accept-subject <app-id:0xHEX>
          Measured dstack app-id to accept for key custody (spec 9.1(5)); repeatable. Custody is checked when this and --accept-dstack-kms-root-public-key are given; with neither, id-5 is skipped.
      --non-interactive
          Never prompt. Mutations require --yes; credential inputs use stdin flags [alias: --no-interactive]
      --accept-dstack-kms-root-public-key <HEX>
          dstack KMS root public key the receipt-key custody chain must end at (spec 3.3, 9.1(5)); repeatable.
      --yes
          Approve a command's documented mutation without prompting
      --nonce <NONCE>
          Nonce to send with the attestation request; a fresh random one is generated when omitted.
      --json
          Print the verification transcript as JSON instead of the human-readable form.
      --explain
          Print a one-line explanation for every check, not just failures.
      --require-production-os
          Require an attested production OS image
  -h, --help
          Print help

"#]]);
    pap()
        .args(["settings", "set", "-h"])
        .assert()
        .success()
        .stdout_eq(str![[r#"
Change one setting. This may restart the Local API or protection

Usage: private-ai-proxy settings set [OPTIONS] <KEY> [VALUE]

Arguments:
  <KEY>    The setting's dotted path in config.toml, for example web-ui.enabled [possible values: auto-cli-registration, notifications.enabled, notifications.gateway, notifications.local-api, notifications.verification, connect-on-launch, appearance, update-channel, local-api.listen-address, local-api.allow-network-access, local-api.port, local-api.client-host, web-ui.enabled, web-ui.port, web-ui.listen-address, web-ui.allow-network-access, web-ui.client-host, web-ui.password]
  [VALUE]  Boolean keys use true/false; appearance uses system/light/dark; update-channel uses beta/stable. web-ui.password takes no value (use --value-stdin or the hidden prompt); "" removes it

Options:
      --json                   Emit compact JSON instead of human-readable output
      --value-stdin            Read the web-ui.password value from stdin instead of a hidden terminal prompt
      --non-interactive        Never prompt. Mutations require --yes; credential inputs use stdin flags [alias: --no-interactive]
      --yes                    Approve a command's documented mutation without prompting
      --require-production-os  Require an attested production OS image
  -h, --help                   Print help

"#]]);
    // Every command, option and value, by digest.
    let script = pap().args(["completions", "bash"]).output().unwrap().stdout;
    snapbox::assert_data_eq!(
        hex::encode(Sha256::digest(&script)),
        str!["d5034ddec38401e69182b4ba88e26679b35bbfa99719e76b3fc99808e417f47d"]
    );
}

#[test]
fn aci_commands() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let invalid = root.join("invalid.json");
    fs::write(&invalid, b"{").unwrap();
    let invalid = invalid.to_str().unwrap();
    let missing = root.join("missing.json");
    let missing = missing.to_str().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let report = fixtures.join("aci_report_fixture.json");
    let service = serve_json(fs::read(&report).unwrap());
    let tls = service.replace("http:", "https:");
    let report = report.to_str().unwrap();
    let refused = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let refused = format!("https://{refused}");
    let assert = redacting(&[
        ("[ROOT]", root.to_str().unwrap()),
        ("[FIXTURES]", fixtures.to_str().unwrap()),
        ("[REFUSED]", &refused),
        ("[SERVICE]", &service),
        ("[TLS_SERVICE]", &tls),
    ]);
    let pap = || pap().with_assert(assert.clone());
    let fails = |code: i32, args: &[&str]| pap().args(args).assert().code(code).stdout_eq("");
    let pin = "a".repeat(64);
    let kms = "--accept-dstack-kms-root-public-key";
    let kms_root = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    // The fixture report binds this nonce, so its checks reach explain material.
    let nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
    pap()
        .args(["verify", &service, "--nonce", nonce, "--explain", "--json"])
        .assert()
        .code(1)
        .stdout_eq(str![[r#"
{
  "checks": [
    {
      "id": "id-1",
      "section": "9.1(1)",
      "title": "hardware quote verifies to TEE vendor root and binds report_data (dstack tdx policy)",
      "status": "fail",
      "detail": "quote does not parse: Could not decode `Header::user_data`:\n\tNot enough data to fill buffer\n (hardware evidence is required)"
    },
    {
      "id": "id-2",
      "section": "9.1(2)",
      "title": "binding chain: keyset JCS -> digest -> statement for our nonce -> report_data",
      "status": "pass",
      "detail": "keyset digest sha256:e338157d18d77aaa3d1186c9d96147c2d4edc0f1a169ace1243c0bc3ad701fad; statement digest for nonce \"cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d\" matches report_data",
      "explain": "keyset JCS (623 bytes): {\"e2ee_public_keys\":[{\"algo\":\"secp256k1-aes-256-gcm-hkdf-sha256\",\"key_id\":\"static-e2ee-key-secp256k1\",\"public_key\":\"042c0b7cf95324a07d05398b240174dc0c2be444d96b159aa6c7f7b1e668680991ae31a9c671a36543f46cea8fce6984608aa316aa0472a7eed08847440218cb2f\"},{\"algo\":\"x25519-aes-256-gcm-hkdf-sha256\",\"key_id\":\"static-e2ee-x25519-key\",\"public_key\":\"38ab664bd86f77d7e66bdd9ae0792913a94fd8b33a1260027e4b46c1f4884c67\"}],\"not_after\":2000000000,\"receipt_signing_keys\":[{\"algo\":\"ed25519\",\"key_id\":\"static-receipt-ed25519\",\"public_key\":\"34b4d9043156cb6dcf0beb0a2949b7559c940d2bcb6dbe8c53a9b30278e3a746\"}],\"subject\":null,\"tls_public_keys\":[]}\ncomputed digest: sha256:e338157d18d77aaa3d1186c9d96147c2d4edc0f1a169ace1243c0bc3ad701fad\nstatement: {\"keyset_digest\":\"sha256:e338157d18d77aaa3d1186c9d96147c2d4edc0f1a169ace1243c0bc3ad701fad\",\"nonce\":\"cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d\",\"purpose\":\"aci.report_data.v1\"}\ncomputed report_data: 80b6c8449dd6a6d9c3a89d5df05439fa5c2587a98a9dff21a3b68653a64d2b00\nexpected report_data: 80b6c8449dd6a6d9c3a89d5df05439fa5c2587a98a9dff21a3b68653a64d2b00"
    },
    {
      "id": "id-3",
      "section": "9.1(3)",
      "title": "keyset not expired (now < not_after)",
      "status": "pass",
      "detail": "now [NOW] < not_after 2000000000"
    },
    {
      "id": "id-4",
      "section": "9.1(4)",
      "title": "source provenance connects workload to public code (dstack compose policy)",
      "status": "fail",
      "detail": "no measurement backs the provenance (repo=https://github.com/Dstack-TEE/private-ai-gateway commit=deadbeef): no app_compose (spec 9.1(4))"
    },
    {
      "id": "id-5",
      "section": "9.1(5)",
      "title": "private-key custody and subject per policy",
      "status": "skip",
      "detail": "no custody policy configured (--accept-dstack-kms-root-public-key with --accept-subject); subject: null (no policy constraints applied)"
    },
    {
      "id": "id-6",
      "section": "9.1(6)",
      "title": "the channel actually used is bound to the attested keyset (TLS SPKI or E2EE key)",
      "status": "fail",
      "detail": "the channel is not bound to the attested keyset (no TLS handshake observed (plain-HTTP base URL)); spec 1.1"
    }
  ],
  "verdict": {
    "verified": false,
    "passed": 2,
    "failed": 3,
    "skipped": 1,
    "workload_keyset_digest": "sha256:e338157d18d77aaa3d1186c9d96147c2d4edc0f1a169ace1243c0bc3ad701fad"
  }
}

"#]])
        .stderr_eq("");

    fails(1, &["audit", "--report", missing, "--json"]).stderr_eq(str![[r#"
{"error":{"code":"command_failed","message":"failed to read report [ROOT]/missing.json: No such file or directory (os error 2)"}}

"#]]);
    fails(1, &["audit", "--report", invalid]).stderr_eq(str![[r#"
private-ai-proxy: failed to parse report JSON: EOF while parsing an object at line 1 column 1

"#]]);
    fails(1, &["audit", "--report", report, "--receipt", invalid]).stderr_eq(str![[r#"
private-ai-proxy: failed to parse receipt JSON [ROOT]/invalid.json: EOF while parsing an object at line 1 column 1

"#]]);
    fails(2, &["audit", "--report", report, "--pin", "x", "--json"]).stderr_eq(str![[r#"
{"error":{"code":"invalid_arguments","message":"error: invalid value 'x' for '--pin <SESSION_ID>': \"x\" is not a 64-hex session id (spec 5.3)\n\nFor more information, try '--help'.\n"}}

"#]]);
    fails(2, &["audit", "--report", report, "--require-claim", "a=b"]).stderr_eq(str![[r#"
error: invalid value 'a=b' for '--require-claim <NAME[=SOURCE]>': unknown claim source "b" (expected one of: hardware_proven, verifier_derived, provider_asserted, operator_asserted)

For more information, try '--help'.

"#]]);
    fails(
        1,
        &["audit", "--report", report, "--accept-subject", "0xab"],
    )
    .stderr_eq(str![[r#"
private-ai-proxy: invalid --accept-subject "0xab": expected app-id:0x<hex>

"#]]);
    let subject = ["--accept-subject", "app-id:0xab"];
    fails(1, &[&["audit", "--report", report][..], &subject].concat()).stderr_eq(str![[r#"
private-ai-proxy: --accept-subject needs --accept-dstack-kms-root-public-key

"#]]);
    fails(1, &["audit", "--report", report, kms, kms_root]).stderr_eq(str![[r#"
private-ai-proxy: --accept-dstack-kms-root-public-key needs --accept-subject

"#]]);
    let custody = [&["audit", "--report", report][..], &subject, &[kms, "02zz"]].concat();
    fails(1, &custody).stderr_eq(str![[r#"
private-ai-proxy: invalid --accept-dstack-kms-root-public-key: Invalid character 'z' at position 2

"#]]);

    fails(1, &["verify", &format!("{service}/busy")]).stderr_eq(str![[r#"
private-ai-proxy: 127.0.0.1 is unavailable right now (HTTP 503). Try again later.

"#]]);
    fails(1, &["verify", &format!("{service}/private")]).stderr_eq(str![[r#"
private-ai-proxy: 127.0.0.1 answered HTTP 403 instead of an attestation report. Check the service URL.

"#]]);
    fails(1, &["verify", &format!("{service}/missing")]).stderr_eq(str![[r#"
private-ai-proxy: 127.0.0.1 is not a Confidential AI service: it has no attestation report. Check the service URL.

"#]]);
    fails(1, &["verify", &tls]).stderr_eq(str![[r#"
private-ai-proxy: A secure connection to 127.0.0.1 could not be established. Check the service URL.

"#]]);
    fails(1, &["verify", &refused, "--json"]).stderr_eq(str![[r#"
{"error":{"code":"command_failed","message":"127.0.0.1 refused the connection. Check the service URL and port."}}

"#]]);
    fails(1, &["verify", ""]).stderr_eq(str![[r#"
private-ai-proxy: base URL is empty

"#]]);
    fails(1, &["verify", "not a url"]).stderr_eq(str![[r#"
private-ai-proxy: invalid URL "not a url": relative URL without a base

"#]]);

    let send = pap()
        .args(["send", &refused, "--api-key-stdin"])
        .stdin("  \n");
    send.assert().code(1).stdout_eq("").stderr_eq(str![[r#"
private-ai-proxy: Enter an API key

"#]]);
    fails(2, &["send", &refused, "--api-key-stdin", "--api-key", "k"]).stderr_eq(str![[r#"
error: the argument '--api-key-stdin' cannot be used with '--api-key <KEY>'

Usage: private-ai-proxy send --api-key-stdin <BASE_URL>

For more information, try '--help'.

"#]]);
    fails(
        2,
        &["send", &refused, "--allow-unverified", "--session", &pin],
    )
    .stderr_eq(str![[r#"
error: the argument '--allow-unverified' cannot be used with '--session <SESSION_ID>'

Usage: private-ai-proxy send --allow-unverified <BASE_URL>

For more information, try '--help'.

"#]]);
    fails(2, &["send", &refused, "--session", "short"]).stderr_eq(str![[r#"
error: invalid value 'short' for '--session <SESSION_ID>': "short" is not a 64-hex session id (spec 5.3)

For more information, try '--help'.

"#]]);

    // pap's own refusals exit 125, apart from curl's codes.
    fails(125, &["curl", "http://example.com/v1"]).stderr_eq(str![[r#"
private-ai-proxy: request URL must use https so the attested TLS key can be pinned: "http://example.com/v1"

"#]]);
    fails(125, &["curl", "https://user@example.com/v1", "--json"]).stderr_eq(str![[r#"
{"error":{"code":"command_failed","message":"request URL must not contain credentials; pass authentication to curl"}}

"#]]);
    fails(125, &["curl", "https://example.com/v1#frag"]).stderr_eq(str![[r#"
private-ai-proxy: request URL must not contain a fragment

"#]]);
    fails(125, &["curl", "https://example.com/v1", "--", "-L"]).stderr_eq(str![[r#"
private-ai-proxy: curl argument "-L" is not supported by pap curl; use a single URL and supported request options

"#]]);
    fails(125, &["curl", "https://example.com/v1", "--", "--header"]).stderr_eq(str![[r#"
private-ai-proxy: curl option "--header" needs a value

"#]]);
    fails(125, &["curl", &refused]).stderr_eq(str![[r#"
private-ai-proxy: 127.0.0.1 refused the connection. Check the service URL and port.

"#]]);
    fails(2, &["curl"]).stderr_eq(str![[r#"
error: the following required arguments were not provided:
  <URL>

Usage: private-ai-proxy curl <URL> [-- <CURL_ARG>...]

For more information, try '--help'.

"#]]);

    let events = pap().args(["serve", &refused, "--json-events"]).assert();
    events
        .code(1)
        .stdout_eq(str![[r#"
{"type":"fatal","message":"127.0.0.1 refused the connection. Check the service URL and port."}

"#]])
        .stderr_eq("");
    let events = pap().args(["--json", "serve", &refused]).assert();
    events
        .code(1)
        .stdout_eq(str![[r#"
{"type":"fatal","message":"127.0.0.1 refused the connection. Check the service URL and port."}

"#]])
        .stderr_eq("");
    fails(1, &["serve", &refused]).stderr_eq(str![[r#"
private-ai-proxy: 127.0.0.1 refused the connection. Check the service URL and port.

"#]]);
    fails(
        2,
        &["serve", &refused, "--session", &pin, "--allow-unverified"],
    )
    .stderr_eq(str![[r#"
error: the argument '--session <SESSION_ID>' cannot be used with '--allow-unverified'

Usage: private-ai-proxy serve --session <SESSION_ID> <BASE_URL>

For more information, try '--help'.

"#]]);

    fails(2, &["unknown-command"]).stderr_eq(str![[r#"
error: unrecognized subcommand 'unknown-command'

Usage: private-ai-proxy [OPTIONS] <COMMAND>

For more information, try '--help'.

"#]]);
    fails(2, &["--json", "--non-interactive", "unknown-command"]).stderr_eq(str![[r#"
{"error":{"code":"invalid_arguments","message":"error: unrecognized subcommand 'unknown-command'\n\nUsage: private-ai-proxy [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.\n"}}

"#]]);
    fails(2, &["verify", &refused, "--json", "--unknown-option"]).stderr_eq(str![[r#"
{"error":{"code":"invalid_arguments","message":"error: unexpected argument '--unknown-option' found\n\n  tip: to pass '--unknown-option' as a value, use '-- --unknown-option'\n\nUsage: private-ai-proxy verify --json <BASE_URL>\n\nFor more information, try '--help'.\n"}}

"#]]);
}

#[test]
fn management_without_a_backend() {
    let home = tempfile::tempdir().unwrap();
    let pap = || pap().env(desktop_core::paths::HOME_OVERRIDE_ENV, home.path());
    let fails = |code: i32, args: &[&str]| pap().args(args).assert().code(code).stdout_eq("");
    pap()
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout_eq(str![[r#"
{"backend":null,"status":"not_running"}

"#]]);
    fails(1, &["status", "--watch"]).stderr_eq(str![[r#"
private-ai-proxy: Backend is not running. Run private-ai-proxy service start.

"#]]);
    fails(1, &["--json", "profiles", "list"]).stderr_eq(str![[r#"
{"error":{"code":"command_failed","message":"Backend is not running. Run private-ai-proxy service start."}}

"#]]);
    fails(1, &["token", "show"]).stderr_eq(str![[r#"
private-ai-proxy: Confirmation required. Pass --yes for noninteractive changes.

"#]]);
    let add = ["profiles", "add", "--id", "t", "--name", "T", "--url"];
    let consent = [
        &["--json"][..],
        &add,
        &["https://example.com", "--key-stdin"],
    ]
    .concat();
    fails(1, &consent).stderr_eq(str![[r#"
{"error":{"code":"command_failed","message":"Confirmation required. Pass --yes for noninteractive changes."}}

"#]]);
    fails(1, &[&add[..], &["not a url", "--yes"]].concat()).stderr_eq(str![[r#"
private-ai-proxy: Gateway URL must be a valid HTTP or HTTPS URL

"#]]);
    fails(2, &["profiles", "add", "--id", "test"]).stderr_eq(str![[r#"
error: the following required arguments were not provided:
  --name <NAME>
  --url <URL>

Usage: private-ai-proxy profiles add --id <ID> --name <NAME> --url <URL>

For more information, try '--help'.

"#]]);
    let edit = [
        "profiles",
        "edit",
        "w",
        "--allow-development-os",
        "--require-production-os",
    ];
    fails(2, &edit).stderr_eq(str![[r#"
error: the argument '--allow-development-os' cannot be used with '--require-production-os'

Usage: private-ai-proxy profiles edit --allow-development-os <ID>

For more information, try '--help'.

"#]]);
    fails(
        2,
        &["agents", "connect", "codex", "--dry-run", "--revision", "r"],
    )
    .stderr_eq(str![[r#"
error: the argument '--dry-run' cannot be used with '--revision <REVISION>'

Usage: private-ai-proxy agents connect --dry-run <ID>

For more information, try '--help'.

"#]]);
    fails(2, &["start", "--timeout", "0"]).stderr_eq(str![[r#"
error: invalid value '0' for '--timeout <TIMEOUT>': 0 is not in 1..=300

For more information, try '--help'.

"#]]);
    fails(1, &["settings", "set", "appearance"]).stderr_eq(str![[r#"
private-ai-proxy: Missing the value for appearance

"#]]);
    fails(2, &["settings", "set", "no-such-key", "true"]).stderr_eq(str![[r#"
error: invalid value 'no-such-key' for '<KEY>'
  [possible values: auto-cli-registration, notifications.enabled, notifications.gateway, notifications.local-api, notifications.verification, connect-on-launch, appearance, update-channel, local-api.listen-address, local-api.allow-network-access, local-api.port, local-api.client-host, web-ui.enabled, web-ui.port, web-ui.listen-address, web-ui.allow-network-access, web-ui.client-host, web-ui.password]

For more information, try '--help'.

"#]]);
    fails(
        1,
        &["settings", "set", "web-ui.password", "secret", "--yes"],
    )
    .stderr_eq(str![[r#"
private-ai-proxy: Pass the web UI password with --value-stdin or at the hidden prompt, not as an argument.

"#]]);
    // Nothing started a backend or saved a credential, even the refused `profiles add`.
    assert!(fs::read_dir(home.path()).unwrap().next().is_none());
}

#[test]
fn management_with_a_backend() {
    let backend = Backend::start();
    let root = backend.directory.path();
    let backup = root.join("profiles.json");
    let profiles = r#"{"version":1,"profiles":[{"name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com"}]}"#;
    fs::write(&backup, profiles).unwrap();
    let backup = backup.to_str().unwrap();
    let exported = root.join("exported.json");
    let exported = exported.to_str().unwrap();
    backend.run(&["profiles", "import", backup, "--yes"]);
    let profile = backend.run(&["profiles", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let port = backend.run(&["status"])["gateway"]["localApi"]["port"].to_string();
    let assert = redacting(&[
        ("[ROOT]", root.to_str().unwrap()),
        ("[PORT]", &port),
        ("[PROFILE]", &profile),
    ]);
    let pap = |args: &[&str]| Command::from_std(backend.command(args)).with_assert(assert.clone());
    let json = |args: &[&str]| pap(&[args, &["--json"]].concat()).assert().success();
    let fails = |code: i32, args: &[&str]| pap(args).assert().code(code).stdout_eq("");

    json(&["settings", "show"]).stdout_eq(str![[r#"
{"files":{"config":"[ROOT]/home/.private-ai-proxy/Config/config.toml","credentials":"[ROOT]/home/.private-ai-proxy/Config/credentials.toml","error":null,"warnings":[]},"settings":{"active-profile":"[PROFILE]","require-production-os":true,"connect-on-launch":false,"appearance":"system","notifications":{"enabled":true,"gateway":true,"local-api":true,"verification":true},"local-api":{"listen-address":"127.0.0.1","allow-network-access":false,"port":[PORT]},"web-ui":{"enabled":false,"listen-address":"127.0.0.1","allow-network-access":false,"port":4182},"profiles":{"[PROFILE]":{"name":"Work","provider":"phala","remote-url":"https://inference.phala.com"}}},"webUi":{"enabled":false,"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4182}}

"#]]);
    json(&["profiles", "list"]).stdout_eq(str![[r#"
[{"id":"[PROFILE]","name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com","auth":{"kind":"apiKey"},"credentialSaved":false}]

"#]]);
    json(&["profiles", "import", backup, "--yes"]).stdout_eq(str![[r#"
{"imported":0,"skipped":1}

"#]]);
    json(&["profiles", "show", &profile]).stdout_eq(str![[r#"
{"id":"[PROFILE]","name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com","auth":{"kind":"apiKey"},"credentialSaved":false}

"#]]);
    json(&["profiles", "export", "--output", exported]).stdout_eq(str![[r#"
{"exported":"[ROOT]/exported.json"}

"#]]);
    fails(1, &["profiles", "export", "--output", exported]).stderr_eq(str![[r#"
private-ai-proxy: Export target already exists; choose a new path.

"#]]);
    fails(1, &["profiles", "show", "missing"]).stderr_eq(str![[r#"
private-ai-proxy: Profile not found

"#]]);
    fails(1, &["profiles", "use", "missing", "--yes", "--json"]).stderr_eq(str![[r#"
{"error":{"code":"invalid_state","message":"Confidential AI profile not found"}}

"#]]);
    json(&["profiles", "remove", &profile, "--yes"]).stdout_eq(str![[r#"
{"backendInstance":"[..]","sequence":[..],"clientKeyRevision":0,"wakeMonitorAvailable":true,"status":"stopped","configurationVerification":false,"proxyUrl":"http://127.0.0.1:[PORT]","checks":[],"activity":[],"reconnecting":false,"sessionActive":false,"sessionUsage":{"requests":0,"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0,"costUsd":0.0,"protected":0,"blockedLocally":0,"failedProof":0},"usageRevision":0,"config":{"remoteUrl":"https://tee.redpill.ai","requireProductionOs":true},"profiles":[],"activeProfileId":"","localApi":{"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":[PORT]},"apiKeySaved":false,"webUi":{"enabled":false,"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4182},"configFiles":{"configPath":"[ROOT]/home/.private-ai-proxy/Config/config.toml","credentialsPath":"[ROOT]/home/.private-ai-proxy/Config/credentials.toml","warnings":[],"revision":4},"agentsRevision":0,"protection":{"phase":"profileRequired","title":"Not protected","tone":"neutral","action":{"operation":"setUpProfile","label":"Set Up Profile…","enabled":true}}}

"#]]);

    fails(1, &["settings", "set", "appearance", "purple", "--yes"]).stderr_eq(str![[r#"
private-ai-proxy: Expected system, light, or dark

"#]]);
    fails(
        1,
        &["settings", "set", "update-channel", "nightly", "--yes"],
    )
    .stderr_eq(str![[r#"
private-ai-proxy: Expected beta or stable

"#]]);
    fails(1, &["settings", "set", "local-api.port", "port", "--yes"]).stderr_eq(str![[r#"
private-ai-proxy: Expected a valid port number

"#]]);
    fails(
        1,
        &["settings", "set", "connect-on-launch", "maybe", "--yes"],
    )
    .stderr_eq(str![[r#"
private-ai-proxy: Expected true or false

"#]]);
    fails(
        1,
        &["settings", "set", "appearance", "--value-stdin", "--yes"],
    )
    .stderr_eq(str![[r#"
private-ai-proxy: Only web-ui.password reads its value from stdin

"#]]);
    let deprecated = json(&["settings", "set", "autoCliRegistration", "false", "--yes"]);
    deprecated.stdout_eq(str![[r#"
{"require-production-os":true,"connect-on-launch":false,"appearance":"system","auto-cli-registration":false,"notifications":{"enabled":true,"gateway":true,"local-api":true,"verification":true},"local-api":{"listen-address":"127.0.0.1","allow-network-access":false,"port":[PORT]},"web-ui":{"enabled":false,"listen-address":"127.0.0.1","allow-network-access":false,"port":4182}}

"#]]).stderr_eq(str![[r#"
warning: `autoCliRegistration` is deprecated and will be removed in 0.3; use `auto-cli-registration`

"#]]);
    let password = pap(&[
        "settings",
        "set",
        "web-ui.password",
        "--value-stdin",
        "--yes",
        "--json",
    ]);
    password
        .stdin("a new password\r\n")
        .assert()
        .success()
        .stdout_eq(str![[r#"
{"backendInstance":"[..]","sequence":[..],"clientKeyRevision":0,"wakeMonitorAvailable":true,"status":"stopped","configurationVerification":false,"proxyUrl":"http://127.0.0.1:[PORT]","checks":[],"activity":[],"reconnecting":false,"sessionActive":false,"sessionUsage":{"requests":0,"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0,"costUsd":0.0,"protected":0,"blockedLocally":0,"failedProof":0},"usageRevision":0,"config":{"remoteUrl":"https://tee.redpill.ai","requireProductionOs":true},"profiles":[],"activeProfileId":"","localApi":{"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":[PORT]},"apiKeySaved":false,"webUi":{"enabled":false,"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4182},"configFiles":{"configPath":"[ROOT]/home/.private-ai-proxy/Config/config.toml","credentialsPath":"[ROOT]/home/.private-ai-proxy/Config/credentials.toml","warnings":[],"revision":6},"agentsRevision":0,"protection":{"phase":"profileRequired","title":"Not protected","tone":"neutral","action":{"operation":"setUpProfile","label":"Set Up Profile…","enabled":true}}}

"#]]);

    json(&["usage", "list", "--agent", "codex", "--limit", "5"]).stdout_eq(str![[r#"
{"items":[],"nextCursor":null,"summary":{"requests":0,"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0,"costUsd":0.0,"protected":0,"blockedLocally":0,"failedProof":0},"series":[],"modelSeries":[],"agents":[],"models":[]}

"#]]);
    fails(1, &["usage", "show", "missing", "--receipt", "--json"]).stderr_eq(str![[r#"
{"error":{"code":"not_found","message":"Usage record not found"}}

"#]]);
    fails(1, &["usage", "clear"]).stderr_eq(str![[r#"
private-ai-proxy: Confirmation required. Pass --yes for noninteractive changes.

"#]]);
    json(&["usage", "clear", "--yes"]).stdout_eq(str![[r#"
{"deleted":0}

"#]]);
    fails(1, &["models", "list", "--json"]).stderr_eq(str![[r#"
{"error":{"code":"command_failed","message":"No verified model catalog. Start protection and wait for verification first."}}

"#]]);
    json(&["token", "rotate", "--yes"]).stdout_eq(str![[r#"
{"rotated":true}

"#]]);
    fails(1, &["token", "clear-credential", "--yes"]).stderr_eq(str![[r#"
private-ai-proxy: The operation could not complete. Check the protection status and supplied configuration before retrying.

"#]]);
    json(&["web-ui", "password", "rotate", "--yes"]).stdout_eq(str![[r#"
{"rotated":true}

"#]]);
    let stale = [
        "agents",
        "disconnect",
        "codex",
        "--revision",
        "x",
        "--yes",
        "--json",
    ];
    fails(1, &stale).stderr_eq(str![[r#"
{"error":{"code":"revision_conflict","message":"The agent settings changed while applying this connection. Try again."}}

"#]]);
    fails(1, &["agents", "connect", "no-such-agent", "--dry-run"]).stderr_eq(str![[r#"
private-ai-proxy: The operation could not complete. Check the protection status and supplied configuration before retrying.

"#]]);
    fails(1, &["stop", "--offline"]).stderr_eq(str![[r#"
private-ai-proxy: The backend is running; restore the agents through it with `stop` or `service stop`.

"#]]);
    fails(1, &["service", "stop"]).stderr_eq(str![[r#"
private-ai-proxy: Confirmation required. Pass --yes for noninteractive changes.

"#]]);
    json(&["service", "stop", "--yes"]).stdout_eq(str![[r#"
{"status":"stopped"}

"#]]);
    json(&["service", "status"]).stdout_eq(str![[r#"
{"backend":null,"status":"not_running"}

"#]]);
}
