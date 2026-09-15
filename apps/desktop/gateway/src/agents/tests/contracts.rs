use super::*;

#[test]
fn every_agent_switches_conservatively_and_reconnects_without_namespace_conflicts() {
    for agent in Agent::ALL {
        let mut sandbox = sandbox(&format!("conservative-{}", agent.id()));
        if agent == Agent::OpenClaw {
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
            _ => None,
        };
        if let Some((path, _, text)) = &defaults {
            write(path, text);
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
        if let Some((path, _, _)) = &defaults {
            assert!(fs::read_to_string(path)
                .unwrap()
                .contains("private-ai-proxy"));
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
        if agent == Agent::ClaudeCode {
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
            let restored = ConfigDoc::parse(*format, &fs::read_to_string(path).unwrap()).unwrap();
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
fn links_survive_stop_restart_and_uninstall_without_owning_inactive_configs() {
    let sandbox = sandbox("link-lifecycle");
    let agent = Agent::ClaudeCode;
    let config = agent.config_path(&sandbox.home, false);
    let cli = sandbox.home.join(".local/bin").join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    write(&cli, "test cli");
    write(
        &config,
        r#"{"model":"original","env":{"ANTHROPIC_AUTH_TOKEN":"original-secret"}}"#,
    );
    let options = claude_options();
    let preview = sandbox
        .projector
        .preview(agent, true, None, &options)
        .unwrap();
    let status = sandbox
        .projector
        .apply(agent, true, &preview.revision, None, &options)
        .unwrap();
    assert!(status.connected && !status.authorized);
    assert_eq!(
        doc(&sandbox, agent).get_value(&["model"]),
        Some(ConfigValue::Str("original".into()))
    );

    assert!(sandbox
        .projector
        .reconcile(Some(&catalog()))
        .unwrap()
        .is_empty());
    assert!(
        sandbox
            .projector
            .scan(None)
            .unwrap()
            .0
            .iter()
            .find(|s| s.id == agent.id())
            .unwrap()
            .authorized
    );
    assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
    assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
    assert_eq!(
        doc(&sandbox, agent).get_value(&["model"]),
        Some(ConfigValue::Str("original".into()))
    );
    assert_eq!(
        doc(&sandbox, agent).get_value(&["env", "ANTHROPIC_AUTH_TOKEN"]),
        Some(ConfigValue::Str("original-secret".into()))
    );
    assert!(sandbox.projector.load_store().unwrap()[agent.id()].suspended);

    assert!(sandbox
        .projector
        .reconcile(Some(&catalog()))
        .unwrap()
        .is_empty());
    fs::remove_file(&cli).unwrap();
    fs::remove_file(&config).unwrap();
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog()))
        .unwrap()
        .is_empty());
    assert!(
        !config.exists(),
        "uninstall must not recreate deleted config"
    );
    let status = sandbox
        .projector
        .scan(None)
        .unwrap()
        .0
        .into_iter()
        .find(|s| s.id == agent.id())
        .unwrap();
    assert!(status.connected && !status.authorized && status.attention.is_some());
    write(&cli, "test cli");
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog()))
        .unwrap()
        .is_empty());
    assert!(
        sandbox
            .projector
            .scan(None)
            .unwrap()
            .0
            .iter()
            .find(|s| s.id == agent.id())
            .unwrap()
            .authorized
    );
    disconnect(&sandbox, agent);
    assert!(sandbox.projector.load_store().unwrap().is_empty());
}

