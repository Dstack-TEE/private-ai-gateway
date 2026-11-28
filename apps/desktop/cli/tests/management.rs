//! Real binaries and isolated user state; no UI, provider calls, or OS secrets.
mod support;

use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Output, Stdio},
    time::{Duration, Instant},
};
use support::{assert_success, Backend};

#[test]
#[ignore = "subprocess fixture that tears down a test sandbox"]
fn sandbox_teardown_watchdog() {
    support::watchdog();
}

#[cfg(target_os = "linux")]
#[test]
fn start_reports_skipped_or_failed_supervision_without_failing_protection() {
    for (json, environment_override, reports_paths) in [
        (false, true, false),
        (true, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let sandbox = support::Sandbox::new();
        let root = sandbox.path();
        let home = root.join("home");
        let bin = root.join("bin");
        let data = home
            .join(".local/share")
            .join(desktop_core::brand::APP_IDENTIFIER);
        let runtime = root.join("runtime");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let socket_path = desktop_core::transport::endpoint_path_for(&data, &runtime).unwrap();
        let socket_dir = socket_path.parent().unwrap();
        fs::create_dir_all(socket_dir).unwrap();
        fs::set_permissions(socket_dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let cli = root.join("private-ai-proxy");
        support::install(env!("CARGO_BIN_EXE_private-ai-proxy"), &cli);
        std::os::unix::fs::symlink(&cli, bin.join("pap")).unwrap();
        std::os::unix::fs::symlink(&cli, bin.join("private-ai-proxy")).unwrap();
        fs::write(
            bin.join("systemctl"),
            "#!/bin/sh\n[ \"$1\" = --user ] && [ \"$2\" = show-environment ]\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(bin.join("systemctl"), fs::Permissions::from_mode(0o700)).unwrap();

        let version_metadata = if reports_paths {
            json!({"agentPathOverrides": {}, "agentValidationOverrides": [], "supervised": true})
        } else {
            json!({})
        };
        let check_legacy_connect = json && environment_override && !reports_paths;
        let expected_requests = if reports_paths {
            18
        } else if check_legacy_connect {
            14
        } else {
            8
        };
        let applied = json!({
            "result": {
                "id": "codex", "name": "Codex",
                "configPath": home.join(".codex/config.toml"),
                "installed": true, "recorded": true, "connected": true,
                "authorized": true,
            }
        });
        let agents = json!({
            "result": [&applied["result"], &json!({
                "id": "oh-my-pi", "name": "Oh My Pi",
                "configPath": home.join(".omp/agent/models.yml"),
                "installed": true, "recorded": true, "connected": true, "authorized": true,
            })]
        })
        .to_string();
        let applied = applied.to_string();
        let server = verified_backend_fixture(
            socket_path,
            &cli,
            expected_requests,
            version_metadata,
            &[
                ("/api/rpc/apply_agent", applied),
                ("/api/rpc/list_agents", agents),
            ],
        );

        let custom = home.join("custom");
        let run = |args: &[&str]| {
            let mut command = std::process::Command::new(&cli);
            command
                .args(args)
                .env_clear()
                .env("HOME", &home)
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("PATH", &bin);
            if environment_override {
                command
                    .env("CODEX_HOME", &custom)
                    .env("OPENCODE_CONFIG_CONTENT", "{}");
            }
            if reports_paths {
                command.env("OMP_PROFILE", "work");
            }
            command.output().unwrap()
        };
        let args: &[&str] = if json {
            &["start", "--json"]
        } else {
            &["start"]
        };
        let first = run(args);
        assert_eq!(first.status.code(), Some(0), "{first:?}");
        let first_stderr = String::from_utf8_lossy(&first.stderr);
        if json {
            let value: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
            assert_eq!(value["status"], "verified");
            let notice = value["supervisionNotice"].as_str().unwrap();
            if environment_override {
                assert!(notice.contains("OPENCODE_CONFIG_CONTENT"), "{notice}");
            } else {
                assert!(
                    notice.contains("Could not enable the login service"),
                    "{notice}"
                );
            }
            assert!(first_stderr.is_empty(), "{first_stderr}");
            if reports_paths {
                let warning = value["agentPathWarnings"][0].as_str().unwrap();
                assert!(warning.contains("CODEX_HOME"), "{warning}");
                assert!(
                    warning.contains(custom.join("config.toml").to_str().unwrap()),
                    "{warning}"
                );
                assert!(warning.contains("environment.d"), "{warning}");
                assert!(value["agentPathWarnings"][1]
                    .as_str()
                    .unwrap()
                    .contains("OMP_PROFILE"));
            }
        } else {
            assert_eq!(
                String::from_utf8_lossy(&first.stdout),
                concat!(
                    "Protected\n",
                    "Profile: Test (test)\n",
                    "Service: custom | http://127.0.0.1\n",
                    "Service credential: Saved (credentials.toml)\n",
                    "Local API: Not listening\n",
                    "Network access: Loopback only\n",
                    "Models: No verified catalog\n",
                )
            );
            assert!(
                first_stderr.contains("OPENCODE_CONFIG_CONTENT"),
                "{first_stderr}"
            );
            assert!(
                first_stderr.contains("environment first, then run `pap service enable`"),
                "{first_stderr}"
            );
        }
        let second = run(args);
        assert_eq!(second.status.code(), Some(0), "{second:?}");
        if json {
            let value: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
            assert_eq!(
                value.get("supervisionNotice").is_none(),
                environment_override
            );
        } else {
            assert!(!String::from_utf8_lossy(&second.stderr).contains("OPENCODE_CONFIG_CONTENT"));
        }
        if check_legacy_connect {
            let outcome = run(&[
                "agents",
                "connect",
                "codex",
                "--revision",
                "test-preview",
                "--yes",
                "--json",
            ]);
            assert_success(&outcome);
            let report: Value = serde_json::from_slice(&outcome.stdout).unwrap();
            assert_eq!(report["connected"], true);
            let warning = String::from_utf8_lossy(&outcome.stderr);
            assert_eq!(warning.lines().count(), 1);
            assert!(
                warning.contains("does not report all agent overrides"),
                "{warning}"
            );
        }
        if reports_paths {
            let outcome = run(&["status", "--json"]);
            assert_success(&outcome);
            let report: Value = serde_json::from_slice(&outcome.stdout).unwrap();
            let warning = report["agentPathWarnings"][0].as_str().unwrap();
            assert!(
                warning.contains(custom.join("config.toml").to_str().unwrap()),
                "{warning}"
            );
            assert!(
                warning.contains(home.join(".codex/config.toml").to_str().unwrap()),
                "{warning}"
            );
            assert!(report["agentPathWarnings"][1]
                .as_str()
                .unwrap()
                .contains("OMP_PROFILE"));
        }
        server.join().unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn package_uninstall_preserves_the_login_preference_and_allows_enrollment_again() {
    use std::{os::unix::fs::PermissionsExt, process::Command};

    let sandbox = support::Sandbox::new();
    let root = sandbox.path();
    let home = root.join("home");
    let bin = root.join("bin");
    let runtime = root.join("runtime");
    let cli = root.join("private-ai-proxy");
    let backend = root.join("private-ai-proxy-service");
    let data = home
        .join(".local/share")
        .join(desktop_core::brand::APP_IDENTIFIER);
    let marker = data.join("login-service-off");
    let unit = home.join(".config/systemd/user/private-ai-proxy.service");
    for directory in [&bin, &runtime, &data] {
        fs::create_dir_all(directory).unwrap();
    }
    let config = home.join(".config/private-ai-proxy");
    fs::create_dir_all(&config).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    fs::write(
        config.join("config.toml"),
        format!("[local-api]\nport = {port}\n"),
    )
    .unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    support::install(env!("CARGO_BIN_EXE_private-ai-proxy"), &cli);
    support::install(env!("CARGO_BIN_EXE_private-ai-proxy-service"), &backend);
    std::os::unix::fs::symlink(&cli, bin.join("private-ai-proxy")).unwrap();
    let systemctl = r#"#!/bin/sh
set -eu
[ "$1" = --user ]
unit="$HOME/.config/systemd/user/private-ai-proxy.service"
case "$2" in
  show-environment|daemon-reload) ;;
  show)
    if [ -f "$unit" ]; then
      state=disabled
      [ ! -f "$HOME/enabled" ] || state=enabled
      pid=0
      active=inactive
      if [ -f "$HOME/managed-pid" ]; then
        read -r pid <"$HOME/managed-pid"
        active=active
      fi
      printf 'LoadState=loaded\nFragmentPath=%s\nDropInPaths=\nNeedDaemonReload=no\nUnitFileState=%s\nActiveState=%s\nMainPID=%s\n' "$unit" "$state" "$active" "$pid"
    else
      printf 'LoadState=not-found\nDropInPaths=\n'
    fi ;;
  enable) : > "$HOME/enabled" ;;
  start)
    "$PAP_FIXTURE_BACKEND" --supervised </dev/null >"$HOME/backend.log" 2>&1 &
    printf '%s\n' "$!" >"$HOME/managed-pid" ;;
  disable) /bin/rm -f "$HOME/enabled" "$HOME/managed-pid" ;;
  *) exit 2 ;;
