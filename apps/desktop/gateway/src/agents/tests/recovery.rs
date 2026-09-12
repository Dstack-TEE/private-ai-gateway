use super::*;

#[test]
fn openclaw_same_path_empty_helper_deauthorizes_but_does_not_block_disconnect() {
    let mut sandbox = sandbox("openclaw-empty-helper");
    let agent = Agent::OpenClaw;
    let options = claude_options();
    let catalog = catalog();
    sandbox.projector.helper_exe = sandbox
        .projector
        .data_dir
        .join("helpers")
        .join(helper_binary_name());
    apply_connect(&sandbox, agent, &catalog, &options);
    fs::write(&sandbox.projector.helper_exe, "").unwrap();
    let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
    let status = statuses
        .iter()
        .find(|status| status.id == "openclaw")
        .unwrap();
    assert!(status.recorded && !status.connected && !status.authorized && tokens.is_empty());
    assert!(sandbox
        .projector
        .preview(agent, true, Some(&catalog), &options)
        .is_err());
    disconnect(&sandbox, agent);
    assert!(sandbox.projector.load_store().unwrap()[agent.id()].disconnected());
}

#[test]
fn interrupted_two_file_connection_restores_from_the_persisted_journal() {
    let sandbox = sandbox("selection-crash");
    let agent = Agent::Pi;
    let config = agent.config_path(&sandbox.home, false);
    write(&config, "{}");
    let defaults = config.with_file_name("settings.json");
    let original = r#"{"defaultProvider":"original","defaultModel":"native"}"#;
    write(&defaults, original);
    let catalog = catalog();
    let mut doc = ConfigDoc::parse(agent.format(), "{}").unwrap();
    let edit = sandbox
        .projector
        .edit(
            agent,
            true,
            &mut doc,
            &Store::new(),
            Some(&catalog),
            &ConnectOptions::default(),
        )
        .unwrap();
    let mut record = edit.record.unwrap();
    record.config_path = Some(config.clone());
    record.disabled = true;
    record.cleanup_pending = true;
    record.suspended = true;
    let store = BTreeMap::from([(agent.id().to_string(), record)]);
    sandbox.projector.save_store(&store).unwrap();
    sandbox.projector.tokens.rotate(agent.id()).unwrap();
    write_atomic(&config, &doc.render().unwrap(), Some(Some("{}"))).unwrap();
    // The process ended after the provider write but before the selection file.
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    assert_eq!(fs::read_to_string(&defaults).unwrap(), original);
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
    assert!(sandbox.projector.load_store().unwrap()[agent.id()].disconnected());
    let preview = sandbox
        .projector
        .preview(agent, true, Some(&catalog), &ConnectOptions::default())
        .unwrap();
    assert!(
        sandbox
            .projector
            .apply(
                agent,
                true,
                &preview.revision,
                Some(&catalog),
                &ConnectOptions::default()
            )
            .unwrap()
            .authorized
    );
}

#[test]
fn native_model_settings_edits_invalidate_preview_and_survive_disconnect() {
    let sandbox = sandbox("selection-drift");
    let agent = Agent::Pi;
    let config = agent.config_path(&sandbox.home, false);
    write(&config, "{}");
    let defaults = config.with_file_name("settings.json");
    write(
        &defaults,
        r#"{"defaultProvider":"original","defaultModel":"native"}"#,
    );
    let catalog = catalog();
    let options = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(agent, true, Some(&catalog), &options)
        .unwrap();
    write(
        &defaults,
        r#"{"defaultProvider":"edited","defaultModel":"edited-model"}"#,
    );
    assert!(sandbox
        .projector
        .apply(agent, true, &preview.revision, Some(&catalog), &options)
        .unwrap_err()
        .contains("changed since the preview"));
    let preview = sandbox
        .projector
        .preview(agent, true, Some(&catalog), &options)
        .unwrap();
    sandbox
        .projector
        .apply(agent, true, &preview.revision, Some(&catalog), &options)
        .unwrap();
    write(
        &defaults,
        r#"{"defaultProvider":"user-choice","defaultModel":"user-model"}"#,
    );
    disconnect(&sandbox, agent);
    assert!(fs::read_to_string(&defaults)
        .unwrap()
        .contains("user-choice"));
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
}

