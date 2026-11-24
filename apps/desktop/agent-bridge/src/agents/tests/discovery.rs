use super::*;

#[test]
fn helper_relocation_requires_explicit_reconnect_without_scan_writes() {
    for agent in Agent::ALL {
        let mut sandbox = sandbox(&format!("helper-relocation-{}", agent.id()));
        let catalog = catalog();
        let options = claude_options();
        let path = agent.config_path(&sandbox.home, false);
        apply_connect(&sandbox, agent, &catalog, &options);
        let current = sandbox.projector.helper_exe.clone();
        let assert_scan = |sandbox: &Sandbox, connected: bool, attention: Option<&str>| {
            let config = fs::read(&path).unwrap();
            let manifest = fs::read(sandbox.projector.store_path()).unwrap();
            let token_path = sandbox.projector.tokens.path(agent.id());
            let token = fs::read(&token_path).unwrap();
            let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
            let status = statuses
                .iter()
                .find(|status| status.id == agent.id())
                .unwrap();
            assert!(status.recorded);
            assert_eq!(status.connected, connected, "{}", agent.id());
            assert_eq!(status.authorized, connected, "{}", agent.id());
            assert_eq!(tokens.is_empty(), !connected);
            if let Some(attention) = attention {
                if cfg!(windows) {
                    let _ = status;
                } else {
                    assert!(status.attention.as_deref().unwrap().contains(attention));
                }
            } else {
                assert!(status.attention.is_none());
            }
            assert_eq!(fs::read(&path).unwrap(), config);
            assert_eq!(fs::read(sandbox.projector.store_path()).unwrap(), manifest);
            assert_eq!(fs::read(token_path).unwrap(), token);
        };
        assert_scan(&sandbox, true, None);
        // A recorded Pi catalog may differ from today's generated metadata.
        if agent == Agent::Pi {
            let mut config = doc(&sandbox, agent);
            config
                .set_str(&["providers", "private-ai-proxy", "name"], "Previous name")
                .unwrap();
            write(&path, &config.render().unwrap());
            let mut store = sandbox.projector.load_store().unwrap();
            store.get_mut(agent.id()).unwrap().fields[0].value =
                config.get_value(&["providers", "private-ai-proxy"]);
            sandbox.projector.save_store(&store).unwrap();
            assert_scan(&sandbox, true, None);
        }
        let stable = sandbox
            .home
            .join("stable helpers")
            .join(helper_binary_name());
        sandbox.projector.helper_exe = stable.clone();
        write_executable(&sandbox.projector.helper_exe, "helper");
        if matches!(agent, Agent::OpenCode | Agent::Dsh)
            || (agent == Agent::OpenClaw && !cfg!(windows))
        {
            assert_scan(&sandbox, true, None);
        } else {
            assert_scan(&sandbox, false, Some("Disconnect, then Connect"));
            sandbox.projector.helper_exe = current;
            assert_scan(&sandbox, true, None);
            sandbox.projector.helper_exe = stable;
        }
        // External edits take precedence over helper relocation and survive cleanup.
        let mut config = doc(&sandbox, agent);
        let field = match agent {
            Agent::Codex => &["model_provider"][..],
            Agent::ClaudeCode => &["apiKeyHelper"][..],
            Agent::OpenCode => &["provider", "private-ai-proxy", "options", "baseURL"][..],
            Agent::Pi => &["providers", "private-ai-proxy", "apiKey"][..],
            Agent::Hermes => &["providers", "private-ai-proxy", "key_cmd"][..],
            Agent::OpenClaw => &["models", "providers", "private-ai-proxy", "baseUrl"][..],
            Agent::OhMyPi => &["providers", "private-ai-proxy", "apiKey"][..],
            Agent::Dsh => &["config", "provider"][..],
        };
        // dsh's fields live in keyed items of its patch list.
        let item = EntryKey {
            key: "id".into(),
            id: "agent-default-model".into(),
        };
        let edited = |config: &ConfigDoc| match agent {
            Agent::Dsh => config.entry(&item).unwrap().unwrap(),
            _ => config.clone(),
        };
        if agent == Agent::Dsh {
            edited(&config).set_str(field, "external-edit").unwrap();
        } else {
            config.set_str(field, "external-edit").unwrap();
        }
        write(&path, &config.render().unwrap());
        assert_scan(&sandbox, false, Some("settings changed"));
        disconnect(&sandbox, agent);
        assert_eq!(
            edited(&doc(&sandbox, agent)).get_str(field).as_deref(),
            Some("external-edit"),
        );
    }
}

