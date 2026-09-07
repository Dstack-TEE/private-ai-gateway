//! Oh My Pi v18.1.12, source 4f429faef639d182633d1cb3f6a15254adcf25c1.
//! models-config.ts/config-file.ts and utils/dirs.ts define the file contract;
//! model-config-values.ts uses execSync, and model-registry.ts gives !command
//! precedence over stored auth without falling back when the command fails.

use super::*;
use serde_json::{json, Value};

const PROVIDER: &str = "private-ai-gateway";
const PROVIDER_PATH: &[&str] = &["providers", PROVIDER];

fn agent_dir(home: &Path, tool_env: bool) -> PathBuf {
    if tool_env {
        if let Some(path) = env_path("PI_CODING_AGENT_DIR") {
            return path;
        }
    }
    let root = tool_env
        .then(|| env_path("PI_CONFIG_DIR"))
        .flatten()
        .unwrap_or_else(|| PathBuf::from(".omp"));
    home.join(root).join("agent")
}

pub(super) fn config_path(home: &Path, tool_env: bool) -> PathBuf {
    let dir = agent_dir(home, tool_env);
    let primary = dir.join("models.yml");
    let fallback = dir.join("models.yaml");
    if !primary.exists() && fallback.exists() {
        fallback
    } else {
        primary
    }
}

pub(super) fn validate_host(home: &Path, tool_env: bool) -> Result<(), String> {
    if tool_env {
        // A named profile ignores the ordinary agent-dir override. Do not guess.
        for key in ["OMP_PROFILE", "PI_PROFILE"] {
            if env::var(key)
                .ok()
                .is_some_and(|value| !value.trim().is_empty() && value.trim() != "default")
            {
                return Err("Oh My Pi integration supports the native default profile only; clear named-profile overrides before connecting".into());
            }
        }
        if env_path("PI_CODING_AGENT_DIR").is_some_and(|path| !path.is_absolute()) {
            return Err("Set PI_CODING_AGENT_DIR to an absolute Oh My Pi directory; relative paths depend on the CLI working directory".into());
        }
        if env_path("PI_CONFIG_DIR").is_some_and(|path| {
            path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        }) {
            return Err("Oh My Pi PI_CONFIG_DIR must be a home-relative directory without parent traversal; use PI_CODING_AGENT_DIR for an absolute override".into());
        }
    }
    let dir = agent_dir(home, tool_env);
    if !dir.is_absolute() {
        return Err("Oh My Pi requires an absolute native agent directory".into());
    }
    if !dir.join("models.yml").exists()
        && !dir.join("models.yaml").exists()
        && dir.join("models.json").exists()
    {
        return Err("Oh My Pi has a legacy models.json. Complete its native YAML migration before connecting; this app will not migrate or overwrite it".into());
    }
    Ok(())
}

pub(super) fn validate_config(doc: &ConfigDoc, prior: Option<&Connection>) -> Result<(), String> {
    let ConfigDoc::Yaml(file) = doc else {
        return Err("Oh My Pi requires YAML configuration".into());
    };
    if file.documents().count() != 1 {
        return Err("Oh My Pi requires a single YAML document".into());
    }
    let Some(ConfigValue::Json(root)) = doc.get_value(&[]) else {
        return Err("Oh My Pi requires a plain YAML mapping; aliases, merge keys, custom tags and duplicate keys cannot be safely projected".into());
    };
    if root
        .get("providers")
        .is_some_and(|value| !value.is_object())
    {
        return Err("Oh My Pi providers must be a mapping".into());
    }
    if doc.contains(PROVIDER_PATH)
        && !prior.is_some_and(|record| {
            record.fields.iter().any(|field| {
                field.path == owned(PROVIDER_PATH) && field.value == doc.get_value(PROVIDER_PATH)
            })
        })
    {
        return Err("The Oh My Pi gateway provider already exists outside this connection; it will not be overwritten".into());
    }
    Ok(())
}

