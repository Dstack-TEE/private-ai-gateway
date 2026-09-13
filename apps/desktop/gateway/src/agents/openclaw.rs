//! Native-host projection, verified against openclaw/openclaw
//! 78d8fcf4e55705a0a3cd7241ea61ae24d12da1ee: config/zod-schema.core.ts,
//! config/paths.ts, secrets/resolve.ts and secrets/exec-provider-path-validation.ts.
//! JSON5 editing and conditional restoration belong to the parent projector.

use super::*;
use serde_json::{json, Map, Value};

const PROVIDER: &str = "private-ai-proxy";
const PROVIDER_PATH: &[&str] = &["models", "providers", PROVIDER];
const SECRET_PATH: &[&str] = &["secrets", "providers", PROVIDER];
const PRIMARY_PATH: &[&str] = &["agents", "defaults", "model", "primary"];

pub(super) fn fields(inputs: &Inputs<'_>) -> Result<Vec<Field>, String> {
    let catalog = inputs
        .catalog
        .ok_or("The verified model list is not available")?;
    let selected = inputs
        .options
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if selected.is_some_and(|id| catalog.get(id).is_none()) {
        return Err("Choose an OpenClaw model from the verified model list".into());
    }
    let endpoint = reqwest::Url::parse(inputs.endpoint).map_err(|_| "Invalid local gateway URL")?;
    if endpoint.scheme() != "http"
        || !matches!(
            endpoint.host_str(),
            Some("127.0.0.1" | "[::1]" | "localhost")
        )
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.path() != "/"
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err("OpenClaw projection requires the native host's loopback gateway".into());
    }
    let models: Vec<Value> = catalog
        .models
        .iter()
        .map(|model| {
            let mut row = json!({"id": model.id(), "name": model.display_name()});
            if let Some(n) = model.remote.context_length.filter(|n| *n > 0) {
                row["contextWindow"] = json!(n);
            }
            if let Some(n) = model.remote.max_output_length.filter(|n| *n > 0) {
                row["maxTokens"] = json!(n);
            }
            let input: Vec<_> = model
                .string_array("input_modalities")
                .into_iter()
                .filter(|mode| matches!(mode.as_str(), "text" | "image"))
                .collect();
            if !input.is_empty() {
                row["input"] = json!(input);
            }
            if model
                .string_array("supported_features")
                .iter()
                .any(|s| s == "reasoning")
            {
                row["reasoning"] = json!(true);
            }
            let mut cost = Map::new();
            for (source, target) in [
                ("prompt", "input"),
                ("completion", "output"),
                ("input_cache_read", "cacheRead"),
                ("input_cache_write", "cacheWrite"),
            ] {
                if let Some(n) = model
                    .price_per_million(source)
                    .and_then(serde_json::Number::from_f64)
                {
                    cost.insert(target.into(), Value::Number(n));
                }
            }
            if !cost.is_empty() {
                row["cost"] = Value::Object(cost);
            }
            row
        })
        .collect();
    let command = helper_path(inputs.helper_exe, inputs.token_path)?;
    let command = command
        .to_str()
        .ok_or("OpenClaw requires a UTF-8 helper path")?;
    validate_command_text(command)?;
    let mut result = vec![
        generated_catalog(
            PROVIDER_PATH,
            json!({
                "baseUrl": format!("{}/v1", inputs.endpoint.trim_end_matches('/')),
                "api": "openai-completions",
                "apiKey": {"source": "exec", "provider": PROVIDER, "id": "openclaw"},
                "models": models,
            }),
            catalog.models.len(),
        ),
        Field {
            path: owned(SECRET_PATH),
            value: Some(ConfigValue::Json(json!({
                "source": "exec", "command": command,
                "args": ["--agent-token", "openclaw"], "jsonOnly": false,
                "env": helper_env(inputs.token_path)?,
            }))),
            preview: Some("Native-host OpenClaw token helper".into()),
        },
    ];
    if let Some(id) = selected {
        result.push(set(PRIMARY_PATH, format!("{PROVIDER}/{id}")));
    }
    Ok(result)
}