#[test]
fn changing_a_connected_agents_model_keeps_access() {
    let catalog = catalog();
    for agent in [
        Agent::Codex,
        Agent::ClaudeCode,
        Agent::OpenCode,
        Agent::Hermes,
        Agent::OpenClaw,
        Agent::Dsh,
    ] {
        let sandbox = sandbox(&format!("model-selection-{}", agent.id()));
        apply_connect(&sandbox, agent, &catalog, &claude_options());
        let path = agent.config_path(&sandbox.home, false);
        let mut config = doc(&sandbox, agent);
        for selection in model_selection_paths(agent) {
            let value = if matches!(agent, Agent::OpenCode | Agent::OpenClaw) {
                "private-ai-proxy/phala/qwen"
            } else {
                "phala/qwen"
            };
            if let Some(entry) = selection.entry {
                config
                    .entry(&EntryKey::id(entry))
                    .unwrap()
                    .unwrap()
                    .set_str(selection.path, value)
                    .unwrap();
            } else {
                config.set_str(selection.path, value).unwrap();
            }
        }
        write(&path, &config.render().unwrap());
        let status = sandbox
            .projector
            .scan(Some(&catalog))
            .unwrap()
            .0
            .into_iter()
            .find(|status| status.id == agent.id())
            .unwrap();
        assert!(status.connected, "{}: {:?}", agent.id(), status);
        assert!(status.authorized, "{}: {:?}", agent.id(), status);
    }
}

#[test]
fn claude_provider_switch_flags_are_removed_and_restored() {
    let sandbox = sandbox("claude-provider-switch");
    let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
    write(
        &path,
        r#"{"env":{"CLAUDE_CODE_USE_BEDROCK":"1","CLAUDE_CODE_USE_VERTEX":"1","CLAUDE_CODE_USE_FOUNDRY":"1","CLAUDE_CODE_USE_MANTLE":"1"}}"#,
    );
    apply_connect(&sandbox, Agent::ClaudeCode, &catalog(), &claude_options());
    let connected = doc(&sandbox, Agent::ClaudeCode);
    for flag in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_MANTLE",
    ] {
        assert_eq!(connected.get_str(&["env", flag]), None);
    }
    disconnect(&sandbox, Agent::ClaudeCode);
    let restored = doc(&sandbox, Agent::ClaudeCode);
    for flag in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_MANTLE",
    ] {
        assert_eq!(restored.get_str(&["env", flag]).as_deref(), Some("1"));
    }
}

#[test]
fn openclaw_user_edit_to_entry_string_survives_disconnect() {
    let sandbox = sandbox("openclaw-entry-model");
    let path = Agent::OpenClaw.config_path(&sandbox.home, false);
    write(
        &path,
        r#"{"agents":{"entries":{"main":{"model":"private-ai-proxy/openai/gpt-oss-20b"}}}}"#,
    );
    apply_connect(&sandbox, Agent::OpenClaw, &catalog(), &claude_options());
    let store = sandbox.projector.load_store().unwrap();
    assert!(!store[Agent::OpenClaw.id()]
        .fields
        .iter()
        .any(|field| field.path.starts_with(&owned(&["agents", "entries"]))));
    let mut config = doc(&sandbox, Agent::OpenClaw);
    config
        .set_str(
            &["agents", "entries", "main", "model"],
            "private-ai-proxy/phala/qwen",
        )
        .unwrap();
    write(&path, &config.render().unwrap());
    disconnect(&sandbox, Agent::OpenClaw);
    assert_eq!(
        doc(&sandbox, Agent::OpenClaw)
            .get_str(&["agents", "entries", "main", "model"])
            .as_deref(),
        Some("private-ai-proxy/phala/qwen")
    );
}

#[test]
fn opencode_external_model_is_reported_as_unrouted() {
    let sandbox = sandbox("external-opencode-model");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::OpenCode, &catalog, &claude_options());
    let path = Agent::OpenCode.config_path(&sandbox.home, false);
    let mut config = doc(&sandbox, Agent::OpenCode);
    config
        .set_str(&["model"], "other/provider-model#fast")
        .unwrap();
    write(&path, &config.render().unwrap());
    let status = sandbox
        .projector
        .scan(Some(&catalog))
        .unwrap()
        .0
        .into_iter()
        .find(|status| status.id == Agent::OpenCode.id())
        .unwrap();
    assert!(status.authorized, "{status:?}");
    assert!(!status.connected, "{status:?}");
    assert!(status.attention.unwrap().contains("not routed"));
}

