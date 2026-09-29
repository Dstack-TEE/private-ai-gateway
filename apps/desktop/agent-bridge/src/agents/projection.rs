use super::*;

#[derive(Clone)]
pub(super) struct Field {
    pub(super) path: Vec<String>,
    /// `None` makes the key absent.
    pub(super) value: Option<ConfigValue>,
    /// Concise preview text for generated structured values.
    pub(super) preview: Option<String>,
    /// The keyed list item `path` is relative to.
    pub(super) entry: Option<EntryKey>,
    /// See [`OwnedField::exact`].
    pub(super) exact: bool,
    /// See [`OwnedField::container`].
    pub(super) container: bool,
}

impl Field {
    /// Scope the field to the list item whose `id` is `id`. Fields in list
    /// items are exact.
    pub(super) fn in_entry(self, id: &str) -> Self {
        Field {
            entry: Some(EntryKey {
                key: "id".to_string(),
                id: id.to_string(),
            }),
            exact: true,
            ..self
        }
    }

    pub(super) fn exact(self) -> Self {
        Field {
            exact: true,
            ..self
        }
    }
}

fn field(path: &[&str], value: Option<ConfigValue>, preview: Option<String>) -> Field {
    Field {
        path: owned(path),
        value,
        preview,
        entry: None,
        exact: false,
        container: false,
    }
}

pub(super) fn set(path: &[&str], value: impl Into<String>) -> Field {
    field(path, Some(ConfigValue::Str(value.into())), None)
}

pub(super) fn number(path: &[&str], value: u64) -> Field {
    field(path, Some(ConfigValue::Number(value)), None)
}

pub(super) fn boolean(path: &[&str], value: bool) -> Field {
    field(path, Some(ConfigValue::Bool(value)), None)
}

pub(super) fn generated_catalog(path: &[&str], value: serde_json::Value, models: usize) -> Field {
    field(
        path,
        Some(ConfigValue::Json(value)),
        Some(format!("Generated catalog ({models} models)")),
    )
}

pub(super) fn list(path: &[&str], values: &[&str]) -> Field {
    field(
        path,
        Some(ConfigValue::List(
            values.iter().map(|value| (*value).to_string()).collect(),
        )),
        None,
    )
}

pub(super) fn absent(path: &[&str]) -> Field {
    field(path, None, None)
}

pub(super) fn owned(path: &[&str]) -> Vec<String> {
    path.iter().map(|key| key.to_string()).collect()
}

/// POSIX command syntax. The quoting test validates `sh`, not the Windows
/// Claude CLI's choice of shell; that compatibility remains unverified.
pub(super) fn helper_command(exe: &Path, agent: &str) -> Result<String, String> {
    let path = exe
        .to_str()
        .ok_or_else(|| "The app path is not valid Unicode".to_string())?;
    let quoted = shlex::try_quote(path)
        .map_err(|_| "The app path cannot be quoted for the shell".to_string())?;
    Ok(format!("{quoted} --agent-token {agent}"))
}

#[cfg(not(windows))]
pub(super) fn credential_helper_command(exe: &Path, agent: Agent) -> Result<String, String> {
    helper_command(exe, agent.id())
}

#[cfg(windows)]
pub(super) fn credential_helper_command(exe: &Path, agent: Agent) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let path = exe
        .to_str()
        .ok_or_else(|| "The app path is not valid Unicode".to_string())?;
    if path.contains('\0') {
        return Err("The app path contains a null character".to_string());
    }
    // Hermes uses cmd.exe; Pi tries Bash then falls back to cmd.exe. Only the
    // fixed switches and base64 reach either shell; the helper path stays data.
    let encoded_path = STANDARD.encode(
        path.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let command = format!(
        "$ErrorActionPreference = 'Stop'; & ([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{encoded_path}'))) --agent-token {}; exit $LASTEXITCODE",
        agent.id()
    );
    let bytes: Vec<u8> = command.encode_utf16().flat_map(u16::to_le_bytes).collect();
    Ok(format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        STANDARD.encode(bytes)
    ))
}

