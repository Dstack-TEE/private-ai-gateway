use super::super::tests::{catalog, disconnect, sandbox, write, Sandbox};
use super::*;

fn private(path: &Path, text: &str) {
    write(path, text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn options(model: &str) -> ConnectOptions {
    ConnectOptions {
        default_model: Some(model.to_string()),
    }
}

fn connect(sandbox: &Sandbox, model: &str) -> Result<AgentStatus, AgentError> {
    let preview = sandbox
        .projector
        .preview(Agent::Dsh, true, Some(&catalog()), &options(model))?;
    sandbox.projector.apply(
        Agent::Dsh,
        true,
        &preview.revision,
        Some(&catalog()),
        &options(model),
    )
}

fn status(sandbox: &Sandbox) -> AgentStatus {
    let (statuses, _) = sandbox.projector.scan(Some(&catalog())).unwrap();
    statuses
        .into_iter()
        .find(|status| status.id == Agent::Dsh.id())
        .unwrap()
}

struct Files {
    patch: PathBuf,
    credentials: PathBuf,
    profiles: PathBuf,
}

fn files(sandbox: &Sandbox) -> Files {
    let dsh_home = home_dir(&sandbox.home, false);
    fs::create_dir_all(&dsh_home).unwrap();
    Files {
        patch: dsh_home.join("cordis.patch.yml"),
        credentials: dsh_home.join(CREDENTIALS_FILE),
        profiles: dsh_home.join("profiles"),
    }
}

fn item(path: &Path, id: &str) -> ConfigDoc {
    let doc = ConfigDoc::parse(Format::YamlList, &fs::read_to_string(path).unwrap()).unwrap();
    doc.entry(&key(id)).unwrap().unwrap()
}

#[test]
fn connect_edits_the_users_items_and_disconnect_restores_every_byte() {
    let sandbox = sandbox("dsh-user-items");
    let files = files(&sandbox);
    let patch = "# machine-wide dsh patches\n\
                 - id: llm-pi-ai\n  config:\n    providers:\n      openrouter: {apiKeyEnv: OPENROUTER_KEY} # mine\n\
                 - id: agent-default-model # chosen in the web app\n  config:\n    provider: 'deepseek-official'\n    model: \"deepseek-flash\"\n\
                 - id: web-search-deepseek\n  disabled: false # keep search\n\
                 - id: acp\n  config:\n    sessionListPageSize: 20\n";
    let credentials = "version: 1\n# my keys\nrefs:\n  DEEPSEEK_API_KEY: sk-user\n";
    write(&files.patch, patch);
    private(&files.credentials, credentials);
    // A profile's own default is masked by the home layer, never edited.
    let profile = files.profiles.join("web").join("cordis.patch.yml");
    let profile_text =
        "- id: agent-default-model\n  config: {provider: deepseek-official, model: deepseek-pro}\n";
    write(&profile, profile_text);
    write(&files.profiles.join(".DS_Store"), "not a profile");

    let status = connect(&sandbox, "phala/qwen").unwrap();
    assert!(status.authorized, "{:?}", status.attention);
    let pi_ai = item(&files.patch, "llm-pi-ai");
    let Some(ConfigValue::Json(provider)) =
        pi_ai.get_value(&["config", "providers", "private-ai-proxy"])
    else {
        panic!("missing provider")
    };
    assert_eq!(provider["apiKeyEnv"], TOKEN_REF);
    assert_eq!(provider["baseURL"], "http://127.0.0.1:4180/v1");
    assert_eq!(provider["models"][0]["name"], "GPT OSS 20B");
    assert!(pi_ai.contains(&["config", "providers", "openrouter"]));
    let default = item(&files.patch, "agent-default-model");
    assert_eq!(
        default.get_str(&["config", "provider"]).as_deref(),
        Some("private-ai-proxy")
    );
    assert_eq!(
        default.get_str(&["config", "model"]).as_deref(),
        Some("phala/qwen")
    );
    assert_eq!(
        item(&files.patch, "web-search-deepseek").get_value(&["disabled"]),
        Some(ConfigValue::Bool(true))
    );
    let acp = item(&files.patch, "acp");
    assert_eq!(
        acp.get_value(&["config"]),
        Some(ConfigValue::Json(serde_json::json!({
            "sessionListPageSize": 20, "provider": "private-ai-proxy", "model": "phala/qwen",
        })))
    );
    let token = sandbox.projector.tokens.read("dsh").unwrap().unwrap();
    assert!(credential_holds(&home_dir(&sandbox.home, false), &token));
    // The connection record keeps digests, never the token.
    let record = fs::read_to_string(sandbox.projector.store_path()).unwrap();
    assert!(!record.contains(&token));
    assert!(fs::read_to_string(&files.credentials)
        .unwrap()
        .contains("DEEPSEEK_API_KEY: sk-user"));
    assert_eq!(fs::read_to_string(&profile).unwrap(), profile_text);

    disconnect(&sandbox, Agent::Dsh);
    assert_eq!(fs::read_to_string(&files.patch).unwrap(), patch);
    assert_eq!(fs::read_to_string(&files.credentials).unwrap(), credentials);
    assert!(sandbox.projector.tokens.read("dsh").unwrap().is_none());
    assert!(sandbox.projector.load_store().unwrap().is_empty());
}

#[test]
fn files_the_connection_creates_are_removed_and_reconnecting_keeps_the_journal() {
    let sandbox = sandbox("dsh-created");
    let files = files(&sandbox);
    assert!(connect(&sandbox, "phala/qwen").unwrap().authorized);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&files.credentials)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(
        item(&files.patch, "acp").get_value(&[]),
        Some(ConfigValue::Json(serde_json::json!({
            "id": "acp", "config": {"provider": "private-ai-proxy", "model": "phala/qwen"},
        })))
    );
    let token = sandbox.projector.tokens.read("dsh").unwrap().unwrap();
    let record = fs::read_to_string(sandbox.projector.store_path()).unwrap();
    assert!(!record.contains(&token));
    // Choosing another model rewrites the connection's own items in place.
    assert!(connect(&sandbox, "openai/gpt-oss-20b").unwrap().authorized);
    assert_eq!(
        item(&files.patch, "agent-default-model")
            .get_str(&["config", "model"])
            .as_deref(),
        Some("openai/gpt-oss-20b")
    );
    disconnect(&sandbox, Agent::Dsh);
    assert!(!files.patch.exists());
    assert!(!files.credentials.exists());

    // A key the user adds to an appended item keeps that item on disconnect.
    assert!(connect(&sandbox, "phala/qwen").unwrap().authorized);
    let doc =
        ConfigDoc::parse(Format::YamlList, &fs::read_to_string(&files.patch).unwrap()).unwrap();
    doc.entry(&key("web-search-deepseek"))
        .unwrap()
        .unwrap()
        .set_str(&["name"], "@deepseek-ai/dsh-web-search-deepseek")
        .unwrap();
    write(&files.patch, &doc.render().unwrap());
    disconnect(&sandbox, Agent::Dsh);
    let kept =
        ConfigDoc::parse(Format::YamlList, &fs::read_to_string(&files.patch).unwrap()).unwrap();
    assert!(kept.entry(&key("web-search-deepseek")).unwrap().is_some());
    assert!(kept.entry(&key("llm-pi-ai")).unwrap().is_none());
    fs::remove_file(&files.patch).unwrap();

    // Whatever the user's file held comes back byte for byte, even when
    // it was empty or blank and so could not be parsed as a list.
    for original in [
        "",
        "  \n\n",
        "[]\n",
        "# mine\n- id: tools\n  config: {mode: native}\n",
    ] {
        write(&files.patch, original);
        assert!(connect(&sandbox, "phala/qwen").unwrap().authorized);
        assert!(connect(&sandbox, "openai/gpt-oss-20b").unwrap().authorized);
        disconnect(&sandbox, Agent::Dsh);
        assert_eq!(fs::read_to_string(&files.patch).unwrap(), original);
    }
}

#[test]
fn configurations_that_cannot_be_restored_or_would_reroute_are_refused_untouched() {
    let refuse = |name: &str, setup: &dyn Fn(&Files), expected: &str| {
        let sandbox = sandbox(&format!("dsh-refuse-{name}"));
        let files = files(&sandbox);
        setup(&files);
        let before: Vec<_> = [&files.patch, &files.credentials]
            .iter()
            .map(|path| fs::read(path).ok())
            .collect();
        let error = connect(&sandbox, "phala/qwen").unwrap_err().to_string();
        assert!(error.contains(expected), "{name}: {error}");
        let after: Vec<_> = [&files.patch, &files.credentials]
            .iter()
            .map(|path| fs::read(path).ok())
            .collect();
        assert_eq!(before, after, "{name}");
        assert!(sandbox.projector.tokens.read("dsh").unwrap().is_none());
        assert!(sandbox.projector.load_store().unwrap().is_empty());
    };
    let profile = |files: &Files, text: &str| {
        write(&files.profiles.join("web").join("cordis.patch.yml"), text);
    };
    refuse(
        "profile-providers",
        &|files| {
            profile(
                files,
                "- id: llm-pi-ai\n  config: {providers: {kimi: {}}}\n",
            )
        },
        "Move that `id: llm-pi-ai` item to",
    );
    refuse(
        "profile-disables",
        &|files| profile(files, "- id: credentials\n  disabled: true\n"),
        "disables the `credentials` row",
    );
    refuse(
        "profile-moves-store",
        &|files| {
            profile(
                files,
                "- id: credentials\n  config: {path: /tmp/elsewhere.yaml}\n",
            )
        },
        "moves dsh's credential store",
    );
    refuse(
        "insert-shadow",
        &|files| {
            write(
                &files.patch,
                "- insert:\n    - {id: agent-default-model, name: x}\n",
            )
        },
        "inserts another `agent-default-model` row",
    );
    refuse(
        "duplicate",
        &|files| write(&files.patch, "- id: llm-pi-ai\n- id: llm-pi-ai\n"),
        "more than one item with id: llm-pi-ai",
    );
    refuse(
        "flow-list",
        &|files| write(&files.patch, "[{id: tools}]\n"),
        "flow style",
    );
    refuse(
        "flow-item",
        &|files| {
            write(
                &files.patch,
                "- {id: agent-default-model, config: {model: m}}\n",
            )
        },
        "flow style",
    );
    refuse(
        "no-newline",
        &|files| write(&files.patch, "- id: tools"),
        "does not end with a newline",
    );
    refuse(
        "effort",
        &|files| {
            write(
                &files.patch,
                "- id: agent-default-model\n  config:\n    reasoningEffort: max\n",
            )
        },
        "config.reasoningEffort",
    );
    refuse(
        "existing-provider",
        &|files| {
            write(
                &files.patch,
                "- id: llm-pi-ai\n  config:\n    providers:\n      private-ai-proxy: {api: x}\n",
            )
        },
        "structured value",
    );
    refuse(
        "foreign-token",
        &|files| {
            private(
                &files.credentials,
                &format!("version: 1\nrefs:\n  {TOKEN_REF}: someone-else\n"),
            )
        },
        "already holds",
    );
    refuse(
        "indented-list",
        &|files| write(&files.patch, "    - id: tools\n"),
        "indented",
    );
    refuse(
        "search-under-another-id",
        &|files| {
            profile(
                files,
                "- insert:\n    - {id: my-search, name: '@deepseek-ai/dsh-web-search-deepseek'}\n",
            )
        },
        "inserts another `@deepseek-ai/dsh-web-search-deepseek` row",
    );
    refuse(
        "acp-disabled",
        &|files| profile(files, "- id: acp\n  disabled: true\n"),
        "disables the `acp` row",
    );
    refuse(
        "acp-under-another-id",
        &|files| {
            profile(
                files,
                "- insert:\n    - {id: my-acp, name: '@deepseek-ai/dsh-acp', config: {provider: deepseek-official, model: m}}\n",
            )
        },
        "inserts another `@deepseek-ai/dsh-acp` row",
    );
    refuse(
        "acp-shadow",
        &|files| {
            write(
                &files.patch,
                "- insert:\n    - {id: acp, name: '@deepseek-ai/dsh-acp'}\n",
            )
        },
        "inserts another `acp` row",
    );
    refuse(
        "legacy-settings",
        &|files| {
            write(
                &files.patch.with_file_name("settings.yaml"),
                "llm-pi-ai: {}\n",
            )
        },
        "settings",
    );
    #[cfg(unix)]
    refuse(
        "readable-store",
        &|files| write(&files.credentials, "version: 1\nrefs: {}\n"),
        "chmod 600",
    );
}

#[test]
fn status_pauses_access_when_dsh_drifts_after_connecting() {
    let sandbox = sandbox("dsh-drift");
    let files = files(&sandbox);
    assert!(connect(&sandbox, "phala/qwen").unwrap().authorized);
    let credentials = fs::read_to_string(&files.credentials).unwrap();
    let token = sandbox.projector.tokens.read("dsh").unwrap().unwrap();
    private(&files.credentials, &credentials.replace(&token, "replaced"));
    let paused = status(&sandbox);
    assert!(!paused.authorized);
    assert!(paused.attention.unwrap().contains(TOKEN_REF));
    private(&files.credentials, &credentials);
    assert!(status(&sandbox).authorized);

    // A profile created later cannot hide its providers behind the connection.
    let profile = files.profiles.join("work").join("cordis.patch.yml");
    write(
        &profile,
        "- id: llm-pi-ai\n  config: {providers: {kimi: {}}}\n",
    );
    let paused = status(&sandbox);
    assert!(!paused.authorized);
    assert!(paused
        .attention
        .unwrap()
        .contains("Move that `id: llm-pi-ai` item"));
    fs::remove_file(&profile).unwrap();
    assert!(status(&sandbox).authorized);
    disconnect(&sandbox, Agent::Dsh);
    assert!(!files.credentials.exists());
}

#[test]
fn writes_wait_for_dshs_own_lock_and_leave_none_behind() {
    let sandbox = sandbox("dsh-lock");
    let file = sandbox.home.join(CREDENTIALS_FILE);
    let lock = sandbox.home.join(format!("{CREDENTIALS_FILE}.lock"));
    write(&lock, "1\n");
    let release = {
        let lock = lock.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            fs::remove_file(lock).unwrap();
        })
    };
    let holder = with_lock(&file, || fs::read_to_string(&lock)).unwrap();
    release.join().unwrap();
    assert_eq!(holder, format!("{}\n", std::process::id()));
    assert!(!lock.exists());
    assert!(with_lock(&file, || Err::<(), _>(io::Error::other("failed"))).is_err());
    assert!(!lock.exists());
}