#[test]
fn openclaw_external_entry_object_is_attention_only() {
    let sandbox = sandbox("openclaw-external-entry-object");
    let path = Agent::OpenClaw.config_path(&sandbox.home, false);
    write(&path, "{}");
    apply_connect(&sandbox, Agent::OpenClaw, &catalog(), &claude_options());
    let mut config = doc(&sandbox, Agent::OpenClaw);
    config
        .set_value(
            &["agents", "entries", "main", "model"],
            &ConfigValue::Json(serde_json::json!({
                "primary": "other/model",
                "fallbacks": ["private-ai-proxy/phala/qwen"]
            })),
        )
        .unwrap();
    write(&path, &config.render().unwrap());
    let status = sandbox
        .projector
        .scan(Some(&catalog()))
        .unwrap()
        .0
        .into_iter()
        .find(|status| status.id == Agent::OpenClaw.id())
        .unwrap();
    assert!(status.authorized);
    assert!(!status.connected);
    assert!(status.attention.unwrap().contains("not routed"));
}

#[test]
fn hermes_rejects_invalid_active_profile_names() {
    let sandbox = sandbox("hermes-invalid-profile");
    let root = if cfg!(windows) {
        sandbox.home.join("AppData").join("Local").join("hermes")
    } else {
        sandbox.home.join(".hermes")
    };
    write(&root.join("active_profile"), "../escape\n");
    let error = sandbox
        .projector
        .preview(Agent::Hermes, true, Some(&catalog()), &claude_options())
        .unwrap_err()
        .to_string();
    assert!(error.contains("active_profile"));
}

#[test]
fn omp_filters_are_read_from_companion_config() {
    let sandbox = sandbox("omp-companion-filters");
    let models = Agent::OhMyPi.config_path(&sandbox.home, false);
    write(&models, "providers: {}\n");
    write(
        &models.with_file_name("config.yml"),
        "disabledProviders:\n  - private-ai-proxy\n",
    );
    let error = sandbox
        .projector
        .preview(Agent::OhMyPi, true, Some(&catalog()), &claude_options())
        .unwrap_err()
        .to_string();
    assert!(error.contains("disabledProviders"));
}

#[cfg(windows)]
#[test]
fn windows_projector_connects_with_only_installed_helper() {
    let sandbox = sandbox("windows-installed-helper");
    let projector = Projector::new(
        sandbox.projector.helper_exe.clone(),
        ENDPOINT,
        sandbox.secrets.clone(),
    )
    .unwrap();
    write(&Agent::ClaudeCode.config_path(&sandbox.home, false), "{}");
    assert!(projector
        .preview(Agent::ClaudeCode, true, Some(&catalog()), &claude_options())
        .is_ok());
}

#[test]
fn legacy_claude_journal_migrates_provider_switch_flags() {
    let sandbox = sandbox("legacy-claude-journal");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::ClaudeCode, &catalog, &claude_options());
    let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
    let mut config = doc(&sandbox, Agent::ClaudeCode);
    config
        .set_str(&["env", "CLAUDE_CODE_USE_BEDROCK"], "1")
        .unwrap();
    config
        .set_str(&["env", "ANTHROPIC_MODEL"], "legacy-model")
        .unwrap();
    write(&path, &config.render().unwrap());
    let mut store = sandbox.projector.load_store().unwrap();
    store
        .get_mut(Agent::ClaudeCode.id())
        .unwrap()
        .fields
        .retain(|field| field.path != owned(&["env", "CLAUDE_CODE_USE_BEDROCK"]));
    store
        .get_mut(Agent::ClaudeCode.id())
        .unwrap()
        .fields
        .push(OwnedField {
            path: owned(&["env", "ANTHROPIC_MODEL"]),
            value: Some(ConfigValue::Str("legacy-model".into())),
            ..OwnedField::default()
        });
    sandbox.projector.save_store(&store).unwrap();
    let status = sandbox
        .projector
        .scan(Some(&catalog))
        .unwrap()
        .0
        .into_iter()
        .find(|status| status.id == Agent::ClaudeCode.id())
        .unwrap();
    assert!(!status.authorized);
    sandbox.projector.reconcile(Some(&catalog)).unwrap();
    assert_eq!(
        doc(&sandbox, Agent::ClaudeCode).get_str(&["env", "CLAUDE_CODE_USE_BEDROCK"]),
        None
    );
}

#[test]
fn audit_generated_catalog_edits_do_not_deauthorize() {
    let sandbox = sandbox("audit-generated-catalog");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::OpenCode, &catalog, &claude_options());
    let path = Agent::OpenCode.config_path(&sandbox.home, false);
    let mut config = doc(&sandbox, Agent::OpenCode);
    config
        .set_value(
            &["provider", "private-ai-proxy", "models"],
            &ConfigValue::Json(serde_json::json!({})),
        )
        .unwrap();
    write(&path, &config.render().unwrap());
    let status = sandbox
        .projector
        .scan(Some(&catalog))
        .unwrap()
        .0
        .into_iter()
        .find(|status| status.id == Agent::OpenCode.id())
        .unwrap();
    assert!(status.connected, "{status:?}");
    assert!(status.authorized, "{status:?}");
}

