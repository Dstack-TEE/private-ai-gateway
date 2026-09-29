//! Every human rendering over representative backend values, as a golden
//! file beside the command-line goldens in `tests/golden`. Fixed messages
//! the backend golden already shows are left out.

use crate::{Cli, Command};
use clap::Parser;

/// A backend state with every field the status summary reads, plus fields it
/// must never print (`activity`, `token`).
const GATEWAY: &str = r#"{
    "status": "verified", "activeProfileId": "work", "apiKeySaved": true,
    "profiles": [{"id": "work", "name": "Work", "provider": "redpill", "remoteUrl": "https://tee.redpill.ai"}],
    "proxyUrl": "http://127.0.0.1:4180", "localApi": {"allowNetworkAccess": true},
    "webUi": {"enabled": true, "url": "http://127.0.0.1:4182", "allowNetworkAccess": false},
    "configFiles": {"error": "bad\u001b[31m\nvalue", "warnings": ["unknown key `x`"]},
    "config": {"requireProductionOs": false},
    "identity": {"teeType": "tdx", "trustLevel": "hardware_verified"},
    "checks": [{"title": "Quote", "status": "pass"}, {"title": "Binding", "status": "fail"},
        {"title": "Custody", "status": "skip"}, {"title": "Channel", "status": "fail"}],
    "catalog": {"models": [{"id": "a"}, {"id": "b"}]}, "sessionId": "session-1",
    "sessionUsage": {"requests": 12, "protected": 10, "blockedLocally": 1, "failedProof": 1,
        "inputTokens": 100, "outputTokens": 20, "cacheReadTokens": 5, "cacheWriteTokens": 0,
        "costUsd": 0.0012},
    "progress": "Checking", "error": "", "endpointError": "Port in use",
    "wakeMonitorAvailable": false, "activity": [{"detail": "never shown"}], "token": "never shown"
}"#;

const CASES: &[(&str, &str)] = &[
    (
        "status",
        r#"{"status": "verified", "backendConnected": false}"#,
    ),
    (
        "status",
        r#"{"status": "verified", "configurationVerification": true}"#,
    ),
    (
        "status",
        r#"{"status": "verified", "reconnecting": true, "webUi": {"enabled": true, "error": "In use"}}"#,
    ),
    (
        "status",
        r#"{"status": "stopped", "activeProfileId": "gone", "catalog": {"models": []}, "config": {"requireProductionOs": true}}"#,
    ),
    (
        "status",
        r#"{"status": "blocked", "profiles": [], "error": "bad\u001b[31m\nline"}"#,
    ),
    ("status", r#"{"status": "error"}"#),
    ("status", r#"{"status": "verifying"}"#),
    (
        "stop --offline",
        r#"{"id": "codex", "name": "Codex", "installed": true}"#,
    ),
    (
        "app open",
        r#"{"url": "http://127.0.0.1:4182", "browserOpened": true}"#,
    ),
    (
        "app open --web",
        r#"{"url": "http://127.0.0.1:4182", "browserOpened": false}"#,
    ),
    ("app open", r#"{"opened": true}"#),
    (
        "profiles list",
        r#"[{"id": "work", "name": "Work", "provider": "redpill", "remoteUrl": null}]"#,
    ),
    (
        "profiles show work",
        r#"{"\u001bid": "work\u202e", "auth": {"kind": "apiKey"}, "tags": []}"#,
    ),
    ("profiles use work", r#"{"status": "verified"}"#),
    ("profiles login work", r#"{"status": "verified"}"#),
    (
        "profiles add --id w --name W --url https://x",
        r#"{"status": "verified"}"#,
    ),
    (
        "agents list",
        r#"[
        {"id": "codex", "name": "Codex", "installed": true, "connected": true, "authorized": true},
        {"id": "claude", "name": "Claude", "installed": true, "connected": true,
            "attention": "Restart it", "error": "changed\u001b"},
        {"id": "saved", "name": "Saved", "installed": true, "recorded": true},
        {"id": "gone", "name": "Gone", "installed": false},
        {"id": "idle", "name": "Idle", "installed": true}]"#,
    ),
    ("agents list", "[]"),
    (
        "agents connect codex --dry-run",
        r#"{"revision": "r1", "changes": [{"path": "config.toml"}]}"#,
    ),
    (
        "models list",
        r#"{"models": [{"id": "m1", "name": "Model One"}, {"id": "m2"}]}"#,
    ),
    ("models list", r#"{"models": []}"#),
    (
        "usage list",
        r#"{"nextCursor": "next\u0007", "items": [
        {"id": "a", "model": "m", "status": 200, "leftDevice": true, "verified": true, "inputTokens": 1, "outputTokens": 2},
        {"id": "b", "model": "m", "status": 200, "leftDevice": true, "verified": false},
        {"id": "c", "status": 0, "leftDevice": false, "verified": null},
        {"id": "d", "status": 500, "leftDevice": true}]}"#,
    ),
    ("usage clear", r#"{"deleted": 7}"#),
    ("settings reset", "{}"),
    ("token show", r#"{"token": "sk-pap-example"}"#),
    ("token clear-credential", "{}"),
    ("web-ui password show", r#"{"password": "pass word"}"#),
];

#[test]
fn human_output() {
    let full =
        format!(r#"{{"backend": {{"processId": 42, "version": "0.2.1"}}, "gateway": {GATEWAY}}}"#);
    let mut transcript = String::new();
    for (args, value) in [("status", full.as_str())]
        .into_iter()
        .chain(CASES.iter().copied())
    {
        let cli = Cli::try_parse_from(std::iter::once("pap").chain(args.split(' '))).unwrap();
        let Command::Manage(action) = cli.command else {
            panic!("{args} is not a management command");
        };
        let text = super::render(&action, &serde_json::from_str(value).unwrap());
        transcript.push_str(&format!("$ pap {args}\n{text}\n\n"));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/human.txt");
    snapbox::Assert::new()
        .action_env(snapbox::assert::DEFAULT_ACTION_ENV)
        .eq(transcript, snapbox::Data::read_from(&path, None).raw());
}
