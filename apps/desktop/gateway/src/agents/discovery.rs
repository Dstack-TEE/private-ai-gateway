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
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
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

pub(super) fn command_output(
    executable: &Path,
    args: &[&str],
    search_paths: &[PathBuf],
) -> io::Result<std::process::Output> {
    let mut command;
    #[cfg(windows)]
    if executable
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
    {
        command = Command::new("cmd");
        command.arg("/C").arg(executable).args(args);
    } else {
        command = Command::new(executable);
        command.args(args);
    }
    #[cfg(not(windows))]
    {
        command = Command::new(executable);
        command.args(args);
    }
    command.env("PATH", command_path(executable, search_paths)?);
    bounded_command_output(command, std::time::Duration::from_secs(15))
}

pub(super) fn bounded_command_output(
    command: Command,
    timeout: std::time::Duration,
) -> io::Result<std::process::Output> {
    // Agent transactions are synchronous and may run inside a Tokio worker.
    // A dedicated thread keeps the bounded I/O runtime out of the caller's runtime.
    std::thread::Builder::new()
        .name("agent-metadata".to_string())
        .spawn(move || {
            use std::process::Stdio;
            use tokio::io::{AsyncRead, AsyncReadExt};

            async fn read_pipe(pipe: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
                let mut bytes = Vec::new();
                pipe.take(limit as u64 + 1).read_to_end(&mut bytes).await?;
                if bytes.len() > limit {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Model metadata output exceeds limit"));
                }
                Ok(bytes)
            }

            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    let mut child = tokio::process::Command::from(command)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()?;
                    let stdout = child.stdout.take().ok_or_else(|| io::Error::other("Missing stdout pipe"))?;
                    let stderr = child.stderr.take().ok_or_else(|| io::Error::other("Missing stderr pipe"))?;
                    let result = tokio::time::timeout(timeout, async {
                        tokio::try_join!(child.wait(), read_pipe(stdout, 8 * 1024 * 1024), read_pipe(stderr, 64 * 1024))
                    }).await;
                    match result {
                        Ok(Ok((status, stdout, stderr))) => Ok(std::process::Output { status, stdout, stderr }),
                        result => {
                            // Preserve the primary I/O failure even if the child exited while
                            // its bounded pipes were being drained.
                            let _ = child.start_kill();
                            let _ = child.wait().await;
                            match result {
                                Ok(Err(error)) => Err(error),
                                _ => Err(io::Error::new(io::ErrorKind::TimedOut, "Codex model metadata export timed out; check the Codex installation and retry")),
                            }
                        }
                    }
                })
        })?
        .join()
        .map_err(|_| io::Error::other("Model metadata worker failed"))?
}