#[test]
fn audit_model_routing_override_deauthorizes() {
    let sandbox = sandbox("audit-model-routing");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::OpenCode, &catalog, &claude_options());
    let path = Agent::OpenCode.config_path(&sandbox.home, false);
    let mut config = doc(&sandbox, Agent::OpenCode);
    config
        .set_str(
            &[
                "provider",
                "private-ai-proxy",
                "models",
                "openai/gpt-oss-20b",
                "options",
                "baseURL",
            ],
            "https://example.invalid/v1",
        )
        .unwrap();
    write(&path, &config.render().unwrap());
    let status = sandbox
        .projector
        .scan(Some(&catalog))
        .unwrap()
        .0
        .into_iter()
        .find(|status| status.id == Agent::OpenCode.id())
        .unwrap();
    assert!(!status.authorized, "{status:?}");
}

#[test]
fn audit_drift_does_not_restore_or_revoke_until_disconnect() {
    let sandbox = sandbox("audit-drift-no-suspend");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::ClaudeCode, &catalog, &claude_options());
    let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
    let mut config = doc(&sandbox, Agent::ClaudeCode);
    config
        .set_str(&["env", "ANTHROPIC_BASE_URL"], "http://127.0.0.1:9999")
        .unwrap();
    let edited = config.render().unwrap();
    write(&path, &edited);
    let token_path = sandbox.projector.tokens.path(Agent::ClaudeCode.id());
    let token = fs::read(&token_path).unwrap();
    sandbox.projector.reconcile(Some(&catalog)).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    assert_eq!(fs::read(&token_path).unwrap(), token);
    assert!(!sandbox.projector.load_store().unwrap()[Agent::ClaudeCode.id()].suspended);
}

#[test]
fn audit_agent_helpers_use_the_staged_helper_location() {
    let mut sandbox = sandbox("audit-staged-helper");
    let staged = sandbox
        .projector
        .data_dir
        .join("helpers")
        .join(helper_binary_name());
    sandbox.projector.set_staged_helper(staged);
    let catalog = catalog();
    apply_connect(&sandbox, Agent::ClaudeCode, &catalog, &claude_options());
    let config = doc(&sandbox, Agent::ClaudeCode);
    let helper = config.get_str(&["apiKeyHelper"]).unwrap();
    if cfg!(windows) {
        assert!(!helper.contains("helpers"));
    } else {
        assert!(helper.contains(
            &sandbox
                .projector
                .data_dir
                .join("helpers")
                .display()
                .to_string()
        ));
    }
}

#[test]
fn audit_record_attention_is_visible_while_connected() {
    let sandbox = sandbox("audit-record-attention");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::ClaudeCode, &catalog, &claude_options());
    let mut store = sandbox.projector.load_store().unwrap();
    store.get_mut(Agent::ClaudeCode.id()).unwrap().attention = Some("audit attention".into());
    sandbox.projector.save_store(&store).unwrap();
    let status = sandbox
        .projector
        .scan(Some(&catalog))
        .unwrap()
        .0
        .into_iter()
        .find(|status| status.id == Agent::ClaudeCode.id())
        .unwrap();
    assert_eq!(status.attention.as_deref(), Some("audit attention"));
}

