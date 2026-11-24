use super::*;

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
    record.config_path = config.clone();
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
fn pi_default_model_edit_restores_when_provider_still_belongs_to_pap() {
    let sandbox = sandbox("pi-selector-restore");
    let config = Agent::Pi.config_path(&sandbox.home, false);
    write(&config, "{}");
    let settings = config.with_file_name("settings.json");
    write(
        &settings,
        r#"{"defaultProvider":"original","defaultModel":"native"}"#,
    );
    let catalog = catalog();
    apply_connect(&sandbox, Agent::Pi, &catalog, &claude_options());
    let mut doc = ConfigDoc::parse(Format::Json, &fs::read_to_string(&settings).unwrap()).unwrap();
    doc.set_str(&["defaultModel"], "user-choice").unwrap();
    write(&settings, &doc.render().unwrap());
    disconnect(&sandbox, Agent::Pi);
    assert_eq!(
        ConfigDoc::parse(Format::Json, &fs::read_to_string(&settings).unwrap())
            .unwrap()
            .get_str(&["defaultModel"])
            .as_deref(),
        Some("native")
    );
}

#[test]
fn omp_default_role_edit_restores_private_model_on_disconnect() {
    let sandbox = sandbox("omp-selector-restore");
    let config = Agent::OhMyPi.config_path(&sandbox.home, false);
    write(&config, "{}");
    let settings = config.with_file_name("config.yml");
    write(&settings, "modelRoles:\n  default: original/native\n");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::OhMyPi, &catalog, &claude_options());
    let mut doc = ConfigDoc::parse(Format::Yaml, &fs::read_to_string(&settings).unwrap()).unwrap();
    doc.set_str(&["modelRoles", "default"], "private-ai-proxy/user-choice")
        .unwrap();
    write(&settings, &doc.render().unwrap());
    disconnect(&sandbox, Agent::OhMyPi);
    assert_eq!(
        ConfigDoc::parse(Format::Yaml, &fs::read_to_string(&settings).unwrap())
            .unwrap()
            .get_str(&["modelRoles", "default"])
            .as_deref(),
        Some("original/native")
    );
}

#[test]
fn hermes_disconnect_restores_pap_model_endpoint_overrides() {
    let sandbox = sandbox("hermes-model-endpoint-restore");
    let config = Agent::Hermes.config_path(&sandbox.home, false);
    write(&config, "model:\n  default: original\n");
    let catalog = catalog();
    apply_connect(&sandbox, Agent::Hermes, &catalog, &claude_options());
    let mut doc = ConfigDoc::parse(Format::Yaml, &fs::read_to_string(&config).unwrap()).unwrap();
    doc.set_str(&["model", "base_url"], "http://127.0.0.1:4180/v1")
        .unwrap();
    doc.set_str(&["model", "api_mode"], "chat_completions")
        .unwrap();
    write(&config, &doc.render().unwrap());
    disconnect(&sandbox, Agent::Hermes);
    let restored = ConfigDoc::parse(Format::Yaml, &fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(restored.get_str(&["model", "base_url"]), None);
    assert_eq!(restored.get_str(&["model", "api_mode"]), None);
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
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_some());
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
        Some(ConfigValue::Str("original".into()))
    );
}

#[test]
fn disconnect_restores_original_opencode_model_after_private_model_edit() {
    let sandbox = sandbox("restore-private-opencode-model");
    let agent = Agent::OpenCode;
    let path = agent.config_path(&sandbox.home, false);
    write(
        &path,
        r#"{"model":"other/original","provider":{"other":{"name":"User provider"}}}"#,
    );
    let catalog = catalog();
    apply_connect(&sandbox, agent, &catalog, &claude_options());
    let mut config = doc(&sandbox, agent);
    config
        .set_str(&["model"], "private-ai-proxy/phala/qwen")
        .unwrap();
    write(&path, &config.render().unwrap());
    disconnect(&sandbox, agent);
    assert_eq!(
        doc(&sandbox, agent).get_str(&["model"]).as_deref(),
        Some("other/original")
    );
}

