//! Cline CLI keeps provider settings in `$CLINE_DIR/data/settings/providers.json`
//! (`~/.cline` by default, `$CLINE_DATA_DIR` replacing the data directory).
//! Its runtime refuses provider ids of its own ("Unknown or disabled
//! provider"), so a connection takes over the built-in `openai-compatible`
//! entry and makes it the provider in use, parking what it held. Cline reads
//! the key only as a value, and lists the endpoint's models itself.
use super::*;

const SLOT: &str = "openai-compatible";

pub(super) fn config_path(home: &Path, tool_env: bool) -> PathBuf {
    data_dir(home, tool_env)
        .join("settings")
        .join("providers.json")
}

/// `cline --version` creates the Cline directory; the settings folder
/// appears only once a provider is saved.
pub(super) fn detection_dir(home: &Path, tool_env: bool) -> PathBuf {
    let var = |name: &str| tool_env.then(|| env_path(name)).flatten();
    var("CLINE_DATA_DIR")
        .or_else(|| var("CLINE_DIR"))
        .unwrap_or_else(|| home.join(".cline"))
}

fn data_dir(home: &Path, tool_env: bool) -> PathBuf {
    let var = |name: &str| tool_env.then(|| env_path(name)).flatten();
    var("CLINE_DATA_DIR").unwrap_or_else(|| {
        var("CLINE_DIR")
            .unwrap_or_else(|| home.join(".cline"))
            .join("data")
    })
}

pub(super) fn fields(
    inputs: &Inputs<'_>,
    base: &str,
    default_model: Option<&str>,
) -> Result<Vec<Field>, AgentError> {
    let settings = |key: &'static str| ["providers", SLOT, "settings", key];
    // Each stored provider needs a timestamp, and a new file needs its version.
    let updated_at = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut fields = vec![
        number(&["version"], 1),
        set(&["lastUsedProvider"], SLOT),
        set(&settings("provider"), SLOT),
        set(&settings("baseUrl"), format!("{base}/v1")),
        set(&settings("apiKey"), inputs.token()?),
        // These route the slot to another API or provider.
        absent(&settings("protocol")),
        absent(&settings("client")),
        absent(&settings("routingProviderId")),
        set(&["providers", SLOT, "updatedAt"], updated_at),
    ];
    if let Some(model) = default_model {
        fields.push(set(&settings("model"), model));
    }
    Ok(fields)
}

/// An OAuth or stored `auth` credential takes priority over `apiKey`.
pub(super) fn validate(doc: &ConfigDoc) -> Result<(), AgentError> {
    if doc.contains(&["providers", SLOT, "settings", "auth"]) {
        return Err(AgentError::AuthenticationConflict(
            "Cline's openai-compatible provider has a stored auth credential that takes priority over the local token; remove it in Cline before connecting".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn user_selection(path: &[&str]) -> bool {
    matches!(
        path,
        ["lastUsedProvider"]
            | ["providers", SLOT, "updatedAt"]
            | ["providers", SLOT, "settings", "model"]
    )
}

/// A model picked in Cline while connected stays in the slot after its
/// provider fields are removed. Cline refuses a whole providers.json with a
/// slot that names no provider, so a slot the connection created goes too
/// when only Cline's own model choice and save time are left in it.
pub(super) fn remove_created_slot(doc: &mut ConfigDoc, edit: &mut Edit) -> Result<(), AgentError> {
    let slot = ["providers", SLOT];
    let Some(ConfigValue::Json(value)) = doc.get_value(&slot) else {
        return Ok(());
    };
    let only_selection = value.as_object().is_some_and(|entry| {
        entry.iter().all(|(key, value)| match key.as_str() {
            "updatedAt" | "tokenSource" => true,
            "settings" => value
                .as_object()
                .is_some_and(|settings| settings.keys().all(|key| key == "model")),
            _ => false,
        })
    });
    if !only_selection {
        return Ok(());
    }
    doc.remove(&slot)
        .map_err(|_| AgentError::RestorationFailed)?;
    edit.changes.push(ConfigChange {
        key: slot.join("."),
        before: Some("Model selection left in the connected provider".into()),
        after: None,
        sensitive: false,
    });
    Ok(())
}

pub(super) fn selected_model(doc: &ConfigDoc) -> Option<String> {
    (doc.get_str(&["lastUsedProvider"]).as_deref() == Some(SLOT))
        .then(|| doc.get_str(&["providers", SLOT, "settings", "model"]))
        .flatten()
}
