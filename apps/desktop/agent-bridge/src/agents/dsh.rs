//! DeepSeek Harness (dsh) 0.1.7-rc.2, tag `dsh-v0.1.7-rc.2` (477b4f42) of
//! deepseek-ai/deepseek-harness; 0.2.0-rc.1 keeps the same contract.
//! app-boot's `readProfilePatches` composes the bundle layers, then
//! `profiles/<name>/cordis.patch.yml`, then `$DSH_HOME/cordis.patch.yml`, then
//! `--patch` overlays, and an `{id, config}` patch replaces the row's whole
//! config. Every profile, the desktop app's included, reads that home layer and
//! dsh never writes it, so the connection lives there as keyed list items.
//! llm-pi-ai resolves `apiKeyEnv` per request through credentials-local: the
//! launch environment, then `.credentials.yaml` refs (owner-only, watched, and
//! never exported to tool processes), then the `.env` files.

use std::{
    io::Write,
    thread,
    time::{Duration, Instant},
};

use super::*;
use serde_json::{json, Value};

const PROVIDER: &str = "private-ai-proxy";
/// The credential reference the provider's `apiKeyEnv` names.
pub(super) const TOKEN_REF: &str = "PRIVATE_AI_PROXY_DSH_TOKEN";
const PI_AI: &str = "llm-pi-ai";
const DEFAULT_MODEL: &str = "agent-default-model";
/// DeepSeek's own search API, reached with the user's key outside the proxy.
const WEB_SEARCH: &str = "web-search-deepseek";
const CREDENTIALS: &str = "credentials";
/// Rows another layer could disable, shadow or move to reroute the connection.
const GUARDED: [&str; 4] = [PI_AI, DEFAULT_MODEL, WEB_SEARCH, CREDENTIALS];
pub(super) const CREDENTIALS_FILE: &str = ".credentials.yaml";

fn key(id: &str) -> EntryKey {
    EntryKey {
        key: "id".to_string(),
        id: id.to_string(),
    }
}

/// `$DSH_HOME` as `resolveDshHome` reads it: blank is unset, `~` is the home.
pub(super) fn home_dir(home: &Path, tool_env: bool) -> PathBuf {
    let configured = tool_env
        .then(|| env::var("DSH_HOME").ok())
        .flatten()
        .filter(|path| !path.trim().is_empty());
    match configured.as_deref() {
        None => home.join(".dsh"),
        Some("~") => home.to_path_buf(),
        Some(path) => path
            .strip_prefix("~/")
            .or_else(|| path.strip_prefix("~\\"))
            .map_or_else(|| PathBuf::from(path), |rest| home.join(rest)),
    }
}

pub(super) fn config_path(home: &Path, tool_env: bool) -> PathBuf {
    home_dir(home, tool_env).join("cordis.patch.yml")
}

pub(super) fn validate_host(home: &Path, tool_env: bool) -> Result<(), String> {
    let dsh_home = home_dir(home, tool_env);
    if !dsh_home.is_absolute() {
        return Err("Set DSH_HOME to an absolute directory; dsh resolves a relative one against the directory it starts in".into());
    }
    if tool_env && env::var_os(TOKEN_REF).is_some() {
        return Err(format!(
            "{TOKEN_REF} is set in the environment and would override this connection's credential in dsh; unset it"
        ));
    }
    let legacy = dsh_home.join("settings.yaml");
    if legacy.exists() {
        return Err(format!(
            "dsh has not imported its legacy settings at {} yet; start dsh once so it imports them, then connect",
            legacy.display()
        ));
    }
    Ok(())
}

