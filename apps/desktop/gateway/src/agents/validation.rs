use super::*;

impl Projector {
    pub(super) fn current_record<'a>(
        &self,
        agent: Agent,
        record: Option<&'a Connection>,
    ) -> Option<&'a Connection> {
        record.filter(|record| {
            !record.disconnected()
                || std::path::absolute(agent.config_path(&self.home, self.tool_env))
                    .is_ok_and(|path| record.config_path == path)
        })
    }

    pub(super) fn action_path(
        &self,
        agent: Agent,
        record: Option<&Connection>,
        connect: bool,
    ) -> Result<PathBuf, String> {
        if !connect {
            if let Some(record) = record.filter(|record| !record.fields.is_empty()) {
                record.validate_recovery()?;
                return record.restore_path().map(Path::to_path_buf);
            }
        }
        if agent == Agent::OhMyPi {
            oh_my_pi::validate_host(&self.home, self.tool_env)?;
        }
        let configured = agent.config_path(&self.home, self.tool_env);
        if !configured.is_absolute() {
            return Err("Set the agent config location to an absolute path; desktop and CLI working directories may differ".to_string());
        }
        let current = std::path::absolute(configured)
            .map_err(|_| "Cannot resolve an absolute agent config path".to_string())?;
        if current.to_str().is_none() {
            return Err("The agent config path must be valid Unicode".to_string());
        }
        if let Some(record) =
            record.filter(|record| !record.disconnected() && !record.fields.is_empty())
        {
            record.validate_recovery()?;
            let previous = record.restore_path()?;
            if previous != current {
                return Err("The agent config location changed. Disconnect to restore the recorded file before connecting at the new location".to_string());
            }
        }
        Ok(current)
    }

    /// Connecting references the bundled helper; an installation without it
    /// cannot issue agent credentials.
    pub(super) fn require_helper(&self) -> Result<(), AgentError> {
        if self.file_credentials
            || executable_metadata(&self.helper_exe).is_some_and(|metadata| metadata.len() > 0)
        {
            Ok(())
        } else {
            Err(AgentError::HelperUnavailable)
        }
    }

    pub(super) fn edit(
        &self,
        agent: Agent,
        connect: bool,
        doc: &mut ConfigDoc,
        store: &Store,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<Edit, AgentError> {
        if !connect {
            let record = store.get(agent.id()).ok_or(AgentError::InvalidState)?;
            let mut edit = restore(doc, record, self.secrets.as_ref())?;
            if let Some(journal) = &record.selection {
                edit.selection = selection::restoration(
                    agent,
                    record
                        .restore_path()
                        .map_err(AgentError::ConfigurationConflict)?,
                    journal,
                    self.secrets.as_ref(),
                )
                .map_err(|_| AgentError::RestorationFailed)?;
                if let Some(selection) = &edit.selection {
                    edit.changes.extend(selection.changes.clone());
                }
            }
            return Ok(edit);
        }
        if catalog.is_none() {
            return Err(AgentError::InvalidState);
        }
        let prior = self.current_record(agent, store.get(agent.id()));
        if agent == Agent::OpenClaw {
            openclaw::validate_host(&self.home, self.tool_env)
                .map_err(AgentError::ConfigurationConflict)?;
            if self.file_credentials {
                Ok(())
            } else {
                openclaw::validate_helper(&self.helper_exe, &self.tokens.path(agent.id()))
            }
            .map_err(|_| AgentError::HelperUnavailable)?;
            openclaw::validate_config(doc, prior).map_err(AgentError::ConfigurationConflict)?;
            openclaw::validate_selection(doc, options)
                .map_err(AgentError::ConfigurationConflict)?;
        }
        let options = connection_options(agent, doc, prior, catalog, options);
        self.validate_native_config(agent, doc, prior, &options, catalog)?;
        let codex_catalog_path = self.codex_catalog_path();
        let inputs = Inputs {
            file_credentials: self.file_credentials,
            endpoint: &self.endpoint,
            helper_exe: &self.helper_exe,
            token_path: &self.tokens.path(agent.id()),
            codex_catalog_path: &codex_catalog_path,
            catalog,
            options: &options,
        };
        let fields = fields(agent, &inputs)?;
        let mut edit =
            project(doc, &fields, prior, agent).map_err(AgentError::ConfigurationConflict)?;
        if let Some(model) = options.default_model.as_deref() {
            let config_path = self
                .action_path(agent, prior, true)
                .map_err(AgentError::ConfigurationConflict)?;
            edit.selection = selection::prepare(
                agent,
                &config_path,
                prior.and_then(|record| record.selection.as_ref()),
                model.trim(),
            )
            .map_err(|_| AgentError::ConfigurationRead)?;
            if let Some(selection) = &edit.selection {
                edit.changes.extend(selection.changes.clone());
                if let Some(record) = &mut edit.record {
                    record.selection = Some(selection.journal.clone());
                }
            }
        }
        if agent == Agent::OpenCode {
            self.check_opencode_merge(doc, fields.iter().any(|field| field.path == ["model"]))
                .map_err(AgentError::ConfigurationConflict)?;
        }
        Ok(edit)
    }

    pub(super) fn validate_native_config(
        &self,
        agent: Agent,
        doc: &ConfigDoc,
        prior: Option<&Connection>,
        options: &ConnectOptions,
        catalog: Option<&Catalog>,
    ) -> Result<(), AgentError> {
        match agent {
            Agent::OhMyPi => oh_my_pi::validate_config(doc, prior)
                .map_err(AgentError::ConfigurationConflict),
            Agent::Codex if doc.contains(&["model_providers", "private_ai_proxy", "aws"]) => {
                Err(AgentError::AuthenticationConflict("Codex's gateway provider has AWS authentication, which conflicts with command authentication. Remove that conflict in Codex; it will not be overwritten".to_string()))
            }
            Agent::Pi => {
                let path = Agent::Pi.config_path(&self.home, self.tool_env).with_file_name("auth.json");
                let auth = read_auth_document(&path)?;
                if auth.get("private-ai-proxy").is_some() {
                    return Err(AgentError::AuthenticationConflict("Pi has a stored credential for private-ai-proxy that takes priority over the helper. Resolve it in Pi before connecting; auth.json is left unchanged".to_string()));
                }
                Ok(())
            }
            Agent::Hermes => {
                let scope = &["providers", "private-ai-proxy"];
                if doc.contains(scope) {
                    let owned = prior.is_some_and(|record| {
                        let fields: Vec<_> = record.fields.iter().filter(|field| field.path.starts_with(&owned(scope))).collect();
                        !fields.is_empty() && fields.iter().all(|field| doc.get_value(&refs(&field.path)) == field.value)
                    });
                    if !owned {
                        return Err(AgentError::ConfigurationConflict("The Hermes private-ai-proxy provider already exists outside this connection; it will not be overwritten".to_string()));
                    }
                }
                if doc.contains(&["providers", "private-ai-proxy", "enabled"])
                    && doc.get_value(&["providers", "private-ai-proxy", "enabled"]) != Some(ConfigValue::Bool(true)) {
                    return Err(AgentError::ConfigurationConflict("The Hermes gateway provider must have enabled: true or omit that field; resolve it in Hermes before connecting".to_string()));
                }
                if doc.contains(&["providers", "private-ai-proxy", "api_mode"])
                    && doc.get_str(&["providers", "private-ai-proxy", "api_mode"]).as_deref() != Some("chat_completions") {
                    return Err(AgentError::ConfigurationConflict("Hermes api_mode overrides the gateway's chat_completions transport; resolve that conflict in Hermes".to_string()));
                }
                for key in ["api_key", "key_env", "api_key_env"] {
                    if doc.contains(&["providers", "private-ai-proxy", key]) || doc.contains(&["model", key]) {
                        return Err(AgentError::AuthenticationConflict("Hermes has an explicit credential source that may override or seed a pool ahead of the helper; remove that conflict in Hermes".to_string()));
                    }
                }
                if doc.contains(&["fallback_model"]) {
                    return Err(AgentError::ConfigurationConflict("Hermes fallback_model is outside this verified connection; disable it in Hermes before connecting. It will not be erased".to_string()));
                }
                let existing = doc.get_str(&["model", "default"]);
                let selected = options.default_model.as_deref().map(str::trim).filter(|s| !s.is_empty())
                    .or(existing.as_deref());
                if selected.is_none_or(|id| id.is_empty() || catalog.is_some_and(|catalog| catalog.get(id).is_none_or(|model| !model.supports_agent(agent.surface())))) {
                    return Err(AgentError::IncompatibleModel);
                }
                let path = Agent::Hermes.config_path(&self.home, self.tool_env);
                let directory = path.parent().ok_or(AgentError::InvalidState)?;
                let native = hermes_native_dir(&self.home, self.tool_env);
                let resolved = directory.canonicalize().unwrap_or_else(|_| directory.to_path_buf());
                let resolved_native = native.canonicalize().unwrap_or_else(|_| native.clone());
                let root = if resolved.starts_with(&resolved_native) { native }
                    else if directory.parent().and_then(Path::file_name).is_some_and(|name| name == "profiles") {
                        directory.parent().and_then(Path::parent).ok_or(AgentError::InvalidState)?.to_path_buf()
                    } else { directory.to_path_buf() };
                // Hermes falls back to the root auth store for named profiles.
                let name = doc.get_str(&["providers", "private-ai-proxy", "name"])
                    .unwrap_or_else(|| PRODUCT_NAME.to_string());
                for path in [directory.join("auth.json"), root.join("auth.json")] {
                    let auth = read_auth_document(&path)?;
                    if let Some(pool) = auth.get("credential_pool") {
                        let pool = pool.as_object().ok_or_else(|| AgentError::InvalidConfiguration("Cannot verify Hermes credential_pool; resolve its shape in Hermes".to_string()))?;
                        for key in ["private-ai-proxy".to_string(), format!("custom:{}", name.trim().to_lowercase().replace(' ', "-"))] {
                            if pool.get(&key).is_some_and(|entries| entries.as_array().is_none_or(|entries| !entries.is_empty())) {
                                return Err(AgentError::AuthenticationConflict("Hermes has a gateway credential pool that takes priority over key_cmd; resolve it in Hermes. Native auth files are left unchanged".to_string()));
                            }
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// OpenCode v1.18.29 deep-merges these process-level sources in order.
    /// Keep the original write/restore path: selecting JSONC instead would
    /// strand old connection journals and our JSON writer would lose comments.
    /// Project/managed/remote sources need CLI context; references stay opaque
    /// here so inspection never reads an API key file or executes anything.
    pub(super) fn check_opencode_merge(
        &self,
        doc: &ConfigDoc,
        owns_model: bool,
    ) -> Result<(), String> {
        let ConfigDoc::Json(projected) = doc else {
            return Err("OpenCode requires a JSON projection".to_string());
        };
        let global = self
            .tool_env
            .then(|| env_path("XDG_CONFIG_HOME"))
            .flatten()
            .unwrap_or_else(|| self.home.join(".config"))
            .join("opencode");
        let target = Agent::OpenCode.config_path(&self.home, self.tool_env);
        let mut paths = vec![
            global.join("config.json"),
            global.join("opencode.json"),
            global.join("opencode.jsonc"),
        ];
        if self.tool_env {
            if let Some(path) = env_path("OPENCODE_CONFIG") {
                paths.push(path);
            }
            if let Some(dir) = env_path("OPENCODE_CONFIG_DIR") {
                paths.extend([dir.join("opencode.json"), dir.join("opencode.jsonc")]);
            }
        }
        let mut merged = serde_json::json!({});
        let mut sources = Vec::new();
        for path in paths {
            let layer = if path == target {
                projected.clone()
            } else {
                let text = match fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(format!(
                        "Cannot verify OpenCode config merge: {} is unreadable. Access is disabled; \
                         fix that file or Disconnect to restore the original config.", path.display(),
                    )),
                };
                parse_jsonc(&text).map_err(|reason| {
                    format!(
                        "Cannot verify OpenCode config merge: {} is {reason}. Access is disabled; \
                     fix that file or Disconnect to restore the original config.",
                        path.display(),
                    )
                })?
            };
            sources.push(path.display().to_string());
            merge_opencode_config(&mut merged, layer);
        }
        if self.tool_env {
            if let Some(text) =
                env::var_os("OPENCODE_CONFIG_CONTENT").filter(|text| !text.is_empty())
            {
                let layer = text
                    .to_str()
                    .ok_or_else(|| "not valid Unicode".to_string())
                    .and_then(parse_jsonc)
                    .map_err(|reason| {
                        format!(
                        "Cannot verify OpenCode config merge: OPENCODE_CONFIG_CONTENT is {reason}. \
                         Access is disabled; fix that override or Disconnect.",
                    )
                    })?;
                sources.push("OPENCODE_CONFIG_CONTENT".to_string());
                merge_opencode_config(&mut merged, layer);
            }
        }
        if merged.get("disabled_providers").is_some_and(|values| {
            values.as_array().is_none_or(|values| {
                values
                    .iter()
                    .any(|value| !value.is_string() || value.as_str() == Some("private-ai-proxy"))
            })
        }) || merged.get("enabled_providers").is_some_and(|values| {
            values.as_array().is_none_or(|values| {
                values.iter().any(|value| !value.is_string())
                    || !values
                        .iter()
                        .any(|value| value.as_str() == Some("private-ai-proxy"))
            })
        }) {
            return Err("OpenCode's enabled_providers/disabled_providers exclude the gateway or are invalid. Resolve those filters in OpenCode; they will not be overwritten".to_string());
        }
        for pointer in ["/provider/private-ai-proxy", "/model"] {
            if pointer == "/model" && !owns_model {
                continue;
            }
            if merged.pointer(pointer) != projected.pointer(pointer) {
                return Err(format!(
                    "OpenCode's merged config changes the gateway-owned field {pointer}. \
                     Access is disabled. Review {} without changing unrelated providers, \
                     or Disconnect to restore the original config.",
                    sources.join(", "),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn status(
        &self,
        agent: Agent,
        store: &Store,
        catalog: Option<&Catalog>,
    ) -> AgentStatus {
        let path = agent.config_path(&self.home, self.tool_env);
        let installed = cli_installed(agent, &self.home, self.tool_env);
        let record = self.current_record(agent, store.get(agent.id()));
        let mut status = AgentStatus {
            id: agent.id().to_string(),
            name: agent.name().to_string(),
            config_path: path.display().to_string(),
            installed,
            connected: false,
            recorded: record.is_some_and(|record| !record.disconnected()),
            authorized: false,
            attention: None,
            error: None,
            repair_action: None,
        };
        if agent == Agent::OpenClaw && !installed && record.is_none() {
            return status;
        }
        if let Err(error) = self.require_helper() {
            status.error = Some(error.to_string());
            if agent == Agent::OpenClaw {
                status.attention = Some(error.to_string());
                return status;
            }
        }
        if agent == Agent::OpenClaw && (installed || record.is_some()) {
            let valid = openclaw::validate_host(&self.home, self.tool_env).and_then(|()| {
                if self.file_credentials {
                    Ok(())
                } else {
                    openclaw::validate_helper(&self.helper_exe, &self.tokens.path(agent.id()))
                }
            });
            if let Err(attention) = valid {
                status.attention = Some(attention);
                return status;
            }
        }
        let current_path = self.action_path(agent, record, true);
        if let Err(attention) = &current_path {
            if let Some(path) = record.map(|record| &record.config_path) {
                status.config_path = path.display().to_string();
            }
            status.attention = Some(attention.clone());
            return status;
        }
        // A broken config is reported, never hidden behind "not connected";
        // the record, the attention line, and Disconnect all stay available.
        let (text, read_error) = self.config_text_at(agent, &current_path);
        let doc = match read_error {
            Some(error) => {
                status.error = Some(error);
                None
            }
            None => match self.parse_config(agent, text.as_deref()) {
                Ok(doc) => Some(doc),
                Err(error) => {
                    status.error = Some(error.to_string());
                    None
                }
            },
        };
        let Some(record) = record else {
            return status;
        };
        if record.disconnected() {
            status.recorded = false;
            return status;
        }
        if record.suspended && !record.cleanup_pending {
            status.connected = true;
            status.attention = record.attention.clone().or_else(|| {
                (!installed).then(|| {
                    "CLI not found; configuration stays restored until the agent is available"
                        .to_string()
                })
            });
            if status.attention.is_some() && installed && status.error.is_none() {
                status.repair_action = Some(AgentRepairAction::Reconnect);
            }
            if !record.restored() {
                status.repair_action = Some(AgentRepairAction::Disconnect);
                status.attention = Some(
                    "Configuration restoration is incomplete; retry stopping protection"
                        .to_string(),
                );
            }
            return status;
        }
        let managed = doc.as_ref().is_some_and(|doc| {
            record.fields.iter().all(|field| {
                (agent == Agent::Codex && field.path == ["model"])
                    || doc.get_value(&refs(&field.path)) == field.value
            })
        });
        let token = self.tokens.read(agent.id()).ok().flatten().is_some();
        status.connected = managed && token;
        status.authorized = !record.disabled && managed && token;
        if record.cleanup_pending {
            status.repair_action = Some(AgentRepairAction::Disconnect);
            status.attention = Some(
                "Disconnect did not complete; this agent's access is disabled until Disconnect \
                 is retried"
                    .to_string(),
            );
        } else if record.disabled {
            status.repair_action = Some(AgentRepairAction::Disconnect);
            status.attention =
                Some("This connection is disabled; Disconnect to restore your config".to_string());
        } else if !managed {
            if status.error.is_none() {
                status.repair_action = Some(AgentRepairAction::Reconnect);
            }
            status.attention = Some(
                "The gateway endpoint or authentication settings changed outside the app. \
                 Access is paused. Reconnect this agent in the app, then restart its CLI to reload the configuration."
                    .to_string(),
            );
        } else if !token {
            status.repair_action = Some(AgentRepairAction::Disconnect);
            status.attention = Some(
                "This agent's access is revoked; retry Disconnect to restore its config"
                    .to_string(),
            );
        } else if stale_helper(
            agent,
            record,
            &self.helper_exe,
            self.file_credentials
                .then_some(self.tokens.path(agent.id()).as_path()),
        ) {
            status.repair_action = Some(AgentRepairAction::Reconnect);
            status.connected = false;
            status.authorized = false;
            status.attention = Some(
                "This connection uses an outdated credential helper path or command. Disconnect, \
                 then Connect again to update it."
                    .to_string(),
            );
        } else if status.connected {
            if let (Some(catalog), Some(model)) = (catalog, selected_model(agent, doc.as_ref())) {
                if catalog
                    .get(&model)
                    .is_none_or(|model| !model.supports_agent(agent.surface()))
                {
                    status.attention = Some(format!(
                        "`{model}` is not available from the current profile. Choose an available model in the agent; the connection does not need to be recreated."
                    ));
                }
            }
        }
        if agent == Agent::OpenCode && status.authorized {
            if let Some(doc) = &doc {
                let owns_model = record.fields.iter().any(|field| field.path == ["model"]);
                if let Err(attention) = self.check_opencode_merge(doc, owns_model) {
                    status.connected = false;
                    status.authorized = false;
                    status.attention = Some(attention);
                }
            }
        }
        if status.authorized {
            if let Some(doc) = &doc {
                if let Err(attention) =
                    self.validate_native_config(agent, doc, Some(record), &record.options, catalog)
                {
                    status.connected = false;
                    status.authorized = false;
                    status.attention = Some(attention.to_string());
                }
            }
        }
        if agent == Agent::OpenClaw && status.authorized {
            let result = doc
                .as_ref()
                .ok_or_else(|| "OpenClaw config is unavailable".to_string())
                .and_then(|doc| openclaw::validate_config(doc, Some(record)));
            let stale = openclaw::stale_credentials(
                record,
                &self.helper_exe,
                &self.tokens.path(agent.id()),
                self.file_credentials,
            );
            if result.is_err() || stale {
                status.connected = false;
                status.authorized = false;
                status.attention = Some(result.err().unwrap_or_else(|| {
                    "The OpenClaw helper changed; Disconnect then Connect again".to_string()
                }));
            }
        }
        status
    }

    /// Lenient read for flows that must survive a broken config (status,
    /// revisions, disconnect): the error is carried, never thrown.
    pub(super) fn config_text_at(
        &self,
        agent: Agent,
        path: &Result<PathBuf, String>,
    ) -> (Option<String>, Option<String>) {
        match path {
            Ok(path) => match self.read_config_at(agent, path) {
                Ok(text) => (text, None),
                Err(error) => (None, Some(error.to_string())),
            },
            Err(error) => (None, Some(error.clone())),
        }
    }

    /// The config text, or `None` when the file does not exist yet.
    pub(super) fn read_config(&self, agent: Agent) -> Result<Option<String>, AgentError> {
        let path = self
            .action_path(agent, None, true)
            .map_err(AgentError::ConfigurationConflict)?;
        self.read_config_at(agent, &path)
    }

    pub(super) fn read_config_at(
        &self,
        _agent: Agent,
        path: &Path,
    ) -> Result<Option<String>, AgentError> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(AgentError::ConfigurationRead),
        }
    }

    pub(super) fn parse_config(
        &self,
        agent: Agent,
        text: Option<&str>,
    ) -> Result<ConfigDoc, AgentError> {
        ConfigDoc::parse(agent.format(), text.unwrap_or_default())
            .map_err(|reason| AgentError::InvalidConfiguration(self.parse_error(agent, &reason)))
    }

    pub(super) fn parse_error(&self, agent: Agent, reason: &str) -> String {
        format!(
            "The {} config at {} is {reason}; fix it before connecting",
            agent.name(),
            agent.config_path(&self.home, self.tool_env).display()
        )
    }
}
