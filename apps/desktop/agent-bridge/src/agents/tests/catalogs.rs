use super::*;

#[test]
fn codex_model_changes_keep_credentials_but_endpoint_changes_revoke_them() {
    let sandbox = sandbox("codex-model-preference");
    let agent = Agent::Codex;
    let path = agent.config_path(&sandbox.home, false);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let options = claude_options();
    let catalog = catalog();
    let preview = sandbox
        .projector
        .preview(agent, true, Some(&catalog), &options)
        .unwrap();
    sandbox
        .projector
        .apply(agent, true, &preview.revision, Some(&catalog), &options)
        .unwrap();
    let token = sandbox.projector.tokens.read(agent.id()).unwrap();
    let mut config = doc(&sandbox, agent);
    config.set_str(&["model"], "phala/qwen").unwrap();
    write(&path, &config.render().unwrap());
    sandbox.projector.reconcile(Some(&catalog)).unwrap();
    assert!(
        sandbox
            .projector
            .status(
                agent,
                &sandbox.projector.load_store().unwrap(),
                Some(&catalog)
            )
            .authorized
    );
    assert_eq!(sandbox.projector.tokens.read(agent.id()).unwrap(), token);
    sandbox.projector.reconcile(None).unwrap();
    let mut interrupted = sandbox.projector.load_store().unwrap();
    let record = interrupted.get_mut(agent.id()).unwrap();
    record.options = options.clone();
    record.attention = Some("Configuration requires explicit repair".into());
    sandbox.projector.save_store(&interrupted).unwrap();
    let repair = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(agent, true, Some(&catalog), &repair)
        .unwrap();
    sandbox
        .projector
        .apply(agent, true, &preview.revision, Some(&catalog), &repair)
        .unwrap();
    sandbox.projector.reconcile(None).unwrap();
    sandbox.projector.reconcile(Some(&catalog)).unwrap();
    assert_eq!(
        doc(&sandbox, agent).get_str(&["model"]).as_deref(),
        Some("phala/qwen")
    );
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_some());
    let mut config = doc(&sandbox, agent);
    config
        .set_str(
            &["model_providers", "private_ai_proxy", "base_url"],
            "https://example.com/v1",
        )
        .unwrap();
    write(&path, &config.render().unwrap());
    sandbox.projector.reconcile(Some(&catalog)).unwrap();
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
    assert_eq!(
        doc(&sandbox, agent)
            .get_str(&["model_providers", "private_ai_proxy", "base_url"])
            .as_deref(),
        Some("https://example.com/v1")
    );
}