esac
"#;
    let busctl = r#"#!/bin/sh
printf '%s\n' '{"data":"on-failure"}' '{"data":[[75],[]]}' '{"data":[[],[]]}' '{"data":[[],[]]}'
"#;
    for (name, script) in [("systemctl", systemctl), ("busctl", busctl)] {
        let path = bin.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let run = |args: &[&str]| {
        let output = Command::new(&cli)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &bin)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("PAP_FIXTURE_BACKEND", &backend)
            .args(args)
            .output()
            .unwrap();
        assert_success(&output);
    };
    let uninstall = ["--yes", "service", "disable", "--for-uninstall"];
    run(&["service", "enable"]);
    run(&["--yes", "service", "disable"]);
    assert!(marker.is_file());
    run(&uninstall);
    assert!(marker.is_file());

    run(&["service", "enable"]);
    assert!(unit.is_file());
    assert!(!marker.exists());
    run(&uninstall);
    assert!(!unit.exists());
    assert!(!home.join("enabled").exists());
    assert!(!marker.exists());

    let socket_path = desktop_core::transport::endpoint_path_for(&data, &runtime).unwrap();
    let socket_dir = socket_path.parent().unwrap();
    fs::create_dir_all(socket_dir).unwrap();
    fs::set_permissions(socket_dir, fs::Permissions::from_mode(0o700)).unwrap();
    // Exercise re-enrollment with a verified backend and no live provider.
    fs::write(
        home.join("managed-pid"),
        format!("{}\n", std::process::id()),
    )
    .unwrap();
    let server = verified_backend_fixture(socket_path, &backend, 5, json!({}), &[]);
    run(&["start"]);
    assert!(unit.is_file());
    assert!(home.join("enabled").exists());
    server.join().unwrap();
}