#[test]
fn suspended_restore_keeps_external_edits_and_retries_invalid_files() {
    let sandbox = sandbox("link-restore-retry");
    let agent = Agent::ClaudeCode;
    let config = agent.config_path(&sandbox.home, false);
    write(&config, r#"{"model":"original"}"#);
    connect(&sandbox);
    let managed = fs::read_to_string(&config).unwrap();
    write(&config, "invalid json");
    assert_eq!(sandbox.projector.reconcile(None).unwrap().len(), 1);
    assert!(!sandbox.projector.load_store().unwrap()[agent.id()]
        .fields
        .is_empty());
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
    write(&config, &managed);
    assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
    assert_eq!(
        doc(&sandbox, agent).get_value(&["model"]),
        Some(ConfigValue::Str("original".into()))
    );
    connect(&sandbox);
    let mut edited = doc(&sandbox, agent);
    edited
        .set_value(&["model"], &ConfigValue::Str("user-choice".into()))
        .unwrap();
    write(&config, &edited.render().unwrap());
    assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
    assert_eq!(
        doc(&sandbox, agent).get_value(&["model"]),
        Some(ConfigValue::Str("user-choice".into()))
    );
}

#[test]
fn opencode_merge_conflicts_revoke_without_writes_and_keep_original_restore_path() {
    let sandbox = sandbox("opencode-merge");
    let path = Agent::OpenCode.config_path(&sandbox.home, false);
    let jsonc = path.with_extension("jsonc");
    let original = json!({
        "model": "other/original",
        "provider": {"other": {"name": "User provider"}}
    });
    write(&path, &original.to_string());
    let benign =
        "{/* user's comment */\"provider\":{\"other\":{\"name\":\"JSONC user provider\"}},}";
    write(&jsonc, benign);
    let catalog = catalog();
    let options = claude_options();
    let preview = sandbox
        .projector
        .preview(Agent::OpenCode, true, Some(&catalog), &options)
        .unwrap();
    assert!(
        sandbox
            .projector
            .apply(
                Agent::OpenCode,
                true,
                &preview.revision,
                Some(&catalog),
                &options,
            )
            .unwrap()
            .authorized
    );
    assert_eq!(fs::read_to_string(&jsonc).unwrap(), benign);
    assert_eq!(
        doc(&sandbox, Agent::OpenCode)
            .get_str(&["provider", "other", "name"])
            .as_deref(),
        Some("User provider")
    );
    let preview = sandbox
        .projector
        .preview(Agent::OpenCode, true, Some(&catalog), &options)
        .unwrap();
    let config_before = fs::read(&path).unwrap();
    let record_before = fs::read(sandbox.projector.store_path()).unwrap();
    let token_path = sandbox.projector.tokens.path("opencode");
    let token_before = fs::read(&token_path).unwrap();
    for conflict in [
        "{\"model\":\"other/override\",}",
        "{/* keep */\"provider\":{\"private-ai-proxy\":{\"options\":{\"baseURL\":\"http://127.0.0.1:1/v1\"}}}}",
        "{\"provider\":{\"private-ai-proxy\":{\"options\":{\"apiKey\":\"synthetic-never-log-me\"}}}}",
        "{\"provider\":null}",
        "{/* broken",
    ] {
        write(&jsonc, conflict);
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        let status = statuses.iter().find(|status| status.id == "opencode").unwrap();
        assert!(status.recorded && !status.connected && !status.authorized);
        assert!(tokens.is_empty());
        let attention = status.attention.as_deref().unwrap();
        assert!(attention.contains("opencode.jsonc"), "{attention}");
        assert!(!attention.contains("synthetic-never-log-me"));
        assert!(sandbox.projector
            .preview(Agent::OpenCode, true, Some(&catalog), &options).is_err());
        // Re-read companion files at apply, even when the main revision is unchanged.
        assert!(sandbox.projector.apply(
            Agent::OpenCode, true, &preview.revision, Some(&catalog), &options,
        ).is_err());
        assert_eq!(fs::read(&path).unwrap(), config_before);
        assert_eq!(fs::read(sandbox.projector.store_path()).unwrap(), record_before);
        assert_eq!(fs::read(&token_path).unwrap(), token_before);
        assert_eq!(fs::read_to_string(&jsonc).unwrap(), conflict);
    }
    fs::remove_file(&jsonc).unwrap();
    fs::create_dir(&jsonc).unwrap();
    let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
    let status = statuses
        .iter()
        .find(|status| status.id == "opencode")
        .unwrap();
    assert!(!status.authorized && tokens.is_empty());
    assert!(status.attention.as_deref().unwrap().contains("unreadable"));
    fs::remove_dir(&jsonc).unwrap();
    write(&jsonc, benign);
    assert!(
        sandbox
            .projector
            .scan(None)
            .unwrap()
            .0
            .iter()
            .find(|status| status.id == "opencode")
            .unwrap()
            .authorized
    );
    write(
        &jsonc,
        "{/* keep on disconnect */\"model\":\"other/override\"}",
    );
    let jsonc_before = fs::read(&jsonc).unwrap();
    let mut edited = doc(&sandbox, Agent::OpenCode);
    edited
        .set_str(&["provider", "other", "name"], "Edited outside the app")
        .unwrap();
    write(&path, &edited.render().unwrap());
    disconnect(&sandbox, Agent::OpenCode);
    let mut restored = original;
    restored["provider"]["other"]["name"] = json!("Edited outside the app");
    let mut actual: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(actual["provider"]
        .as_object_mut()
        .unwrap()
        .remove("private-ai-proxy")
        .is_some());
    assert_eq!(actual, restored);
    assert_eq!(fs::read(&jsonc).unwrap(), jsonc_before);
    assert!(sandbox.projector.load_store().unwrap()["opencode"].disconnected());
    assert!(!token_path.exists());
}

#[test]
fn credential_restore_revokes_before_refusing_a_changed_route() {
    let sandbox = sandbox("secret-route-ownership");
    let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
    write(
        &path,
        r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-test-parked"}}"#,
    );
    connect(&sandbox);
    let projection = fs::read_to_string(&path).unwrap();
    let mut edited = doc(&sandbox, Agent::ClaudeCode);
    edited
        .set_str(&["env", "ANTHROPIC_BASE_URL"], "http://127.0.0.1:1")
        .unwrap();
    write(&path, &edited.render().unwrap());
    let before = fs::read(&path).unwrap();
    let options = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, false, None, &options)
        .unwrap();
    assert!(preview.changes.is_empty());
    assert!(preview.note.contains("ambiguous"));
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("sk-test-parked"));
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
    assert!(sandbox.secrets.holds("sk-test-parked"));
    assert!(sandbox
        .projector
        .load_store()
        .unwrap()
        .contains_key("claude-code"));
    assert_eq!(fs::read(&path).unwrap(), before);
    write(&path, &projection);
    disconnect(&sandbox, Agent::ClaudeCode);
    assert_eq!(
        doc(&sandbox, Agent::ClaudeCode)
            .get_str(&["env", "ANTHROPIC_AUTH_TOKEN"])
            .as_deref(),
        Some("sk-test-parked")
    );
}