pub(super) fn agent_credential_command(
    exe: &Path,
    agent: Agent,
    token_path: Option<&Path>,
) -> Result<String, String> {
    if let Some(path) = token_path {
        let path = path
            .to_str()
            .filter(|path| Path::new(path).is_absolute())
            .ok_or("The Agent token path must be absolute Unicode")?;
        let quoted = shlex::try_quote(path).map_err(|_| "The Agent token path cannot be quoted")?;
        return Ok(format!("/bin/cat {quoted}"));
    }
    if agent == Agent::ClaudeCode {
        helper_command(exe, agent.id())
    } else {
        credential_helper_command(exe, agent)
    }
}

pub(super) fn stale_helper(
    agent: Agent,
    record: &Connection,
    exe: &Path,
    token_path: Option<&Path>,
) -> bool {
    if agent == Agent::Codex {
        let expected_args = if let Some(path) = token_path {
            vec![path.display().to_string()]
        } else {
            vec!["--agent-token".into(), "codex".into()]
        };
        if record.fields.iter().any(|field| {
            field.path == owned(&["model_providers", "private_ai_proxy", "auth", "args"])
                && field.value != Some(ConfigValue::List(expected_args.clone()))
        }) {
            return true;
        }
    }
    let (path, expected) = match agent {
        Agent::Codex => (
            &["model_providers", "private_ai_proxy", "auth", "command"][..],
            if token_path.is_some() {
                Some("/bin/cat".into())
            } else {
                exe.to_str().map(str::to_string)
            },
        ),
        Agent::ClaudeCode => (
            &["apiKeyHelper"][..],
            agent_credential_command(exe, agent, token_path).ok(),
        ),
        Agent::Pi => (
            &["providers", "private-ai-proxy"][..],
            agent_credential_command(exe, agent, token_path)
                .ok()
                .map(|command| format!("!{command}")),
        ),
        Agent::Hermes => (
            &["providers", "private-ai-proxy", "key_cmd"][..],
            agent_credential_command(exe, agent, token_path).ok(),
        ),
        Agent::OpenCode => return false,
        Agent::OpenClaw => return false, // Validated with the token path by Projector.
        Agent::OhMyPi => return oh_my_pi::stale_helper(record, exe, token_path),
        // The token itself is stored; status compares it with the token file.
        Agent::Dsh => return false,
    };
    // Only inspect the helper-bearing field we recorded, not provider metadata.
    record.fields.iter().any(|field| {
        if field.path != owned(path) {
            return false;
        }
        let command = match &field.value {
            Some(ConfigValue::Str(command)) if agent != Agent::Pi => Some(command.as_str()),
            Some(ConfigValue::Json(provider)) if agent == Agent::Pi => {
                provider.get("apiKey").and_then(serde_json::Value::as_str)
            }
            _ => None,
        };
        expected
            .as_deref()
            .is_none_or(|expected| command != Some(expected))
    })
}

/// Credential-bearing keys: their values never reach previews, manifests,
/// or logs.
pub(super) fn is_sensitive(path: &[String]) -> bool {
    let last = path
        .last()
        .map(|key| key.to_ascii_lowercase())
        .unwrap_or_default();
    [
        "apikey",
        "api_key",
        "token",
        "secret",
        "password",
        "bearer",
        "authorization",
    ]
    .iter()
    .any(|needle| last.contains(needle))
}

// Definitions are passive after the local token is revoked. Global selectors
// and credentials are always restored; pre-existing provider fields are too.
pub(super) fn provider_namespace(path: &[String]) -> Option<&[String]> {
    match path {
        [root, name, ..]
            if (root == "model_providers" && name == "private_ai_proxy")
                || ((root == "provider" || root == "providers") && name == "private-ai-proxy") =>
        {
            Some(&path[..2])
        }
        [root, providers, name, ..]
            if (root == "models" || root == "secrets")
                && providers == "providers"
                && name == "private-ai-proxy" =>
        {
            Some(&path[..3])
        }
        _ => None,
    }
}

pub(super) fn retain_provider_field(field: &OwnedField, fields: &[OwnedField]) -> bool {
    if field.exact {
        return false;
    }
    if field.path == owned(&["model_providers", "private_ai_proxy", "name"]) {
        return !matches!(
            &field.previous,
            Some(Previous::Plain(ConfigValue::Str(name))) if !name.trim().is_empty()
        );
    }
    provider_namespace(&field.path).is_some_and(|prefix| {
        !fields
            .iter()
            .any(|other| other.path.starts_with(prefix) && other.previous.is_some())
    })
}

