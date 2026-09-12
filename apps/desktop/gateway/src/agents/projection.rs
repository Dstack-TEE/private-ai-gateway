use super::*;

pub(super) struct Field {
    pub(super) path: Vec<String>,
    /// `None` makes the key absent.
    pub(super) value: Option<ConfigValue>,
    /// Concise preview text for generated structured values.
    pub(super) preview: Option<String>,
}

pub(super) fn set(path: &[&str], value: impl Into<String>) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Str(value.into())),
        preview: None,
    }
}

pub(super) fn number(path: &[&str], value: u64) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Number(value)),
        preview: None,
    }
}

pub(super) fn boolean(path: &[&str], value: bool) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Bool(value)),
        preview: None,
    }
}

pub(super) fn generated_catalog(path: &[&str], value: serde_json::Value, models: usize) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::Json(value)),
        preview: Some(format!("Generated catalog ({models} models)")),
    }
}

pub(super) fn list(path: &[&str], values: &[&str]) -> Field {
    Field {
        path: owned(path),
        value: Some(ConfigValue::List(
            values.iter().map(|value| (*value).to_string()).collect(),
        )),
        preview: None,
    }
}

pub(super) fn absent(path: &[&str]) -> Field {
    Field {
        path: owned(path),
        value: None,
        preview: None,
    }
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

pub(super) fn stale_helper(agent: Agent, record: &Connection, exe: &Path) -> bool {
    let (path, expected) = match agent {
        Agent::Codex => (
            &["model_providers", "private_ai_proxy", "auth", "command"][..],
            exe.to_str().map(str::to_string),
        ),
        Agent::ClaudeCode => (&["apiKeyHelper"][..], helper_command(exe, agent.id()).ok()),
        Agent::Pi => (
            &["providers", "private-ai-proxy"][..],
            credential_helper_command(exe, agent)
                .ok()
                .map(|command| format!("!{command}")),
        ),
        Agent::Hermes => (
            &["providers", "private-ai-proxy", "key_cmd"][..],
            credential_helper_command(exe, agent).ok(),
        ),
        Agent::OpenCode => return false,
        Agent::OpenClaw => return false, // Validated with the token path by Projector.
        Agent::OhMyPi => return oh_my_pi::stale_helper(record, exe),
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
pub(super) fn project(
    doc: &mut ConfigDoc,
    fields: &[Field],
    prior: Option<&Connection>,
    agent: Agent,
) -> Result<Edit, String> {
    let mut record = Connection::default();
    let mut changes = Vec::new();
    let mut pending_secrets = Vec::new();
    for field in fields {
        let path = refs(&field.path);
        let sensitive = is_sensitive(&field.path);
        let current = doc.get_value(&path);
        let still_ours = prior.and_then(|prior| {
            prior
                .fields
                .iter()
                .find(|owned| owned.path == field.path && current == owned.value)
        });
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
                    pending_secrets.push(PendingSecret {
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
            changes.push(if sensitive {
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
        record.fields.push(OwnedField {
            path: field.path.clone(),
            value: field.value.clone(),
            previous,
        });
    }
    Ok(Edit {
        selection: None,
        changes,
        record: Some(record),
        pending_secrets,
        consumed_secrets: Vec::new(),
    })
}

/// Undo a connection: every owned field that still holds what we wrote goes
/// back to its previous value (plain, or fetched from the credential store)
/// or disappears, pruning emptied containers; anything the user changed since
/// is left alone. Idempotent: a field already restored is skipped.
pub(super) fn restore(
    doc: &mut ConfigDoc,
    record: &Connection,
    secrets: &dyn SecretStore,
) -> Result<Edit, String> {
    record.validate_recovery()?;
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
        return Err("Credential restoration is ambiguous after routing or helper edits. Access is disabled; the recovery record and parked credentials are retained".to_string());
    }
    let mut changes = Vec::new();
    let mut consumed_secrets = Vec::new();
    for field in &record.fields {
        if retain_provider_field(field, &record.fields) {
            let path = refs(&field.path);
            let inactive = inactive_provider_value(field);
            if doc.get_value(&path) == field.value && inactive != field.value {
                match &inactive {
                    Some(value) => doc.set_value(&path, value)?,
                    None => doc.remove(&path)?,
                }
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
        if current != field.value {
            continue;
        }
        let sensitive = is_sensitive(&field.path);
        let (restored, after_label) = match &field.previous {
            Some(Previous::Plain(value)) => (Some(value.clone()), None),
            Some(Previous::Secret { secret_ref }) => match secrets.get(secret_ref)? {
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
            Some(value) => doc.set_value(&path, value)?,
            None => doc.remove(&path)?,
        }
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
    Ok(Edit {
        selection: None,
        changes,
        record: None,
        pending_secrets: Vec::new(),
        consumed_secrets,
    })
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
    let current = selected_model(agent, Some(doc)).filter(|model| {
        catalog.is_some_and(|catalog| {
            catalog
                .get(model)
                .is_some_and(|entry| entry.supports(agent.surface()))
        })
    });
    let saved = prior.and_then(|record| record.options.default_model.clone());
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
                        .find(|model| model.supports(agent.surface()))
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
    }
}
