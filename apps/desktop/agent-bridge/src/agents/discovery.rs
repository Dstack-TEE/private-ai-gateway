use super::*;

pub(super) fn hermes_native_dir(home: &Path, tool_env: bool) -> PathBuf {
    if cfg!(windows) {
        tool_env
            .then(|| env_path("LOCALAPPDATA"))
            .flatten()
            .and_then(|path| {
                path.to_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("hermes")
    } else {
        home.join(".hermes")
    }
}

pub(super) fn read_auth_document(path: &Path) -> Result<serde_json::Value, AgentError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(serde_json::json!({})),
        Err(_) => return Err(AgentError::ConfigurationRead),
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        AgentError::InvalidConfiguration(format!(
            "Cannot inspect native credential conflicts at {}; invalid JSON",
            path.display()
        ))
    })?;
    if !value.is_object() {
        return Err(AgentError::InvalidConfiguration(format!(
            "Cannot inspect native credential conflicts at {}; expected an object",
            path.display()
        )));
    }
    Ok(value)
}

// OpenCode uses remeda mergeDeep: objects merge recursively; other values replace.
pub(super) fn merge_opencode_config(target: &mut serde_json::Value, source: serde_json::Value) {
    match (target, source) {
        (serde_json::Value::Object(target), serde_json::Value::Object(source)) => {
            for (key, value) in source {
                merge_opencode_config(target.entry(key).or_insert(serde_json::Value::Null), value);
            }
        }
        (target, source) => *target = source,
    }
}

/// An agent is detected by the directory that holds the config file this app
/// edits (for example `~/.codex`), which each agent creates on first run.
/// Unlike an executable search this does not depend on how the CLI was
/// installed and needs neither PATH nor a login shell, so it behaves the same
/// inside the macOS App Sandbox.
pub(super) fn detected(agent: Agent, home: &Path, tool_env: bool) -> bool {
    agent
        .config_path(home, tool_env)
        .parent()
        .is_some_and(Path::is_dir)
}

pub(super) fn executable_metadata(path: &Path) -> Option<fs::Metadata> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(metadata)
}

pub(super) fn validate_authorized_home(home: &Path) -> Result<(), String> {
    let metadata = fs::metadata(home).map_err(|_| {
        "Agent Home access is not available to the backend. Re-enable Agent integrations and try again."
            .to_string()
    })?;
    if !metadata.is_dir() {
        return Err("The authorized Agent Home path is not a directory".to_string());
    }
    fs::read_dir(home).map_err(|_| {
        "Agent Home access is not available to the backend. Re-enable Agent integrations and try again."
            .to_string()
    })?;
    Ok(())
}

impl Projector {
    /// Project the app-owned Codex baseline and verified provider metadata.
    /// Both distributions embed the same catalog; no host executable is involved.
    pub fn sync_codex_catalog(&self, catalog: &Catalog) -> Result<(), AgentError> {
        let metadata = codex_catalog(catalog).map_err(AgentError::MetadataUnavailable)?;
        let text = serde_json::to_string_pretty(&metadata).map_err(|_| AgentError::Internal)?;
        private_fs::create_private_dir(&self.data_dir)
            .map_err(|_| AgentError::ConfigurationWrite)?;
        let path = self.codex_catalog_path();
        if fs::read_to_string(&path).is_ok_and(|current| current == text) {
            return Ok(());
        }
        write_atomic(&path, &text, None).map_err(|_| AgentError::ConfigurationWrite)
    }
}