pub(super) fn fields(inputs: &Inputs<'_>) -> Result<Vec<Field>, AgentError> {
    let catalog = inputs.catalog.ok_or(AgentError::InvalidState)?;
    let model = inputs
        .options
        .default_model
        .as_deref()
        .ok_or(AgentError::IncompatibleModel)?;
    let models: Vec<Value> = catalog
        .models
        .iter()
        .map(|model| {
            let mut row = json!({"id": model.id(), "name": model.display_name()});
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
            row
        })
        .collect();
    let provider = json!({
        "displayName": PRODUCT_NAME,
        "api": "openai-completions",
        "baseURL": format!("{}/v1", inputs.endpoint.trim_end_matches('/')),
        "apiKeyEnv": TOKEN_REF,
        // The proxy serves many upstreams; send the request shape all accept.
        "compat": {"supportsDeveloperRole": false, "maxTokensField": "max_tokens"},
        "models": models,
    });
    Ok(vec![
        generated_catalog(
            &["config", "providers", PROVIDER],
            provider,
            catalog.models.len(),
        )
        .in_entry(PI_AI),
        set(&["config", "provider"], PROVIDER).in_entry(DEFAULT_MODEL),
        set(&["config", "model"], model).in_entry(DEFAULT_MODEL),
        // An effort chosen for another provider does not apply to these models.
        absent(&["config", "reasoningEffort"]).in_entry(DEFAULT_MODEL),
        boolean(&["disabled"], true).in_entry(WEB_SEARCH),
    ])
}

pub(super) fn selected_model(doc: &ConfigDoc) -> Option<String> {
    let item = doc.entry(&key(DEFAULT_MODEL)).ok().flatten()?;
    (item.get_str(&["config", "provider"]).as_deref() == Some(PROVIDER))
        .then(|| item.get_str(&["config", "model"]))
        .flatten()
}

/// The preview line for the token the connection stores for dsh.
pub(super) fn credential_change() -> ConfigChange {
    ConfigChange {
        key: format!("{CREDENTIALS_FILE} refs.{TOKEN_REF}"),
        before: None,
        after: Some("Managed local credential".to_string()),
        sensitive: true,
    }
}

/// Refuse every configuration that could route dsh around the connection, or
/// hide the user's own providers, while it shows Connected.
pub(super) fn validate_config(
    dsh_home: &Path,
    doc: &ConfigDoc,
    prior: Option<&Connection>,
) -> Result<(), String> {
    let home_layer = dsh_home.join("cordis.patch.yml");
    check_layer(&home_layer, doc)?;
    let config_is_users = doc
        .entry(&key(PI_AI))?
        .is_some_and(|item| item.contains(&["config"]))
        && !prior.is_some_and(|record| {
            record.fields.iter().any(|field| {
                field.entry.as_ref().is_some_and(|entry| {
                    entry.key.id == PI_AI && (entry.created || field.path == ["config"])
                })
            })
        });
    let profiles = dsh_home.join("profiles");
    let mut layers = match fs::read_dir(&profiles) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path().join("cordis.patch.yml")))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| format!("Cannot read dsh profiles in {}", profiles.display()))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            return Err(format!(
                "Cannot read dsh profiles in {}",
                profiles.display()
            ))
        }
    };
    layers.sort();
    for path in layers {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(format!(
                    "Cannot read the dsh profile layer {}",
                    path.display()
                ))
            }
        };
        let layer = ConfigDoc::parse(Format::YamlList, &text).map_err(|reason| {
            format!(
                "Cannot verify the dsh profile layer {}: {reason}",
                path.display()
            )
        })?;
        check_layer(&path, &layer)?;
        if !config_is_users
            && layer
                .entry(&key(PI_AI))?
                .is_some_and(|item| item.contains(&["config"]))
        {
            return Err(format!(
                "{} configures its own dsh model providers, which connecting would hide from every profile. Move that `id: {PI_AI}` item to {}, then connect again",
                path.display(),
                home_layer.display()
            ));
        }
    }
    let expected = prior
        .and_then(|record| record.selection.as_ref())
        .and_then(selection::Journal::token);
    check_credentials(&dsh_home.join(CREDENTIALS_FILE), expected.as_deref())
}

