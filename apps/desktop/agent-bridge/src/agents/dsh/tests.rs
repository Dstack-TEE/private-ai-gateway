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
fn writes_wait_for_dshs_own_lock_and_leave_none_behind() {
    let sandbox = sandbox("dsh-lock");
    let file = sandbox.home.join(CREDENTIALS_FILE);
    let lock = sandbox.home.join(format!("{CREDENTIALS_FILE}.lock"));
    // Held by a live process: this one, which dsh's rule never takes over.
    write(&lock, &format!("{}\n", std::process::id()));
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

#[test]
fn a_lock_whose_holder_exited_is_taken_over_as_dsh_does_and_a_live_one_is_not() {
    const HOLDER: &str = "PAP_DSH_LOCK_HOLDER";
    const NAME: &str =
        "agents::dsh::tests::a_lock_whose_holder_exited_is_taken_over_as_dsh_does_and_a_live_one_is_not";
    if env::var_os(HOLDER).is_some() {
        // Run as a live lock holder until the test kills it.
        thread::sleep(Duration::from_secs(60));
        return;
    }
    let sandbox = sandbox("dsh-stale-lock");
    let file = sandbox.home.join(CREDENTIALS_FILE);
    let lock = sandbox.home.join(format!("{CREDENTIALS_FILE}.lock"));
    let test_binary = env::current_exe().unwrap();
    let mut exited = std::process::Command::new(&test_binary)
        .args(["--exact", "no-such-test"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = exited.id();
    exited.wait().unwrap();
    write(&lock, &format!("{pid}\n"));
    assert!(with_lock(&file, || Ok(())).is_ok());
    assert!(!lock.exists());
    // An incomplete record, or a live holder, proves nothing: wait, then give up.
    let mut live = std::process::Command::new(&test_binary)
        .args(["--exact", NAME])
        .env(HOLDER, "1")
        .stdout(std::process::Stdio::null())
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