pub(super) fn inactive_provider_value(field: &OwnedField) -> Option<ConfigValue> {
    match &field.value {
        Some(ConfigValue::Json(value)) => {
            let mut value = value.clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("apiKey");
                if let Some(options) = object
                    .get_mut("options")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    options.remove("apiKey");
                }
            }
            Some(ConfigValue::Json(value))
        }
        _ if field.path.last().is_some_and(|key| key == "key_cmd") => {
            Some(ConfigValue::Str(String::new()))
        }
        _ if field
            .path
            .last()
            .is_some_and(|key| key == "discover_models") =>
        {
            Some(ConfigValue::Bool(false))
        }
        _ => field.value.clone(),
    }
}

/// Write the owned fields, remembering what each held before. A field that
/// still holds what an earlier connection wrote keeps that connection's
/// `previous`, so reconnecting never records our own value as the original.
/// Previous values of sensitive fields are returned as pending secrets and
/// referenced by entry name; they never appear in changes or the record.
/// Exact fields, including every field in a keyed list item, only add keys or
/// replace scalars so that [`restore`] puts back every byte.
pub(super) fn project(
    doc: &mut ConfigDoc,
    fields: &[Field],
    prior: Option<&Connection>,
    agent: Agent,
) -> Result<Edit, String> {
    let prior = prior.map_or(&[][..], |prior| prior.fields.as_slice());
    let mut edit = Edit {
        record: Some(Connection::default()),
        ..Edit::default()
    };
    let mut entries: Vec<&EntryKey> = Vec::new();
    for key in fields.iter().filter_map(|field| field.entry.as_ref()) {
        if !entries.contains(&key) {
            entries.push(key);
        }
    }
    let lists_ours = prior.iter().any(|field| {
        field
            .entry
            .as_ref()
            .is_some_and(|entry| entry.created && doc.entry(&entry.key).ok().flatten().is_some())
    });
    if !entries.is_empty() && !lists_ours {
        doc.list_editable()
            .map_err(|reason| format!("the list cannot be edited safely: {reason}"))?;
    }
    for key in entries {
        project_entry(doc, key, fields, prior, &mut edit)?;
    }
    let exact: Vec<&Field> = fields
        .iter()
        .filter(|field| field.exact && field.entry.is_none())
        .collect();
    let prior_exact: Vec<&OwnedField> = prior
        .iter()
        .filter(|field| field.exact && field.entry.is_none())
        .collect();
    for field in anchor(Some(doc), &exact, &prior_exact)? {
        project_exact(doc, field, &prior_exact, None, &mut edit)?;
    }
    for field in fields.iter().filter(|field| !field.exact) {
        let path = refs(&field.path);
        let sensitive = is_sensitive(&field.path);
        let current = doc.get_value(&path);
        let still_ours = prior
            .iter()
            .find(|owned| owned.path == field.path && owned.holds(current.as_ref()));
        // App-owned structured providers must never absorb an unmanaged object:
        // it may contain nested credentials that a field-name check cannot see.
        if matches!(field.value, Some(ConfigValue::Json(_)))
            && doc.contains(&path)
            && still_ours.is_none()
        {
            return Err(format!("The {} provider already exists outside this connection. Leave it unchanged and resolve the ownership conflict before connecting", agent.name()));
        }
        let previous = match still_ours {
            Some(owned) => owned.previous.clone(),
            None => match current
                .clone()
                .filter(|held| Some(held) != field.value.as_ref())
            {
                Some(held) if sensitive => {
                    let entry = secret_entry(agent, &field.path);
                    edit.pending_secrets.push(PendingSecret {
                        entry: entry.clone(),
                        value: held.display(),
                    });
                    Some(Previous::Secret { secret_ref: entry })
                }
                Some(held) => Some(Previous::Plain(held)),
                None => None,
            },
        };
        if current != field.value {
            match &field.value {
                Some(value) => doc.set_value(&path, value)?,
                None => doc.remove(&path)?,
            }
            edit.changes.push(if sensitive {
                ConfigChange {
                    key: path.join("."),
                    before: current.as_ref().map(|_| "Existing secret".to_string()),
                    after: field
                        .value
                        .as_ref()
                        .map(|_| "Managed local credential".to_string()),
                    sensitive: true,
                }
            } else {
                preview_change(
                    &path,
                    current,
                    field.value.clone(),
                    field.preview.as_deref(),
                )
            });
        }
        owned_fields(&mut edit).push(OwnedField {
            path: field.path.clone(),
            value: field.value.clone(),
            previous,
            entry: None,
            exact: false,
            source: None,
            hashed: false,
            container: false,
        });
    }
    Ok(edit)
}