/// Refuse existing namespaces, including nulls, without reading any secret file.
pub(super) fn validate_config(doc: &ConfigDoc, prior: Option<&Connection>) -> Result<(), String> {
    let Some(ConfigValue::Json(root)) = doc.get_value(&[]) else {
        return Err("OpenClaw requires an object-shaped JSON5 configuration".into());
    };
    validate_references(&root)?;
    if root
        .pointer("/gateway/mode")
        .and_then(Value::as_str)
        .is_some_and(|s| s != "local")
    {
        return Err("OpenClaw remote gateways are not native-host projections".into());
    }
    for path in [PROVIDER_PATH, SECRET_PATH] {
        let mut node = &root;
        for (index, key) in path.iter().enumerate() {
            let Some(object) = node.as_object() else {
                return Err(format!(
                    "OpenClaw {} must be an object",
                    path[..index].join(".")
                ));
            };
            if index + 1 == path.len()
                && object
                    .keys()
                    .any(|key| key != PROVIDER && key.trim().eq_ignore_ascii_case(PROVIDER))
            {
                return Err("OpenClaw has a conflicting alias for the managed provider".into());
            }
            let Some(next) = object.get(*key) else { break };
            if index + 1 == path.len() {
                let ours = prior.is_some_and(|record| {
                    record.fields.iter().any(|field| {
                        field.path == owned(path) && field.value == doc.get_value(path)
                    })
                });
                if !ours {
                    return Err(format!(
                        "OpenClaw {} already exists outside this connection",
                        path.join(".")
                    ));
                }
            }
            node = next;
        }
    }
    Ok(())
}

fn validate_references(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            if object.contains_key("$include") {
                return Err("OpenClaw configurations with $include are not supported; use a native standalone config".into());
            }
            if object.get("provider").and_then(Value::as_str) == Some(PROVIDER)
                && object.contains_key("source")
                && (object.get("source").and_then(Value::as_str) != Some("exec")
                    || object.get("id").and_then(Value::as_str) != Some("openclaw"))
            {
                return Err(
                    "The OpenClaw token helper supports only its single openclaw exec SecretRef"
                        .into(),
                );
            }
            for child in object.values() {
                validate_references(child)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                validate_references(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn validate_selection(doc: &ConfigDoc, options: &ConnectOptions) -> Result<(), String> {
    if options
        .default_model
        .as_deref()
        .is_none_or(|s| s.trim().is_empty())
    {
        return Ok(());
    }
    let Some(ConfigValue::Json(root)) = doc.get_value(&[]) else {
        return Err("OpenClaw requires an object-shaped JSON5 configuration".into());
    };
    for path in ["/agents", "/agents/defaults", "/agents/defaults/model"] {
        if root.pointer(path).is_some_and(|value| !value.is_object()) {
            return Err("Selecting an OpenClaw primary requires object-shaped agents.defaults.model; scalar models are not migrated".into());
        }
    }
    Ok(())
}

pub(super) fn selected_model(doc: &ConfigDoc) -> Option<String> {
    doc.get_str(PRIMARY_PATH)?
        .strip_prefix(&format!("{PROVIDER}/"))
        .map(str::to_owned)
}

pub(super) fn config_path(home: &Path, tool_env: bool) -> PathBuf {
    let override_path = |key| tool_env.then(|| env_path(key)).flatten();
    override_path("OPENCLAW_CONFIG_PATH").unwrap_or_else(|| {
        override_path("OPENCLAW_STATE_DIR")
            .unwrap_or_else(|| {
                override_path("OPENCLAW_HOME")
                    .unwrap_or_else(|| home.into())
                    .join(".openclaw")
            })
            .join("openclaw.json")
    })
}

pub(super) fn validate_host(home: &Path, tool_env: bool) -> Result<(), String> {
    if tool_env {
        for key in [
            "OPENCLAW_CONFIG_PATH",
            "OPENCLAW_STATE_DIR",
            "OPENCLAW_HOME",
        ] {
            if env_path(key)
                .is_some_and(|p| !p.is_absolute() || p.to_str().is_none_or(|s| s.trim() != s))
            {
                return Err(format!(
                    "Set {key} to an absolute native-host path before connecting OpenClaw"
                ));
            }
        }
        if env::var("OPENCLAW_PROFILE")
            .ok()
            .is_some_and(|p| !p.trim().is_empty() && !p.eq_ignore_ascii_case("default"))
            || env::var("OPENCLAW_NIX_MODE").as_deref() == Ok("1")
        {
            return Err("Only the native default OpenClaw profile is supported".into());
        }
    }
    let path = config_path(home, tool_env);
    if !path.is_absolute() || path.to_string_lossy().starts_with("\\\\") {
        return Err("OpenClaw requires an absolute local host configuration path".into());
    }
    if !(tool_env && env_path("OPENCLAW_CONFIG_PATH").is_some()) {
        let effective_home = if tool_env {
            env_path("OPENCLAW_HOME").unwrap_or_else(|| home.into())
        } else {
            home.into()
        };
        if effective_home.join(".clawdbot").exists()
            || path
                .parent()
                .is_some_and(|p| p.join("clawdbot.json").exists())
        {
            return Err("OpenClaw legacy config discovery is ambiguous; set OPENCLAW_CONFIG_PATH explicitly".into());
        }
        if tool_env
            && env_path("OPENCLAW_STATE_DIR").is_some()
            && !path.exists()
            && effective_home.join(".openclaw/openclaw.json").exists()
        {
            return Err("OpenClaw config discovery falls back outside OPENCLAW_STATE_DIR; set OPENCLAW_CONFIG_PATH explicitly".into());
        }
    }
    Ok(())
}

fn data_dir(token_path: &Path) -> Result<&Path, String> {
    if !token_path.is_absolute()
        || token_path.file_name().is_none_or(|s| s != "openclaw")
        || token_path
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|s| s != "agent-tokens")
    {
        return Err("OpenClaw requires its own native-host token path".into());
    }
    token_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "Invalid OpenClaw token directory".into())
}

fn helper_path(source: &Path, token_path: &Path) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let _ = source;
        Ok(data_dir(token_path)?
            .join("helpers")
            .join(helper_binary_name()))
    }
    #[cfg(not(unix))]
    {
        let _ = data_dir(token_path)?;
        Ok(source.into())
    }
}