#[test]
fn reconnect_after_endpoint_drift_preserves_original_value() {
    let sandbox = sandbox("reconnect-endpoint-drift");
    let agent = Agent::ClaudeCode;
    let path = agent.config_path(&sandbox.home, false);
    write(
        &path,
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://original.invalid"}}"#,
    );
    let catalog = catalog();
    connect(&sandbox);
    let mut config = doc(&sandbox, agent);
    config
        .set_str(&["env", "ANTHROPIC_BASE_URL"], "https://drift.invalid")
        .unwrap();
    write(&path, &config.render().unwrap());
    let preview = sandbox
        .projector
        .preview(agent, true, Some(&catalog), &claude_options())
        .unwrap();
    assert!(
        sandbox
            .projector
            .apply(
                agent,
                true,
                &preview.revision,
                Some(&catalog),
                &claude_options(),
            )
            .unwrap()
            .authorized
    );
    disconnect(&sandbox, agent);
    assert_eq!(
        doc(&sandbox, agent)
            .get_str(&["env", "ANTHROPIC_BASE_URL"])
            .as_deref(),
        Some("https://original.invalid")
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
                .as_path(),
            path.as_path()
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
fn claude_keeps_preexisting_gateway_flags_through_reconnect_and_disconnect() {
    let sandbox = sandbox("claude-existing-flags");
    let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
    write(
        &path,
        r#"{"env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1","CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY":"1"}}"#,
    );
    for _ in 0..2 {
        assert!(connect(&sandbox).authorized);
    }
    disconnect(&sandbox, Agent::ClaudeCode);
    let restored = doc(&sandbox, Agent::ClaudeCode);
    for flag in [
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
    ] {
        assert_eq!(restored.get_str(&["env", flag]).as_deref(), Some("1"));
    }
}

#[test]
fn claude_takes_over_credentials_via_the_secret_store_and_restores_them() {
    let sandbox = sandbox("claude");
    let path = sandbox.home.join(".claude").join("settings.json");
    write(
        &path,
        r#"{"model": "opus", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-old-secret", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "0"}}"#,
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
        doc.get_str(&["env", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"])
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        doc.get_str(&["env", "ANTHROPIC_BASE_URL"]).as_deref(),
        Some(ENDPOINT)
    );
    assert_eq!(doc.get_str(&["env", "ANTHROPIC_MODEL"]), None);
    assert_eq!(
        doc.get_str(&["model"]).as_deref(),
        Some("openai/gpt-oss-20b")
    );
    assert!(doc
        .get_str(&["apiKeyHelper"])
        .unwrap()
        .contains("--agent-token claude-code"));
    let settings: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(settings["modelPicker"]["replaceBuiltInOptions"], true);
    let models = catalog().for_surface(Agent::ClaudeCode.surface());
    assert_eq!(
        settings["modelPicker"]["options"],
        serde_json::json!(models
            .models
            .iter()
            .map(|model| serde_json::json!({"model": model.id(), "label": model.display_name()}))
            .collect::<Vec<_>>())
    );
    assert!(
        sandbox.secrets.holds("sk-old-secret"),
        "old secret parked in the store"
    );
    let manifest = fs::read_to_string(sandbox.projector.store_path()).unwrap();
    assert!(!manifest.contains("sk-old-secret"));
    assert!(manifest.contains("secret_ref"));

    let smaller = Catalog::from_remote(&json!({ "data": [{ "id": "phala/qwen" }] }), 2).unwrap();
    let statuses = sandbox.projector.scan(Some(&smaller)).unwrap().0;
    let status = agent_status(&statuses, Agent::ClaudeCode);
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
    let restored: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        restored["env"]["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"],
        "0"
    );
    assert!(restored.get("modelPicker").is_none());
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

    // Make the config invalid so the restore step fails after the record was
    // tombstoned.
    let original = fs::read_to_string(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, "invalid json").unwrap();
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
    let statuses = sandbox.projector.scan(None).unwrap().0;
    let status = agent_status(&statuses, Agent::ClaudeCode);
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
    fs::write(&path, &original).unwrap();
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
                .map_err(|error| error.to_string())
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
        let status = agent_status(&statuses, Agent::ClaudeCode);
        assert!(status.recorded);
        assert!(status.attention.as_deref().unwrap().contains("revoked"));
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
    assert_eq!(
        error.code(),
        desktop_core::protocol::ErrorCode::ConfigurationRestoreFailed
    );
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
    let status = agent_status(&statuses, Agent::ClaudeCode);
    assert!(status.recorded && status.attention.is_some());

    sandbox
        .projector
        .tokens
        .set_sync_parent(private_fs::sync_dir);
    disconnect(&sandbox, Agent::ClaudeCode);
    assert!(sandbox.projector.load_store().unwrap().is_empty());
}

#[test]
fn claude_stop_restores_an_edited_owned_model_picker_and_start_regenerates_it() {
    let sandbox = sandbox("claude-stop-edited-picker");
    let agent = Agent::ClaudeCode;
    let path = agent.config_path(&sandbox.home, false);
    let catalog = catalog();
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let mut config = doc(&sandbox, agent);
    let original_picker = config.get_value(&["modelPicker"]).unwrap();
    config
        .set_value(&["modelPicker"], &ConfigValue::Json(json!({"options": []})))
        .unwrap();
    write(&path, &config.render().unwrap());
    assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
    let stopped = doc(&sandbox, agent);
    let failures = sandbox.projector.reconcile(Some(&catalog)).unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(stopped.get_value(&["modelPicker"]), None);
    assert_eq!(
        doc(&sandbox, agent).get_value(&["modelPicker"]),
        Some(original_picker)
    );
    let (statuses, _) = sandbox.projector.scan(Some(&catalog)).unwrap();
    assert!(agent_status(&statuses, agent).authorized);
}

#[test]
fn dsh_disconnect_removes_an_appended_entry_with_only_its_model_edited() {
    let sandbox = sandbox("dsh-disconnect-edited-model");
    let catalog = catalog();
    let agent = Agent::Dsh;
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let path = agent.config_path(&sandbox.home, false);
    let config = doc(&sandbox, agent);
    let entry = EntryKey::id("agent-default-model");
    config
        .entry(&entry)
        .unwrap()
        .unwrap()
        .set_str(&["config", "model"], "phala/qwen")
        .unwrap();
    write(&path, &config.render().unwrap());
    disconnect(&sandbox, agent);
    assert!(doc(&sandbox, agent).entry(&entry).unwrap().is_none());
}

#[test]
fn dsh_stop_removes_an_appended_entry_with_only_its_model_edited_and_start_reprojects() {
    let sandbox = sandbox("dsh-stop-edited-model");
    let catalog = catalog();
    let agent = Agent::Dsh;
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let path = agent.config_path(&sandbox.home, false);
    let config = doc(&sandbox, agent);
    let entry = EntryKey::id("agent-default-model");
    config
        .entry(&entry)
        .unwrap()
        .unwrap()
        .set_str(&["config", "model"], "phala/qwen")
        .unwrap();
    write(&path, &config.render().unwrap());
    assert!(sandbox.projector.reconcile(None).unwrap().is_empty());
    let stopped = doc(&sandbox, agent);
    let failures = sandbox.projector.reconcile(Some(&catalog)).unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert!(stopped.entry(&entry).unwrap().is_none());
    assert_eq!(
        dsh::selected_model(&doc(&sandbox, agent)).as_deref(),
        Some("phala/qwen")
    );
    let (statuses, _) = sandbox.projector.scan(Some(&catalog)).unwrap();
    assert!(agent_status(&statuses, agent).authorized);
}

#[test]
fn dsh_disconnect_keeps_an_appended_entry_with_unrelated_user_edits() {
    let sandbox = sandbox("dsh-keep-edited-entry");
    let catalog = catalog();
    let agent = Agent::Dsh;
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let path = agent.config_path(&sandbox.home, false);
    let config = doc(&sandbox, agent);
    let entry = EntryKey::id("agent-default-model");
    let mut item = config.entry(&entry).unwrap().unwrap();
    item.set_str(&["config", "model"], "phala/qwen").unwrap();
    item.set_str(&["config", "reasoningEffort"], "max").unwrap();
    write(&path, &config.render().unwrap());
    let expected = item.get_value(&[]);
    disconnect(&sandbox, agent);
    assert_eq!(
        doc(&sandbox, agent)
            .entry(&entry)
            .unwrap()
            .unwrap()
            .get_value(&[]),
        expected
    );
}
