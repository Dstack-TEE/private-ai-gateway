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

pub(super) fn cli_paths(home: &Path, tool_env: bool) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = if tool_env {
        env::var_os("PATH")
            .map(|paths| env::split_paths(&paths).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    paths.extend([
        home.join(".local/bin"),
        home.join(".local/share/pnpm"),
        home.join(".cargo/bin"),
        home.join(".opencode/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join("Library/pnpm"),
        home.join(".bun/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".mise/shims"),
        home.join(".asdf/shims"),
        home.join(".local/share/rtx/shims"),
        home.join(".rtx/shims"),
    ]);
    if tool_env {
        paths.extend([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    }
    if cfg!(windows) {
        let app_data = tool_env
            .then(|| env::var_os("APPDATA"))
            .flatten()
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        paths.push(app_data.join("npm"));
    }
    paths.extend(versioned_runtime_bins(
        &home.join(".nvm/versions/node"),
        &["bin"],
    ));
    paths.extend(versioned_runtime_bins(
        &home.join(".local/share/fnm/node-versions"),
        &["installation", "bin"],
    ));
    paths.extend(versioned_runtime_bins(
        &home.join(".local/share/mise/installs/node"),
        &["bin"],
    ));
    paths.extend(versioned_runtime_bins(
        &home.join(".mise/installs/node"),
        &["bin"],
    ));
    paths.extend(versioned_runtime_bins(
        &home.join(".asdf/installs/nodejs"),
        &["bin"],
    ));
    paths.extend(versioned_runtime_bins(
        &home.join(".local/share/rtx/installs/node"),
        &["bin"],
    ));
    paths.extend(versioned_runtime_bins(
        &home.join(".rtx/installs/node"),
        &["bin"],
    ));
    paths
}

pub(super) fn find_cli(agent: Agent, home: &Path, tool_env: bool) -> Option<PathBuf> {
    let mut paths = cli_paths(home, tool_env);
    if cfg!(windows) && agent == Agent::Hermes {
        if let Some(directory) = agent.config_path(home, tool_env).parent() {
            paths.push(directory.join("bin"));
        }
    }
    find_cli_in_paths(agent, &paths)
}

pub(super) fn find_cli_in_paths(agent: Agent, paths: &[PathBuf]) -> Option<PathBuf> {
    agent
        .cli_names()
        .iter()
        .find_map(|name| cli_in_paths(name, paths))
}

pub(super) fn cli_installed(agent: Agent, home: &Path, tool_env: bool) -> bool {
    find_cli(agent, home, tool_env).is_some()
}

pub(super) fn versioned_runtime_bins(root: &Path, suffix: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .take(64)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| {
            suffix
                .iter()
                .fold(entry.path(), |path, part| path.join(part))
        })
        .collect()
}

pub(super) fn cli_in_paths(name: &str, paths: &[PathBuf]) -> Option<PathBuf> {
    let candidates: &[String] = &if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    } else {
        vec![name.to_string()]
    };
    paths
        .iter()
        .flat_map(|dir| candidates.iter().map(move |candidate| dir.join(candidate)))
        .find(|candidate| executable_metadata(candidate).is_some())
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