fn owned_fields(edit: &mut Edit) -> &mut Vec<OwnedField> {
    &mut edit.record.get_or_insert_with(Connection::default).fields
}

/// Project the fields of one keyed list item. An item the connection appends
/// holds nothing else and is replaced whole; in the user's own item the
/// fields only add keys or replace scalars.
fn project_entry(
    doc: &mut ConfigDoc,
    key: &EntryKey,
    fields: &[Field],
    prior: &[OwnedField],
    edit: &mut Edit,
) -> Result<(), String> {
    let context = |reason: String| format!("its `{}: {}` item {reason}", key.key, key.id);
    let group: Vec<&Field> = fields
        .iter()
        .filter(|field| field.entry.as_ref() == Some(key))
        .collect();
    let prior: Vec<&OwnedField> = prior
        .iter()
        .filter(|field| field.entry.as_ref().is_some_and(|entry| entry.key == *key))
        .collect();
    let item = doc.entry(key)?;
    let appended = item
        .as_ref()
        .is_none_or(|item| !prior.is_empty() && appended_intact(item, key, &prior));
    if let (Some(mut item), false) = (item.clone(), appended) {
        if item.is_flow(&[]) {
            return Err(context(
                "is written in flow style ({...}); write it as a block mapping".to_string(),
            ));
        }
        let prior: Vec<&OwnedField> = prior
            .into_iter()
            .filter(|field| field.entry.as_ref().is_some_and(|entry| !entry.created))
            .collect();
        let entry = OwnedEntry {
            key: key.clone(),
            created: false,
        };
        for field in anchor(Some(&item), &group, &prior).map_err(context)? {
            project_exact(&mut item, field, &prior, Some(entry.clone()), edit).map_err(context)?;
        }
        return Ok(());
    }
    let fields = anchor(None, &group, &[]).map_err(context)?;
    let unchanged = item.as_ref().is_some_and(|item| {
        fields
            .iter()
            .all(|field| item.get_value(&refs(&field.path)) == field.value)
    });
    if !unchanged {
        let mut content = serde_json::Map::new();
        content.insert(key.key.clone(), serde_json::Value::String(key.id.clone()));
        for field in &fields {
            if let Some(value) = &field.value {
                content.insert(field.path[0].clone(), value.to_json());
            }
        }
        if item.is_some() {
            doc.remove_entry(key)?;
        }
        doc.insert_entry(&serde_json::Value::Object(content))?;
    }
    for field in fields {
        let path = refs(&field.path);
        let current = item.as_ref().and_then(|item| item.get_value(&path));
        if current != field.value {
            let mut change = preview_change(
                &path,
                current,
                field.value.clone(),
                field.preview.as_deref(),
            );
            change.key = format!("{}.{}", key.id, change.key);
            edit.changes.push(change);
        }
        owned_fields(edit).push(OwnedField {
            path: field.path,
            value: field.value,
            previous: None,
            entry: Some(OwnedEntry {
                key: key.clone(),
                created: true,
            }),
            exact: true,
            source: None,
            hashed: false,
            container: false,
        });
    }
    Ok(())
}

/// Whether an item the connection appended still holds exactly what it wrote,
/// with nothing the user added.
fn appended_intact(item: &ConfigDoc, key: &EntryKey, fields: &[&OwnedField]) -> bool {
    let mut expected = serde_json::Map::new();
    expected.insert(key.key.clone(), serde_json::Value::String(key.id.clone()));
    for field in fields {
        match (&field.entry, &field.value) {
            (Some(entry), Some(value)) if entry.created && field.path.len() == 1 => {
                expected.insert(field.path[0].clone(), value.to_json());
            }
            _ => return false,
        }
    }
    item.get_value(&[]) == Some(ConfigValue::Json(serde_json::Value::Object(expected)))
}