#[test]
fn recorded_paths_restore_original_files_after_location_changes() {
    for agent in [Agent::ClaudeCode, Agent::OpenCode] {
        let mut sandbox = sandbox(&format!("recorded-path-{}", agent.id()));
        let catalog = catalog();
        let options = claude_options();
        let path = agent.config_path(&sandbox.home, false);
        let original = if agent == Agent::ClaudeCode {
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-test-original"},"model":"user-model"}"#
        } else {
            r#"{"model":"other/user-model","provider":{"other":{"name":"User provider"}}}"#
        };
        write(&path, original);
        apply_connect(&sandbox, agent, &catalog, &options);
        assert_eq!(
            sandbox.projector.load_store().unwrap()[agent.id()]
                .config_path
                .as_deref(),
            Some(path.as_path())
        );
        let projected = fs::read_to_string(&path).unwrap();
        sandbox.projector.home = sandbox.home.join("different-profile");
        let foreign = agent.config_path(&sandbox.projector.home, false);
        write(&foreign, &projected);
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        let status = statuses
            .iter()
            .find(|status| status.id == agent.id())
            .unwrap();
        assert!(status.recorded && !status.connected && !status.authorized && tokens.is_empty());
        assert!(status
            .attention
            .as_deref()
            .unwrap()
            .contains("location changed"));
        assert!(sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .is_err());
        fs::remove_file(&sandbox.projector.helper_exe).unwrap();
        disconnect(&sandbox, agent);
        assert_eq!(fs::read_to_string(foreign).unwrap(), projected);
        let mut actual: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        if agent == Agent::OpenCode {
            assert!(actual["provider"]
                .as_object_mut()
                .unwrap()
                .remove("private-ai-proxy")
                .is_some());
        }
        assert_eq!(
            actual,
            serde_json::from_str::<serde_json::Value>(original).unwrap()
        );
    }
}