pub(super) fn fields(inputs: &Inputs<'_>) -> Result<Vec<Field>, String> {
    let catalog = inputs
        .catalog
        .ok_or("The verified model list is not available")?;
    let models: Vec<Value> = catalog
        .models
        .iter()
        .map(|model| {
            let mut row = json!({"id":model.id(), "name":model.display_name()});
            if let Some(value) = model.remote.context_length.filter(|value| *value > 0) {
                row["contextWindow"] = json!(value);
            }
            if let Some(value) = model.remote.max_output_length.filter(|value| *value > 0) {
                row["maxTokens"] = json!(value);
            }
            let input: Vec<_> = model
                .string_array("input_modalities")
                .into_iter()
                .filter(|value| matches!(value.as_str(), "text" | "image"))
                .collect();
            if !input.is_empty() {
                row["input"] = json!(input);
            }
            if model
                .string_array("supported_features")
                .iter()
                .any(|value| value == "reasoning")
            {
                row["reasoning"] = json!(true);
            }
            // New OMP models require all four rates when cost is present.
            let mut cost = serde_json::Map::new();
            for (source, target) in [
                ("prompt", "input"),
                ("completion", "output"),
                ("input_cache_read", "cacheRead"),
                ("input_cache_write", "cacheWrite"),
            ] {
                if let Some(value) = model
                    .price_per_million(source)
                    .and_then(serde_json::Number::from_f64)
                {
                    cost.insert(target.into(), Value::Number(value));
                }
            }
            if !cost.is_empty() {
                for key in ["input", "output", "cacheRead", "cacheWrite"] {
                    cost.entry(key).or_insert(json!(0));
                }
                row["cost"] = Value::Object(cost);
            }
            row
        })
        .collect();
    Ok(vec![generated_catalog(
        PROVIDER_PATH,
        json!({
            "baseUrl":format!("{}/v1", inputs.endpoint.trim_end_matches('/')),
            "api":"openai-completions", "auth":"apiKey",
            "apiKey":format!("!{}", credential_helper_command(inputs.helper_exe, Agent::OhMyPi)?),
            "models":models,
        }),
        catalog.models.len(),
    )])
}

