use super::*;

#[test]
fn codex_model_changes_keep_credentials_but_endpoint_changes_revoke_them() {
    let sandbox = sandbox("codex-model-preference");
    let agent = Agent::Codex;
    let path = agent.config_path(&sandbox.home, false);
    write_executable(
        &sandbox
            .home
            .join(".local/bin")
            .join(if cfg!(windows) { "codex.exe" } else { "codex" }),
        "test cli",
    );
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
    assert_eq!(&models[..bundled.len()], bundled);
    assert_eq!(models.len(), bundled.len() + 2);
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
    assert_eq!(custom["prefer_websockets"], false);
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
            assert_eq!(projected, original);
        }
    }
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
    assert_eq!(alias["prefer_websockets"], false);
}

#[test]
fn inventory_refresh_updates_active_catalog_without_rotating_credentials() {
    let sandbox = sandbox("inventory-refresh");
    let agent = Agent::Pi;
    let path = agent.config_path(&sandbox.home, false);
    write(&path, r#"{"custom":true}"#);
    write_executable(
        &sandbox
            .home
            .join(".local/bin")
            .join(if cfg!(windows) { "pi.exe" } else { "pi" }),
        "test cli",
    );
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
            == "incompatible_model"
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
                == "helper_unavailable"
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
            == "helper_unavailable"
    );
}