#[test]
fn disconnected_provider_ownership_does_not_follow_a_new_config_path() {
    for agent in [
        Agent::Codex,
        Agent::OpenCode,
        Agent::Pi,
        Agent::Hermes,
        Agent::OpenClaw,
        Agent::OhMyPi,
    ] {
        let mut sandbox = sandbox(&format!("disconnected-path-{}", agent.id()));
        let catalog = catalog();
        let options = ConnectOptions::default();
        let original = agent.config_path(&sandbox.home, false);
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .unwrap();
        sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        disconnect(&sandbox, agent);
        let retained = fs::read_to_string(&original).unwrap();
        sandbox.projector.home = sandbox.home.join("new-home");
        let target = agent.config_path(&sandbox.projector.home, false);
        // Identical retained definitions in a different file are user data,
        // not an extension of the old ownership journal.
        write(&target, &retained);
        let status = sandbox
            .projector
            .scan(None)
            .unwrap()
            .0
            .into_iter()
            .find(|s| s.id == agent.id())
            .unwrap();
        assert!(!status.recorded);
        assert!(!status
            .attention
            .as_deref()
            .is_some_and(|s| s.contains("location changed")));
        if agent == Agent::OpenCode {
            assert!(sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .unwrap_err()
                .contains("already exists"));
            let deferred = sandbox
                .projector
                .preview(agent, true, None, &options)
                .unwrap();
            sandbox
                .projector
                .apply(agent, true, &deferred.revision, None, &options)
                .unwrap();
            assert!(sandbox.projector.load_store().unwrap()[agent.id()]
                .fields
                .is_empty());
            assert!(sandbox
                .projector
                .preview(agent, true, Some(&catalog), &options)
                .unwrap_err()
                .contains("already exists"));
            disconnect(&sandbox, agent);
        }
        fs::remove_file(&target).unwrap();
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog), &options)
            .unwrap();
        sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog), &options)
            .unwrap();
        disconnect(&sandbox, agent);
        assert_eq!(
            sandbox.projector.load_store().unwrap()[agent.id()]
                .config_path
                .as_deref(),
            Some(target.as_path())
        );
        assert_eq!(fs::read_to_string(&original).unwrap(), retained);
    }
}