#[cfg(target_os = "linux")]
fn verified_backend_fixture(
    socket_path: std::path::PathBuf,
    cli: &std::path::Path,
    expected_requests: usize,
    version_metadata: Value,
    additional_responses: &[(&'static str, String)],
) -> std::thread::JoinHandle<()> {
    let state = desktop_core::contracts::AppState {
        backend_connected: Some(true),
        status: desktop_core::contracts::VerificationStatus::Verified,
        config: desktop_core::contracts::StartConfig {
            remote_url: "http://127.0.0.1".into(),
        },
        profiles: vec![desktop_core::contracts::ConfidentialProfile {
            id: "test".into(),
            name: "Test".into(),
            provider: desktop_core::contracts::ServiceProvider::Custom,
            remote_url: "http://127.0.0.1".into(),
            auth: desktop_core::contracts::ProfileAuth::ApiKey,
            credential_saved: true,
            credential_ref: None,
            verified_at: None,
        }],
        active_profile_id: "test".into(),
        api_key_saved: true,
        ..Default::default()
    };
    let state = serde_json::to_string(&desktop_core::contracts::AppStateWire::from(state)).unwrap();
    let mut version = json!({
        "apiVersion": desktop_core::protocol::API_VERSION,
        "product": desktop_core::brand::APP_IDENTIFIER,
        "version": desktop_core::protocol::BUILD_VERSION,
        "instanceId": "test-instance",
        "processId": std::process::id(),
        "executable": cli,
    });
    let mut extra = version_metadata;
    version
        .as_object_mut()
        .unwrap()
        .append(extra.as_object_mut().unwrap());
    let version = version.to_string();
    let state = format!("{{\"result\":{state}}}");
    let mut responses = additional_responses.to_vec();
    responses.extend([
        (desktop_core::protocol::VERSION_PATH, version),
        ("/api/rpc/get_state", state),
    ]);
    let listener = std::os::unix::net::UnixListener::bind(socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut requests = 0;
        while requests < expected_requests && Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            loop {
                let mut request = Vec::new();
                let mut byte = [0; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                if request.is_empty() || !request.ends_with(b"\r\n\r\n") {
                    break;
                }
                let request = String::from_utf8_lossy(&request);
                let body_length = request
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then_some(value.trim())
                    })
                    .and_then(|length| length.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0; body_length];
                stream.read_exact(&mut body).unwrap();
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let response_body = responses
                    .iter()
                    .find_map(|(route, response)| (*route == path).then_some(response))
                    .unwrap_or_else(|| panic!("unexpected request {path}"));
                let body = response_body.as_bytes();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
                requests += 1;
                if requests >= expected_requests {
                    break;
                }
            }
        }
        assert_eq!(requests, expected_requests);
    })
}

#[cfg(windows)]
#[test]
fn service_disable_succeeds_without_a_windows_backend_login_service() {
    let backend = Backend::start();
    assert_eq!(
        backend.run(&["service", "disable", "--yes"]),
        json!({"startAtLogin": false, "supported": false})
    );
    assert!(backend.run(&["service", "status"])["backend"].is_object());
}

#[test]
fn agents_connect_starts_protection_only_with_consent_and_outside_dry_run() {
    let backend = Backend::start();
    let backup = backend.directory.path().join("profiles-connect.json");
    fs::write(&backup, r#"{"version":1,"profiles":[{"name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com"}]}"#).unwrap();
    backend.run(&["profiles", "import", backup.to_str().unwrap(), "--yes"]);
    backend.run(&["agents", "connect", "codex", "--dry-run"]);
    assert_eq!(backend.run(&["status"])["gateway"]["sessionActive"], false);
    backend.run(&["service", "stop", "--yes"]);

    for args in [&["--dry-run", "--yes"][..], &["--non-interactive"][..]] {
        let refused = backend
            .command(&["agents", "connect", "codex", "--json"])
            .args(args)
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(backend.run(&["status"])["status"], "not_running");
    }

    let failed = backend
        .command(&["agents", "connect", "codex", "--yes", "--json"])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Run pap profiles verify "));
    assert!(backend
        .run(&["agents", "list"])
        .as_array()
        .unwrap()
        .iter()
        .all(|agent| agent["recorded"] == false));
}

#[cfg(target_os = "linux")]
#[test]
fn listing_and_preview_show_the_client_config_path_under_supervision() {
    let backend = Backend::start_in("", true);
    let custom = backend.directory.path().join("custom-codex");
    let native = "model = 'native'\n";
    fs::create_dir_all(&custom).unwrap();
    let config = custom.join("config.toml");
    fs::write(&config, native).unwrap();
    assert_eq!(backend.run(&["status"])["backend"]["supervised"], true);

    let list = backend
        .command(&["agents", "list", "--json"])
        .env("CODEX_HOME", &custom)
        .output()
        .unwrap();
    assert_success(&list);
    let statuses: Vec<Value> = serde_json::from_slice(&list.stdout).unwrap();
    let codex = statuses
        .iter()
        .find(|agent| agent["id"] == "codex")
        .unwrap();
    assert_eq!(codex["installed"], true);
    assert_eq!(codex["configPath"], config.to_str().unwrap());
    assert_eq!(codex["configPathSource"], "CODEX_HOME");

    let preview = backend
        .command(&["agents", "connect", "codex", "--dry-run", "--json"])
        .env("CODEX_HOME", &custom)
        .output()
        .unwrap();
    assert_success(&preview);
    let report: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["agent"]["configPath"], config.to_str().unwrap());
    assert_eq!(report["agent"]["configPathSource"], "CODEX_HOME");
    assert_eq!(fs::read_to_string(&config).unwrap(), native);
    assert_eq!(backend.run(&["status"])["gateway"]["sessionActive"], false);
}

#[test]
fn agents_connect_rejects_shell_only_inline_config() {
    let backend = Backend::start();
    let output = backend
        .command(&["agents", "connect", "opencode", "--yes"])
        .env("OPENCODE_CONFIG_CONTENT", "private-inline-config")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("OPENCODE_CONFIG_CONTENT is set in one environment only"),
        "{error}"
    );
    assert!(!error.contains("private-inline-config"));
    assert!(error.contains("pap service restart"), "{error}");
    assert_eq!(backend.run(&["status"])["gateway"]["sessionActive"], false);
}

#[cfg(unix)]
#[test]
fn supervised_sigterm_preserves_the_session_and_service_stop_restores_agents() {
    assert_supervised_stop(false);
}

#[cfg(unix)]
#[test]
fn service_stop_after_supervised_sigterm_restores_agents_offline() {
    assert_supervised_stop(true);
}

#[cfg(unix)]
#[test]
fn detached_sigterm_ends_the_session_and_restores_agents() {
    assert_signal_shutdown(false, false);
}