#[test]
fn hermes_paths_follow_platform_overrides_and_isolate_test_home() {
    const CASE_ENV: &str = "PAP_TEST_HERMES_PATH_CASE";
    const ROOT_ENV: &str = "PAP_TEST_HERMES_PATH_ROOT";
    if let Ok(case) = env::var(CASE_ENV) {
        let root = PathBuf::from(env::var_os(ROOT_ENV).unwrap());
        let home = root.join(if case == "isolated" {
            "isolated"
        } else {
            "user"
        });
        let default = if cfg!(windows) {
            home.join("AppData").join("Local").join("hermes")
        } else {
            home.join(".hermes")
        };
        let expected = match case.as_str() {
            "override" => root.join("custom-profile"),
            "local-appdata" if cfg!(windows) => root.join("local-data").join("hermes"),
            _ => default,
        };
        let projector = Projector::new(
            root.join(helper_binary_name()),
            ENDPOINT,
            Arc::new(MemoryStore::default()),
        )
        .unwrap();
        assert_eq!(projector.home, home);
        assert_eq!(
            Agent::Hermes.config_path(&projector.home, projector.tool_env),
            expected.join("config.yaml")
        );
        assert_eq!(
            Agent::Pi.config_path(&projector.home, projector.tool_env),
            if case == "isolated" {
                home.join(".pi").join("agent").join("models.json")
            } else {
                root.join("pi-override").join("models.json")
            }
        );
        if case == "isolated" {
            assert!(!projector.tool_env);
            assert_eq!(projector.data_dir, home.join(".private-ai-proxy"));
            for agent in Agent::ALL {
                assert!(agent
                    .config_path(&projector.home, projector.tool_env)
                    .starts_with(&home));
            }
        }
        // Detection follows the same overrides as the config location.
        if case == "override" {
            fs::create_dir_all(root.join("custom-profile")).unwrap();
            assert!(detected(Agent::Hermes, &projector.home, projector.tool_env));
        }
        if case == "isolated" {
            fs::create_dir_all(root.join("outside-codex")).unwrap();
            assert!(!detected(Agent::Codex, &projector.home, projector.tool_env));
        }
        return;
    }

    // Process-local environment avoids races with the other agent tests.
    let root = tempfile::tempdir().unwrap();
    for case in [
        "default",
        "local-appdata",
        "override",
        "isolated",
        "native-home",
    ] {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "agents::tests::discovery::hermes_paths_follow_platform_overrides_and_isolate_test_home",
            ])
            .env(CASE_ENV, case)
            .env(ROOT_ENV, root.path())
            .env("HOME", root.path().join("user"))
            .env("USERPROFILE", root.path().join("user"))
            .env("APPDATA", root.path().join("roaming"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("PI_CODING_AGENT_DIR", root.path().join("pi-override"))
            .env("PI_AGENT_DIR", root.path().join("unused-pi-dir"))
            .env("PATH", "")
            .env_remove(HOME_OVERRIDE_ENV)
            .env_remove("HERMES_HOME")
            .env_remove("LOCALAPPDATA");
        if matches!(case, "local-appdata" | "override" | "isolated") {
            command.env("LOCALAPPDATA", root.path().join("local-data"));
        }
        if matches!(case, "override" | "isolated") {
            command.env("HERMES_HOME", root.path().join("custom-profile"));
        }
        if case == "isolated" {
            command
                .env(HOME_OVERRIDE_ENV, root.path().join("isolated"))
                .env("CODEX_HOME", root.path().join("outside-codex"));
        }
        if cfg!(windows) && case == "native-home" {
            command.env("HOME", root.path().join("git-home"));
        }
        let output = command.output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
        assert!(output.status.success(), "{case}: {output:?}");
    }
}

#[test]
fn hermes_active_profile_selects_the_profile_config() {
    let sandbox = sandbox("hermes-active-profile");
    let root = if cfg!(windows) {
        sandbox.home.join("AppData").join("Local").join("hermes")
    } else {
        sandbox.home.join(".hermes")
    };
    write(&root.join("active_profile"), "work\n");
    assert_eq!(
        Agent::Hermes.config_path(&sandbox.home, false),
        root.join("profiles").join("work").join("config.yaml")
    );
}

#[test]
fn explicit_home_projector_scans_the_authorized_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("authorized-home");
    let data_dir = root.path().join("app-data");
    let helper = root.path().join(helper_binary_name());
    write_executable(&helper, "#!/bin/sh\n");
    fs::create_dir_all(home.join(".codex")).unwrap();

    let projector = Projector::new_for_home(
        home.clone(),
        data_dir,
        helper,
        ENDPOINT,
        Arc::new(MemoryStore::default()),
    )
    .unwrap();
    let statuses = projector.scan(None).unwrap().0;
    let codex_status = statuses.iter().find(|status| status.id == "codex").unwrap();

    assert!(codex_status.installed);
    assert_eq!(
        PathBuf::from(&codex_status.config_path),
        home.join(".codex").join("config.toml")
    );
}