fn check_layer(path: &Path, layer: &ConfigDoc) -> Result<(), String> {
    let at = |reason: String| format!("{}: {reason}", path.display());
    for id in GUARDED {
        layer.entry(&key(id)).map_err(at)?;
    }
    for item in layer.items().map_err(at)? {
        if !item.contains(&["insert"]) {
            continue;
        }
        let Some(ConfigValue::Json(rows)) = item.get_value(&["insert"]) else {
            return Err(at("an `insert` list cannot be read safely".to_string()));
        };
        if let Some(id) = inserted_id(&rows) {
            return Err(at(format!(
                "it inserts another `{id}` row, which would take the connection's place; remove that insert"
            )));
        }
    }
    for id in [PI_AI, DEFAULT_MODEL, CREDENTIALS] {
        let Some(item) = layer.entry(&key(id)).map_err(at)? else {
            continue;
        };
        if item.contains(&["disabled"])
            && item.get_value(&["disabled"]) != Some(ConfigValue::Bool(false))
        {
            return Err(at(format!(
                "it disables the `{id}` row; remove `disabled` there"
            )));
        }
        if id == CREDENTIALS && item.contains(&["config"]) {
            return Err(at(
                "it moves dsh's credential store (`credentials` config); remove that config"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// A guarded row id anywhere in inserted rows, including group children.
fn inserted_id(value: &Value) -> Option<&str> {
    match value {
        Value::Array(rows) => rows.iter().find_map(inserted_id),
        Value::Object(row) => row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| GUARDED.contains(id))
            .or_else(|| row.get("config").and_then(inserted_id)),
        _ => None,
    }
}

/// dsh refuses a store that is not version 1, readable beyond its owner, or
/// holding unknown sections; the token name must be absent or ours.
fn check_credentials(path: &Path, expected: Option<&str>) -> Result<(), String> {
    let at = |reason: &str| format!("dsh's credential store {} {reason}", path.display());
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(at("cannot be read")),
    };
    if !metadata.is_file() {
        return Err(at("must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::getuid() } || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(at("must be private to you; run chmod 600 on it"));
        }
    }
    let text = fs::read_to_string(path).map_err(|_| at("cannot be read"))?;
    if text.trim().is_empty() {
        return Err(at("is empty; delete it, or save a key in dsh first"));
    }
    if !text.ends_with('\n') {
        return Err(at("does not end with a newline; add one"));
    }
    let root = match ConfigDoc::parse(Format::Yaml, &text).map(|doc| doc.get_value(&[])) {
        Ok(Some(ConfigValue::Json(Value::Object(root)))) => root,
        _ => return Err(at("cannot be read safely")),
    };
    if root.get("version") != Some(&json!(1))
        || root
            .keys()
            .any(|key| !matches!(key.as_str(), "version" | "refs" | "records"))
    {
        return Err(at(
            "is not a version 1 store; start dsh once so it upgrades or reports it",
        ));
    }
    match root.get("refs").and_then(|refs| refs.get(TOKEN_REF)) {
        None => Ok(()),
        Some(value) if expected.is_some() && value.as_str() == expected => Ok(()),
        Some(_) => Err(at(&format!(
            "already holds {TOKEN_REF}; remove it there before connecting"
        ))),
    }
}

/// Whether dsh's credential store holds `token` for the connection.
pub(super) fn credential_holds(dsh_home: &Path, token: &str) -> bool {
    fs::read_to_string(dsh_home.join(CREDENTIALS_FILE))
        .ok()
        .and_then(|text| ConfigDoc::parse(Format::Yaml, &text).ok())
        .and_then(|doc| doc.get_str(&["refs", TOKEN_REF]))
        .is_some_and(|stored| stored == token)
}

/// Run `write` under dsh's writer lock on `file` (`withFileLock` in
/// @deepseek-ai/dsh-atomic-write): `<file>.lock`, created exclusively with the
/// writer's PID and removed afterwards. Waits as dsh does, backing off from
/// 20 ms to 200 ms, but never takes over another writer's lock.
pub(super) fn with_lock<T>(file: &Path, write: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let mut lock_name = file.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock = PathBuf::from(lock_name);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut delay = Duration::from_millis(20);
    let mut handle = loop {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&lock) {
            Ok(handle) => break handle,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("dsh holds {}; try again", lock.display()),
                    ));
                }
                thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(200));
            }
            Err(error) => return Err(error),
        }
    };
    let result = writeln!(handle, "{}", std::process::id()).and_then(|()| write());
    drop(handle);
    let released = fs::remove_file(&lock);
    let value = result?;
    released?;
    Ok(value)
}

#[cfg(test)]
mod tests;
