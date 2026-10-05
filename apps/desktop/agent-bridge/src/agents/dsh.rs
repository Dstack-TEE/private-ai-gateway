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
/// The acp profile's row, which selects its own model instead of the default:
/// the acp-app bundle ships it as `{provider: deepseek-official, model:
/// deepseek-v4-flash}`, so an item replacing that config names only those two.
const ACP: &str = "acp";
/// DeepSeek's own search API, reached with the user's key outside the proxy.
const WEB_SEARCH: &str = "web-search-deepseek";
const WEB_SEARCH_PACKAGE: &str = "@deepseek-ai/dsh-web-search-deepseek";
/// Packages that act under any row id: the search provider registers
/// `deepseek-official`, and the ACP server serves with its own model choice.
const GUARDED_PACKAGES: [&str; 2] = [WEB_SEARCH_PACKAGE, "@deepseek-ai/dsh-acp"];
const CREDENTIALS: &str = "credentials";
/// Rows another layer could disable, shadow or move to reroute the connection.
const GUARDED: [&str; 5] = [PI_AI, DEFAULT_MODEL, ACP, WEB_SEARCH, CREDENTIALS];
pub(super) const CREDENTIALS_FILE: &str = ".credentials.yaml";

fn key(id: &str) -> EntryKey {
    EntryKey::id(id)
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
    let models = model_rows(
        catalog,
        &ModelRows {
            positive_limits: true,
            any_input: false,
            reasoning: false,
            cost: Cost::Omitted,
        },
    );
    let provider = json!({
        "displayName": PRODUCT_NAME,
        "api": "openai-completions",
        "baseURL": api_url(inputs.endpoint).map_err(|_| AgentError::InvalidState)?,
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
        set(&["config", "provider"], PROVIDER).in_entry(ACP),
        set(&["config", "model"], model).in_entry(ACP),
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
    secret_change(
        format!("{CREDENTIALS_FILE} refs.{TOKEN_REF}"),
        None,
        Some(MANAGED_CREDENTIAL),
    )
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
        // Only directories are profiles; files such as .DS_Store are not.
        Ok(entries) => entries
            .filter_map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map(|path| path.is_dir().then(|| path.join("cordis.patch.yml")))
                    .transpose()
            })
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
    let journal = prior.and_then(|record| record.selection.as_ref());
    check_credentials(&dsh_home.join(CREDENTIALS_FILE), journal)
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
    for id in [PI_AI, DEFAULT_MODEL, ACP, CREDENTIALS] {
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

/// A guarded row anywhere in inserted rows, including group children: one
/// with a guarded id, or a row of a guarded package under any other id.
fn inserted_id(value: &Value) -> Option<&str> {
    match value {
        Value::Array(rows) => rows.iter().find_map(inserted_id),
        Value::Object(row) => row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| GUARDED.contains(id))
            .or_else(|| {
                row.get("name")
                    .and_then(Value::as_str)
                    .filter(|name| GUARDED_PACKAGES.contains(name))
            })
            .or_else(|| row.get("config").and_then(inserted_id)),
        _ => None,
    }
}

/// dsh refuses a store that is not version 1, readable beyond its owner, or
/// holding unknown sections; the token name must be absent or ours.
fn check_credentials(path: &Path, journal: Option<&selection::Journal>) -> Result<(), String> {
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
        Some(Value::String(value)) if journal.is_some_and(|journal| journal.wrote_token(value)) => {
            Ok(())
        }
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
/// 20 ms to 200 ms, and takes over only a lock whose holder has exited, by
/// dsh's own rule ([`take_over_exited_lock`]).
pub(super) fn with_lock<T>(file: &Path, write: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let lock = sibling(file, ".lock");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut delay = Duration::from_millis(20);
    let mut handle = loop {
        match create_exclusive(&lock) {
            Ok(handle) => break handle,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if take_over_exited_lock(&lock)? {
                    continue;
                }
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

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn create_exclusive(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// dsh's `takeOverExitedLock`: remove the lock only when its `<pid>\n`
/// record names a process that has exited. Contenders that read the same
/// record serialize on `<lock>.takeover-<sha256(record)[..16]>`, and under it
/// re-read the record and probe the PID again before removing the lock.
fn take_over_exited_lock(lock: &Path) -> io::Result<bool> {
    let Some(record) = fs::read_to_string(lock)
        .ok()
        .filter(|record| holder_exited(record))
    else {
        return Ok(false);
    };
    let digest = hex::encode(Sha256::digest(&record));
    let claim = sibling(lock, &format!(".takeover-{}", &digest[..16]));
    match create_exclusive(&claim) {
        Ok(mut handle) => {
            if let Err(error) = writeln!(handle, "{}", std::process::id()) {
                let _ = fs::remove_file(&claim);
                return Err(error);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    }
    let still =
        fs::read_to_string(lock).ok().as_deref() == Some(record.as_str()) && holder_exited(&record);
    let removed = still && fs::remove_file(lock).is_ok();
    let _ = fs::remove_file(&claim);
    Ok(removed)
}

/// dsh's `holderExited`: a complete `<pid>\n` record whose process a signal
/// probe cannot find. Anything else, our own PID, or a process that exists
/// under another user proves nothing.
fn holder_exited(record: &str) -> bool {
    let Some(pid) = record
        .strip_suffix('\n')
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u32>().ok())
    else {
        return false;
    };
    if pid == 0 || pid > 0x7fff_ffff || pid == std::process::id() {
        return false;
    }
    #[cfg(unix)]
    {
        let probe = unsafe { libc::kill(pid as libc::pid_t, 0) };
        probe != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    // Node's `process.kill(pid, 0)` on Windows is libuv's `uv_kill`: ESRCH when
    // OpenProcess reports ERROR_INVALID_PARAMETER, or the process has an exit
    // code or a signaled handle.
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{
                CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, STILL_ACTIVE, WAIT_OBJECT_0,
            },
            System::Threading::{
                GetExitCodeProcess, OpenProcess, WaitForSingleObject, PROCESS_QUERY_INFORMATION,
                PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
            },
        };
        let access = PROCESS_TERMINATE | PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE;
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return unsafe { GetLastError() } == ERROR_INVALID_PARAMETER;
        }
        let mut status = 0;
        let exited = unsafe { GetExitCodeProcess(handle, &mut status) } != 0
            && (status != STILL_ACTIVE as u32
                || unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0);
        unsafe { CloseHandle(handle) };
        exited
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Without a process probe, no holder is proven gone.
        false
    }
}

#[cfg(test)]
mod tests;
