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
                assert!(status.attention.as_deref().unwrap().contains(attention));
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
        if agent == Agent::OpenCode {
            assert_scan(&sandbox, true, None);
        } else if agent == Agent::OpenClaw {
            assert_scan(
                &sandbox,
                false,
                Some(if cfg!(unix) {
                    "differs"
                } else {
                    "helper changed"
                }),
            );
            sandbox.projector.helper_exe = current;
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
            Agent::OpenCode => &["model"][..],
            Agent::Pi => &["providers", "private-ai-proxy", "apiKey"][..],
            Agent::Hermes => &["providers", "private-ai-proxy", "key_cmd"][..],
            Agent::OpenClaw => &["agents", "defaults", "model", "primary"][..],
            Agent::OhMyPi => &["providers", "private-ai-proxy", "apiKey"][..],
        };
        config.set_str(field, "external-edit").unwrap();
        write(&path, &config.render().unwrap());
        assert_scan(&sandbox, false, Some("settings changed"));
        disconnect(&sandbox, agent);
        assert_eq!(
            doc(&sandbox, agent).get_str(field).as_deref(),
            Some("external-edit"),
        );
    }
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
        if case == "path" {
            let executable = PathBuf::from(env::var_os("PAP_TEST_HERMES_PATH_ROOT").unwrap())
                .join("path-bin")
                .join(if cfg!(windows) { "codex.exe" } else { "codex" });
            assert!(projector.tool_env);
            assert_eq!(find_cli(Agent::Codex, &projector.home, true), Some(executable));
        }
        if cfg!(windows) {
            let executable = expected.join("bin").join("hermes.exe");
            write(&executable, "launcher fixture");
            assert_eq!(
                find_cli(Agent::Hermes, &projector.home, projector.tool_env),
                Some(executable)
            );
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
        "path",
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
        if case == "path" {
            let path = root.path().join("path-bin");
            fs::create_dir_all(&path).unwrap();
            write_executable(
                &path.join(if cfg!(windows) { "codex.exe" } else { "codex" }),
                "launcher",
            );
            command.env("PATH", &path);
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
fn explicit_home_projector_scans_the_authorized_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("authorized-home");
    let data_dir = root.path().join("app-data");
    let helper = root.path().join(helper_binary_name());
    write_executable(&helper, "#!/bin/sh\n");
    let codex = home.join(".local/bin").join(if cfg!(windows) {
        "codex.exe"
    } else {
        "codex"
    });
    write_executable(&codex, "#!/bin/sh\n");

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
        codex_status.config_path,
        home.join(".codex/config.toml").display().to_string()
    );
    assert!(!cli_paths(&home, false).contains(&PathBuf::from("/opt/homebrew/bin")));
    assert!(!cli_paths(&home, false).contains(&PathBuf::from("/usr/local/bin")));
}

