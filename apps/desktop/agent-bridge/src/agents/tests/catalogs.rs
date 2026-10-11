use super::*;

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