pub(super) fn stale_helper(record: &Connection, helper: &Path) -> bool {
    let expected = credential_helper_command(helper, Agent::OhMyPi)
        .ok()
        .map(|command| format!("!{command}"));
    record
        .fields
        .iter()
        .filter(|field| field.path == owned(PROVIDER_PATH))
        .any(|field| {
            let Some(ConfigValue::Json(provider)) = &field.value else {
                return true;
            };
            expected.as_deref().is_none_or(|expected| {
                provider.get("apiKey").and_then(Value::as_str) != Some(expected)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{catalog, disconnect, sandbox, write};
    use super::*;

    #[test]
    fn native_directory_overrides_are_checked_in_isolated_processes() {
        const CASE: &str = "PAG_OMP_DIRECTORY_TEST";
        if let Ok(case) = env::var(CASE) {
            let sandbox = sandbox("omp-directory");
            let home = &sandbox.home;
            match case.as_str() {
                "root" => {
                    env::set_var("PI_CONFIG_DIR", "custom");
                }
                "absolute" => {
                    env::set_var("PI_CODING_AGENT_DIR", home.join("custom"));
                }
                "relative" => {
                    env::set_var("PI_CODING_AGENT_DIR", "relative");
                }
                "profile" => {
                    env::set_var("OMP_PROFILE", "work");
                }
                _ => {}
            }
            if matches!(case.as_str(), "relative" | "profile") {
                assert!(validate_host(home, true).is_err());
            } else {
                validate_host(home, true).unwrap();
                let expected = match case.as_str() {
                    "root" => home.join("custom/agent/models.yml"),
                    "absolute" => home.join("custom/models.yml"),
                    _ => home.join(".omp/agent/models.yml"),
                };
                assert_eq!(config_path(home, true), expected);
            }
            return;
        }
        for case in ["default", "root", "absolute", "relative", "profile"] {
            let mut command = Command::new(env::current_exe().unwrap());
            command.args(["--exact", "agents::oh_my_pi::tests::native_directory_overrides_are_checked_in_isolated_processes"])
                .env(CASE, case);
            for key in [
                "PI_CONFIG_DIR",
                "PI_CODING_AGENT_DIR",
                "OMP_PROFILE",
                "PI_PROFILE",
            ] {
                command.env_remove(key);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn external_provider_edit_revokes_without_overwriting_on_disconnect() {
        let sandbox = sandbox("omp-external-edit");
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(Agent::OhMyPi, true, Some(&catalog()), &options)
            .unwrap();
        sandbox
            .projector
            .apply(
                Agent::OhMyPi,
                true,
                &preview.revision,
                Some(&catalog()),
                &options,
            )
            .unwrap();
        let path = config_path(&sandbox.home, false);
        let external =
            "# externally replaced\nproviders: {private-ai-gateway: {apiKey: external-secret}}\n";
        write(&path, external);
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        assert!(
            !statuses
                .iter()
                .find(|status| status.id == "oh-my-pi")
                .unwrap()
                .authorized
        );
        assert!(tokens.is_empty());
        disconnect(&sandbox, Agent::OhMyPi);
        assert_eq!(fs::read_to_string(path).unwrap(), external);
    }

    #[test]
    fn yaml_provider_is_independent_and_restores_the_recorded_fallback_file() {
        let sandbox = sandbox("omp-yaml-journal");
        let dir = sandbox.home.join(".omp/agent");
        let path = dir.join("models.yaml");
        let original = "# user's model file\nproviders:\n  other: {apiKey: 'user-owned', models: []} # keep this\n";
        write(&path, original);
        let pi_path = sandbox.home.join(".pi/agent/models.json");
        write(&pi_path, r#"{"providers":{"user":{"apiKey":"pi-only"}}}"#);
        let auth = dir.join("auth.db");
        write(&auth, "native-owned auth fixture");
        let pi_before = fs::read(&pi_path).unwrap();
        let auth_before = fs::read(&auth).unwrap();
        let pi_token = sandbox.projector.tokens.ensure("pi").unwrap();
        let agent = Agent::OhMyPi;
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(agent, true, Some(&catalog()), &options)
            .unwrap();
        let status = sandbox
            .projector
            .apply(agent, true, &preview.revision, Some(&catalog()), &options)
            .unwrap();
        assert!(status.authorized, "{:?}", status.attention);
        assert!(!dir.join("models.yml").exists());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# user's model file"));
        assert!(text.contains("other: {apiKey: 'user-owned', models: []} # keep this"));
        let doc = ConfigDoc::parse(Format::Yaml, &text).unwrap();
        let Some(ConfigValue::Json(provider)) = doc.get_value(PROVIDER_PATH) else {
            panic!("missing provider")
        };
        assert_eq!(provider["api"], "openai-completions");
        assert_eq!(provider["auth"], "apiKey");
        assert!(
            provider["apiKey"]
                .as_str()
                .unwrap()
                .ends_with("--agent-token oh-my-pi")
                || cfg!(windows)
        );
        assert_eq!(provider["models"].as_array().unwrap().len(), 2);
        let token = sandbox.projector.tokens.read("oh-my-pi").unwrap().unwrap();
        assert_ne!(token, pi_token);
        let (_, tokens) = sandbox.projector.scan(None).unwrap();
        assert_eq!(tokens.agent_for(&token), Some("oh-my-pi"));
        assert_eq!(tokens.agent_for(&pi_token), None);
        write(&dir.join("models.yml"), "providers: {} # higher priority\n");
        let foreign = fs::read(dir.join("models.yml")).unwrap();
        let (statuses, tokens) = sandbox.projector.scan(None).unwrap();
        let status = statuses
            .iter()
            .find(|status| status.id == "oh-my-pi")
            .unwrap();
        assert!(!status.authorized && tokens.is_empty());
        assert!(status
            .attention
            .as_deref()
            .unwrap()
            .contains("location changed"));
        disconnect(&sandbox, agent);
        let restored = ConfigDoc::parse(Format::Yaml, &fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!restored.contains(PROVIDER_PATH));
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("other: {apiKey: 'user-owned', models: []} # keep this"));
        assert_eq!(fs::read(dir.join("models.yml")).unwrap(), foreign);
        assert_eq!(fs::read(pi_path).unwrap(), pi_before);
        assert_eq!(fs::read(auth).unwrap(), auth_before);
    }

    #[test]
    fn legacy_and_ambiguous_yaml_are_refused_without_capture() {
        let sandbox = sandbox("omp-yaml-conflicts");
        let dir = sandbox.home.join(".omp/agent");
        write(
            &dir.join("models.json"),
            r#"{"providers":{"private-ai-gateway":{"apiKey":"legacy-secret"}}}"#,
        );
        assert!(validate_host(&sandbox.home, false)
            .unwrap_err()
            .contains("migration"));
        assert_eq!(
            fs::read_to_string(dir.join("models.json")).unwrap(),
            r#"{"providers":{"private-ai-gateway":{"apiKey":"legacy-secret"}}}"#
        );
        for text in [
            "providers:\n  private-ai-gateway: {apiKey: secret}\n",
            "providers:\n  private-ai-gateway: null\n",
            "providers: []\n",
            "providers: {other: {}, other: {}}\n",
            "base: &base {other: {}}\nproviders: {<<: *base}\n",
            "providers: {}\n---\nproviders: {}\n",
        ] {
            let path = dir.join("models.yml");
            write(&path, text);
            assert!(
                sandbox
                    .projector
                    .preview(
                        Agent::OhMyPi,
                        true,
                        Some(&catalog()),
                        &ConnectOptions::default()
                    )
                    .is_err(),
                "{text}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
            assert!(sandbox.projector.tokens.read("oh-my-pi").unwrap().is_none());
            assert!(!sandbox.projector.store_path().exists());
        }
    }

    #[test]
    fn commented_empty_provider_parent_is_preserved() {
        let sandbox = sandbox("omp-comment-parent");
        let path = config_path(&sandbox.home, false);
        write(&path, "providers: { # important comment\n}\n");
        let options = ConnectOptions::default();
        let preview = sandbox
            .projector
            .preview(Agent::OhMyPi, true, Some(&catalog()), &options)
            .unwrap();
        let status = sandbox
            .projector
            .apply(
                Agent::OhMyPi,
                true,
                &preview.revision,
                Some(&catalog()),
                &options,
            )
            .unwrap();
        assert!(
            status.authorized,
            "{:?}: {}",
            status.error,
            fs::read_to_string(&path).unwrap()
        );
        disconnect(&sandbox, Agent::OhMyPi);
        assert!(fs::read_to_string(path)
            .unwrap()
            .contains("# important comment"));
    }
}