/// Exact fields only add keys or replace scalars. In an item the connection
/// appends, each field is written under its first key and the item leaves
/// whole. Elsewhere a missing parent is created as an empty block mapping of
/// its own (a container field, also one a prior connection created), which
/// restoring removes only while it is empty, so keys others add later neither
/// keep the connection's own keys in place nor block reconnecting.
fn anchor(
    target: Option<&ConfigDoc>,
    fields: &[&Field],
    prior: &[&OwnedField],
) -> Result<Vec<Field>, String> {
    let mut anchored: Vec<Field> = Vec::new();
    for field in fields {
        let path = refs(&field.path);
        let Some(value) = &field.value else {
            if target.is_some_and(|target| target.contains(&path)) {
                return Err(format!(
                    "sets `{}`, which cannot be removed and put back exactly; remove it",
                    path.join(".")
                ));
            }
            continue;
        };
        let Some(target) = target else {
            let nested = field.path[1..].iter().rev().fold(
                value.to_json(),
                |value, key| serde_json::json!({ key.as_str(): value }),
            );
            match anchored
                .iter_mut()
                .find(|other| other.path[..] == field.path[..1])
            {
                Some(other) => {
                    let mut merged = other.value.as_ref().map(ConfigValue::to_json);
                    merge_json(merged.as_mut(), nested)
                        .ok_or_else(|| format!("sets `{}` twice", path.join(".")))?;
                    other.value = merged.and_then(|merged| ConfigValue::from_json(&merged));
                    other.preview = other.preview.take().or_else(|| field.preview.clone());
                }
                None => anchored.push(Field {
                    path: field.path[..1].to_vec(),
                    value: ConfigValue::from_json(&nested),
                    ..(*field).clone()
                }),
            }
            continue;
        };
        for depth in 1..path.len() {
            let parent = &field.path[..depth];
            let created = prior
                .iter()
                .any(|owned| owned.container && owned.path == parent)
                || !target.contains(&path[..depth]);
            if created && !anchored.iter().any(|other| other.path == parent) {
                anchored.push(Field {
                    path: parent.to_vec(),
                    value: None,
                    preview: None,
                    container: true,
                    ..(*field).clone()
                });
            }
        }
        anchored.push((*field).clone());
    }
    Ok(anchored)
}

fn merge_json(target: Option<&mut serde_json::Value>, source: serde_json::Value) -> Option<()> {
    let (Some(serde_json::Value::Object(target)), serde_json::Value::Object(source)) =
        (target, source)
    else {
        return None;
    };
    for (key, value) in source {
        match target.get_mut(&key) {
            Some(existing) => merge_json(Some(existing), value)?,
            None => {
                target.insert(key, value);
            }
        }
    }
    Some(())
}