#[cfg(unix)]
#[test]
fn replacement_after_disabling_login_stays_unprotected_despite_protect_on_launch() {
    let backend = Backend::start_with_credentials("[profiles.test]\napi-key = \"test-key\"\n");
    backend.run(&["service", "stop", "--yes"]);
    let home = backend.directory.path().join("home");
    let config = home.join(".private-ai-proxy/Config/config.toml");
    let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
    fs::write(
        &config,
        format!(
            "active-profile = \"test\"\nconnect-on-launch = true\n{}\n[profiles.test]\nname = \"Test\"\nprovider = \"custom\"\nremote-url = \"https://{}\"\n",
            fs::read_to_string(&config).unwrap(),
            gateway.local_addr().unwrap(),
        ),
    )
    .unwrap();
    let mut replacement =
        std::process::Command::new(backend.cli().with_file_name("private-ai-proxy-service"))
            .arg("--skip-protect-on-launch")
            .env(desktop_core::paths::HOME_OVERRIDE_ENV, &home)
            .spawn()
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if backend.run(&["status"])["backend"].is_object() {
            break;
        }
        assert!(replacement.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(backend.run(&["status"])["gateway"]["sessionActive"], false);
    assert_eq!(
        backend.run(&["settings", "show"])["settings"]["connect-on-launch"],
        true
    );
    backend.run(&["service", "stop", "--yes"]);
    assert!(replacement.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture running the desktop client's background follower"]
fn desktop_follow_fixture() {
    let ready = std::env::var_os("PAP_TEST_FOLLOW_READY").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = desktop_core::client::Client::attach(runtime.handle().clone());
    let mut states = client.subscribe();
    runtime.block_on(async move {
        let mut previous = None;
        loop {
            let state = states
                .wait_for(|state| {
                    state.backend_instance.is_some()
                        && state.backend_connected != Some(false)
                        && state.backend_instance != previous
                })
                .await
                .unwrap();
            previous.clone_from(&state.backend_instance);
            desktop_core::private_fs::write_private_atomic(
                std::path::Path::new(&ready),
                previous.as_deref().unwrap(),
                None,
            )
            .unwrap();
        }
    });
}

#[cfg(unix)]
#[test]
fn desktop_follower_recovers_crashes_respects_the_gate_and_leaves_expected_stops_down() {
    assert_desktop_follower(FollowerScenario::Normal);
}

#[cfg(unix)]
#[test]
fn desktop_follower_leaves_an_orderly_stop_down_when_event_streams_are_full() {
    assert_desktop_follower(FollowerScenario::FullStreams);
}

#[cfg(unix)]
#[test]
fn desktop_follower_recovers_replacements_that_crash_before_their_first_event() {
    assert_desktop_follower(FollowerScenario::EarlyCrashes);
}

#[cfg(unix)]
#[test]
fn desktop_follower_leaves_a_replacement_stopped_before_its_first_event() {
    assert_desktop_follower(FollowerScenario::EarlyStop);
}

#[cfg(unix)]
#[derive(PartialEq, Eq)]
enum FollowerScenario {
    Normal,
    FullStreams,
    EarlyCrashes,
    EarlyStop,
}

#[cfg(unix)]
fn assert_desktop_follower(scenario: FollowerScenario) {
    let backend = Backend::start();
    let _streams =
        (scenario == FollowerScenario::FullStreams).then(|| saturate_backend_events(&backend));
    let sandbox = backend.directory.path();
    let ready = sandbox.join("follower.ready");
    let fixture = sandbox.join("desktop-follower");
    let home = sandbox.join("home");
    let shell = sandbox.join("shell");
    let shell_started = sandbox.join("shell-started");
    fs::write(
        &shell,
        "#!/bin/sh\nprintf started > \"$PAP_TEST_SHELL_STARTED\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    support::install(std::env::current_exe().unwrap().to_str().unwrap(), &fixture);
    let mut follower = std::process::Command::new(fixture)
        .args([
            "--exact",
            "desktop_follow_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("PAP_TEST_FOLLOW_READY", &ready)
        .env("SHELL", &shell)
        .env("PAP_TEST_SHELL_STARTED", &shell_started)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env(desktop_core::paths::HOME_OVERRIDE_ENV, &home)
        .spawn()
        .unwrap();
    let check = std::panic::catch_unwind(|| {
        let wait = |previous: &str| {
            let started = Instant::now();
            loop {
                if let Ok(instance) = fs::read_to_string(&ready) {
                    if !instance.is_empty() && instance != previous {
                        assert!(
                            !shell_started.exists(),
                            "Backend attachment ran the login shell"
                        );
                        return instance;
                    }
                }
                assert!(
                    started.elapsed() < Duration::from_secs(8),
                    "The desktop never attached to a new backend"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let kill = || {
            let version = backend.run(&["status"]);
            let id = version["backend"]["processId"].as_u64().unwrap();
            let pid = rustix::process::Pid::from_raw(i32::try_from(id).unwrap()).unwrap();
            rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
            desktop_core::launch::wait_for_exit(u32::try_from(id).unwrap(), Duration::from_secs(5))
                .unwrap();
        };
        let assert_down = || {
            std::thread::sleep(Duration::from_millis(1250));
            assert_eq!(backend.run(&["service", "status"])["status"], "not_running");
        };
        if scenario == FollowerScenario::FullStreams {
            wait("");
            backend.run(&["service", "stop", "--yes"]);
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                assert_eq!(backend.run(&["service", "status"])["status"], "not_running");
                std::thread::sleep(Duration::from_millis(100));
            }
            return;
        }
        let data = home.join(".private-ai-proxy");
        if matches!(
            scenario,
            FollowerScenario::EarlyCrashes | FollowerScenario::EarlyStop
        ) {
            let original = wait("");
            let mut previous = original.clone();
            kill();
            for _ in 0..4 {
                let deadline = Instant::now() + Duration::from_secs(5);
                let gate = loop {
                    let state = backend.run(&["service", "status"]);
                    if let Some(instance) = state["backend"]["instanceId"].as_str() {
                        if instance != previous {
                            // Recovery holds this gate until its own version
                            // handshake succeeds. Keep it held through the exit.
                            if let Some(gate) = desktop_core::lock::startup(&data).unwrap() {
                                previous = instance.to_owned();
                                break gate;
                            }
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "The follower did not start another replacement"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                };
                if scenario == FollowerScenario::EarlyStop {
                    backend.run(&["service", "stop", "--yes"]);
                } else {
                    kill();
                }
                assert_eq!(
                    fs::read_to_string(&ready).unwrap(),
                    original,
                    "The follower received a replacement's state before it exited"
                );
                drop(gate);
                if scenario == FollowerScenario::EarlyStop {
                    break;
                }
            }
            assert_down();
            return;
        }
        let gate = desktop_core::lock::startup(&data).unwrap().unwrap();
        let original = wait("");
        kill();
        assert_down();
        drop(gate);
        let restarted = wait(&original);
        std::thread::sleep(Duration::from_millis(200));
        backend.run(&["service", "stop", "--yes"]);
        assert_down();
        backend.run(&["service", "start"]);
        let mut last = wait(&restarted);
        for _ in 0..3 {
            kill();
            last = wait(&last);
        }
        kill();
        assert_down();
    });
    let _ = follower.kill();
    follower.wait().unwrap();
    if let Err(error) = check {
        std::panic::resume_unwind(error);
    }
}

#[cfg(unix)]
fn saturate_backend_events(
    backend: &Backend,
) -> Vec<std::io::BufReader<std::os::unix::net::UnixStream>> {
    use std::{io::BufRead, os::unix::net::UnixStream};

    let doctor = backend.command(&["doctor", "--json"]).output().unwrap();
    let diagnostic: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let endpoint = diagnostic["endpoint"].as_str().unwrap();
    let open = |status: &str| {
        let mut stream = UnixStream::connect(endpoint).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /api/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with(status), "{line}");
        reader
    };
    let streams = (0..32).map(|_| open("HTTP/1.1 200 ")).collect();
    drop(open("HTTP/1.1 503 "));
    streams
}

#[cfg(unix)]
fn assert_supervised_stop(offline_stop: bool) {
    assert_signal_shutdown(true, offline_stop);
}

#[cfg(unix)]
fn assert_signal_shutdown(supervised: bool, offline_stop: bool) {
    use agent_bridge::{agents::Projector, catalog::Catalog};
    use desktop_core::agents::{Agent, ConnectOptions};
    use desktop_runtime::{local_state::LocalState, usage::UsageStore};
    use std::sync::Arc;

    let backend = Backend::start_with_credentials("[profiles.test]\napi-key = \"test-key\"\n");
    backend.run(&["service", "stop", "--yes"]);
    let home = backend.directory.path().join("home");
    let data = home.join(".private-ai-proxy");
    let config = data.join("Config/config.toml");
    let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("https://{}", gateway.local_addr().unwrap());
    fs::write(
        &config,
        format!(
            "active-profile = \"test\"\n{}\n[profiles.test]\nname = \"Test\"\nprovider = \"custom\"\nremote-url = \"{url}\"\n",
            fs::read_to_string(&config).unwrap(),
        ),
    )
    .unwrap();
    // A previous verified run left a session and an agent projection. The
    // loopback listener observes re-verification without contacting a gateway.
    let usage = UsageStore::open(data.join("usage.sqlite3")).unwrap();
    usage.save_active_session("saved-session", 1).unwrap();
    let agent = home.join(".claude/settings.json");
    fs::create_dir_all(agent.parent().unwrap()).unwrap();
    let original = json!({"env": {"ANTHROPIC_API_KEY": "original-key"}});
    fs::write(&agent, original.to_string()).unwrap();
    let projector = Projector::new_for_home(
        home.clone(),
        data.clone(),
        backend.directory.path().join("private-ai-proxy-helper"),
        "http://127.0.0.1:4180",
        Arc::new(LocalState::open(&data)),
    )
    .unwrap();
    let catalog = Catalog::from_remote(&json!({"data":[{"id":"test/model"}]}), 1).unwrap();
    let options = ConnectOptions::default();
    let kind = Agent::ClaudeCode;
    let preview = projector
        .preview(kind, true, Some(&catalog), &options)
        .unwrap();
    projector
        .apply(kind, true, &preview.revision, Some(&catalog), &options)
        .unwrap();
    let projected = fs::read(&agent).unwrap();
    gateway.set_nonblocking(true).unwrap();

    let start = || {
        // Discard any retry queued by the previous backend before it exited.
        while gateway.accept().is_ok() {}
        let mut command = if supervised {
            backend.command(&["service", "run"])
        } else {
            let mut command = std::process::Command::new(
                backend.cli().with_file_name("private-ai-proxy-service"),
            );
            command.env(desktop_core::paths::HOME_OVERRIDE_ENV, &home);
            command
        };
        let mut child = command
            .env("NO_PROXY", "*")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let expires = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok((connection, _)) = gateway.accept() {
                let state = backend.run(&["status"]);
                assert_eq!(state["gateway"]["sessionId"], "saved-session");
                assert_eq!(state["gateway"]["sessionActive"], true);
                drop(connection);
                return child;
            }
            assert!(child.try_wait().unwrap().is_none());
            assert!(
                Instant::now() < expires,
                "The backend did not resume verification"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let terminate = |child: &mut std::process::Child| {
        let pid = rustix::process::Pid::from_child(child);
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
        desktop_core::launch::wait_for_exit(child.id(), Duration::from_secs(35)).unwrap();
        assert!(child.wait().unwrap().success());
    };
    terminate(&mut start());
    if !supervised {
        assert_eq!(usage.active_session().unwrap(), None);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&agent).unwrap()).unwrap(),
            original
        );
        return;
    }
    assert_eq!(
        usage.active_session().unwrap(),
        Some(("saved-session".into(), 1))
    );
    assert_eq!(fs::read(&agent).unwrap(), projected);

    let mut child = start();
    if offline_stop {
        terminate(&mut child);
        assert_eq!(backend.run(&["status"])["status"], "not_running");
        assert!(usage.active_session().unwrap().is_some());
        assert_eq!(fs::read(&agent).unwrap(), projected);
    }
    backend.run(&["service", "stop", "--yes"]);
    assert!(child.wait().unwrap().success());
    assert_eq!(usage.active_session().unwrap(), None);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&agent).unwrap()).unwrap(),
        original
    );
}

#[cfg(target_os = "linux")]
#[test]
fn an_already_enabled_user_unit_takes_over_a_detached_backend_once() {
    assert_user_unit_handoff(None);
}

#[cfg(target_os = "linux")]
#[test]
fn a_rejected_handoff_restores_the_detached_backend_and_session() {
    assert_user_unit_handoff(Some((
        "reject-start",
        "Cannot start the backend through its login service",
    )));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "exercises the production 60-second readiness deadline"]
fn a_timed_out_handoff_restores_the_detached_backend_and_session() {
    assert_user_unit_handoff(Some(("no-replacement", "Backend readiness timed out")));
}

#[cfg(target_os = "linux")]
fn assert_user_unit_handoff(failure: Option<(&str, &str)>) {
    use desktop_runtime::usage::UsageStore;
    use std::{os::unix::fs::PermissionsExt, process::Command};

    let sandbox = support::Sandbox::new();
    let root = sandbox.path();
    let home = root.join("home");
    let bin = root.join("bin");
    let runtime = root.join("runtime");
    let cli = root.join("private-ai-proxy");
    let backend = root.join("private-ai-proxy-service");
    let config = home.join(".config/private-ai-proxy");
    let data = home
        .join(".local/share")
        .join(desktop_core::brand::APP_IDENTIFIER);
    for directory in [&bin, &runtime, &config, &data] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
    gateway.set_nonblocking(true).unwrap();
    let gateway_addr = gateway.local_addr().unwrap();
    fs::write(
        config.join("config.toml"),
        format!("active-profile = \"test\"\n[local-api]\nport = {port}\n[profiles.test]\nname = \"Test\"\nprovider = \"custom\"\nremote-url = \"https://{gateway_addr}\"\n"),
    )
    .unwrap();
    desktop_core::private_fs::write_private(
        &config.join("credentials.toml"),
        "[profiles.test]\napi-key = \"test-key\"\n",
    )
    .unwrap();
    let usage = UsageStore::open(data.join("usage.sqlite3")).unwrap();
    usage.save_active_session("handoff-session", 1).unwrap();
    support::install(env!("CARGO_BIN_EXE_private-ai-proxy"), &cli);
    support::install(env!("CARGO_BIN_EXE_private-ai-proxy-service"), &backend);

    // PATH contains only these sandbox commands, even when the user's real
    // user manager is running under the same UID as the test.
    let systemctl = r#"#!/bin/sh
set -eu
[ "$1" = --user ]
[ ! -e "$HOME/no-bus" ] || exit 1
unit="$HOME/.config/systemd/user/private-ai-proxy.service"
shift
case "$1" in
  show-environment) ;;
  show)
    if [ -f "$unit" ]; then
      unit_state=disabled
      [ ! -e "$HOME/enabled" ] || unit_state=enabled
      pid=0
      active_state=inactive
      if [ -f "$HOME/managed-pid" ]; then
        read -r pid <"$HOME/managed-pid"
        active_state=active
      fi
      printf 'LoadState=loaded\nFragmentPath=%s\nDropInPaths=\nNeedDaemonReload=no\nUnitFileState=%s\nActiveState=%s\nMainPID=%s\n' "$unit" "$unit_state" "$active_state" "$pid"
    else
      printf 'LoadState=not-found\nDropInPaths=\n'
    fi ;;
  daemon-reload) ;;
  enable) : > "$HOME/enabled" ;;
  start)
    [ ! -e "$HOME/reject-start" ] || exit 1
    [ ! -e "$HOME/no-replacement" ] || exit 0
    "$PAP_FIXTURE_BACKEND" --supervised </dev/null >"$HOME/backend.log" 2>&1 &
    printf '%s\n' "$!" >"$HOME/managed-pid" ;;
  *) exit 2 ;;
