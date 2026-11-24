//! Oh My Pi v18.3.2, source 7853b4e499936f9dcc13c9b64adb55f6b342aabf.
//! models-config.ts/config-file.ts and utils/dirs.ts define the file contract;
//! resolve-config-value.ts runs `!command` values through `/bin/sh -c`, and
//! model-registry.ts gives them precedence over stored auth.

use super::*;
use serde_json::{json, Value};

const PROVIDER: &str = "private-ai-proxy";
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
                field.path == owned(PROVIDER_PATH) && holds_managed_field(Agent::OhMyPi, doc, field)
            })
        })
    {
        return Err("The Oh My Pi private-ai-proxy provider already exists outside this connection; it will not be overwritten".into());
    }
    Ok(())
}

pub(super) fn validate_settings(
    path: &Path,
    projection: &ConfigDoc,
    catalog: Option<&Catalog>,
) -> Result<Option<String>, String> {
    let Some(settings) = selection::omp_settings(path)? else {
        return Ok(None);
    };
    let disabled = filter_array(&settings, "disabledProviders");
    let (global, scoped) = filter_values(&disabled, "providers");
    if global.contains(&PROVIDER) {
        return Err("Oh My Pi disabledProviders excludes private-ai-proxy; resolve that filter before connecting".into());
    }
    if scoped.iter().any(|scope| scope.contains(&PROVIDER)) {
        return Ok(Some(
            "Oh My Pi disabledProviders excludes private-ai-proxy in a path-scoped configuration."
                .into(),
        ));
    }

    let enabled = filter_array(&settings, "enabledModels");
    let (global, scoped) = filter_values(&enabled, "models");
    if global.is_empty() && scoped.is_empty() {
        return Ok(None);
    }
    let ids = chat_model_ids(projection, catalog);
    if let Some(reason) = model_filter_notice(&global, &ids, false) {
        return Ok(Some(format!("Oh My Pi enabledModels {reason}.")));
    }
    // A scope adds to the global list. When a global selector matches a PAP
    // model, further selectors cannot exclude it.
    Ok(scoped
        .iter()
        .find_map(|scope| model_filter_notice(scope, &ids, !global.is_empty()))
        .map(|reason| format!("Oh My Pi path-scoped enabledModels {reason}.")))
}

fn filter_array(settings: &ConfigDoc, key: &str) -> Vec<Value> {
    match settings.get_value(&[key]).map(|value| value.to_json()) {
        Some(Value::Array(entries)) => entries,
        _ => Vec::new(),
    }
}

// settings.ts@v19.1.1 resolvePathScopedArray adds applicable scoped strings
// to the unscoped strings. PAP has no Oh My Pi session cwd, so it reports a
// possible scope conflict without treating it as a global provider block.
fn filter_values<'a>(entries: &'a [Value], value_key: &str) -> (Vec<&'a str>, Vec<Vec<&'a str>>) {
    let mut global = Vec::new();
    let mut scoped = Vec::new();
    for entry in entries {
        if let Some(id) = entry.as_str() {
            global.push(id);
        } else if ["path", "paths", "pathPrefix", "pathPrefixes"]
            .iter()
            .any(|key| value_strings(entry.get(key)).next().is_some())
        {
            let values: Vec<_> = ["values", "items", value_key]
                .iter()
                .flat_map(|key| value_strings(entry.get(key)))
                .collect();
            if !values.is_empty() {
                scoped.push(values);
            }
        }
    }
    (global, scoped)
}

fn value_strings(value: Option<&Value>) -> impl Iterator<Item = &str> {
    value.and_then(Value::as_str).into_iter().chain(
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    )
}

fn chat_model_ids(projection: &ConfigDoc, catalog: Option<&Catalog>) -> Vec<String> {
    if let Some(catalog) = catalog {
        return catalog
            .models
            .iter()
            .filter(|model| model.supports(Surface::ChatCompletions))
            .map(|model| model.id().to_lowercase())
            .collect();
    }
    // Token-only scans have no remote catalog. The native projection has chat rows.
    let Some(ConfigValue::Json(Value::Array(rows))) =
        projection.get_value(&["providers", PROVIDER, "models"])
    else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .map(str::to_lowercase)
        .collect()
}