#[test]
fn codex_and_opencode_use_official_custom_provider_configs() {
    let sandbox = sandbox("providers");
    let catalog = catalog();
    let options = claude_options();
    let path = Agent::Codex.config_path(&sandbox.home, false);
    write(
        &path,
        "[model_providers.private_ai_proxy]\n\
                  env_key = 'OLD_KEY'\n\
                  experimental_bearer_token = 'old-synthetic-token'\n\
                  requires_openai_auth = true\n",
    );

    let preview = sandbox
        .projector
        .preview(Agent::Codex, true, Some(&catalog), &options)
        .unwrap();
    let status = sandbox
        .projector
        .apply(
            Agent::Codex,
            true,
            &preview.revision,
            Some(&catalog),
            &options,
        )
        .unwrap();
    assert!(status.connected);
    let codex = doc(&sandbox, Agent::Codex);
    for key in [
        "env_key",
        "experimental_bearer_token",
        "requires_openai_auth",
    ] {
        assert_eq!(
            codex.get_value(&["model_providers", "private_ai_proxy", key]),
            None,
        );
    }
    assert!(!fs::read_to_string(sandbox.projector.store_path())
        .unwrap()
        .contains("old-synthetic-token"));
    assert_eq!(
        codex.get_str(&["model_provider"]).as_deref(),
        Some("private_ai_proxy")
    );
    assert_eq!(
        codex
            .get_str(&["model_providers", "private_ai_proxy", "wire_api"])
            .as_deref(),
        Some("responses")
    );
    assert_eq!(
        codex
            .get_str(&["model_providers", "private_ai_proxy", "base_url"])
            .as_deref(),
        Some("http://127.0.0.1:4180/v1")
    );
    assert_eq!(
        codex.get_str(&["model_catalog_json"]).as_deref(),
        Some(
            sandbox
                .projector
                .codex_catalog_path()
                .to_string_lossy()
                .as_ref()
        )
    );
    let generated: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(sandbox.projector.codex_catalog_path()).unwrap())
            .unwrap();
    let baseline: serde_json::Value =
        serde_json::from_str(include_str!("../../../resources/codex/models.json")).unwrap();
    let models = generated["models"].as_array().unwrap();
    let bundled = baseline["models"].as_array().unwrap();
    assert_eq!(models.len(), bundled.len() + 2);
    // Bundled entries are kept for Codex internals but only provider models are listed.
    for (projected, original) in models.iter().zip(bundled) {
        let mut hidden = original.clone();
        hidden["visibility"] = json!("hide");
        assert_eq!(projected, &hidden);
    }
    assert_eq!(
        models
            .iter()
            .filter(|model| model["visibility"] == "list")
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["openai/gpt-oss-20b", "phala/qwen"]
    );
    assert!(models
        .iter()
        .all(|model| model.get("base_instructions").is_none()));
    let custom = models
        .iter()
        .find(|model| model["slug"] == "openai/gpt-oss-20b")
        .unwrap();
    let unknown_limit = models
        .iter()
        .find(|model| model["slug"] == "phala/qwen")
        .unwrap();
    assert_eq!(custom["context_window"], 131072);
    assert_eq!(custom["max_context_window"], 131072);
    assert!(custom["auto_compact_token_limit"].is_null());
    assert!(unknown_limit["context_window"].as_u64().unwrap() > 0);
    assert!(custom["model_messages"]["instructions_template"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert_eq!(custom["support_verbosity"], false);
    assert!(custom["default_verbosity"].is_null());
    assert!(custom["tool_mode"].is_null());
    assert!(custom["apply_patch_tool_type"].is_null());
    assert!(custom["multi_agent_version"].is_null());
    assert_eq!(custom["experimental_supported_tools"], json!([]));
    assert_eq!(custom["web_search_tool_type"], "text");
    assert_eq!(custom["shell_type"], "unified_exec");
    assert_eq!(custom["use_responses_lite"], false);
    assert_eq!(custom["supports_reasoning_effort_updates"], false);
    assert!(custom.get("prefer_websockets").is_none());
    assert_eq!(custom["supports_experimental_context"], false);

    let config_before = fs::read(&path).unwrap();
    let catalog_before = fs::read(sandbox.projector.codex_catalog_path()).unwrap();
    let token_before = sandbox.projector.tokens.read("codex").unwrap();
    apply_connect(&sandbox, Agent::Codex, &catalog, &options);
    assert_eq!(fs::read(&path).unwrap(), config_before);
    assert_eq!(
        fs::read(sandbox.projector.codex_catalog_path()).unwrap(),
        catalog_before
    );
    assert_eq!(
        sandbox.projector.tokens.read("codex").unwrap(),
        token_before
    );
    fs::remove_file(sandbox.projector.codex_catalog_path()).unwrap();
    apply_connect(&sandbox, Agent::Codex, &catalog, &options);
    assert_eq!(
        fs::read(sandbox.projector.codex_catalog_path()).unwrap(),
        catalog_before
    );
    disconnect(&sandbox, Agent::Codex);
    let restored = doc(&sandbox, Agent::Codex);
    assert_eq!(
        restored
            .get_str(&["model_providers", "private_ai_proxy", "name"])
            .as_deref(),
        Some(PRODUCT_NAME)
    );
    for (key, value) in [
        ("env_key", ConfigValue::Str("OLD_KEY".into())),
        (
            "experimental_bearer_token",
            ConfigValue::Str("old-synthetic-token".into()),
        ),
        ("requires_openai_auth", ConfigValue::Bool(true)),
    ] {
        assert_eq!(
            restored.get_value(&["model_providers", "private_ai_proxy", key]),
            Some(value),
        );
    }

    apply_connect(&sandbox, Agent::Codex, &catalog, &options);
    assert_eq!(fs::read(&path).unwrap(), config_before);
    assert_eq!(
        fs::read(sandbox.projector.codex_catalog_path()).unwrap(),
        catalog_before
    );
    disconnect(&sandbox, Agent::Codex);

    let preview = sandbox
        .projector
        .preview(Agent::OpenCode, true, Some(&catalog), &options)
        .unwrap();
    assert!(preview.changes.iter().any(|change| {
        change.key == "provider.private-ai-proxy"
            && change.after.as_deref() == Some("Generated catalog (2 models)")
    }));
    let status = sandbox
        .projector
        .apply(
            Agent::OpenCode,
            true,
            &preview.revision,
            Some(&catalog),
            &options,
        )
        .unwrap();
    assert!(status.connected);
    let opencode = doc(&sandbox, Agent::OpenCode);
    assert_eq!(
        opencode
            .get_str(&["provider", "private-ai-proxy", "npm"])
            .as_deref(),
        Some("@ai-sdk/openai-compatible")
    );
    assert_eq!(
        opencode
            .get_str(&["provider", "private-ai-proxy", "options", "baseURL"])
            .as_deref(),
        Some("http://127.0.0.1:4180/v1")
    );
    disconnect(&sandbox, Agent::OpenCode);
}

#[test]
fn startup_repairs_only_invalid_codex_provider_name() {
    let sandbox = sandbox("codex-provider-name");
    let path = Agent::Codex.config_path(&sandbox.home, false);
    write(
        &path,
        "# keep this comment\n[model_providers.private_ai_proxy]\nbase_url = 'https://example.com/v1'\n",
    );

    sandbox.projector.initialize_store().unwrap();
    let repaired = fs::read_to_string(&path).unwrap();
    assert!(repaired.contains("# keep this comment"));
    let config = doc(&sandbox, Agent::Codex);
    assert_eq!(
        config
            .get_str(&["model_providers", "private_ai_proxy", "name"])
            .as_deref(),
        Some(PRODUCT_NAME)
    );
    assert_eq!(
        config
            .get_str(&["model_providers", "private_ai_proxy", "base_url"])
            .as_deref(),
        Some("https://example.com/v1")
    );

    let mut config = config;
    config
        .set_str(
            &["model_providers", "private_ai_proxy", "name"],
            "Existing Name",
        )
        .unwrap();
    write(&path, &config.render().unwrap());
    sandbox.projector.initialize_store().unwrap();
    assert_eq!(
        doc(&sandbox, Agent::Codex)
            .get_str(&["model_providers", "private_ai_proxy", "name"])
            .as_deref(),
        Some("Existing Name")
    );
}

#[test]
fn codex_overlay_preserves_native_templates_and_unique_slugs() {
    let sandbox = sandbox("codex-native-overlay");
    let catalog = Catalog::from_remote(&json!({"data": [
        {"id": "gpt-5.4", "context_length": 32768,
         "supported_features": ["reasoning", "verbosity"], "input_modalities": ["text", "image"]},
        {"id": "openai/gpt-5.4", "context_length": 65536}
    ]}), 1).unwrap();
    let options = ConnectOptions {
        default_model: Some("openai/gpt-5.4".into()),
    };
    apply_connect(&sandbox, Agent::Codex, &catalog, &options);
    let config = doc(&sandbox, Agent::Codex);
    assert_eq!(
        config.get_str(&["model"]).as_deref(),
        Some("openai/gpt-5.4")
    );
    let generated: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(config.get_str(&["model_catalog_json"]).unwrap()).unwrap(),
    )
    .unwrap();
    let baseline: serde_json::Value =
        serde_json::from_str(include_str!("../../../resources/codex/models.json")).unwrap();
    let models = generated["models"].as_array().unwrap();
    assert_eq!(
        models.len(),
        baseline["models"].as_array().unwrap().len() + 1
    );
    assert_eq!(
        models
            .iter()
            .filter(|model| model["slug"] == "gpt-5.4")
            .count(),
        1
    );
    let native = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.4")
        .unwrap();
    let alias = models
        .iter()
        .find(|model| model["slug"] == "openai/gpt-5.4")
        .unwrap();
    for original in baseline["models"].as_array().unwrap() {
        let projected = models
            .iter()
            .find(|model| model["slug"] == original["slug"])
            .unwrap();
        if original["slug"] == "gpt-5.4" {
            assert_eq!(projected["model_messages"], original["model_messages"]);
            assert_eq!(alias["model_messages"], original["model_messages"]);
            assert_eq!(
                alias["apply_patch_tool_type"],
                original["apply_patch_tool_type"]
            );
        } else {
            let mut hidden = original.clone();
            hidden["visibility"] = json!("hide");
            assert_eq!(projected, &hidden);
        }
    }
    assert_eq!(
        models
            .iter()
            .filter(|model| model["visibility"] == "list")
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["gpt-5.4", "openai/gpt-5.4"]
    );
    assert_eq!(native["context_window"], 32768);
    assert_eq!(native["max_context_window"], 32768);
    assert!(native["auto_compact_token_limit"].is_null());
    assert_eq!(native["default_reasoning_level"], "medium");
    assert_eq!(
        native["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(native["support_verbosity"], true);
    assert_eq!(native["input_modalities"], json!(["text", "image"]));
    assert_eq!(alias["context_window"], 65536);
    assert_eq!(alias["use_responses_lite"], false);
}

#[test]
fn inventory_refresh_updates_active_catalog_without_rotating_credentials() {
    let sandbox = sandbox("inventory-refresh");
    let agent = Agent::Pi;
    let path = agent.config_path(&sandbox.home, false);
    write(&path, r#"{"custom":true}"#);
    let inventory = crate::catalog::EndpointInventory::bundled().unwrap();
    let value = serde_json::to_value(&inventory).unwrap();
    let model = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["endpoint"].as_str() == Some("/v1/chat/completions")
                && entry["status"].as_str() == Some("supported")
                && entry["checks"].as_object().is_some_and(|checks| {
                    checks.values().all(|check| check["status"] == "supported")
                })
        })
        .and_then(|entry| entry["model"].as_str())
        .unwrap();
    let mut catalog = Catalog::from_remote(
        &json!({"data": [{"id": model}, {"id": "unprobed/model"}]}),
        1,
    )
    .unwrap();
    let options = ConnectOptions::default();
    apply_connect(&sandbox, agent, &catalog, &options);
    let token = sandbox.projector.tokens.read(agent.id()).unwrap();
    catalog
        .apply_endpoint_inventory("https://tee.redpill.ai", &inventory)
        .unwrap();
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    assert_eq!(sandbox.projector.tokens.read(agent.id()).unwrap(), token);
    let ConfigValue::Json(provider) = doc(&sandbox, agent)
        .get_value(&["providers", "private-ai-proxy"])
        .unwrap()
    else {
        panic!("missing Pi catalog");
    };
    assert_eq!(provider["models"].as_array().unwrap().len(), 1);
    let record = fs::read(sandbox.projector.store_path()).unwrap();
    assert!(sandbox
        .projector
        .reconcile(Some(&catalog))
        .unwrap()
        .is_empty());
    assert_eq!(fs::read(sandbox.projector.store_path()).unwrap(), record);
    disconnect(&sandbox, agent);
    assert!(doc(&sandbox, agent)
        .get_value(&["providers", "private-ai-proxy"])
        .is_some());
    assert_eq!(
        doc(&sandbox, agent).get_value(&["custom"]),
        Some(ConfigValue::Bool(true))
    );
}

#[test]
fn claude_connect_selects_a_messages_model_without_discovery() {
    let sandbox = sandbox("claude-model-selection");
    let path = Agent::ClaudeCode.config_path(&sandbox.home, false);
    write(&path, r#"{"model":"opus","env":{"KEEP":"value"}}"#);
    let mut catalog = catalog();
    catalog.models[0].supported_surfaces = Some(vec![Surface::Responses]);
    catalog.models[1].supported_surfaces = Some(vec![Surface::Messages]);
    let options = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, true, Some(&catalog), &options)
        .unwrap();
    sandbox
        .projector
        .apply(
            Agent::ClaudeCode,
            true,
            &preview.revision,
            Some(&catalog),
            &options,
        )
        .unwrap();
    let connected = doc(&sandbox, Agent::ClaudeCode);
    assert_eq!(
        connected.get_str(&["env", "ANTHROPIC_MODEL"]).as_deref(),
        Some(catalog.models[1].id())
    );
    assert_eq!(
        connected.get_str(&["env", "ANTHROPIC_BASE_URL"]).as_deref(),
        Some(ENDPOINT)
    );
    assert_eq!(
        connected.get_str(&["env", "KEEP"]).as_deref(),
        Some("value")
    );
    disconnect(&sandbox, Agent::ClaudeCode);
    assert_eq!(
        doc(&sandbox, Agent::ClaudeCode)
            .get_str(&["model"])
            .as_deref(),
        Some("opus")
    );
    assert!(doc(&sandbox, Agent::ClaudeCode)
        .get_str(&["env", "ANTHROPIC_MODEL"])
        .is_none());
}

#[test]
fn pi_and_hermes_use_verified_model_discovery() {
    let sandbox = sandbox("discovery-providers");
    let catalog = Catalog::from_remote(
        &json!({"data": [
            {"id": "openai/gpt-oss-20b", "input_modalities": ["text", "image", "audio"],
             "pricing": {"prompt": "0.000001"}},
            {"id": "phala/qwen"}
        ]}),
        1,
    )
    .unwrap();
    let options = ConnectOptions::default();

    let preview = sandbox
        .projector
        .preview(Agent::Pi, true, Some(&catalog), &options)
        .unwrap();
    assert!(preview.changes.iter().any(|change| {
        change.key == "providers.private-ai-proxy"
            && change.after.as_deref() == Some("Generated catalog (2 models)")
    }));
    sandbox
        .projector
        .apply(Agent::Pi, true, &preview.revision, Some(&catalog), &options)
        .unwrap();
    let pi = doc(&sandbox, Agent::Pi);
    let provider = pi.get_value(&["providers", "private-ai-proxy"]).unwrap();
    let ConfigValue::Json(provider) = provider else {
        panic!("Pi provider must be a generated JSON catalog");
    };
    assert_eq!(provider["api"], "openai-completions");
    assert_eq!(provider["models"].as_array().unwrap().len(), 2);
    assert_eq!(provider["models"][0]["id"], "openai/gpt-oss-20b");
    assert_eq!(provider["models"][0]["input"], json!(["text", "image"]));
    assert_eq!(
        provider["models"][0]["cost"],
        json!({"input": 1.0, "output": 0, "cacheRead": 0, "cacheWrite": 0}),
    );
    assert!(provider["models"][1].get("cost").is_none());
    assert_eq!(
        provider["apiKey"],
        format!(
            "!{}",
            credential_helper_command(&sandbox.projector.helper_exe, Agent::Pi).unwrap()
        )
    );
    #[cfg(windows)]
    {
        let pi_token = sandbox.projector.tokens.read("pi").unwrap().unwrap();
        let config_path = Agent::Pi.config_path(&sandbox.home, sandbox.projector.tool_env);
        let mut changed = provider.clone();
        changed["apiKey"] = json!(format!(
            "!{}",
            helper_command(&sandbox.projector.helper_exe, "pi").unwrap()
        ));
        let value = ConfigValue::Json(changed);
        let mut config = doc(&sandbox, Agent::Pi);
        config
            .set_value(&["providers", "private-ai-proxy"], &value)
            .unwrap();
        write(&config_path, &config.render().unwrap());
        let mut store = sandbox.projector.load_store().unwrap();
        store
            .get_mut("pi")
            .unwrap()
            .fields
            .iter_mut()
            .find(|field| field.path == owned(&["providers", "private-ai-proxy"]))
            .unwrap()
            .value = Some(value);
        sandbox.projector.save_store(&store).unwrap();
        let config_before = fs::read(&config_path).unwrap();
        let record_before = fs::read(sandbox.projector.store_path()).unwrap();
        let token_before = fs::read(sandbox.projector.tokens.path("pi")).unwrap();
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        let pi = statuses.iter().find(|status| status.id == "pi").unwrap();
        assert!(pi.recorded && !pi.connected && !pi.authorized);
        assert!(pi
            .attention
            .as_deref()
            .unwrap()
            .contains("Disconnect, then Connect"));
        assert_eq!(tokens.agent_for(&pi_token), None);
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(
            fs::read(sandbox.projector.store_path()).unwrap(),
            record_before
        );
        assert_eq!(
            fs::read(sandbox.projector.tokens.path("pi")).unwrap(),
            token_before
        );

        config
            .set_str(
                &["providers", "private-ai-proxy", "apiKey"],
                "!user-command",
            )
            .unwrap();
        write(&config_path, &config.render().unwrap());
        let edited = fs::read(&config_path).unwrap();
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        let pi = statuses.iter().find(|status| status.id == "pi").unwrap();
        assert!(pi.recorded && !pi.connected && !pi.authorized);
        assert!(pi.attention.is_some());
        assert!(matches!(
            pi.repair_action,
            Some(AgentRepairAction::Reconnect)
        ));
        assert_eq!(tokens.agent_for(&pi_token), None);
        assert_eq!(fs::read(&config_path).unwrap(), edited);
        assert_eq!(
            fs::read(sandbox.projector.store_path()).unwrap(),
            record_before
        );
        assert_eq!(
            fs::read(sandbox.projector.tokens.path("pi")).unwrap(),
            token_before
        );
    }
    disconnect(&sandbox, Agent::Pi);

    let options = claude_options();
    let path = Agent::Hermes.config_path(&sandbox.home, sandbox.projector.tool_env);
    write(&path, "# keep this comment\ntheme: dark\n");
    let preview = sandbox
        .projector
        .preview(Agent::Hermes, true, Some(&catalog), &options)
        .unwrap();
    sandbox
        .projector
        .apply(
            Agent::Hermes,
            true,
            &preview.revision,
            Some(&catalog),
            &options,
        )
        .unwrap();
    let hermes = doc(&sandbox, Agent::Hermes);
    assert_eq!(
        hermes.get_value(&["providers", "private-ai-proxy", "discover_models"]),
        Some(ConfigValue::Bool(true))
    );
    assert_eq!(
        hermes.get_str(&["model", "provider"]).as_deref(),
        Some("custom:private-ai-proxy")
    );
    disconnect(&sandbox, Agent::Hermes);
    let restored = fs::read_to_string(path).unwrap();
    assert!(restored.contains("# keep this comment"));
    assert!(restored.contains("theme: dark"));
    assert!(restored.contains("private-ai-proxy"));
    let restored = ConfigDoc::parse(Format::Yaml, &restored).unwrap();
    assert!(restored.get_str(&["model", "provider"]).is_none());

    let fresh = self::sandbox("fresh-hermes");
    assert!(fresh
        .projector
        .preview(
            Agent::Hermes,
            true,
            Some(&catalog),
            &ConnectOptions::default()
        )
        .is_ok());
    let preview = fresh
        .projector
        .preview(Agent::Hermes, true, Some(&catalog), &options)
        .unwrap();
    fresh
        .projector
        .apply(
            Agent::Hermes,
            true,
            &preview.revision,
            Some(&catalog),
            &options,
        )
        .unwrap();
    let hermes = doc(&fresh, Agent::Hermes);
    assert_eq!(
        hermes
            .get_str(&["providers", "private-ai-proxy", "transport"])
            .as_deref(),
        Some("chat_completions")
    );
    assert_eq!(
        hermes.get_str(&["providers", "private-ai-proxy", "key_cmd"]),
        Some(credential_helper_command(&fresh.projector.helper_exe, Agent::Hermes).unwrap())
    );
}

#[test]
fn agents_require_a_verified_catalog_model() {
    let sandbox = sandbox("claude-gate");
    assert!(sandbox
        .projector
        .preview(
            Agent::ClaudeCode,
            true,
            Some(&catalog()),
            &ConnectOptions::default()
        )
        .is_ok());
    assert!(
        sandbox
            .projector
            .preview(
                Agent::ClaudeCode,
                true,
                Some(&catalog()),
                &ConnectOptions {
                    default_model: Some("claude-sonnet-4-6".to_string()),
                },
            )
            .unwrap_err()
            .code()
            == desktop_core::protocol::ErrorCode::IncompatibleModel
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(
            &sandbox.projector.helper_exe,
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(sandbox.projector.require_helper().is_err());
        assert!(
            sandbox
                .projector
                .preview(Agent::Codex, true, Some(&catalog()), &claude_options())
                .unwrap_err()
                .code()
                == desktop_core::protocol::ErrorCode::HelperUnavailable
        );
    }
    // Without the bundled helper, agents cannot authenticate.
    fs::remove_file(&sandbox.projector.helper_exe).unwrap();
    let statuses = sandbox.projector.scan(Some(&catalog())).unwrap().0;
    let status = agent_status(&statuses, Agent::ClaudeCode);
    assert!(status
        .error
        .as_deref()
        .is_some_and(|error| error.contains("helper")));
    assert!(
        sandbox
            .projector
            .preview(Agent::ClaudeCode, true, Some(&catalog()), &claude_options())
            .unwrap_err()
            .code()
            == desktop_core::protocol::ErrorCode::HelperUnavailable
    );
}

#[test]
fn a_record_projected_to_another_endpoint_is_projected_again() {
    // As after a crash between saving a new Local API address and
    // re-projecting the agents: the relaunched backend has the new endpoint
    // and a verified catalog of the same revision.
    let sandbox = sandbox("endpoint-moved");
    let agent = Agent::ClaudeCode;
    let path = agent.config_path(&sandbox.home, false);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let catalog = catalog();
    apply_connect(&sandbox, agent, &catalog, &claude_options());
    let moved = Projector::at(
        sandbox.home.clone(),
        sandbox.projector.data_dir.clone(),
        sandbox.projector.helper_exe.clone(),
        "http://127.0.0.1:5190",
        false,
        sandbox.secrets.clone(),
    );
    let token = moved.tokens.read(agent.id()).unwrap();
    assert!(token.is_some());

    moved.reconcile(Some(&catalog)).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("127.0.0.1:5190") && !text.contains(ENDPOINT),
        "{text}"
    );
    let store = moved.load_store().unwrap();
    assert!(moved.status(agent, &store, Some(&catalog)).authorized);
    assert_eq!(moved.tokens.read(agent.id()).unwrap(), token);
}

#[test]
fn qwen_code_projects_a_named_catalog_and_follows_model_switches() {
    let sandbox = sandbox("qwen-code-catalog");
    let agent = Agent::QwenCode;
    let path = agent.config_path(&sandbox.home, false);
    let original = r#"{"modelProviders":{"openai":[{"id":"gpt-native","envKey":"OPENAI_API_KEY"}]},"security":{"auth":{"selectedType":"qwen-oauth"}},"env":{"OTHER":"kept"}}"#;
    write(&path, original);
    let catalog = Catalog::from_remote(
        &json!({"data": [
            {"id": "openai/gpt-oss-20b", "name": "GPT OSS 20B", "is_tee": true, "context_length": 131072},
            {"id": "phala/qwen", "input_modalities": ["text", "image"]}
        ]}),
        1,
    )
    .unwrap();
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let token = sandbox.projector.tokens.read(agent.id()).unwrap().unwrap();
    let ConfigDoc::Json(connected) = doc(&sandbox, agent) else {
        unreachable!()
    };
    assert_eq!(
        connected["modelProviders"]["private-ai-proxy"],
        json!([
            {
                "id": "openai/gpt-oss-20b",
                "name": "GPT OSS 20B [TEE]",
                "envKey": "PRIVATE_AI_PROXY_API_KEY",
                "baseUrl": "http://127.0.0.1:4180/v1",
                "generationConfig": {"contextWindowSize": 131072}
            },
            {
                "id": "phala/qwen",
                "name": "phala/qwen",
                "envKey": "PRIVATE_AI_PROXY_API_KEY",
                "baseUrl": "http://127.0.0.1:4180/v1",
                "generationConfig": {"modalities": {"image": true}}
            }
        ])
    );
    assert_eq!(connected["modelProviders"]["openai"][0]["id"], "gpt-native");
    assert_eq!(connected["providerProtocol"]["private-ai-proxy"], "openai");
    assert_eq!(connected["env"]["PRIVATE_AI_PROXY_API_KEY"], token.as_str());
    assert_eq!(connected["security"]["auth"]["selectedType"], "openai");
    assert_eq!(connected["model"]["name"], "openai/gpt-oss-20b");
    let preview = sandbox
        .projector
        .preview(agent, false, None, &ConnectOptions::default())
        .unwrap();
    assert!(!serde_json::to_string(&preview).unwrap().contains(&token));

    // /model in Qwen Code rewrites model.name; that keeps access.
    let mut switched = doc(&sandbox, agent);
    switched.set_str(&["model", "name"], "phala/qwen").unwrap();
    write(&path, &switched.render().unwrap());
    let (statuses, tokens) = sandbox.projector.scan(Some(&catalog)).unwrap();
    assert!(agent_status(&statuses, agent).authorized);
    assert_eq!(tokens.agent_for(&token), Some(agent.id()));
    // A changed endpoint does not.
    switched
        .set_str(&["providerProtocol", "private-ai-proxy"], "anthropic")
        .unwrap();
    write(&path, &switched.render().unwrap());
    let (statuses, _) = sandbox.projector.scan(Some(&catalog)).unwrap();
    assert!(!agent_status(&statuses, agent).authorized);
    switched
        .set_str(&["providerProtocol", "private-ai-proxy"], "openai")
        .unwrap();
    switched
        .set_str(&["model", "name"], "openai/gpt-oss-20b")
        .unwrap();
    write(&path, &switched.render().unwrap());

    disconnect(&sandbox, agent);
    assert!(sandbox.projector.tokens.read(agent.id()).unwrap().is_none());
    assert_eq!(
        doc(&sandbox, agent).get_value(&[]),
        ConfigDoc::parse(Format::Json, original)
            .unwrap()
            .get_value(&[])
    );
}

#[test]
fn cline_takes_over_its_openai_compatible_provider_and_follows_model_switches() {
    let sandbox = sandbox("cline-provider");
    let agent = Agent::ClineCli;
    // `cline --version` creates only ~/.cline; the settings folder comes later.
    assert!(!agent_status(&sandbox.projector.scan(None).unwrap().0, agent).installed);
    fs::create_dir_all(sandbox.home.join(".cline")).unwrap();
    assert!(agent_status(&sandbox.projector.scan(None).unwrap().0, agent).installed);

    let catalog = catalog();
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let token = sandbox.projector.tokens.read(agent.id()).unwrap().unwrap();
    let path = sandbox.home.join(".cline/data/settings/providers.json");
    assert_eq!(agent.config_path(&sandbox.home, false), path);
    let ConfigDoc::Json(connected) = doc(&sandbox, agent) else {
        unreachable!()
    };
    let slot = &connected["providers"]["openai-compatible"];
    assert_eq!(connected["version"], 1);
    assert_eq!(connected["lastUsedProvider"], "openai-compatible");
    assert_eq!(
        slot["settings"],
        json!({
            "provider": "openai-compatible",
            "baseUrl": "http://127.0.0.1:4180/v1",
            "apiKey": token,
            "model": "openai/gpt-oss-20b",
        })
    );
    // Cline's schema requires an ISO timestamp on every stored provider.
    let updated_at = slot["updatedAt"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(updated_at).is_ok());
    assert!(updated_at.ends_with('Z'));

    // Picking another model or provider in Cline keeps access.
    let mut switched = doc(&sandbox, agent);
    switched
        .set_str(
            &["providers", "openai-compatible", "settings", "model"],
            "phala/qwen",
        )
        .unwrap();
    switched
        .set_str(
            &["providers", "openai-compatible", "updatedAt"],
            "2026-09-28T00:00:00.000Z",
        )
        .unwrap();
    write(&path, &switched.render().unwrap());
    assert!(agent_status(&sandbox.projector.scan(Some(&catalog)).unwrap().0, agent).authorized);

    disconnect(&sandbox, agent);
    // The model picked in the slot the connection created goes with it.
    assert_eq!(
        doc(&sandbox, agent).get_value(&[]),
        Some(ConfigValue::Json(json!({})))
    );
}

#[test]
fn crush_writes_its_data_file_and_keeps_model_picks() {
    let sandbox = sandbox("crush-data");
    let agent = Agent::Crush;
    let path = agent.config_path(&sandbox.home, false);
    if !cfg!(windows) {
        assert_eq!(path, sandbox.home.join(".local/share/crush/crush.json"));
    }
    let original =
        r#"{"models":{"large":{"model":"claude","provider":"anthropic"}},"recent_models":{}}"#;
    write(&path, original);
    let catalog = Catalog::from_remote(
        &json!({"data": [{
            "id": "openai/gpt-oss-20b", "name": "GPT OSS 20B", "is_tee": true,
            "context_length": 131072, "max_output_length": 8192,
            "supported_features": ["reasoning"], "input_modalities": ["text", "image"],
            "pricing": {"prompt": "0.0000001", "completion": "0.0000005"}
        }]}),
        1,
    )
    .unwrap();
    assert!(apply_connect(&sandbox, agent, &catalog, &claude_options()).authorized);
    let ConfigDoc::Json(connected) = doc(&sandbox, agent) else {
        unreachable!()
    };
    let provider = &connected["providers"]["private-ai-proxy"];
    let command = agent_credential_command(&sandbox.projector.helper_exe, agent, None).unwrap();
    assert_eq!(
        *provider,
        json!({
            "name": PRODUCT_NAME,
            "type": "openai-compat",
            "base_url": "http://127.0.0.1:4180/v1",
            "api_key": format!("$({command})"),
            "discover_models": false,
            "models": [{
                "id": "openai/gpt-oss-20b",
                "name": "GPT OSS 20B [TEE]",
                "can_reason": true,
                "supports_attachments": true,
                "context_window": 131072,
                "default_max_tokens": 8192,
                "cost_per_1m_in": 0.1,
                "cost_per_1m_out": 0.5
            }]
        })
    );
    assert_eq!(
        connected["models"]["large"],
        json!({"model": "openai/gpt-oss-20b", "provider": "private-ai-proxy"})
    );
    #[cfg(unix)]
    {
        // Crush expands the key like a POSIX shell does.
        let token = sandbox.projector.tokens.read(agent.id()).unwrap().unwrap();
        write_executable(
            &sandbox.projector.helper_exe,
            &format!("#!/bin/sh\nprintf %s {token}\n"),
        );
        let output = Command::new("/bin/sh")
            .args(["-c", &format!("printf %s \"$({command})\"")])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), token);
    }

    // Crush saves a model picked in its UI as a whole models.large object.
    let mut picked = doc(&sandbox, agent);
    picked
        .set_value(
            &["models", "large"],
            &ConfigValue::Json(json!({
                "model": "openai/gpt-oss-20b", "provider": "private-ai-proxy",
                "reasoning_effort": "high"
            })),
        )
        .unwrap();
    write(&path, &picked.render().unwrap());
    assert!(agent_status(&sandbox.projector.scan(Some(&catalog)).unwrap().0, agent).authorized);
    picked
        .set_value(
            &["models", "large"],
            &ConfigValue::Json(
                json!({"model": "openai/gpt-oss-20b", "provider": "private-ai-proxy"}),
            ),
        )
        .unwrap();
    write(&path, &picked.render().unwrap());

    disconnect(&sandbox, agent);
    let mut restored = doc(&sandbox, agent);
    assert!(restored
        .get_value(&["providers", "private-ai-proxy", "api_key"])
        .is_none());
    restored.remove(&["providers", "private-ai-proxy"]).unwrap();
    assert_eq!(
        restored.get_value(&[]),
        ConfigDoc::parse(Format::Json, original)
            .unwrap()
            .get_value(&[])
    );
}