esac
"#;
    let busctl = r#"#!/bin/sh
set -eu
[ "$1" = --user ]
restart=on-failure
[ ! -e "$HOME/unsafe-restart" ] || restart=always
printf '{"data":"%s"}\n{"data":[[75],[]]}\n{"data":[[],[]]}\n{"data":[[],[]]}\n' "$restart"
"#;
    for (name, script) in [("systemctl", systemctl), ("busctl", busctl)] {
        let path = bin.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let launch = |args: &[&str]| {
        Command::new(&cli)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &bin)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("PAP_FIXTURE_BACKEND", &backend)
            .env("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/dev/null")
            .env("NO_PROXY", "*")
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    };
    let run = |args: &[&str]| {
        let output = launch(args);
        assert_success(&output);
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let wait_for_verification = || {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match gateway.accept() {
                Ok((stream, _)) => return stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("Cannot observe verification: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "The backend did not resume verification"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // Enabling starts the backend under the service manager, which resumes the
    // saved session. Stop it and save the session again so a detached backend
    // can start with it while the manager is unavailable.
    run(&["service", "enable"]);
    assert!(home.join("managed-pid").exists());
    assert_eq!(
        run(&["status"])["loginService"],
        json!({"enabled": true, "active": true, "reason": null, "detail": "Enabled (active)"})
    );
    // Disabling a systemd unit does not stop a running service.
    fs::remove_file(home.join("enabled")).unwrap();
    assert_eq!(
        run(&["status"])["loginService"],
        json!({"enabled": false, "active": true, "reason": null, "detail": "Active (disabled at login)"})
    );
    fs::write(home.join("enabled"), "").unwrap();
    drop(wait_for_verification());
    run(&["service", "stop", "--yes"]);
    fs::remove_file(home.join("managed-pid")).unwrap();
    assert_eq!(
        run(&["status"])["loginService"],
        json!({"enabled": true, "active": false, "reason": null, "detail": "Enabled (inactive)"})
    );
    usage.save_active_session("handoff-session", 1).unwrap();
    fs::write(home.join("no-bus"), "").unwrap();
    let detached = run(&["service", "start"])["processId"].clone();
    assert_eq!(
        run(&["status"])["loginService"],
        json!({
            "enabled": false,
            "active": false,
            "reason": "service_manager_unavailable",
            "detail": "Unavailable because no user service manager is running",
        })
    );
    let verifying = wait_for_verification();
    fs::remove_file(home.join("no-bus")).unwrap();

    if let Some((flag, error)) = failure {
        fs::write(home.join(flag), "").unwrap();
        let output = launch(&["service", "enable"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{output:?}"
        );
        let status = run(&["status"]);
        assert!(status["backend"]["processId"].is_number(), "{status}");
        assert_ne!(status["backend"]["processId"], detached);
        assert_eq!(status["gateway"]["sessionId"], "handoff-session");
        assert_eq!(status["gateway"]["sessionActive"], true);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        assert!(!home.join("managed-pid").exists());
        let _reverifying = wait_for_verification();
        run(&["service", "stop", "--yes"]);
        return;
    }
    drop(verifying);

    fs::write(home.join("unsafe-restart"), "").unwrap();
    let output = launch(&["service", "enable"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected restart policy"));
    assert_eq!(run(&["status"])["backend"]["processId"], detached);
    fs::remove_file(home.join("unsafe-restart")).unwrap();

    run(&["service", "enable"]);
    let managed = run(&["status"])["backend"]["processId"].clone();
    assert_ne!(managed, detached);
    assert_eq!(
        managed.as_u64().unwrap().to_string(),
        fs::read_to_string(home.join("managed-pid")).unwrap().trim()
    );
    run(&["service", "enable"]);
    assert_eq!(run(&["status"])["backend"]["processId"], managed);
    run(&["service", "stop", "--yes"]);
}

/// Scripts such as `scripts/live_e2e` run `aci audit --json` and read its
/// streams; the legacy alias must stay byte-for-byte the canonical command.
#[cfg(unix)]
#[test]
fn legacy_aci_alias_output_matches_the_canonical_command() {
    use std::path::Path;
    use std::process::Command;
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
fn completions_and_argument_errors_use_the_invoked_alias() {
    #[cfg(unix)]
    let directory = tempfile::tempdir().unwrap();
    for name in ["pap", "aci", "private-ai-proxy"] {
        #[cfg(unix)]
        let executable = {
            let alias = directory.path().join(name);
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_private-ai-proxy"), &alias).unwrap();
            alias
        };
        #[cfg(not(unix))]
        let executable = std::path::PathBuf::from(env!("CARGO_BIN_EXE_private-ai-proxy"));
        for shell in ["bash", "zsh"] {
            let mut command = std::process::Command::new(&executable);
            command.args(["completions", shell]);
            #[cfg(unix)]
            command.env_remove(desktop_core::launch::ALIAS_ENV);
            #[cfg(not(unix))]
            command.env(desktop_core::launch::ALIAS_ENV, name);
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let script = String::from_utf8(output.stdout).unwrap();
            if shell == "bash" {
                assert!(
                    script
                        .lines()
                        .any(|line| line.trim().starts_with("complete -F ")
                            && line.ends_with(&format!(" {name}"))),
                    "{name}: missing bash registration"
                );
            } else {
                assert!(
                    script.contains(&format!("#compdef {name}\n")),
                    "{name}: missing zsh registration"
                );
            }
            assert!(script.contains("profiles"));
            assert!(script.contains("verify"));
        }
        for value in [None, Some("dark")] {
            for json in [false, true] {
                let mut command = std::process::Command::new(&executable);
                command.args(["settings", "set", "appearance", "--value-stdin"]);
                command.args(value);
                if json {
                    command.arg("--json");
                }
                command.env(desktop_core::launch::ALIAS_ENV, name);
                let output = command.output().unwrap();
                assert_eq!(output.status.code(), Some(2), "{output:?}");
                assert!(output.stdout.is_empty());
                let stderr = String::from_utf8(output.stderr).unwrap();
                let message = if json {
                    let error: Value = serde_json::from_str(&stderr).unwrap();
                    assert_eq!(error["error"]["code"], "invalid_arguments");
                    error["error"]["message"].as_str().unwrap().to_string()
                } else {
                    stderr
                };
                assert!(
                    message.starts_with("error: Only web-ui.password reads its value from stdin"),
                    "{message}"
                );
                assert!(
                    message.contains(&format!("Usage: {name} [OPTIONS] <COMMAND>")),
                    "{message}"
                );
            }
        }
    }
}

const WEB_PASSWORD: &str = "correct horse battery staple";

#[test]
fn web_ui_opens_with_a_generated_password_only_local_clients_read() {
    let backend = Backend::start();
    let status = &backend.run(&["status"])["gateway"]["webUi"];
    assert_eq!(status["enabled"], false);
    // Revealing the password needs consent, like `token show`.
    let refused = backend
        .command(&["web-ui", "password", "show", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let password = backend.web_ui_password();
    assert_eq!(password.len(), 32, "{password}");
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

    let data = backend
        .directory
        .path()
        .join("home/.private-ai-proxy/Config");
    let saved = fs::read_to_string(data.join("credentials.toml")).unwrap();
    assert!(
        saved.contains(&format!("[web-ui]\npassword = \"{password}\"\n")),
        "{saved}"
    );
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
    let diagnostics = backend.directory.path().join("web-diagnostics.json");
    backend.run(&["diagnostics", "--output", diagnostics.to_str().unwrap()]);
    for (name, text) in [
        (
            "settings show",
            backend.run(&["settings", "show"]).to_string(),
        ),
        ("status", backend.run(&["status"]).to_string()),
        ("diagnostics", fs::read_to_string(&diagnostics).unwrap()),
        ("config.toml", config),
        (
            "backend log",
            fs::read_to_string(backend.directory.path().join("backend.log")).unwrap(),
        ),
    ] {
        assert!(!text.contains(&password), "{name} contains the password");
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
    let token = web_session(&authority, &password);
    let other = web_session(&authority, &password);
    assert_eq!(
        http(&authority, "GET", "/api/bootstrap", Some(&token), "").0,
        200
    );
    assert_eq!(http(&authority, "GET", "/api/bootstrap", None, "").0, 401);
    // A signed-in browser can neither read nor change the password.
    for method in [
        "get_web_ui_password",
        "rotate_web_ui_password",
        "set_web_ui_password",
    ] {
        let body = json!({ "password": "another long passphrase" }).to_string();
        let path = format!("/api/rpc/{method}");
        assert_eq!(http(&authority, "POST", &path, Some(&token), &body).0, 404);
    }

    // Rotating ends every session; only the new password signs in.
    let refused = backend
        .command(&["web-ui", "password", "rotate", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    backend.run(&["web-ui", "password", "rotate", "--yes"]);
    let rotated = backend.web_ui_password();
    assert_ne!(rotated, password);
    for token in [&token, &other] {
        assert_eq!(
            http(&authority, "GET", "/api/bootstrap", Some(token), "").0,
            401
        );
    }
    let old = json!({ "password": password }).to_string();
    assert_eq!(http(&authority, "POST", "/api/session", None, &old).0, 401);
    let token = web_session(&authority, &rotated);

    // A chosen password replaces it the same way.
    assert_success(&backend.set_web_ui_password(WEB_PASSWORD));
    assert_eq!(backend.web_ui_password(), WEB_PASSWORD);
    assert_eq!(
        http(&authority, "GET", "/api/bootstrap", Some(&token), "").0,
        401
    );
    web_session(&authority, WEB_PASSWORD);

    backend.run(&["settings", "set", "web-ui.enabled", "false", "--yes"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(&authority).is_ok() {
        assert!(Instant::now() < deadline, "the web UI listener stayed open");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn an_unreadable_password_reports_its_cause() {
    // Not TOML, so no password can be read or generated until the file is fixed.
    let backend = Backend::start_with_credentials("[web-ui\n");
    let failed = backend
        .command(&["web-ui", "password", "show", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let error = String::from_utf8_lossy(&failed.stderr);
    assert!(error.contains("credentials.toml:"), "{error}");
    assert!(!error.contains("earlier version"), "{error}");
}

/// `correct horse battery staple` as a version that kept only an Argon2id hash saved it.
const LEGACY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$djbiNHLjbn9XvtGL3ibJIw$m2FuaZoY7r75d2/IFuBacNsBsp03lFrOk8x5zAWO8mU";

#[test]
fn a_password_hash_from_an_earlier_version_signs_in_until_replaced() {
    let backend =
        Backend::start_with_credentials(&format!("[web-ui]\npassword-hash = \"{LEGACY_HASH}\"\n"));
    // Only the hash is known, so there is nothing to show.
    let hidden = backend
        .command(&["web-ui", "password", "show", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(!hidden.status.success());
    assert!(String::from_utf8_lossy(&hidden.stderr).contains("pap web-ui password rotate"));
    let port = {
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        free.local_addr().unwrap().port().to_string()
    };
    backend.run(&["settings", "set", "web-ui.port", &port, "--yes"]);
    let state = backend.run(&["settings", "set", "web-ui.enabled", "true", "--yes"]);
    if state["webUi"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("assets are not built"))
    {
        return;
    }
    let authority = format!("127.0.0.1:{port}");
    let token = web_session(&authority, WEB_PASSWORD);

    backend.run(&["web-ui", "password", "rotate", "--yes"]);
    let password = backend.web_ui_password();
    let saved = fs::read_to_string(
        backend
            .directory
            .path()
            .join("home/.private-ai-proxy/Config/credentials.toml"),
    )
    .unwrap();
    assert!(!saved.contains("password-hash"), "{saved}");
    assert_eq!(
        http(&authority, "GET", "/api/bootstrap", Some(&token), "").0,
        401
    );
    let old = json!({ "password": WEB_PASSWORD }).to_string();
    assert_eq!(http(&authority, "POST", "/api/session", None, &old).0, 401);
    web_session(&authority, &password);
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

    /// Reads the web UI password the way scripts do.
    fn web_ui_password(&self) -> String {
        self.run(&["web-ui", "password", "show", "--yes"])["password"]
            .as_str()
            .unwrap()
            .to_string()
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
    // A backend that never answers fails the test instead of hanging it.
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
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
    // A fresh password replaces the chosen one.
    assert_ne!(backend.web_ui_password(), WEB_PASSWORD);
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
    // Outputs and errors are pinned by `tests/golden/management.txt`; this
    // checks the state behind them.
    let backend = Backend::start();
    #[cfg(unix)]
    assert!(backend
        .directory
        .path()
        .join("home/.private-ai-proxy/helpers/private-ai-proxy-helper")
        .is_file());
    let first = backend.run(&["status"]);
    // Every state output carries the protection it presents, as the API sends it.
    assert_eq!(first["gateway"]["protection"]["phase"], "profileRequired");
    assert_eq!(
        backend.run(&["stop"])["protection"],
        first["gateway"]["protection"]
    );
    let second = backend.run(&["service", "start"]);
    assert_eq!(first["backend"]["instanceId"], second["instanceId"]);
    backend.run(&["token", "rotate", "--yes"]);
    let state = backend.run(&["status"]);
    assert_eq!(state["gateway"]["clientKeyRevision"], 1);
    assert_eq!(state["gateway"]["clientKeyAvailable"], true);
    let backup = backend.directory.path().join("profiles.json");
    fs::write(&backup, r#"{"version":1,"profiles":[{"name":"Work","provider":"phala","remoteUrl":"https://inference.phala.com"}]}"#).unwrap();
    backend.run(&["profiles", "import", backup.to_str().unwrap(), "--yes"]);
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
    // A hand edit is saved the way editors and this store save: written
    // beside the file and renamed over it, so the watcher never reads a
    // truncated file. (`fs::write` truncates first; a runner stalled past the
    // debounce window between truncate and write would apply an empty file.)
    let save = |contents: &str| {
        let staged = data.join("config.toml.edit");
        fs::write(&staged, contents).unwrap();
        fs::rename(&staged, &config).unwrap();
    };
    let edited = |line: &str| {
        text.replacen(
            "appearance = \"dark\"\n",
            &format!("appearance = \"dark\"\n{line}\n"),
            1,
        )
    };
    save(&edited("connect-on-launch = true"));
    wait(&|show| show["settings"]["connect-on-launch"] == true);

    // An unknown key warns but never rejects the file.
    save(&edited("connect-on-launch = true\nfuture-option = 1"));
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
    save(&edited("connect-on-launch = \"yes\""));
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

    save(&text);
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
    // It restores without starting one.
    let agents = backend.run(&["stop", "--offline"]);
    assert!(agents
        .as_array()
        .unwrap()
        .iter()
        .all(|agent| agent["recorded"] == false));
    assert_eq!(backend.run(&["status"]), stopped);
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
    let restarted = backend.run(&["service", "restart"]);
    assert_ne!(restarted["instanceId"], started["instanceId"]);
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
    use std::process::Command;
    use std::{
        io::{BufRead, BufReader},
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };
    use support::{executable, install, Sandbox};
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