#[test]
fn claude_takes_over_credentials_via_the_keyring_and_restores_them() {
    let sandbox = sandbox("claude");
    let path = sandbox.home.join(".claude").join("settings.json");
    write(
        &path,
        r#"{"model": "opus", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-old-secret"}}"#,
    );
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, true, Some(&catalog()), &claude_options())
        .unwrap();
    let token_change = preview
        .changes
        .iter()
        .find(|change| change.key == "env.ANTHROPIC_AUTH_TOKEN")
        .unwrap();
    assert_eq!(token_change.before.as_deref(), Some("Existing secret"));
    assert_eq!(token_change.after, None);
    assert!(token_change.sensitive);
    let preview_json = serde_json::to_string(&preview).unwrap();
    assert!(!preview_json.contains("sk-old-secret"));
    assert!(sandbox.secrets.is_empty(), "preview parks nothing");

    let status = connect(&sandbox);
    assert!(status.connected);
    let doc = doc(&sandbox, Agent::ClaudeCode);
    assert_eq!(doc.get_str(&["env", "ANTHROPIC_AUTH_TOKEN"]), None);
    assert_eq!(
        doc.get_str(&["env", "ANTHROPIC_BASE_URL"]).as_deref(),
        Some(ENDPOINT)
    );
    assert_eq!(
        doc.get_str(&["env", "ANTHROPIC_MODEL"]).as_deref(),
        Some("openai/gpt-oss-20b")
    );
    assert!(doc
        .get_str(&["apiKeyHelper"])
        .unwrap()
        .contains("--agent-token claude-code"));
    assert_eq!(doc.get_str(&["model"]).as_deref(), Some("opus"));
    assert!(
        sandbox.secrets.holds("sk-old-secret"),
        "old secret parked in the store"
    );
    let manifest = fs::read_to_string(sandbox.projector.store_path()).unwrap();
    assert!(!manifest.contains("sk-old-secret"));
    assert!(manifest.contains("secret_ref"));

    let smaller = Catalog::from_remote(&json!({ "data": [{ "id": "phala/qwen" }] }), 2).unwrap();
    let status = &sandbox.projector.scan(Some(&smaller)).unwrap().0[1];
    assert!(status.connected);
    assert!(status
        .attention
        .as_deref()
        .unwrap()
        .contains("not available"));

    disconnect(&sandbox, Agent::ClaudeCode);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"ANTHROPIC_AUTH_TOKEN\": \"sk-old-secret\""));
    assert!(!text.contains("apiKeyHelper"));
    assert!(sandbox.secrets.is_empty(), "restore entry released");
    assert!(sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .is_none());
    assert!(sandbox.projector.load_store().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn a_failed_disconnect_leaves_a_retryable_tombstone_and_never_reuses_the_token() {
    let sandbox = sandbox("tombstone");
    let path = sandbox.home.join(".claude").join("settings.json");
    write(&path, r#"{"model": "opus"}"#);
    connect(&sandbox);
    let token = sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .unwrap();
    assert!(!sandbox.projector.scan(None).unwrap().1.is_empty());

    // Make the config unwritable (a symlink target is refused) so the
    // restore step fails after the record was tombstoned.
    let dir = path.parent().unwrap().to_path_buf();
    let original = fs::read_to_string(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(dir.join("elsewhere.json"), &original).unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere.json"), &path).unwrap();
    let options = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, false, None, &options)
        .unwrap();
    assert!(sandbox
        .projector
        .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
        .is_err());
    assert!(
        sandbox.projector.scan(None).unwrap().1.is_empty(),
        "access stays revoked"
    );
    let status = &sandbox.projector.scan(None).unwrap().0[1];
    assert!(status.attention.as_deref().unwrap().contains("retried"));
    // Reconnecting is refused while the tombstone exists.
    assert!(sandbox
        .projector
        .apply(
            Agent::ClaudeCode,
            true,
            "any",
            Some(&catalog()),
            &claude_options()
        )
        .is_err());

    // Repair the config and retry: idempotent cleanup completes.
    fs::remove_file(&path).unwrap();
    fs::rename(dir.join("elsewhere.json"), &path).unwrap();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, false, None, &options)
        .unwrap();
    sandbox
        .projector
        .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
        .unwrap();
    assert!(sandbox.projector.load_store().unwrap().is_empty());
    assert!(sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .is_none());
    assert!(!fs::read_to_string(&path).unwrap().contains("apiKeyHelper"));

    // A new connection issues a fresh token, never the old value.
    connect(&sandbox);
    assert_ne!(
        sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .unwrap(),
        token
    );
}

#[test]
fn disconnect_all_restores_every_agent() {
    let sandbox = sandbox("emergency");
    write(
        &sandbox.home.join(".claude").join("settings.json"),
        r#"{"model": "opus"}"#,
    );
    connect(&sandbox);
    let codex = sandbox.home.join(".codex").join("config.toml");
    write(&codex, "model_provider = \"private_ai_proxy\"\n");
    sandbox.projector.tokens.ensure("codex").unwrap();
    let mut store = sandbox.projector.load_store().unwrap();
    store.insert(
        "codex".into(),
        Connection {
            config_path: Some(codex.clone()),
            fields: vec![OwnedField {
                path: owned(&["model_provider"]),
                value: Some(ConfigValue::Str("private_ai_proxy".into())),
                previous: None,
            }],
            disabled: true,
            cleanup_pending: false,
            ..Connection::default()
        },
    );
    sandbox.projector.save_store(&store).unwrap();

    assert!(sandbox.projector.disconnect_all().unwrap().is_empty());
    assert!(sandbox.projector.load_store().unwrap().is_empty());
    assert_eq!(fs::read_to_string(&codex).unwrap(), "");
    assert!(
        !fs::read_to_string(sandbox.home.join(".claude").join("settings.json"))
            .unwrap()
            .contains("apiKeyHelper")
    );
    assert!(sandbox
        .projector
        .tokens
        .load(&["codex", "claude-code"])
        .unwrap()
        .is_empty());
}

/// Restore-all revokes every token and tombstones every record before
/// any config is read; an unreadable config fails only its own cleanup
/// and never leaves the agent authorized.
#[test]
fn restore_all_revokes_before_reading_any_config() {
    let sandbox = sandbox("tombstone-first");
    write(
        &sandbox.home.join(".claude").join("settings.json"),
        r#"{"model": "opus"}"#,
    );
    connect(&sandbox);
    // Replace the config with a directory: reading it fails outright.
    let path = sandbox.home.join(".claude").join("settings.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let failures = sandbox.projector.disconnect_all().unwrap();
    assert_eq!(failures.len(), 1);
    assert!(sandbox.projector.load_store().unwrap()["claude-code"].cleanup_pending);
    assert!(sandbox.projector.scan(None).unwrap().1.is_empty());
}

/// Deleting the token file is the revocation itself: even when the very
/// first manifest save fails, a restart (a fresh Projector) authorizes
/// nothing, the record stays visible for a retry, and a new connection
/// rotates to a fresh token. Covers single Disconnect and Restore all.
#[cfg(unix)]
#[test]
fn revocation_is_durable_even_when_the_first_manifest_save_fails() {
    use std::os::unix::fs::PermissionsExt;
    for all in [false, true] {
        let sandbox = sandbox(if all {
            "revoke-first-all"
        } else {
            "revoke-first"
        });
        write(
            &sandbox.home.join(".claude").join("settings.json"),
            r#"{"model": "opus"}"#,
        );
        connect(&sandbox);
        let old = sandbox
            .projector
            .tokens
            .read("claude-code")
            .unwrap()
            .unwrap();
        // Make the record unsaveable: the tombstone write fails after the
        // token file is already gone.
        let data_dir = sandbox.projector.data_dir.clone();
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o500)).unwrap();
        let result = if all {
            sandbox.projector.disconnect_all().map(|_| ())
        } else {
            let options = ConnectOptions::default();
            let preview = sandbox
                .projector
                .preview(Agent::ClaudeCode, false, None, &options)
                .unwrap();
            sandbox
                .projector
                .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
                .map(|_| ())
        };
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert!(
            sandbox
                .projector
                .tokens
                .read("claude-code")
                .unwrap()
                .is_none(),
            "the capability itself is gone"
        );
        // A restart is a fresh scan: nothing is authorized, the record is
        // still shown with an attention line.
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        assert!(tokens.is_empty());
        assert!(statuses[1].recorded);
        assert!(statuses[1]
            .attention
            .as_deref()
            .unwrap()
            .contains("revoked"));
        // The retry completes; a new connection never reuses the token.
        disconnect(&sandbox, Agent::ClaudeCode);
        assert!(sandbox.projector.load_store().unwrap().is_empty());
        connect(&sandbox);
        assert_ne!(
            sandbox
                .projector
                .tokens
                .read("claude-code")
                .unwrap()
                .unwrap(),
            old
        );
    }
}