fn project_exact(
    target: &mut ConfigDoc,
    field: Field,
    prior: &[&OwnedField],
    entry: Option<OwnedEntry>,
    edit: &mut Edit,
) -> Result<(), String> {
    let path = refs(&field.path);
    let name = path.join(".");
    if field.container {
        if !target.contains(&path) {
            if target.is_flow(&path[..path.len() - 1]) {
                return Err(format!(
                    "writes `{name}` in flow style ({{...}}); write it as a block mapping"
                ));
            }
            target.create_mapping(&path)?;
        }
        owned_fields(edit).push(OwnedField {
            path: field.path,
            value: None,
            previous: None,
            entry,
            exact: true,
            source: None,
            hashed: false,
            container: true,
        });
        return Ok(());
    }
    let current = target.get_value(&path);
    if current.is_none() && target.contains(&path) {
        return Err(format!(
            "has `{name}`, which cannot be read safely (an alias, tag or duplicate key)"
        ));
    }
    if target.is_flow(&path[..path.len() - 1]) {
        return Err(format!(
            "writes `{name}` in flow style ({{...}}); write it as a block mapping"
        ));
    }
    let still_ours = prior
        .iter()
        .find(|owned| owned.path == field.path && owned.holds(current.as_ref()));
    let (previous, source) = match (still_ours, &current) {
        (Some(owned), _) => (owned.previous.clone(), owned.source.clone()),
        (None, None) => (None, None),
        (None, Some(ConfigValue::List(_) | ConfigValue::Json(_))) => {
            return Err(format!(
                "already has `{name}`, a structured value that cannot be replaced and put back exactly"
            ))
        }
        (None, Some(_)) if matches!(field.value, Some(ConfigValue::List(_) | ConfigValue::Json(_))) => {
            return Err(format!(
                "already has `{name}`, which cannot be replaced by a structured value and put back exactly"
            ))
        }
        (None, Some(_)) if is_sensitive(&field.path) => {
            return Err(format!("already holds a credential at `{name}`"))
        }
        (None, Some(held)) => (
            Some(Previous::Plain(held.clone())),
            target.scalar_source(&path),
        ),
    };
    if current != field.value {
        match &field.value {
            Some(value) => target.set_value(&path, value)?,
            None => target.remove_exact(&path)?,
        }
        let mut change = if is_sensitive(&field.path) {
            ConfigChange {
                key: name,
                before: None,
                after: Some("Managed local credential".to_string()),
                sensitive: true,
            }
        } else {
            preview_change(
                &path,
                current,
                field.value.clone(),
                field.preview.as_deref(),
            )
        };
        if let Some(entry) = &entry {
            change.key = format!("{}.{}", entry.key.id, change.key);
        }
        edit.changes.push(change);
    }
    owned_fields(edit).push(OwnedField {
        path: field.path,
        value: field.value,
        previous,
        entry,
        exact: true,
        source,
        hashed: false,
        container: false,
    });
    Ok(())
}

/// What the config holds at an owned field, inside its list item if it has one.
pub(super) fn owned_value(doc: &ConfigDoc, field: &OwnedField) -> Option<ConfigValue> {
    match &field.entry {
        Some(entry) => doc
            .entry(&entry.key)
            .ok()
            .flatten()?
            .get_value(&refs(&field.path)),
        None => doc.get_value(&refs(&field.path)),
    }
}