/// Every install method leaves the CLI somewhere different, but each agent
/// creates the same documented configuration folder on first run. Fixtures
/// mirror what each method leaves in Home.
#[test]
fn detection_follows_the_configuration_folder_for_every_install_method() {
    let hermes = if cfg!(windows) {
        "AppData/Local/hermes"
    } else {
        ".hermes"
    };
    // (agent, CLI left by the install method, folder created on first run)
    let layouts = [
        // Superset runs Codex through its own wrapper and merges hooks.json.
        (Agent::Codex, ".superset/bin/codex", ".codex/hooks.json"),
        (Agent::Codex, ".npm-global/bin/codex", ".codex/config.toml"),
        (Agent::ClaudeCode, ".local/bin/claude", ".claude/sessions"),
        (
            Agent::OpenCode,
            ".opencode/bin/opencode",
            ".config/opencode",
        ),
        (Agent::Pi, ".bun/bin/pi", ".pi/agent"),
        (Agent::OhMyPi, ".local/bin/omp", ".omp/agent/agent.db"),
        (Agent::Hermes, ".local/bin/hermes", hermes),
        (
            Agent::OpenClaw,
            ".volta/bin/openclaw",
            ".openclaw/migration",
        ),
    ];
    for (agent, cli, state) in layouts {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        write_executable(&home.join(cli), "launcher");
        assert!(
            !detected(agent, home, false),
            "{}: installed but never run",
            agent.id()
        );
        let state = home.join(state);
        if state.extension().is_some() {
            write(&state, "");
        } else {
            fs::create_dir_all(&state).unwrap();
        }
        assert!(detected(agent, home, false), "{}", agent.id());
    }

    // A dotfiles-managed folder is a symlink; one left after the CLI was
    // removed still counts, because connecting there is harmless.
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().unwrap();
        let dotfiles = root.path().join("dotfiles/codex");
        fs::create_dir_all(&dotfiles).unwrap();
        std::os::unix::fs::symlink(&dotfiles, root.path().join(".codex")).unwrap();
        assert!(detected(Agent::Codex, root.path(), false));
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires the installed helper and Python on a disposable Windows runner"]
fn hermes_windows_installed_command_round_trip() {
    let installed = PathBuf::from(env::var_os("PAP_TEST_HELPER_PATH").unwrap());
    assert!(installed.is_absolute() && installed.is_file());
    assert!(installed.to_str().unwrap().contains(' '));
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".private-ai-proxy");
    let tokens = TokenFiles::new(&data);
    let hostile = home
        .path()
        .join("quote'\u{2019}; Write-Output injected; # %PAP_CMD_PROBE% ! ^ & (meta)")
        .join(helper_binary_name());
    fs::create_dir_all(hostile.parent().unwrap()).unwrap();
    fs::copy(&installed, &hostile).unwrap();
    for executable in [&installed, &hostile] {
        let token = tokens.ensure("hermes").unwrap();
        let fields = fields(
            Agent::Hermes,
            &Inputs {
                file_credentials: false,
                endpoint: ENDPOINT,
                helper_exe: executable,
                token_path: &tokens.path("hermes"),
                codex_catalog_path: &data.join(CODEX_CATALOG_FILE),
                catalog: Some(&catalog()),
                options: &ConnectOptions::default(),
            },
        )
        .unwrap();
        let command = fields
            .into_iter()
            .find(|field| field.path == owned(&["providers", "private-ai-proxy", "key_cmd"]))
            .and_then(|field| match field.value {
                Some(ConfigValue::Str(command)) => Some(command),
                _ => None,
            })
            .unwrap();
        let run = || {
            Command::new("python")
                .args([
                    "-c",
                    "import subprocess,sys; p=subprocess.run(sys.argv[1],shell=True,capture_output=True,text=True,timeout=15); sys.stdout.write(p.stdout); sys.stderr.write(p.stderr); sys.exit(p.returncode)",
                    &command,
                ])
                .env(HOME_OVERRIDE_ENV, home.path())
                .env("PAP_CMD_PROBE", "expanded-by-shell")
                .output()
                .unwrap()
        };
        let output = run();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), token);
        fs::remove_file(tokens.path("hermes")).unwrap();
        let output = run();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        if executable == &hostile {
            fs::remove_file(executable).unwrap();
            let output = run();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
        }
    }
    assert!(credential_helper_command(Path::new("bad\0path"), Agent::Hermes).is_err());
}

/// POSIX quoting round-trips through shlex and, where available, sh.
/// This does not prove which shell the Windows Claude CLI selects.
#[test]
fn helper_command_quotes_hostile_paths_for_the_shell() {
    for hostile in [
        "/Applications/Private AI Proxy.app/Contents/MacOS/helper",
        "/tmp/it's here/$HOME`echo`;rm -rf/helper",
        "/tmp/quote\"double\"/helper",
    ] {
        let command = helper_command(Path::new(hostile), "claude-code").unwrap();
        let suffix = " --agent-token claude-code";
        assert!(command.ends_with(suffix));
        let quoted = &command[..command.len() - suffix.len()];
        assert_eq!(shlex::split(quoted), Some(vec![hostile.to_string()]));
        match std::process::Command::new("sh")
            .args(["-c", &format!("printf %s {quoted}")])
            .output()
        {
            Ok(output) => {
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout),
                    hostile,
                    "{command}"
                )
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot run sh: {error}"),
        }
    }
    #[cfg(windows)]
    {
        use base64::{engine::general_purpose::STANDARD, Engine};

        // Pi's Bash and cmd.exe fallback see only fixed switches and a
        // base64 word. Neither shell interprets the Windows helper path.
        let hostile = Path::new(r"C:\Users\O'Brien %USERPROFILE% ! &\helper.exe");
        let provider = pi_provider(
            &catalog(),
            &api_url(ENDPOINT).unwrap(),
            &credential_helper_command(hostile, Agent::Pi).unwrap(),
        )
        .unwrap();
        let command = provider["apiKey"]
            .as_str()
            .unwrap()
            .strip_prefix('!')
            .unwrap();
        let words: Vec<_> = command.split_ascii_whitespace().collect();
        assert_eq!(words.len(), 5);
        assert_eq!(
            &words[..4],
            &[
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand"
            ]
        );
        assert_eq!(shlex::split(command).unwrap(), words);
        let bytes = STANDARD.decode(words[4]).unwrap();
        let script = String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let encoded_path = STANDARD.encode(
            hostile
                .to_str()
                .unwrap()
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        assert!(script.contains(&format!("FromBase64String('{encoded_path}')")));
        assert!(script.ends_with("--agent-token pi; exit $LASTEXITCODE"));
    }
}