#[test]
fn common_home_cli_layouts_and_symlinks_are_detected() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let layouts = [
        (Agent::Codex, ".local/bin"),
        (Agent::ClaudeCode, ".local/share/pnpm"),
        (Agent::Hermes, ".cargo/bin"),
        (Agent::Pi, ".npm-global/bin"),
        (Agent::OhMyPi, "Library/pnpm"),
        (Agent::OpenCode, ".opencode/bin"),
        (Agent::OpenClaw, ".volta/bin"),
        (Agent::ClaudeCode, ".local/share/mise/shims"),
        (Agent::Codex, ".asdf/shims"),
        (Agent::Pi, ".rtx/shims"),
    ];

    for (agent, layout) in layouts {
        let name = agent.cli_names()[0];
        let command = home.join(layout).join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = command.with_extension("real");
            write_executable(&target, "#!/bin/sh\n");
            fs::create_dir_all(command.parent().unwrap()).unwrap();
            symlink(&target, &command).unwrap();
        }
        #[cfg(windows)]
        write_executable(&command, "launcher");
        assert_eq!(find_cli(agent, &home, false), Some(command), "{}", agent.id());
    }

    assert!(cli_paths(&home, false).contains(&home.join(".bun/bin")));
    for directory in [
        ".local/share/mise/shims",
        ".mise/shims",
        ".asdf/shims",
        ".local/share/rtx/shims",
        ".rtx/shims",
    ] {
        assert!(cli_paths(&home, false).contains(&home.join(directory)));
    }

    let nvm_home = root.path().join("nvm-home");
    let nvm = nvm_home
        .join(".nvm/versions/node/v22.19.0/bin")
        .join(if cfg!(windows) { "codex.exe" } else { "codex" });
    write_executable(&nvm, "launcher");
    assert_eq!(find_cli(Agent::Codex, &nvm_home, false), Some(nvm));

    let fnm_home = root.path().join("fnm-home");
    let fnm = fnm_home
        .join(".local/share/fnm/node-versions/v22.19.0/installation/bin")
        .join(if cfg!(windows) { "pi.exe" } else { "pi" });
    write_executable(&fnm, "launcher");
    assert_eq!(find_cli(Agent::Pi, &fnm_home, false), Some(fnm));

    #[cfg(windows)]
    for extension in ["cmd", "bat"] {
        let directory = root.path().join(extension);
        let command = directory.join(format!("claude.{extension}"));
        write_executable(&command, "launcher");
        assert_eq!(
            find_cli_in_paths(Agent::ClaudeCode, &[directory]),
            Some(command)
        );
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

#[test]
fn invalid_recovery_never_guesses_paths_or_displays_structured_secrets() {
    for structured in [false, true] {
        let sandbox = sandbox(if structured {
            "invalid-structured"
        } else {
            "invalid-path"
        });
        connect(&sandbox);
        let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
        let before = fs::read(&path).unwrap();
        let mut store = sandbox.projector.load_store().unwrap();
        let record = store.get_mut("claude-code").unwrap();
        record.config_path = PathBuf::new();
        if structured {
            record.fields[0].previous = Some(Previous::Plain(ConfigValue::Json(
                json!({"apiKey":"sk-test-hidden"}),
            )));
        }
        write(
            &sandbox.projector.store_path(),
            &serde_json::to_string(&store).unwrap(),
        );
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        assert!(!agent_status(&statuses, Agent::ClaudeCode).authorized && tokens.is_empty());
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(Agent::ClaudeCode, false, None, &options)
            .unwrap();
        assert!(preview.changes.is_empty());
        assert!(!serde_json::to_string(&preview)
            .unwrap()
            .contains("sk-test-hidden"));
        assert!(sandbox
            .projector
            .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
            .is_err());
        assert!(sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .is_none());
        assert!(sandbox
            .projector
            .load_store()
            .unwrap()
            .contains_key("claude-code"));
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[test]
fn finds_opencode_installed_by_the_official_script_without_shell_path() {
    let sandbox = sandbox("opencode-native-install");
    assert!(find_cli(Agent::OpenCode, &sandbox.home, false).is_none());
    let executable = sandbox.home.join(".opencode/bin").join(if cfg!(windows) {
        "opencode.exe"
    } else {
        "opencode"
    });
    write_executable(&executable, "test executable");
    assert_eq!(
        find_cli(Agent::OpenCode, &sandbox.home, false),
        Some(executable)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let stale = sandbox.home.join("stale/codex");
        let installed = sandbox.home.join("installed/codex");
        write(&stale, "not executable");
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o600)).unwrap();
        write_executable(&installed, "#!/bin/sh\n");
        let paths = [
            stale.parent().unwrap().to_path_buf(),
            installed.parent().unwrap().to_path_buf(),
        ];
        assert_eq!(find_cli_in_paths(Agent::Codex, &paths), Some(installed));
        assert!(find_cli_in_paths(Agent::Codex, &paths[..1]).is_none());
    }
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
            ENDPOINT,
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