pub(super) fn command_path(executable: &Path, search_paths: &[PathBuf]) -> io::Result<OsString> {
    // npm launchers should resolve the runtime from their own installation first.
    let mut paths: Vec<PathBuf> = executable
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    let inherited: Vec<PathBuf> = env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).collect())
        .unwrap_or_default();
    for path in search_paths.iter().chain(&inherited) {
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    env::join_paths(paths).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

pub(super) fn codex_metadata(
    output: io::Result<std::process::Output>,
) -> Result<serde_json::Value, AgentError> {
    let output = output.map_err(|error| AgentError::MetadataUnavailable(match error.kind() {
        io::ErrorKind::TimedOut => "Codex model metadata export timed out. Run `codex debug models --bundled` in Terminal to check the installation, then retry.".to_string(),
        io::ErrorKind::InvalidData => "Codex returned invalid or oversized model metadata output. Check or reinstall the Codex installation, then retry.".to_string(),
        io::ErrorKind::PermissionDenied => "Codex could not start because execution permission was denied. Check the Codex installation, then retry.".to_string(),
        _ => "Codex could not start to export model metadata. Check that Codex and its runtime are installed, then retry.".to_string(),
    }))?;
    if !output.status.success() {
        // Inspect known CLI diagnostics without returning arbitrary stderr to clients.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("unrecognized subcommand")
            || stderr.contains("unexpected argument '--bundled'")
        {
            return Err(AgentError::MetadataUnavailable("Codex CLI does not support `codex debug models --bundled`. Update Codex before connecting.".to_string()));
        }
        return Err(AgentError::MetadataUnavailable(format!(
            "Codex exited with {} while exporting model metadata. Run `codex debug models --bundled` in Terminal to see the cause, then retry.",
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| AgentError::MetadataUnavailable("Codex returned malformed bundled model metadata. Update or reinstall Codex, then retry.".to_string()))
}

pub(super) fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(super) fn home_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    if let Some(home) = env_path("USERPROFILE") {
        return Ok(home);
    }
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .ok_or_else(|| "Cannot determine the home directory".to_string())
}

/// The per-user app data directory (tokens, connection record, locks),
/// resolved the same way by the desktop shell and the
/// bundled helper.
pub fn app_data_dir() -> Result<PathBuf, String> {
    if let Some(home) = env_path(HOME_OVERRIDE_ENV) {
        return Ok(home.join(".private-ai-proxy"));
    }
    let base = if cfg!(target_os = "macos") {
        home_dir()?.join("Library").join("Application Support")
    } else if cfg!(windows) {
        env_path("APPDATA").ok_or_else(|| "APPDATA is not set".to_string())?
    } else {
        env_path("XDG_DATA_HOME").map_or_else(
            || home_dir().map(|home| home.join(".local").join("share")),
            Ok,
        )?
    };
    Ok(base.join(APP_IDENTIFIER))
}

impl Projector {
    /// Keep Codex's startup metadata in sync with the verified service
    /// catalog. The installed Codex binary supplies its exact catalog schema
    /// and complete built-in instructions; only public model metadata from
    /// the verified service is overlaid. No credentials are written here.
    pub fn sync_codex_catalog(&self, catalog: &Catalog) -> Result<(), AgentError> {
        let bundled = self.codex_bundled_catalog()?;
        let metadata = codex_catalog(catalog, &bundled).map_err(AgentError::MetadataUnavailable)?;
        let text = serde_json::to_string_pretty(&metadata).map_err(|_| AgentError::Internal)?;
        tokens::create_private_dir(&self.data_dir).map_err(|_| AgentError::ConfigurationWrite)?;
        if fs::read_to_string(self.codex_catalog_path()).is_ok_and(|current| current == text) {
            return Ok(());
        }
        write_atomic(&self.codex_catalog_path(), &text, None)
            .map_err(|_| AgentError::ConfigurationWrite)
    }

    #[cfg(not(test))]
    pub(super) fn codex_bundled_catalog(&self) -> Result<serde_json::Value, AgentError> {
        let search_paths = cli_paths(&self.home, self.tool_env);
        let executable = find_cli_in_paths(Agent::Codex, &search_paths).ok_or_else(|| {
            AgentError::MetadataUnavailable(
                "Codex CLI was not found; install or update Codex before connecting".to_string(),
            )
        })?;
        codex_metadata(command_output(
            &executable,
            &["debug", "models", "--bundled"],
            &search_paths,
        ))
    }

    #[cfg(test)]
    pub(super) fn codex_bundled_catalog(&self) -> Result<serde_json::Value, AgentError> {
        Ok(serde_json::json!({
            "models": [{
                "slug": "gpt-test",
                "display_name": "GPT Test",
                "description": "Bundled test model",
                "model_messages": { "instructions_template": "Complete official Codex instructions" },
                "base_instructions": "Complete official Codex instructions",
                "supported_in_api": true,
                "visibility": "list",
                "shell_type": "unified_exec",
                "tool_mode": "code_mode_only",
                "apply_patch_tool_type": "freeform",
                "multi_agent_version": "v2",
                "web_search_tool_type": "text_and_image",
                "truncation_policy": { "mode": "tokens", "limit": 10_000 },
                "input_modalities": ["text"]
            }]
        }))
    }
}
