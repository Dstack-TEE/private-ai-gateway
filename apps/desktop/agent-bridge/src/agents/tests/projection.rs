use super::*;

#[test]
fn equal_fields_keep_their_original_values_and_bytes_for_every_agent() {
    let sandbox = sandbox("equal-fields");
    let catalog = catalog();
    let options = claude_options();
    for agent in Agent::ALL {
        let token_path = sandbox.projector.tokens.path(agent.id());
        let catalog_path = sandbox.projector.codex_catalog_path();
        let inputs = Inputs {
            file_credentials: false,
            endpoint: ENDPOINT,
            helper_exe: &sandbox.projector.helper_exe,
            token_path: &token_path,
            codex_catalog_path: &catalog_path,
            catalog: Some(&catalog),
            options: &options,
        };
        // Generated provider objects refuse unmanaged values separately. These
        // are all the agent's scalar/list fields, including dsh's exact rows.
        let fields: Vec<_> = fields(agent, &inputs)
            .unwrap()
            .into_iter()
            .filter(|field| {
                field.value.is_some() && !matches!(field.value, Some(ConfigValue::Json(_)))
            })
            .collect();
        let mut doc = if agent == Agent::Dsh {
            ConfigDoc::parse(
                Format::YamlList,
                "# already configured\n\
                 - id: agent-default-model\n  config:\n    provider: 'private-ai-proxy'\n    model: 'openai/gpt-oss-20b'\n\
                 - id: acp\n  config:\n    provider: 'private-ai-proxy'\n    model: 'openai/gpt-oss-20b'\n\
                 - id: web-search-deepseek\n  disabled: true # mine\n\
                 - id: credentials\n  disabled: false\n",
            ).unwrap()
        } else {
            let mut doc = ConfigDoc::parse(agent.format(), "").unwrap();
            for field in &fields {
                doc.set_value(&refs(&field.path), field.value.as_ref().unwrap())
                    .unwrap();
            }
            doc
        };
        let original = doc.render().unwrap();
        let first = project(&mut doc, &fields, None, agent).unwrap();
        assert!(first.changes.is_empty(), "{}", agent.id());
        for secret in &first.pending_secrets {
            sandbox.secrets.set(&secret.entry, &secret.value).unwrap();
        }
        let record = first.record.unwrap();
        assert!(
            record
                .fields
                .iter()
                .filter(|field| !field.container)
                .all(|field| field.previous.is_some()),
            "{}",
            agent.id()
        );
        let serialized = serde_json::to_string(&record).unwrap();
        let record = serde_json::from_str(&serialized).unwrap();
        let second = project(&mut doc, &fields, Some(&record), agent).unwrap();
        assert!(second.changes.is_empty(), "{}", agent.id());
        let restored =
            restore(&mut doc, &second.record.unwrap(), sandbox.secrets.as_ref()).unwrap();
        assert_eq!(doc.render().unwrap(), original, "{}", agent.id());
        // Exact YAML rows may report an equal scalar's source restoration.
        if agent != Agent::Dsh {
            assert!(restored.changes.is_empty(), "{}", agent.id());
        }
    }
}