#[test]
fn omp_enabled_models_keep_the_selected_pap_model_authorized() {
    let catalog = catalog();
    for pattern in [
        "private-ai-proxy/openai/gpt-oss-20b",
        "openai/gpt-oss-20b",
        "OPENAI/gpt-oss-?0[b]",
        "private-ai-proxy/**/gpt-oss-*",
        "",
    ] {
        let sandbox = sandbox("omp-enabled-models");
        let settings = Agent::OhMyPi
            .config_path(&sandbox.home, false)
            .with_file_name("config.yml");
        let filter = if pattern.is_empty() {
            "enabledModels: []\n".into()
        } else {
            format!("enabledModels: ['{pattern}']\n")
        };
        write(&settings, &filter);
        let status = apply_connect(&sandbox, Agent::OhMyPi, &catalog, &claude_options());
        assert!(status.authorized, "{pattern}: {status:?}");
        for catalog in [Some(&catalog), None] {
            let (statuses, tokens) = sandbox.projector.scan(catalog).unwrap();
            let status = agent_status(&statuses, Agent::OhMyPi);
            assert!(
                status.authorized && !tokens.is_empty(),
                "{pattern}: {status:?}"
            );
        }
    }
    let sandbox = sandbox("omp-enabled-excludes-pap");
    let agent = Agent::OhMyPi;
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    write(
        &agent
            .config_path(&sandbox.home, false)
            .with_file_name("config.yml"),
        "enabledModels: [other/model]
",
    );
    let (statuses, tokens) = sandbox.projector.scan(Some(&catalog)).unwrap();
    let status = agent_status(&statuses, agent);
    assert!(!status.authorized && tokens.is_empty(), "{status:?}");
}

#[test]
fn omp_companion_config_yaml_can_disable_pap() {
    let sandbox = sandbox("omp-disabled-yaml");
    let agent = Agent::OhMyPi;
    let catalog = catalog();
    let settings = agent
        .config_path(&sandbox.home, false)
        .with_file_name("config.yaml");
    write(&settings, "disabledProviders: []\n");
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    assert!(!settings.with_file_name("config.yml").exists());
    write(&settings, "disabledProviders: [private-ai-proxy]\n");
    let (statuses, tokens) = sandbox.projector.scan(Some(&catalog)).unwrap();
    let status = agent_status(&statuses, agent);
    assert!(!status.authorized && tokens.is_empty(), "{status:?}");
    assert!(status
        .attention
        .as_deref()
        .unwrap()
        .contains("disabledProviders"));
}

#[test]
fn opencode_without_program_data_keeps_the_merged_selector() {
    const CASE: &str = "PAP_TEST_NO_PROGRAM_DATA";
    if env::var_os(CASE).is_none() {
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "agents::tests::discovery::opencode_without_program_data_keeps_the_merged_selector",
            ])
            .env(CASE, "1")
            .env_remove("ProgramData")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }

    let sandbox = sandbox("opencode-no-system-layer");
    let path = Agent::OpenCode.config_path(&sandbox.home, false);
    write(&path, r#"{"model":"other/model"}"#);
    let system_dir = validation::opencode_system_config_dir("windows");
    assert_eq!(system_dir, None);
    assert_eq!(
        sandbox
            .projector
            .merged_opencode_selector(&doc(&sandbox, Agent::OpenCode), system_dir),
        Some(json!("other/model"))
    );
    if cfg!(windows) {
        let agent = Agent::OpenCode;
        let catalog = catalog();
        assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
        let mut config = doc(&sandbox, agent);
        config.set_str(&["model"], "other/model").unwrap();
        write(&path, &config.render().unwrap());
        let (statuses, tokens) = sandbox.projector.scan(Some(&catalog)).unwrap();
        let status = agent_status(&statuses, agent);
        assert!(status.authorized && !tokens.is_empty(), "{status:?}");
        assert!(
            status
                .attention
                .as_deref()
                .is_some_and(|s| s.contains("not routed")),
            "{status:?}"
        );
    }
}