#[test]
fn temporary_connection_failure_retries_without_clearing_user_conflicts() {
    let sandbox = sandbox("transient-connect");
    let agent = Agent::OpenCode;
    write(
        &sandbox.home.join(".opencode/bin").join(if cfg!(windows) {
            "opencode.exe"
        } else {
            "opencode"
        }),
        "test cli",
    );
    let options = ConnectOptions::default();
    let catalog = catalog();
    let preview = sandbox
        .projector
        .preview(agent, true, None, &options)
        .unwrap();
    sandbox
        .projector
        .apply(agent, true, &preview.revision, None, &options)
        .unwrap();
    let token = sandbox.projector.tokens.path(agent.id());
    fs::create_dir_all(&token).unwrap();
    assert!(!sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    assert!(sandbox.projector.load_store().unwrap()[agent.id()]
        .attention
        .is_none());
    fs::remove_dir(&token).unwrap();
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    assert!(sandbox
        .projector
        .scan(Some(&catalog))
        .unwrap()
        .0
        .into_iter()
        .any(|status| status.id == agent.id() && status.authorized));
    sandbox.projector.reconcile(None).unwrap();
    let path = agent.config_path(&sandbox.home, false);
    let foreign = r#"{"provider":{"private-ai-proxy":{"name":"User-owned"}}}"#;
    write(&path, foreign);
    assert!(!sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    assert!(sandbox.projector.load_store().unwrap()[agent.id()]
        .attention
        .is_some());
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    assert_eq!(fs::read_to_string(path).unwrap(), foreign);
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
        let expected = ConfigDoc::Json(json!({
            "model": "private-ai-proxy/test",
            "provider": {"private-ai-proxy": {"name": "Gateway"}}
        }));
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
fn unmanaged_provider_objects_are_not_captured_as_plain_backups() {
    for agent in [Agent::OpenCode, Agent::Pi] {
        for value in [
            json!(null),
            json!({"apiKey":"sk-test-hidden","options":{"headers":{"Authorization":"sk-test-hidden"}}}),
        ] {
            let sandbox = sandbox(&format!("namespace-{}", agent.id()));
            let path = agent.config_path(&sandbox.home, false);
            let key = if agent == Agent::OpenCode {
                "provider"
            } else {
                "providers"
            };
            let text = json!({key:{"private-ai-proxy":value}}).to_string();
            write(&path, &text);
            let error = sandbox
                .projector
                .preview(agent, true, Some(&catalog()), &claude_options())
                .unwrap_err();
            assert!(!error.contains("sk-test-hidden"));
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
            assert!(!sandbox.projector.store_path().exists());
            assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
        }
    }
}

#[test]
fn native_auth_and_routing_conflicts_are_read_only_and_deauthorize() {
    for (agent, case) in [
        (Agent::Codex, "aws"),
        (Agent::Pi, "stored-key"),
        (Agent::OpenCode, "disabled"),
        (Agent::OpenCode, "allowlist"),
        (Agent::Hermes, "api_mode"),
        (Agent::Hermes, "disabled"),
        (Agent::Hermes, "explicit-key"),
        (Agent::Hermes, "pool"),
        (Agent::Hermes, "fallback"),
    ] {
        let sandbox = sandbox(&format!("conflict-{}-{case}", agent.id()));
        let catalog = catalog();
        let options = claude_options();
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .unwrap();
        sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .unwrap();
        let path = agent.config_path(&sandbox.home, false);
        let auth_path = path.with_file_name("auth.json");
        let mut edited = doc(&sandbox, agent);
        match (agent, case) {
            (Agent::Codex, _) => edited
                .set_str(
                    &["model_providers", "private_ai_proxy", "aws", "region"],
                    "test-region",
                )
                .unwrap(),
            (Agent::Pi, _) => write(
                &auth_path,
                r#"{"private-ai-proxy":{"type":"api_key","key":"sk-test-hidden"}}"#,
            ),
            (Agent::OpenCode, "disabled") => edited
                .set_value(
                    &["disabled_providers"],
                    &ConfigValue::List(vec!["private-ai-proxy".into()]),
                )
                .unwrap(),
            (Agent::OpenCode, _) => edited
                .set_value(
                    &["enabled_providers"],
                    &ConfigValue::List(vec!["other".into()]),
                )
                .unwrap(),
            (Agent::Hermes, "api_mode") => edited
                .set_str(
                    &["providers", "private-ai-proxy", "api_mode"],
                    "codex_responses",
                )
                .unwrap(),
            (Agent::Hermes, "disabled") => edited
                .set_value(
                    &["providers", "private-ai-proxy", "enabled"],
                    &ConfigValue::Bool(false),
                )
                .unwrap(),
            (Agent::Hermes, "explicit-key") => edited
                .set_str(&["model", "api_key"], "sk-test-hidden")
                .unwrap(),
            (Agent::Hermes, "pool") => write(
                &auth_path,
                r#"{"credential_pool":{"private-ai-proxy":[{"access_token":"sk-test-hidden"}]}}"#,
            ),
            (Agent::Hermes, _) => edited
                .set_str(&["fallback_model", "provider"], "other")
                .unwrap(),
            _ => unreachable!(),
        }
        write(&path, &edited.render().unwrap());
        let config_before = fs::read(&path).unwrap();
        let auth_before = fs::read(&auth_path).ok();
        let record_before = fs::read(sandbox.projector.store_path()).unwrap();
        let token_path = sandbox.projector.tokens.path(agent.id());
        let token_before = fs::read(&token_path).unwrap();
        let (statuses, tokens) = sandbox.projector.scan(Some(&catalog)).unwrap();
        let status = statuses
            .iter()
            .find(|status| status.id == agent.id())
            .unwrap();
        assert!(
            status.recorded && !status.authorized && !status.connected && tokens.is_empty(),
            "{agent:?}/{case}"
        );
        assert!(!status
            .attention
            .as_deref()
            .unwrap()
            .contains("sk-test-hidden"));
        assert!(sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .is_err());
        assert!(sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &options)
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), config_before);
        assert_eq!(fs::read(&auth_path).ok(), auth_before);
        assert_eq!(
            fs::read(sandbox.projector.store_path()).unwrap(),
            record_before
        );
        assert_eq!(fs::read(&token_path).unwrap(), token_before);
        disconnect(&sandbox, agent);
        assert_eq!(fs::read(&auth_path).ok(), auth_before);
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
    let provider = opencode_provider(&catalog, ENDPOINT, Path::new("token"));
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
fn projections_and_codex_defaults_use_the_same_endpoint_filter() {
    let sandbox = sandbox("endpoint-filter");
    let mut catalog = catalog();
    catalog.models[0].supported_surfaces = Some(vec![Surface::ChatCompletions]);
    catalog.models[1].supported_surfaces = Some(vec![Surface::Responses]);
    let options = ConnectOptions::default();
    for agent in [Agent::Codex, Agent::Pi] {
        apply_connect(&sandbox, agent, &catalog, &options);
    }
    assert_eq!(
        doc(&sandbox, Agent::Codex).get_str(&["model"]).as_deref(),
        Some("phala/qwen")
    );
    let bundled: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(sandbox.projector.codex_catalog_path()).unwrap())
            .unwrap();
    assert_eq!(bundled["models"].as_array().unwrap().len(), 1);
    assert_eq!(bundled["models"][0]["slug"], "phala/qwen");
    let ConfigValue::Json(provider) = doc(&sandbox, Agent::Pi)
        .get_value(&["providers", "private-ai-proxy"])
        .unwrap()
    else {
        panic!("missing Pi provider");
    };
    assert_eq!(provider["models"].as_array().unwrap().len(), 1);
    assert_eq!(provider["models"][0]["id"], "openai/gpt-oss-20b");
    assert!(sandbox
        .projector
        .preview(Agent::ClaudeCode, true, Some(&catalog), &options)
        .unwrap_err()
        .contains("No models with confirmed"));
    assert!(sandbox
        .projector
        .preview(Agent::Codex, true, Some(&catalog), &claude_options())
        .is_err());

    // Losing support must not silently replace the saved Codex selection.
    catalog.models[0].supported_surfaces = Some(vec![Surface::Responses]);
    catalog.models[1].supported_surfaces = Some(vec![]);
    assert!(sandbox
        .projector
        .preview(Agent::Codex, true, Some(&catalog), &options)
        .is_err());
}

#[test]
fn installation_detection_uses_executables_not_config_directories() {
    let sandbox = sandbox("installed");
    fs::create_dir_all(sandbox.home.join(".pi/agent")).unwrap();
    let statuses = sandbox.projector.scan(None).unwrap().0;
    assert!(
        !statuses
            .iter()
            .find(|status| status.id == "pi")
            .unwrap()
            .installed
    );

    let executable =
        sandbox
            .home
            .join(".local/bin")
            .join(if cfg!(windows) { "pi.exe" } else { "pi" });
    write(&executable, "#!/bin/sh\n");
    let statuses = sandbox.projector.scan(None).unwrap().0;
    assert!(
        statuses
            .iter()
            .find(|status| status.id == "pi")
            .unwrap()
            .installed
    );

    let codex = sandbox
        .home
        .join(".nvm/versions/node/v22.19.0/bin")
        .join(if cfg!(windows) { "codex.cmd" } else { "codex" });
    write(&codex, "#!/bin/sh\n");
    let statuses = sandbox.projector.scan(None).unwrap().0;
    assert!(
        statuses
            .iter()
            .find(|status| status.id == "codex")
            .unwrap()
            .installed
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
    assert!(error.contains("changed since the preview"));
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
    assert!(error.contains("changed since the preview"));
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
    assert!(
        error.contains("nothing was changed") || error.contains("lock"),
        "{error}"
    );
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
    let dir = env::temp_dir().join(format!("pap-atomic-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
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
    assert!(fs::read_dir(&dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
    let _ = fs::remove_dir_all(&dir);
}

/// Config drift or corruption deauthorizes the token on the next load,
/// keeps the record visible, and stays recoverable via Disconnect.
#[test]
fn drifted_or_broken_configs_deauthorize_tokens_but_stay_recoverable() {
    let sandbox = sandbox("drift");
    let path = sandbox.home.join(".claude").join("settings.json");
    write(
        &path,
        r#"{"model": "opus", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-old-secret"}}"#,
    );
    connect(&sandbox);
    assert!(!sandbox.projector.scan(None).unwrap().1.is_empty());

    // The user edits an owned field outside the app (a restart is just a
    // fresh scan, which is what the shell does at startup).
    let mut doc = doc(&sandbox, Agent::ClaudeCode);
    doc.set_str(&["env", "ANTHROPIC_MODEL"], "somewhere/else")
        .unwrap();
    write(&path, &doc.render().unwrap());
    assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
    let status = &sandbox.projector.scan(None).unwrap().0[1];
    assert!(status.recorded && !status.authorized && !status.connected);
    assert!(status
        .attention
        .as_deref()
        .unwrap()
        .contains("settings changed"));

    // Corrupt the file entirely: still recorded, error reported, token
    // still unauthorized, and Disconnect retains its recovery journal.
    write(&path, "{ not json");
    assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
    let status = &sandbox.projector.scan(None).unwrap().0[1];
    assert!(status.recorded && !status.authorized);
    assert!(status.error.is_some());
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, false, None, &ConnectOptions::default())
        .unwrap();
    assert!(sandbox
        .projector
        .apply(
            Agent::ClaudeCode,
            false,
            &preview.revision,
            None,
            &ConnectOptions::default()
        )
        .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    assert!(sandbox.projector.load_store().unwrap()["claude-code"].cleanup_pending);
    assert!(sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .is_none());
    // The parked previous secret was not consumed: it stays retrievable.
    assert!(sandbox.secrets.holds("sk-old-secret"));
}

/// Install detection is informational only: with an empty home (and no
/// CLI consulted), connect still previews and creates the official
/// settings file from scratch.
#[test]
fn connect_creates_the_official_config_from_scratch() {
    let sandbox = sandbox("fresh-home");
    let (statuses, _) = sandbox.projector.scan(None).unwrap();
    assert!(!statuses[1].installed);
    let status = connect(&sandbox);
    assert!(status.connected);
    let text = fs::read_to_string(sandbox.home.join(".claude").join("settings.json")).unwrap();
    assert!(text.contains("ANTHROPIC_BASE_URL"));
    assert!(text.contains("apiKeyHelper"));
}

/// H1 at the proxy layer: a scan is what publishes authority. A token
/// obtained while connected stops opening the proxy as soon as a scan
/// runs after the config drifted or broke — the request is refused at
/// auth and nothing reaches the sidecar.
#[tokio::test]
async fn a_scan_after_config_drift_revokes_the_old_token_at_the_proxy() {
    use crate::proxy::{router, ProxyState, Session};
    use axum::{routing::get, routing::post, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    // A counting sidecar and a verified session for it.
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let sidecar = Router::new()
        .route(
            "/v1/messages",
            post(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Json(serde_json::json!({"ok": true})) }
            }),
        )
        .route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({ "data": [{ "id": "openai/gpt-oss-20b" }] })) }),
        );
    let sidecar_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", sidecar_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(sidecar_listener, sidecar).await.unwrap() });

    let (sender, _events) = tokio::sync::mpsc::channel(8);
    let state = ProxyState::new(sender).unwrap();
    state.set_api_key(Some("sk-live".into()));
    state.publish(Session {
        generation: 1,
        epoch: 1,
        session_id: Some("test-session".to_string()),
        base_url: Some(base_url.clone()),
        verified: false,
        catalog: None,
    });
    let catalog = state.fetch_catalog(1, 1).await.unwrap();
    state.publish(Session {
        generation: 1,
        epoch: 1,
        session_id: Some("test-session".to_string()),
        base_url: Some(base_url),
        verified: true,
        catalog: Some(catalog),
    });
    state.set_tokens(sandbox.projector.scan(None).unwrap().1);
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy_listener.local_addr().unwrap());
    let app = router(state.clone());
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
        "nothing reached the sidecar"
    );
}