/// Undo a connection: every owned field that still holds what we wrote goes
/// back to its previous value (plain, or fetched from the credential store)
/// or disappears, pruning emptied containers; anything the user changed since
/// is left alone. Idempotent: a field already restored is skipped. Exact
/// fields put back the recorded bytes, and list items the connection appended
/// are removed whole, first to last, while every field in them is unchanged.
pub(super) fn restore(
    doc: &mut ConfigDoc,
    record: &Connection,
    secrets: &dyn SecretStore,
) -> Result<Edit, AgentError> {
    record
        .validate_recovery()
        .map_err(AgentError::ConfigurationConflict)?;
    let routing_unchanged = record
        .fields
        .iter()
        .filter(|field| {
            matches!(
                field.path.last().map(String::as_str),
                Some(
                    "apiKeyHelper"
                        | "ANTHROPIC_BASE_URL"
                        | "base_url"
                        | "baseURL"
                        | "command"
                        | "args"
                        | "model_provider"
                )
            )
        })
        .all(|field| doc.get_value(&refs(&field.path)) == field.value);
    if !routing_unchanged
        && record.fields.iter().any(|field| {
            is_sensitive(&field.path)
                && field.value.is_none()
                && field.previous.is_some()
                && doc.get_value(&refs(&field.path)).is_none()
        })
    {
        return Err(AgentError::ConfigurationConflict("Credential restoration is ambiguous after routing or helper edits. Access is disabled; restore the agent's original routing/helper settings, then retry disconnecting. The recovery record and parked credentials are retained".to_string()));
    }
    let mut changes = Vec::new();
    let mut consumed_secrets = Vec::new();
    let mut appended: Vec<(usize, &EntryKey)> = Vec::new();
    for entry in record
        .fields
        .iter()
        .filter_map(|field| field.entry.as_ref().filter(|entry| entry.created))
    {
        if appended.iter().any(|(_, key)| *key == &entry.key) {
            continue;
        }
        let fields: Vec<&OwnedField> = record
            .fields
            .iter()
            .filter(|field| field.entry.as_ref() == Some(entry))
            .collect();
        let intact = doc
            .entry(&entry.key)
            .map_err(AgentError::ConfigurationConflict)?
            .is_some_and(|item| appended_intact(&item, &entry.key, &fields));
        if intact {
            let index = doc
                .entry_index(&entry.key)
                .map_err(AgentError::ConfigurationConflict)?;
            appended.extend(index.map(|index| (index, &entry.key)));
        }
    }
    appended.sort_by_key(|(index, _)| *index);
    for (_, key) in appended {
        doc.remove_entry(key)
            .map_err(|_| AgentError::RestorationFailed)?;
        changes.push(ConfigChange {
            key: key.id.clone(),
            before: Some("Connected item".into()),
            after: None,
            sensitive: false,
        });
    }
    for field in &record.fields {
        if field.container || field.entry.as_ref().is_some_and(|entry| entry.created) {
            continue;
        }
        if field.exact {
            let change = match &field.entry {
                Some(entry) => match doc
                    .entry(&entry.key)
                    .map_err(AgentError::ConfigurationConflict)?
                {
                    Some(mut item) => restore_exact(&mut item, field)?.map(|mut change| {
                        change.key = format!("{}.{}", entry.key.id, change.key);
                        change
                    }),
                    None => None,
                },
                None => restore_exact(doc, field)?,
            };
            changes.extend(change);
            continue;
        }
        if retain_provider_field(field, &record.fields) {
            let path = refs(&field.path);
            let inactive = inactive_provider_value(field);
            if doc.get_value(&path) == field.value && inactive != field.value {
                match &inactive {
                    Some(value) => doc.set_value(&path, value),
                    None => doc.remove(&path),
                }
                .map_err(|_| AgentError::RestorationFailed)?;
                changes.push(ConfigChange {
                    key: path.join("."),
                    before: Some("Connected provider".into()),
                    after: Some("Provider retained without active credentials".into()),
                    sensitive: false,
                });
            }
            continue;
        }
        let path = refs(&field.path);
        let current = doc.get_value(&path);
        if let Some(Previous::Secret { secret_ref }) = &field.previous {
            consumed_secrets.push(secret_ref.clone());
        }
        if !field.holds(current.as_ref()) {
            continue;
        }
        let sensitive = is_sensitive(&field.path);
        let (restored, after_label) = match &field.previous {
            Some(Previous::Plain(value)) => (Some(value.clone()), None),
            Some(Previous::Secret { secret_ref }) => match secrets
                .get(secret_ref)
                .map_err(|_| AgentError::CredentialStore)?
            {
                Some(value) => (
                    Some(ConfigValue::Str(value)),
                    Some("Previous secret restored".to_string()),
                ),
                None => (
                    None,
                    Some("Previous secret unavailable; left unset".to_string()),
                ),
            },
            None => (None, None),
        };
        match &restored {
            Some(value) => doc.set_value(&path, value),
            None => doc.remove(&path),
        }
        .map_err(|_| AgentError::RestorationFailed)?;
        changes.push(if sensitive {
            ConfigChange {
                key: path.join("."),
                before: current
                    .as_ref()
                    .map(|_| "Managed local credential".to_string()),
                after: after_label,
                sensitive: true,
            }
        } else {
            change(&path, current, restored)
        });
    }
    // Containers go last, innermost first, and only while nothing is left in them.
    for field in record.fields.iter().rev().filter(|field| field.container) {
        let empty = Some(ConfigValue::Json(serde_json::json!({})));
        let path = refs(&field.path);
        let removed = match &field.entry {
            Some(entry) => match doc
                .entry(&entry.key)
                .map_err(AgentError::ConfigurationConflict)?
            {
                Some(mut item) if item.get_value(&path) == empty => Some(item.remove_exact(&path)),
                _ => None,
            },
            None if doc.get_value(&path) == empty => Some(doc.remove_exact(&path)),
            None => None,
        };
        if let Some(removed) = removed {
            removed.map_err(|_| AgentError::RestorationFailed)?;
            changes.push(change(&path, empty, None));
        }
    }
    Ok(Edit {
        selection: None,
        changes,
        record: None,
        pending_secrets: Vec::new(),
        consumed_secrets,
    })
}