#[test]
fn aider_routes_openai_models_and_keeps_its_yaml() {
    let sandbox = sandbox("aider-config");
    let agent = Agent::Aider;
    // Aider creates ~/.aider on its first run; its config sits in Home.
    assert!(!agent_status(&sandbox.projector.scan(None).unwrap().0, agent).installed);
    fs::create_dir_all(sandbox.home.join(".aider")).unwrap();
    assert!(agent_status(&sandbox.projector.scan(None).unwrap().0, agent).installed);
    let path = sandbox.home.join(".aider.conf.yml");
    assert_eq!(agent.config_path(&sandbox.home, false), path);
    let original = "# my settings\nmodel: gpt-4o\nopenai-api-base: https://api.openai.com/v1\ndark-mode: true\n";
    write(&path, original);
    assert!(apply_connect(&sandbox, agent, &catalog(), &claude_options()).authorized);
    let token = sandbox.projector.tokens.read(agent.id()).unwrap().unwrap();
    let connected = fs::read_to_string(&path).unwrap();
    assert!(connected.starts_with("# my settings\n"));
    let connected = doc(&sandbox, agent);
    assert_eq!(
        connected.get_str(&["openai-api-base"]).as_deref(),
        Some("http://127.0.0.1:4180/v1")
    );
    assert_eq!(connected.get_str(&["openai-api-key"]), Some(token));
    assert_eq!(
        connected.get_str(&["model"]).as_deref(),
        Some("openai/openai/gpt-oss-20b")
    );
    assert_eq!(
        connected.get_value(&["dark-mode"]),
        Some(ConfigValue::Bool(true))
    );
    disconnect(&sandbox, agent);
    assert_eq!(
        doc(&sandbox, agent).get_value(&[]),
        ConfigDoc::parse(Format::Yaml, original)
            .unwrap()
            .get_value(&[])
    );
    assert!(fs::read_to_string(&path)
        .unwrap()
        .starts_with("# my settings\n"));
}
