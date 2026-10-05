use super::*;

#[test]
fn every_agent_switches_conservatively_and_reconnects_without_namespace_conflicts() {
    for file_credentials in [false, true] {
        for agent in Agent::ALL {
            let mut sandbox = sandbox(&format!(
                "conservative-{}-{file_credentials}{}",
                agent.id(),
                if file_credentials {
                    "-space ' quote"
                } else {
                    ""
                }
            ));
            if file_credentials {
                sandbox.projector = Projector::at(
                    sandbox.home.clone(),
                    sandbox.projector.data_dir.clone(),
                    sandbox.home.join("missing-helper"),
                    ENDPOINT,
                    false,
                    sandbox.secrets.clone(),
                )
                .with_home_credentials();
            }
            if agent == Agent::OpenClaw && !file_credentials {
                sandbox.projector.helper_exe = sandbox
                    .projector
                    .data_dir
                    .join("helpers")
                    .join(helper_binary_name());
            }
            let config = agent.config_path(&sandbox.home, false);
            let original = match agent {
            Agent::Codex => "model_provider = 'original'\nmodel = 'native'\nmodel_catalog_json = 'native-catalog.json'\n",
            Agent::ClaudeCode => r#"{"env":{"ANTHROPIC_BASE_URL":"https://original.invalid","ANTHROPIC_MODEL":"native","ANTHROPIC_AUTH_TOKEN":"old-secret"},"model":"native"}"#,
            Agent::OpenCode => r#"{"model":"original/native","provider":{"original":{"name":"User provider"}}}"#,
            Agent::Hermes => "model:\n  provider: original\n  default: native\ntheme: dark\n",
            Agent::OpenClaw => r#"{"agents":{"defaults":{"model":{"primary":"original/native","fallbacks":["original/fallback"]}}}}"#,
            Agent::Pi => "{}",
            Agent::OhMyPi => "theme: dark\n",
            Agent::Dsh => "# user patches\n- id: session-telemetry-otel\n  disabled: true\n",
        };
            write(&config, original);
            let defaults = match agent {
            Agent::Pi => Some((
                config.with_file_name("settings.json"),
                Format::Json,
                r#"{"defaultProvider":"original","defaultModel":"native","theme":"dark"}"#,
            )),
            Agent::OhMyPi => Some((
                config.with_file_name("config.yaml"),
                Format::Yaml,
                "modelRoles:\n  default: original/native\n  smol: original/small\ntheme: dark\n",
            )),
            Agent::Dsh => Some((
                config.with_file_name(dsh::CREDENTIALS_FILE),
                Format::Yaml,
                "version: 1\nrefs:\n  DEEPSEEK_API_KEY: user-key\n",
            )),
            _ => None,
        };
            if let Some((path, _, text)) = &defaults {
                write(path, text);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            let catalog = catalog();
            let options = ConnectOptions::default();
            let enable = |projector: &Projector| {
                let preview = projector
                    .preview(agent, true, Some(&catalog), &options)
                    .unwrap();
                projector
                    .apply(agent, true, &preview.revision, Some(&catalog), &options)
                    .unwrap()
            };
            assert!(enable(&sandbox.projector).authorized, "{}", agent.id());
            let first_token = sandbox.projector.tokens.read(agent.id()).unwrap().unwrap();
            let connected = fs::read_to_string(&config).unwrap();
            if file_credentials {
                let token_path = sandbox.projector.tokens.path(agent.id());
                assert!(
                    token_path.starts_with(sandbox.home.join(format!(".{APP_IDENTIFIER}-agents")))
                );
                assert!(!connected.contains("private-ai-proxy-helper"));
                assert!(!connected.contains(&sandbox.projector.data_dir.display().to_string()));
                assert!(!connected.contains("old-secret"));
                assert!(!connected.contains(&first_token));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                    // Exercise the exact POSIX credential command, including hostile
                    // path characters, without any PAP executable or container access.
                    let command = agent_credential_command(
                        Path::new("missing-helper"),
                        agent,
                        Some(&token_path),
                    )
                    .unwrap();
                    let output = Command::new("/bin/sh")
                        .args(["-c", &command])
                        .output()
                        .unwrap();
                    assert!(output.status.success());
                    assert_eq!(String::from_utf8(output.stdout).unwrap(), first_token);
                }
                if agent == Agent::OpenClaw {
                    let document: serde_json::Value = serde_json::from_str(&connected).unwrap();
                    assert_eq!(
                        document["secrets"]["providers"]["private-ai-proxy"]["mode"],
                        "singleValue"
                    );
                    assert_eq!(
                        document["models"]["providers"]["private-ai-proxy"]["apiKey"]["source"],
                        "file"
                    );
                }
            }
            if let Some((path, _, _)) = &defaults {
                assert!(fs::read_to_string(path)
                    .unwrap()
                    .contains(if agent == Agent::Dsh {
                        dsh::TOKEN_REF
                    } else {
                        "private-ai-proxy"
                    }));
            }
            disconnect(&sandbox, agent);
            assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
            let (statuses, tokens) = sandbox.projector.scan(Some(&catalog)).unwrap();
            let status = statuses
                .iter()
                .find(|status| status.id == agent.id())
                .unwrap();
            assert!(!status.connected && !status.authorized);
            assert!(tokens.agent_for(&first_token).is_none());
            let mut restored = doc(&sandbox, agent);
            match agent {
                Agent::OpenCode => assert!(restored
                    .get_value(&["provider", "private-ai-proxy", "options", "apiKey"])
                    .is_none()),
                Agent::Pi | Agent::OhMyPi => assert!(restored
                    .get_value(&["providers", "private-ai-proxy", "apiKey"])
                    .is_none()),
                Agent::Hermes => assert_eq!(
                    restored
                        .get_str(&["providers", "private-ai-proxy", "key_cmd"])
                        .as_deref(),
                    Some("")
                ),
                Agent::OpenClaw => assert!(restored
                    .get_value(&["models", "providers", "private-ai-proxy", "apiKey"])
                    .is_none()),
                _ => {}
            }
            let record = sandbox
                .projector
                .load_store()
                .unwrap()
                .get(agent.id())
                .cloned();
            if matches!(agent, Agent::ClaudeCode | Agent::Dsh) {
                assert!(record.is_none());
            } else {
                let record = record.unwrap();
                assert!(record.disconnected());
                for field in &record.fields {
                    assert_eq!(restored.get_value(&refs(&field.path)), field.value);
                }
                let roots: Vec<_> = record
                    .fields
                    .iter()
                    .filter_map(|field| provider_namespace(&field.path).map(|path| path.to_vec()))
                    .collect();
                for root in roots {
                    restored.remove(&refs(&root)).unwrap();
                }
            }
            let mut before = ConfigDoc::parse(agent.format(), original).unwrap();
            if agent == Agent::OpenClaw {
                before
                    .set_value(
                        &["models", "providers"],
                        &ConfigValue::Json(serde_json::json!({})),
                    )
                    .unwrap();
                before
                    .set_value(
                        &["secrets", "providers"],
                        &ConfigValue::Json(serde_json::json!({})),
                    )
                    .unwrap();
            }
            assert_eq!(
                restored.get_value(&[]),
                before.get_value(&[]),
                "{}",
                agent.id()
            );
            if let Some((path, format, original)) = &defaults {
                let restored =
                    ConfigDoc::parse(*format, &fs::read_to_string(path).unwrap()).unwrap();
                assert_eq!(
                    restored.get_value(&[]),
                    ConfigDoc::parse(*format, original).unwrap().get_value(&[])
                );
            }
            let stopped_files = fs::read_to_string(&config).unwrap();
            sandbox.projector.reconcile(Some(&catalog)).unwrap();
            assert_eq!(fs::read_to_string(&config).unwrap(), stopped_files);
            assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
            assert!(enable(&sandbox.projector).authorized);
            assert_ne!(
                sandbox.projector.tokens.read(agent.id()).unwrap().unwrap(),
                first_token
            );
            assert_eq!(fs::read_to_string(&config).unwrap(), connected);
            assert!(sandbox.projector.disconnect_all().unwrap().is_empty());
        }
    }
}