/// Put an exact field back: remove the key it added, or the scalar source it
/// replaced. A field the user changed since is left alone.
fn restore_exact(
    target: &mut ConfigDoc,
    field: &OwnedField,
) -> Result<Option<ConfigChange>, AgentError> {
    let path = refs(&field.path);
    let current = target.get_value(&path);
    if !field.holds(current.as_ref()) {
        return Ok(None);
    }
    let restored = match &field.previous {
        Some(Previous::Plain(value)) => Some(value.clone()),
        Some(Previous::Secret { .. }) => return Err(AgentError::RestorationFailed),
        None => None,
    };
    match (&restored, &field.source) {
        (Some(_), Some(source)) => target.set_scalar_source(&path, source),
        (Some(value), None) => target.set_value(&path, value),
        (None, _) => target.remove_exact(&path),
    }
    .map_err(|_| AgentError::RestorationFailed)?;
    Ok(Some(if is_sensitive(&field.path) {
        ConfigChange {
            key: path.join("."),
            before: Some("Managed local credential".to_string()),
            after: None,
            sensitive: true,
        }
    } else {
        change(&path, current, restored)
    }))
}

pub(super) fn change(
    path: &[&str],
    before: Option<ConfigValue>,
    after: Option<ConfigValue>,
) -> ConfigChange {
    ConfigChange {
        key: path.join("."),
        before: before.map(|value| value.display()),
        after: after.map(|value| value.display()),
        sensitive: false,
    }
}

pub(super) fn preview_change(
    path: &[&str],
    before: Option<ConfigValue>,
    after: Option<ConfigValue>,
    preview: Option<&str>,
) -> ConfigChange {
    ConfigChange {
        key: path.join("."),
        before: before.map(|value| {
            if preview.is_some() && matches!(value, ConfigValue::Json(_)) {
                "Existing provider configuration".to_string()
            } else {
                value.display()
            }
        }),
        after: after.map(|value| {
            preview
                .map(str::to_string)
                .unwrap_or_else(|| value.display())
        }),
        sensitive: false,
    }
}

pub(super) fn refs(path: &[String]) -> Vec<&str> {
    path.iter().map(String::as_str).collect()
}

pub(super) fn connection_options(
    agent: Agent,
    doc: &ConfigDoc,
    prior: Option<&Connection>,
    catalog: Option<&Catalog>,
    options: &ConnectOptions,
) -> ConnectOptions {
    if options.default_model.is_some() {
        return options.clone();
    }
    let compatible = |model: &String| {
        catalog.is_some_and(|catalog| {
            catalog
                .get(model)
                .is_some_and(|entry| entry.supports_agent(agent.surface()))
        })
    };
    let current = selected_model(agent, Some(doc)).filter(compatible);
    let saved = prior
        .and_then(|record| record.options.default_model.clone())
        .filter(compatible);
    let preferred = if prior.is_some_and(|record| record.suspended && record.attention.is_none()) {
        saved.or(current)
    } else {
        current.or(saved)
    };
    ConnectOptions {
        default_model: preferred.or_else(|| {
            catalog
                .and_then(|catalog| {
                    catalog
                        .models
                        .iter()
                        .find(|model| model.supports_agent(agent.surface()))
                })
                .map(|model| model.id().to_string())
        }),
    }
}

pub(super) fn selected_model(agent: Agent, doc: Option<&ConfigDoc>) -> Option<String> {
    let doc = doc?;
    match agent {
        Agent::Codex => doc.get_str(&["model"]),
        Agent::ClaudeCode => doc.get_str(&["env", "ANTHROPIC_MODEL"]),
        Agent::OpenCode => doc
            .get_str(&["model"])
            .and_then(|value| value.strip_prefix("private-ai-proxy/").map(str::to_string)),
        Agent::Pi => None,
        Agent::Hermes => doc.get_str(&["model", "default"]),
        Agent::OpenClaw => openclaw::selected_model(doc),
        Agent::OhMyPi => None,
        Agent::Dsh => dsh::selected_model(doc),
    }
}