fn helper_env(token_path: &Path) -> Result<Value, String> {
    let dir = data_dir(token_path)?;
    if dir.components().any(|part| {
        matches!(
            part,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err("OpenClaw requires a normalized native-host token directory".into());
    }
    let parent = dir.parent().ok_or("Invalid OpenClaw app data directory")?;
    if dir.file_name().is_some_and(|s| s == ".private-ai-proxy")
        && env_path(HOME_OVERRIDE_ENV).as_deref() == Some(parent)
    {
        return Ok(json!({HOME_OVERRIDE_ENV: path_text(parent)?}));
    }
    let canonical =
        fs::canonicalize(dir).map_err(|_| "Cannot resolve the native OpenClaw token directory")?;
    let dir = canonical.as_path();
    let parent = dir.parent().ok_or("Invalid OpenClaw app data directory")?;
    if dir.file_name().is_none_or(|s| s != APP_IDENTIFIER) {
        return Err(
            "The OpenClaw token directory does not match the native app data layout".into(),
        );
    }
    if cfg!(target_os = "macos") {
        if parent
            .file_name()
            .is_none_or(|s| s != "Application Support")
            || parent
                .parent()
                .and_then(Path::file_name)
                .is_none_or(|s| s != "Library")
        {
            return Err(
                "The OpenClaw token directory does not match the macOS app data layout".into(),
            );
        }
        let home = parent
            .parent()
            .and_then(Path::parent)
            .ok_or("Invalid macOS app data directory")?;
        Ok(json!({"HOME": path_text(home)?}))
    } else if cfg!(windows) {
        Ok(json!({"APPDATA": path_text(parent)?}))
    } else {
        Ok(json!({"XDG_DATA_HOME": path_text(parent)?}))
    }
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "OpenClaw requires UTF-8 native-host paths".into())
}

fn validate_command_text(command: &str) -> Result<(), String> {
    if !Path::new(command).is_absolute()
        || command.trim() != command
        || command.chars().any(|c| "\0\r\n;&|`$<>\"'".contains(c))
    {
        return Err("The OpenClaw helper path does not satisfy its executable safety rules".into());
    }
    Ok(())
}

pub(super) fn validate_helper(source: &Path, token_path: &Path) -> Result<(), String> {
    let command = helper_path(source, token_path)?;
    validate_command_text(path_text(&command)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(&command).map_err(|_| {
            "The staged OpenClaw helper is missing or unreadable; restart the PAP backend"
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("The OpenClaw helper must be a regular file, not a symlink".into());
        }
        // getuid has no preconditions and matches OpenClaw's process.getuid().
        let uid = unsafe { libc::getuid() };
        if metadata.uid() != uid || metadata.mode() & 0o022 != 0 || metadata.mode() & 0o100 == 0 {
            return Err("The OpenClaw helper must be owned and executable by the current user, without group/world write permission".into());
        }
    }
    #[cfg(windows)]
    windows::validate_helper(&command)?;
    if source != command
        && !same_content(source, &command)
            .map_err(|_| "Cannot verify the staged OpenClaw helper against this installation")?
    {
        return Err(
            "The staged OpenClaw helper differs from this installation; restart the PAP backend"
                .into(),
        );
    }
    Ok(())
}

fn same_content(source: &Path, staged: &Path) -> io::Result<bool> {
    use std::io::Read;
    if !fs::metadata(source)?.is_file() || !fs::metadata(staged)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Helper is not a regular file",
        ));
    }
    let mut source = fs::File::open(source)?;
    let mut staged = fs::File::open(staged)?;
    if source.metadata()?.len() != staged.metadata()?.len() {
        return Ok(false);
    }
    let mut remaining = source.metadata()?.len();
    let (mut left, mut right) = ([0u8; 65536], [0u8; 65536]);
    while remaining > 0 {
        let count = remaining.min(left.len() as u64) as usize;
        source.read_exact(&mut left[..count])?;
        staged.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    Ok(source.read(&mut left[..1])? == 0 && staged.read(&mut right[..1])? == 0)
}