/// Revocation is remove-plus-parent-sync, strictly before any manifest
/// write: with a failing sync injected, disconnect fails closed — the
/// token entry is already gone, the manifest was never touched, a scan
/// authorizes nothing — and the retry with a working sync completes.
#[test]
fn disconnect_fails_closed_when_revocation_cannot_be_persisted() {
    let mut sandbox = sandbox("sync-fail");
    write(
        &sandbox.home.join(".claude").join("settings.json"),
        r#"{"model": "opus"}"#,
    );
    connect(&sandbox);
    let manifest_before = fs::read(sandbox.projector.store_path()).unwrap();
    sandbox
        .projector
        .tokens
        .set_sync_parent(|_| Err(io::Error::other("injected sync failure")));

    let options = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, false, None, &options)
        .unwrap();
    let error = sandbox
        .projector
        .apply(Agent::ClaudeCode, false, &preview.revision, None, &options)
        .unwrap_err();
    assert!(error.contains("could not be persisted"), "{error}");
    // The removal happened before the failed sync stopped everything…
    assert!(sandbox
        .projector
        .tokens
        .read("claude-code")
        .unwrap()
        .is_none());
    // …and the manifest write never ran: not even the tombstone landed.
    assert_eq!(
        fs::read(sandbox.projector.store_path()).unwrap(),
        manifest_before
    );
    // Fail closed either way: a scan authorizes nothing, the record stays
    // visible for a retry.
    let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
    assert!(tokens.is_empty());
    assert!(statuses[1].recorded && statuses[1].attention.is_some());

    sandbox.projector.tokens.set_sync_parent(tokens::sync_dir);
    disconnect(&sandbox, Agent::ClaudeCode);
    assert!(sandbox.projector.load_store().unwrap().is_empty());
}