// model-resolver.ts@v19.1.1 matches both full and bare IDs, including a
// thinking suffix. Unsupported selector syntax is advisory.
fn model_filter_notice(
    patterns: &[&str],
    ids: &[String],
    global_match: bool,
) -> Option<&'static str> {
    let mut matched = global_match;
    for pattern in patterns {
        let (base, literal_first) = split_thinking_suffix(pattern);
        let result = (|| {
            if literal_first && pattern_matches(pattern, ids, false)? {
                return Some(true);
            }
            pattern_matches(base, ids, true)
        })();
        match result {
            Some(value) => matched |= value,
            None => return Some("uses a selector Private AI Proxy cannot evaluate"),
        }
    }
    (!patterns.is_empty() && !matched).then_some("appears to exclude all Private AI Proxy models")
}

// model-selector.ts and thinking.ts@v19.1.1 accept exact thinking levels
// and unambiguous prefixes. Let literal IDs or globs ending in :max or
// :auto match first, as model-resolver.ts does.
fn split_thinking_suffix(pattern: &str) -> (&str, bool) {
    let Some((base, suffix)) = pattern.rsplit_once(':') else {
        return (pattern, false);
    };
    let levels = [
        "inherit", "off", "minimal", "low", "medium", "high", "xhigh", "max",
    ];
    let mut matching = levels
        .iter()
        .filter(|level| suffix.len() >= 2 && level.starts_with(suffix));
    match (matching.next(), matching.next()) {
        (Some(level), None) => (base, *level == "max"),
        _ if suffix == "auto" => (base, true),
        _ => (pattern, false),
    }
}

fn pattern_matches(pattern: &str, ids: &[String], fuzzy: bool) -> Option<bool> {
    // Bun supports syntax outside the Rust glob crate's grammar.
    if pattern.starts_with(['!', '@'])
        || pattern.contains(['{', '}', '\\'])
        || ["?(", "*(", "+(", "@(", "!(", "[^"]
            .iter()
            .any(|part| pattern.contains(part))
    {
        return None;
    }
    let pattern = pattern.to_lowercase();
    if !pattern.contains(['*', '?', '[']) {
        return Some(ids.iter().any(|id| {
            id == &pattern
                || pattern
                    .strip_prefix(PROVIDER)
                    .and_then(|rest| rest.strip_prefix('/'))
                    == Some(id.as_str())
                || (fuzzy && !pattern.is_empty() && !pattern.contains('/') && id.contains(&pattern))
        }));
    }
    let pattern = glob::Pattern::new(&pattern).ok()?;
    let options = glob::MatchOptions {
        require_literal_separator: true,
        ..glob::MatchOptions::new()
    };
    Some(ids.iter().any(|id| {
        pattern.matches_with(id, options)
            || pattern.matches_with(&format!("{PROVIDER}/{id}"), options)
    }))
}

pub(super) fn fields(inputs: &Inputs<'_>) -> Result<Vec<Field>, String> {
    let catalog = inputs
        .catalog
        .ok_or("The verified model list is not available")?;
    let models = model_rows(
        catalog,
        &ModelRows {
            positive_limits: true,
            any_input: false,
            reasoning: true,
            cost: Cost::Complete,
        },
    );
    let models: Vec<_> = models
        .into_iter()
        .zip(&catalog.models)
        .filter_map(|(row, model)| model.supports(Surface::ChatCompletions).then_some(row))
        .collect();
    let count = models.len();
    Ok(vec![generated_catalog(
        PROVIDER_PATH,
        json!({
            "baseUrl": api_url(inputs.endpoint)?,
            "api":"openai-completions", "auth":"apiKey",
            "apiKey":format!("!{}", inputs.credential_command(Agent::OhMyPi)?),
            "models":models,
        }),
        count,
    )])
}

#[cfg(test)]
mod tests {
    use super::super::tests::{catalog, disconnect, sandbox, write};
    use super::*;

    #[test]
    fn native_directory_overrides_are_checked_in_isolated_processes() {
        const CASE: &str = "PAP_OMP_DIRECTORY_TEST";
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
        assert!(restored.contains(PROVIDER_PATH));
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("other: {apiKey: 'user-owned', models: []} # keep this"));
        assert_eq!(fs::read(dir.join("models.yml")).unwrap(), foreign);
        assert_eq!(fs::read(pi_path).unwrap(), pi_before);
        assert_eq!(fs::read(auth).unwrap(), auth_before);
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