pub(super) fn stale_helper(record: &Connection, source: &Path, token_path: &Path) -> bool {
    let Ok(command) = helper_path(source, token_path) else {
        return true;
    };
    let Ok(environment) = helper_env(token_path) else {
        return true;
    };
    let Some(field) = record
        .fields
        .iter()
        .find(|field| field.path == owned(SECRET_PATH))
    else {
        return true;
    };
    let Some(ConfigValue::Json(value)) = &field.value else {
        return true;
    };
    value.get("command").and_then(Value::as_str) != command.to_str()
        || value.get("env") != Some(&environment)
}

// OpenClaw pins @openclaw/fs-safe 0.8.1. Match its native basic-ACE rules:
// https://github.com/openclaw/fs-safe/blob/v0.8.1/native/src/windows_security.rs
// can_write (257-269), read_owner_and_dacl (354-440). Owner trust is not an
// exec-provider requirement. Unsupported ACEs cannot be treated as empty ACLs.
#[cfg(any(windows, test))]
#[cfg_attr(test, derive(Deserialize))]
#[cfg_attr(test, serde(rename_all = "camelCase", deny_unknown_fields))]
struct WindowsAcl {
    owner_sid: String,
    current_user_sid: String,
    local: bool,
    dacl_present: bool,
    aces: Vec<WindowsAce>,
}

#[cfg(any(windows, test))]
#[cfg_attr(test, derive(Deserialize))]
#[cfg_attr(test, serde(deny_unknown_fields))]
struct WindowsAce {
    sid: String,
    kind: u8,
    flags: u8,
    mask: u32,
}