#[cfg(unix)]
#[test]
fn a_lock_whose_holder_exited_is_taken_over_as_dsh_does_and_a_live_one_is_not() {
    let sandbox = sandbox("dsh-stale-lock");
    let file = sandbox.home.join(CREDENTIALS_FILE);
    let lock = sandbox.home.join(format!("{CREDENTIALS_FILE}.lock"));
    let exited = std::process::Command::new("true").spawn().unwrap();
    let pid = exited.id();
    let mut exited = exited;
    exited.wait().unwrap();
    write(&lock, &format!("{pid}\n"));
    assert!(with_lock(&file, || Ok(())).is_ok());
    assert!(!lock.exists());
    // An incomplete record, or a live holder, proves nothing: wait, then give up.
    let mut live = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    for record in [format!("{}\n", live.id()), format!("{pid}")] {
        write(&lock, &record);
        let error = with_lock(&file, || Ok(())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(fs::read_to_string(&lock).unwrap(), record);
    }
    live.kill().unwrap();
    live.wait().unwrap();
}

/// dsh saving its own key after the connection neither leaves the token
/// behind on disconnect nor blocks connecting again.
#[test]
fn keys_dsh_adds_later_keep_the_store_and_leave_no_token_behind() {
    for (name, original) in [
        ("created", None),
        ("existing", Some("version: 1\n# my records\nrecords: {}\n")),
    ] {
        let sandbox = sandbox(&format!("dsh-refs-{name}"));
        let files = files(&sandbox);
        if let Some(original) = original {
            private(&files.credentials, original);
        }
        assert!(connect(&sandbox, "phala/qwen").unwrap().authorized);
        let token = sandbox.projector.tokens.read("dsh").unwrap().unwrap();
        // dsh writes through the same YAML document model.
        let text = fs::read_to_string(&files.credentials).unwrap();
        let mut store = ConfigDoc::parse(Format::Yaml, &text).unwrap();
        assert!(!store.is_flow(&["refs"]), "{name}: {text}");
        store
            .set_str(&["refs", "DEEPSEEK_API_KEY"], "sk-user")
            .unwrap();
        let with_user_key = store.render().unwrap();
        private(&files.credentials, &with_user_key);
        assert!(status(&sandbox).authorized, "{name}");

        disconnect(&sandbox, Agent::Dsh);
        let left = fs::read_to_string(&files.credentials).unwrap();
        assert!(
            !left.contains(&token) && !left.contains(TOKEN_REF),
            "{name}: {left}"
        );
        let left = ConfigDoc::parse(Format::Yaml, &left).unwrap();
        assert_eq!(
            left.get_str(&["refs", "DEEPSEEK_API_KEY"]).as_deref(),
            Some("sk-user")
        );

        assert!(
            connect(&sandbox, "phala/qwen").unwrap().authorized,
            "{name}"
        );
        disconnect(&sandbox, Agent::Dsh);
        // Removing dsh's key too leaves exactly what was there before.
        let mut store = ConfigDoc::parse(
            Format::Yaml,
            &fs::read_to_string(&files.credentials).unwrap(),
        )
        .unwrap();
        store.remove_exact(&["refs", "DEEPSEEK_API_KEY"]).unwrap();
        store.remove_exact(&["refs"]).unwrap();
        let expected = original.map_or_else(|| "version: 1\n".to_string(), str::to_string);
        assert_eq!(store.render().unwrap(), expected, "{name}");
    }
}