#[test]
fn openclaw_connect_preserves_existing_entry_objects_and_checks_fallbacks() {
    for entry in [
        json!({"primary": "other/model", "fallbacks": ["private-ai-proxy/phala/qwen"]}),
        json!({"fallbacks": ["other/model"]}),
    ] {
        let sandbox = sandbox("openclaw-existing-entry-object");
        let agent = Agent::OpenClaw;
        let path = agent.config_path(&sandbox.home, false);
        write(
            &path,
            &json!({"agents":{"entries":{"main":{"model":entry}}}}).to_string(),
        );
        let status = apply_connect(&sandbox, agent, &catalog(), &claude_options());
        assert!(status.authorized, "{entry}: {status:?}");
        assert!(
            status
                .attention
                .as_deref()
                .is_some_and(|s| s.contains("not routed")),
            "{entry}: {status:?}"
        );
        let journal = sandbox.projector.load_store().unwrap();
        assert!(journal[agent.id()]
            .fields
            .iter()
            .all(|field| !field.path.starts_with(&owned(&["agents", "entries"]))));
        assert_eq!(
            doc(&sandbox, agent).get_value(&["agents", "entries", "main", "model"]),
            Some(ConfigValue::Json(entry))
        );
    }
}

#[test]
fn claude_v090_model_journal_migrates_without_a_provider_switch() {
    let sandbox = sandbox("claude-v090-model-journal");
    let agent = Agent::ClaudeCode;
    let path = agent.config_path(&sandbox.home, false);
    let catalog = catalog();
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let mut config = doc(&sandbox, agent);
    config.remove(&["model"]).unwrap();
    config
        .set_str(&["env", "ANTHROPIC_MODEL"], "openai/gpt-oss-20b")
        .unwrap();
    write(&path, &config.render().unwrap());
    let mut store = sandbox.projector.load_store().unwrap();
    let fields = &mut store.get_mut(agent.id()).unwrap().fields;
    // Claude Code projection paths shipped at desktop-v0.9.0 in providers.rs.
    fields.retain(|field| {
        field.path != ["model"]
            && field.path != ["env", "ANTHROPIC_MODEL"]
            && !field
                .path
                .last()
                .is_some_and(|key| key.starts_with("CLAUDE_CODE_USE_"))
    });
    fields.push(OwnedField {
        path: owned(&["env", "ANTHROPIC_MODEL"]),
        value: Some(ConfigValue::Str("openai/gpt-oss-20b".into())),
        previous: Some(Previous::Plain(ConfigValue::Str("claude-original".into()))),
        ..OwnedField::default()
    });
    sandbox.projector.save_store(&store).unwrap();

    let (statuses, _) = sandbox.projector.scan(Some(&catalog)).unwrap();
    let status = agent_status(&statuses, agent);
    assert!(!status.authorized);
    assert!(status
        .attention
        .as_deref()
        .unwrap()
        .contains("Legacy Claude Code"));
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    let config = doc(&sandbox, agent);
    assert_eq!(config.get_str(&["env", "ANTHROPIC_MODEL"]), None);
    assert_eq!(
        config.get_str(&["model"]).as_deref(),
        Some("openai/gpt-oss-20b")
    );
    let (statuses, _) = sandbox.projector.scan(Some(&catalog)).unwrap();
    assert!(agent_status(&statuses, agent).authorized);
    assert!(agent_status(&statuses, agent).attention.is_none());
    let migrated = sandbox.projector.load_store().unwrap();
    assert!(migrated[agent.id()]
        .fields
        .iter()
        .any(|field| field.path == ["model"]
            && field.value == Some(ConfigValue::Str("openai/gpt-oss-20b".into()))));
    disconnect(&sandbox, agent);
    let config = doc(&sandbox, agent);
    assert_eq!(
        config.get_str(&["env", "ANTHROPIC_MODEL"]).as_deref(),
        Some("claude-original")
    );
    assert_eq!(config.get_str(&["model"]), None);
}

#[test]
fn fresh_claude_model_journal_needs_no_migration() {
    let sandbox = sandbox("fresh-claude-model-journal");
    let catalog = catalog();
    let agent = Agent::ClaudeCode;
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let (statuses, _) = sandbox.projector.scan(Some(&catalog)).unwrap();
    let status = agent_status(&statuses, agent);
    assert!(
        status.authorized && status.attention.is_none(),
        "{status:?}"
    );
}