#[cfg(any(windows, test))]
fn validate_windows_acl(acl: &WindowsAcl) -> Result<(), String> {
    let valid_sid = |sid: &str| {
        let parts: Vec<_> = sid.split('-').collect();
        parts.len() >= 4
            && parts[0] == "S"
            && parts[1] == "1"
            && parts[2..]
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    };
    if !acl.local
        || !acl.dacl_present
        || !valid_sid(&acl.owner_sid)
        || !valid_sid(&acl.current_user_sid)
    {
        return Err("Cannot verify a local DACL for the OpenClaw helper".into());
    }
    for ace in &acl.aces {
        if !valid_sid(&ace.sid) || !matches!(ace.kind, 0 | 1) {
            return Err("The OpenClaw helper has an unsupported ACL entry".into());
        }
        if ace.flags & 0x08 != 0 || ace.kind == 1 {
            continue;
        }
        let trusted = ace.sid == acl.current_user_sid
            || matches!(ace.sid.as_str(), "S-1-5-18" | "S-1-5-32-544");
        if !trusted && ace.mask & 0x500d0156 != 0 {
            return Err(
                "The OpenClaw helper ACL permits another user or group to modify it".into(),
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
#[path = "openclaw_windows.rs"]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog::from_remote(
            &json!({"data": [
                {"id":"vendor/model", "name":"Verified model", "context_length":32000,
                 "max_output_length":0, "input_modalities":["text","image","unknown"],
                 "pricing":{"prompt":"0.000002"}},
                {"id":"other/model"}
            ]}),
            1,
        )
        .unwrap()
    }

    fn token_path(root: &Path) -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            root.join("Library/Application Support")
        } else {
            root.into()
        };
        base.join(APP_IDENTIFIER).join("agent-tokens/openclaw")
    }

    fn projection(root: &Path, options: &ConnectOptions) -> Vec<Field> {
        fs::create_dir_all(data_dir(&token_path(root)).unwrap()).unwrap();
        fields(&Inputs {
            endpoint: "http://127.0.0.1:4180",
            helper_exe: &root.join(helper_binary_name()),
            token_path: &token_path(root),
            codex_catalog_path: &root.join("unused.json"),
            catalog: Some(&catalog()),
            options,
        })
        .unwrap()
    }

    #[test]
    fn catalog_and_secret_ref_use_the_official_contract() {
        let temp = tempfile::tempdir().unwrap();
        let fields = projection(temp.path(), &ConnectOptions::default());
        assert_eq!(fields.len(), 2);
        let Some(ConfigValue::Json(provider)) = &fields[0].value else {
            panic!("provider")
        };
        assert_eq!(provider["api"], "openai-completions");
        assert_eq!(provider["baseUrl"], "http://127.0.0.1:4180/v1");
        assert_eq!(
            provider["apiKey"],
            json!({"source":"exec","provider":PROVIDER,"id":"openclaw"})
        );
        assert_eq!(provider["models"][0]["contextWindow"], 32000);
        assert_eq!(provider["models"][0]["cost"], json!({"input":2.0}));
        assert!(provider["models"][0].get("maxTokens").is_none());
        assert_eq!(provider["models"][0]["input"], json!(["text", "image"]));
        assert_eq!(
            provider["models"][1],
            json!({"id":"other/model","name":"other/model"})
        );
        let Some(ConfigValue::Json(secret)) = &fields[1].value else {
            panic!("secret")
        };
        assert_eq!(secret["args"], json!(["--agent-token", "openclaw"]));
        assert_eq!(secret["jsonOnly"], false);
        assert!(secret.get("passEnv").is_none());
        assert_eq!(secret["env"].as_object().unwrap().len(), 1);
        assert!(secret["env"].get(HOME_OVERRIDE_ENV).is_none());
        let key = if cfg!(windows) {
            "APPDATA"
        } else if cfg!(target_os = "macos") {
            "HOME"
        } else {
            "XDG_DATA_HOME"
        };
        assert_eq!(
            secret["env"][key],
            json!(fs::canonicalize(temp.path()).unwrap().to_str().unwrap())
        );
        assert!(helper_env(&temp.path().join("wrong/agent-tokens/openclaw")).is_err());
        assert!(helper_env(&temp.path().join(APP_IDENTIFIER).join("agent-tokens/pi")).is_err());
    }

    #[test]
    fn conflicts_and_unsupported_sources_are_rejected_without_mutation() {
        for root in [
            json!({"models":{"providers":{PROVIDER:{"apiKey":"synthetic-existing"}}}}),
            json!({"secrets":{"providers":{PROVIDER:null}}}),
            json!({"models":{"providers":{format!(" {} ", PROVIDER.to_ascii_uppercase()):{}}}}),
            json!({"models":{"$include":"models.json"}}),
            json!({"gateway":{"mode":"remote"}}),
            json!({"env":{"KEY":{"source":"exec","provider":PROVIDER,"id":"another"}}}),
            json!({"env":{"KEY":{"source":"file","provider":PROVIDER,"id":"openclaw"}}}),
        ] {
            let doc = ConfigDoc::Json(root);
            let before = doc.render().unwrap();
            assert!(validate_config(&doc, None).is_err());
            assert_eq!(doc.render().unwrap(), before);
        }
        let doc = ConfigDoc::Json(json!({"agents":{"defaults":{"model":"external/model"}}}));
        assert!(validate_config(&doc, None).is_ok());
        assert!(validate_selection(&doc, &ConnectOptions::default()).is_ok());
        assert!(validate_selection(
            &doc,
            &ConnectOptions {
                default_model: Some("vendor/model".into())
            }
        )
        .is_err());
    }

    #[test]
    fn json5_journal_preserves_fallbacks_comments_and_external_edits() {
        let temp = tempfile::tempdir().unwrap();
        let mut doc = ConfigDoc::parse(Format::Json5, r#"{
            // Keep the user's routing policy.
            agents: {defaults: {model: {primary: 'external/original', fallbacks: ['external/fallback'],},},},
        }"#).unwrap();
        let options = ConnectOptions {
            default_model: Some("vendor/model".into()),
        };
        validate_config(&doc, None).unwrap();
        validate_selection(&doc, &options).unwrap();
        let projected = projection(temp.path(), &options);
        let edit = project(&mut doc, &projected, None, Agent::OpenClaw).unwrap();
        assert!(edit.pending_secrets.is_empty());
        let record = edit.record.unwrap();
        assert_eq!(record.fields.len(), 3);
        validate_config(&doc, Some(&record)).unwrap();
        assert_eq!(selected_model(&doc).as_deref(), Some("vendor/model"));
        assert_eq!(
            doc.get_value(&["agents", "defaults", "model", "fallbacks"]),
            Some(ConfigValue::List(vec!["external/fallback".into()]))
        );
        let mut normal = doc.clone();
        restore(
            &mut normal,
            &record,
            &crate::secrets::MemoryStore::default(),
        )
        .unwrap();
        assert_eq!(
            normal.get_str(PRIMARY_PATH).as_deref(),
            Some("external/original")
        );
        doc.set_value(PRIMARY_PATH, &ConfigValue::Str("external/new".into()))
            .unwrap();
        doc.set_value(
            &["agents", "defaults", "model", "fallbacks"],
            &ConfigValue::List(vec!["external/new-fallback".into()]),
        )
        .unwrap();
        restore(&mut doc, &record, &crate::secrets::MemoryStore::default()).unwrap();
        assert_eq!(doc.get_str(PRIMARY_PATH).as_deref(), Some("external/new"));
        assert_eq!(
            doc.get_value(&["agents", "defaults", "model", "fallbacks"]),
            Some(ConfigValue::List(vec!["external/new-fallback".into()]))
        );
        assert!(doc.get_value(PROVIDER_PATH).is_some());
        assert!(doc.get_value(SECRET_PATH).is_some());
        assert!(doc
            .render()
            .unwrap()
            .contains("// Keep the user's routing policy."));

        let mut drifted = ConfigDoc::Json(json!({}));
        let fields = projection(temp.path(), &ConnectOptions::default());
        let record = project(&mut drifted, &fields, None, Agent::OpenClaw)
            .unwrap()
            .record
            .unwrap();
        drifted
            .set_value(
                &["models", "providers", PROVIDER, "baseUrl"],
                &ConfigValue::Str("http://external.invalid/v1".into()),
            )
            .unwrap();
        assert!(validate_config(&drifted, Some(&record)).is_err());
        restore(
            &mut drifted,
            &record,
            &crate::secrets::MemoryStore::default(),
        )
        .unwrap();
        assert_eq!(
            drifted
                .get_str(&["models", "providers", PROVIDER, "baseUrl"])
                .as_deref(),
            Some("http://external.invalid/v1")
        );
    }

    #[test]
    fn windows_acl_matches_native_basic_ace_rules() {
        let validate_windows_acl = |bytes: &[u8]| {
            let acl = serde_json::from_slice(bytes).map_err(|_| "Invalid fixture".to_string())?;
            super::validate_windows_acl(&acl)
        };
        let current = "S-1-5-21-100-200-300-1001";
        let acl = |aces: Value| {
            serde_json::to_vec(&json!({
                "ownerSid":"S-1-5-21-100-200-300-9999", "currentUserSid":current,
                "local":true, "daclPresent":true, "aces":aces
            }))
            .unwrap()
        };
        for sid in [current, "S-1-5-18", "S-1-5-32-544"] {
            assert!(validate_windows_acl(&acl(
                json!([{"sid":sid,"kind":0,"flags":0,"mask":0x1f01ff}])
            ))
            .is_ok());
        }
        for mask in [
            2, 4, 0x10, 0x40, 0x100, 0x10000, 0x40000, 0x80000, 0x10000000, 0x40000000,
        ] {
            let ace = json!({"sid":"S-1-1-0","kind":0,"flags":0,"mask":mask});
            assert!(validate_windows_acl(&acl(json!([ace]))).is_err());
        }
        for (kind, flags) in [(1, 0), (0, 8)] {
            assert!(validate_windows_acl(&acl(
                json!([{"sid":"S-1-1-0","kind":kind,"flags":flags,"mask":2}])
            ))
            .is_ok());
        }
        assert!(validate_windows_acl(&acl(
            json!([{"sid":"S-1-1-0","kind":0,"flags":0,"mask":0x120089}])
        ))
        .is_ok());
        for kind in [5, 6, 9, 10, 17, 255] {
            for flags in [0, 8] {
                assert!(validate_windows_acl(&acl(
                    json!([{"sid":current,"kind":kind,"flags":flags,"mask":0}])
                ))
                .is_err());
            }
        }
        assert!(validate_windows_acl(&acl(json!([{
            "sid":"S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
            "kind":0,"flags":0,"mask":2
        }])))
        .is_err()); // TrustedInstaller is not an exception.
        let mut missing: Value = serde_json::from_slice(&acl(json!([]))).unwrap();
        missing["daclPresent"] = json!(false);
        assert!(validate_windows_acl(&serde_json::to_vec(&missing).unwrap()).is_err());
        assert!(validate_windows_acl(b"{}").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn staged_helper_must_be_secure_and_match_the_current_installation() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("bundled");
        let token = token_path(temp.path());
        let staged = helper_path(&source, &token).unwrap();
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        let bytes = vec![42; 131073];
        fs::write(&source, &bytes).unwrap();
        fs::write(&staged, &bytes).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_helper(&source, &token).is_ok());
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(validate_helper(&source, &token).is_err());
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o700)).unwrap();
        let mut changed = bytes.clone();
        changed[131072] = 0;
        fs::write(&source, &changed).unwrap();
        assert!(validate_helper(&source, &token).is_err());
        fs::write(&source, &bytes[..131072]).unwrap();
        assert!(validate_helper(&source, &token).is_err());
        fs::remove_file(&staged).unwrap();
        symlink(&source, &staged).unwrap();
        assert!(validate_helper(&source, &token).is_err());
        assert!(validate_command_text("/home/user/unsafe;helper").is_err());
        assert!(validate_command_text("relative/helper").is_err());
        assert!(validate_command_text("/home/user/App Name/helper").is_ok());
    }
}