#[cfg(unix)]
#[test]
fn failed_secondary_selection_write_rolls_back_provider_and_revokes_token() {
    let sandbox = sandbox("selection-write-failure");
    let agent = Agent::Pi;
    let config = agent.config_path(&sandbox.home, false);
    write(&config, "{}");
    let target = sandbox.home.join("user-settings.json");
    write(
        &target,
        r#"{"defaultProvider":"original","defaultModel":"native"}"#,
    );
    let defaults = config.with_file_name("settings.json");
    std::os::unix::fs::symlink(&target, &defaults).unwrap();
    let original = fs::read(&target).unwrap();
    let catalog = catalog();
    let options = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(agent, true, Some(&catalog), &options)
        .unwrap();
    assert!(sandbox
        .projector
        .apply(agent, true, &preview.revision, Some(&catalog), &options)
        .is_err());
    assert_eq!(fs::read_to_string(&config).unwrap(), "{}");
    assert_eq!(fs::read(&target).unwrap(), original);
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
    assert!(sandbox.projector.load_store().unwrap().is_empty());
}

#[test]
fn opencode_process_overrides_follow_official_merge_order() {
    const CASE: &str = "PAP_TEST_OPENCODE_MERGE_CASE";
    if let Ok(case) = env::var(CASE) {
        let mut sandbox = sandbox("opencode-env-merge");
        sandbox.projector.tool_env = true;
        let global = env_path("XDG_CONFIG_HOME").unwrap().join("opencode");
        write(
            &global.join("opencode.jsonc"),
            "{/* preserved */\"model\":\"other/model\"}",
        );
        let expected = ConfigDoc::Json(
            json!({
                "model": "private-ai-proxy/test",
                "provider": {"private-ai-proxy": {"name": "Gateway"}}
            })
            .to_string(),
        );
        if let Some(dir) = env_path("OPENCODE_CONFIG_DIR") {
            write(&dir.join("opencode.json"), "{\"model\":\"other/dir-json\"}");
            write(
                &dir.join("opencode.jsonc"),
                "{\"model\":\"private-ai-proxy/test\",}",
            );
        }
        let result = sandbox.projector.check_opencode_merge(&expected, true);
        assert_eq!(
            result.is_ok(),
            matches!(case.as_str(), "explicit" | "directory"),
            "{case}: {result:?}"
        );
        if case == "global" {
            // A default model is not owned when the user did not select one.
            assert!(sandbox
                .projector
                .check_opencode_merge(&expected, false)
                .is_ok());
        }
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for case in [
        "global",
        "explicit",
        "directory",
        "content",
        "invalid-content",
    ] {
        let dir = root.path().join(case);
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "agents::tests::contracts::opencode_process_overrides_follow_official_merge_order",
            ])
            .env(CASE, case)
            .env("XDG_CONFIG_HOME", &dir)
            .env_remove("OPENCODE_CONFIG")
            .env_remove("OPENCODE_CONFIG_DIR")
            .env_remove("OPENCODE_CONFIG_CONTENT");
        if case != "global" {
            // Even pointing back at global JSON makes it higher priority than global JSONC.
            command.env("OPENCODE_CONFIG", dir.join("opencode/opencode.json"));
        }
        if matches!(case, "directory" | "content" | "invalid-content") {
            command.env("OPENCODE_CONFIG_DIR", dir.join("extra"));
        }
        if case == "content" {
            command.env("OPENCODE_CONFIG_CONTENT", "{\"model\":\"other/content\"}");
        } else if case == "invalid-content" {
            command.env("OPENCODE_CONFIG_CONTENT", "{invalid");
        }
        let output = command.output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
        assert!(
            output.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn opencode_limits_and_hermes_defaults_do_not_invent_metadata() {
    let catalog = Catalog::from_remote(
        &json!({"data":[
            {"id":"context-only","context_length":123},
            {"id":"output-only","max_output_length":45},
            {"id":"complete","context_length":123,"max_output_length":45}
        ]}),
        1,
    )
    .unwrap();
    let api = api_url(ENDPOINT).unwrap();
    let provider = opencode_provider(&catalog, &api, Path::new("/data/token")).unwrap();
    assert_eq!(provider["options"]["apiKey"], "{file:/data/token}");
    for path in ["/data/{env:HOME}/token", "/data/a}b/token"] {
        assert!(opencode_provider(&catalog, &api, Path::new(path)).is_err());
    }
    assert!(provider["models"]["context-only"].get("limit").is_none());
    assert!(provider["models"]["output-only"].get("limit").is_none());
    assert_eq!(
        provider["models"]["complete"]["limit"],
        json!({"context":123,"output":45})
    );
    let sandbox = sandbox("hermes-default-conflict");
    let path = Agent::Hermes.config_path(&sandbox.home, false);
    write(&path, "model:\n  default: unrelated-model\n");
    assert!(sandbox
        .projector
        .preview(
            Agent::Hermes,
            true,
            Some(&catalog),
            &ConnectOptions::default()
        )
        .is_ok());
    assert!(sandbox
        .projector
        .preview(
            Agent::Hermes,
            true,
            Some(&catalog),
            &ConnectOptions {
                default_model: Some("complete".into())
            }
        )
        .is_ok());
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "model:\n  default: unrelated-model\n"
    );
}

#[test]
fn apply_refuses_a_stale_revision() {
    let sandbox = sandbox("revision");
    let path = sandbox.home.join(".claude").join("settings.json");
    write(&path, r#"{"model": "opus"}"#);
    let catalog = catalog();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, true, Some(&catalog), &claude_options())
        .unwrap();
    let error = sandbox
        .projector
        .apply(
            Agent::ClaudeCode,
            false,
            &preview.revision,
            Some(&catalog),
            &claude_options(),
        )
        .unwrap_err();
    assert_eq!(
        error.code(),
        desktop_core::protocol::ErrorCode::RevisionConflict
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "opus"}"#);
    write(&path, r#"{"model": "sonnet"}"#);
    let error = sandbox
        .projector
        .apply(
            Agent::ClaudeCode,
            true,
            &preview.revision,
            Some(&catalog),
            &claude_options(),
        )
        .unwrap_err();
    assert_eq!(
        error.code(),
        desktop_core::protocol::ErrorCode::RevisionConflict
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "sonnet"}"#);
    assert!(sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .is_none());
}

#[cfg(unix)]
#[test]
fn connect_rolls_everything_back_when_the_record_cannot_be_saved() {
    use std::os::unix::fs::PermissionsExt;
    let mut sandbox = sandbox("rollback");
    let blocked = sandbox.home.join("blocked");
    fs::create_dir_all(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).unwrap();
    sandbox.projector.data_dir = blocked.clone();
    let path = sandbox.home.join(".claude").join("settings.json");
    write(&path, r#"{"env": {"ANTHROPIC_API_KEY": "sk-user"}}"#);
    let catalog = catalog();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, true, Some(&catalog), &claude_options())
        .unwrap();
    let error = sandbox
        .projector
        .apply(
            Agent::ClaudeCode,
            true,
            &preview.revision,
            Some(&catalog),
            &claude_options(),
        )
        .unwrap_err();
    assert_eq!(
        error.code(),
        desktop_core::protocol::ErrorCode::ConfigurationLockFailed
    );
    assert!(!error.to_string().contains("sk-user"));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{"env": {"ANTHROPIC_API_KEY": "sk-user"}}"#
    );
    assert!(sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .is_none());
    assert!(sandbox.secrets.is_empty(), "parked secret rolled back");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn atomic_writes_refuse_symlinks_and_changed_files() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let target = dir.join("config.json");
    write_atomic(&target, "{}", Some(None)).unwrap();
    assert!(write_atomic(&target, "{\"a\":1}", Some(Some("changed"))).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
    write_atomic(&target, "{\"a\":1}", Some(Some("{}"))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_atomic(&link, "{}", None).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "{\"a\":1}");
    }
    assert!(fs::read_dir(dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

/// H1 at the proxy layer: a scan is what publishes authority. A token
/// obtained while connected stops opening the proxy as soon as a scan
/// runs after the config drifted or broke — the request is refused at
/// auth and nothing reaches the verified upstream.
#[tokio::test]
async fn a_scan_after_config_drift_revokes_the_old_token_at_the_proxy() {
    use crate::proxy::{ForwardContext, ProxyState, Session, VerifiedResponse, VerifiedService};
    use axum::{body::Body, response::Response, Json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DirectService(Arc<AtomicUsize>);
    impl VerifiedService for DirectService {
        fn call(
            self: Arc<Self>,
            request: axum::http::Request<Body>,
            _context: Option<ForwardContext>,
        ) -> VerifiedResponse {
            Box::pin(async move {
                if request.uri().path() == "/v1/models" {
                    return axum::response::IntoResponse::into_response(Json(
                        serde_json::json!({ "data": [{ "id": "openai/gpt-oss-20b" }] }),
                    ));
                }
                self.0.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .status(200)
                    .body(Body::from("{\"ok\":true}"))
                    .unwrap()
            })
        }
    }

    let sandbox = sandbox("proxy-drift");
    let path = sandbox.home.join(".claude").join("settings.json");
    write(&path, r#"{"model": "opus"}"#);
    connect(&sandbox);
    let token = sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .unwrap();

    // A counting verifier and a verified session for it.
    let hits = Arc::new(AtomicUsize::new(0));
    let service: Arc<dyn VerifiedService> = Arc::new(DirectService(hits.clone()));

    let (sender, _events) = tokio::sync::mpsc::channel(8);
    let state = ProxyState::new(sender).unwrap();
    state.set_api_key(Some("sk-live".into()));
    state.publish(Session {
        generation: 1,
        epoch: 1,
        session_id: Some("test-session".to_string()),
        service: Some(service.clone()),
        verified: false,
        catalog: None,
    });
    let catalog = state.fetch_catalog(1, 1).await.unwrap();
    state.publish(Session {
        generation: 1,
        epoch: 1,
        session_id: Some("test-session".to_string()),
        service: Some(service),
        verified: true,
        catalog: Some(catalog),
    });
    state.set_tokens(sandbox.projector.scan(None).unwrap().1);
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy_listener.local_addr().unwrap());
    let app = crate::proxy::router(state.clone());
    tokio::spawn(async move { axum::serve(proxy_listener, app).await.unwrap() });

    let client = reqwest::Client::new();
    let send = |token: String| {
        client
            .post(format!("{proxy_url}/v1/messages"))
            .bearer_auth(token)
            .json(&serde_json::json!({"model": "openai/gpt-oss-20b"}))
            .send()
    };
    assert_eq!(send(token.clone()).await.unwrap().status().as_u16(), 200);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // The config drifts outside the app; the next scan republishes the
    // token set and the old token stops working immediately.
    write(&path, "{ not json");
    state.set_tokens(sandbox.projector.scan(None).unwrap().1);
    assert_eq!(send(token).await.unwrap().status().as_u16(), 401);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "nothing reached the verified upstream"
    );
}
