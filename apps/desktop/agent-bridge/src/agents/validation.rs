use super::*;

impl Projector {
    fn active_helper(&self) -> &Path {
        if cfg!(windows) {
            &self.helper_exe
        } else {
            self.staged_helper.as_deref().unwrap_or(&self.helper_exe)
        }
    }

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
        if agent == Agent::Dsh {
            dsh::validate_host(&self.home, self.tool_env)?;
        }
        if agent == Agent::Hermes && !hermes_profile_valid(&self.home, self.tool_env) {
            return Err("Hermes active_profile must be a profile name using letters, numbers, '_' or '-'; clear it or choose a valid profile before connecting".to_string());
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
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(format!(
                "Refusing to use symlinked {} config at {}; replace the symlink with a regular file before connecting or disconnecting",
                agent.name(),
                current.display()
            ));
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
        if cfg!(unix)
            && self.staged_helper_required
            && !self.file_credentials
            && self.staged_helper.is_none()
        {
            return Err(AgentError::ConfigurationConflict(
                "The credential helper could not be staged in the app data directory; agent connections are disabled until staging succeeds".to_string(),
            ));
        }
        if self.file_credentials
            || executable_metadata(&self.helper_exe).is_some_and(|metadata| metadata.len() > 0)
        {
            Ok(())
        } else {
            Err(AgentError::HelperUnavailable)
        }
    }

    /// OpenClaw runs the staged helper unless it reads the token file.
    fn openclaw_helper(&self) -> Result<(), String> {
        if self.file_credentials {
            return Ok(());
        }
        openclaw::validate_staged_helper(
            &self.bundled_helper,
            self.active_helper(),
            &self.tokens.path(Agent::OpenClaw.id()),
        )
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
            self.openclaw_helper()
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
            helper_exe: self.active_helper(),
            token_path: &self.tokens.path(agent.id()),
            codex_catalog_path: &codex_catalog_path,
            catalog,
            options: &options,
        };
        let mut fields = fields(agent, &inputs)?;
        if agent == Agent::OpenClaw {
            for (id, model) in openclaw::entry_model_values(doc) {
                fields.push(Field {
                    path: owned(&["agents", "entries", id.as_str(), "model"]),
                    value: Some(model),
                    preview: None,
                    entry: None,
                    exact: false,
                    container: false,
                });
            }
        }
        if agent == Agent::OpenCode && self.opencode_v2_config(doc) {
            if let Some(field) = fields
                .iter_mut()
                .find(|field| field.path == ["provider", "private-ai-proxy"])
            {
                if let Some(ConfigValue::Json(provider)) = field.value.take() {
                    field.path = owned(&["providers", "private-ai-proxy"]);
                    field.value = Some(ConfigValue::Json(opencode_provider_v2(&provider)));
                }
            }
            if prior.is_some_and(|record| {
                record
                    .fields
                    .iter()
                    .any(|field| field.path == owned(&["provider", "private-ai-proxy"]))
            }) {
                fields.push(absent(&["provider", "private-ai-proxy"]));
            }
        }
        let mut edit = project(doc, &fields, prior, agent).map_err(|reason| {
            AgentError::ConfigurationConflict(if agent.format() == Format::YamlList {
                format!(
                    "{} cannot be changed safely: {reason}",
                    agent.config_path(&self.home, self.tool_env).display()
                )
            } else {
                reason
            })
        })?;
        if agent == Agent::Dsh {
            let token = self.tokens.read(agent.id()).ok().flatten();
            let dsh_home = dsh::home_dir(&self.home, self.tool_env);
            if token.is_none_or(|token| !dsh::credential_holds(&dsh_home, &token)) {
                edit.changes.push(dsh::credential_change());
            }
        }
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
            self.check_opencode_merge(doc)
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
            Agent::OhMyPi => {
                oh_my_pi::validate_config(doc, prior)
                    .and_then(|()| {
                        let path = Agent::OhMyPi.config_path(&self.home, self.tool_env);
                        let settings = path.with_file_name("config.yml");
                        oh_my_pi::validate_settings(
                            &settings,
                            catalog,
                        )
                    })
                    .map_err(AgentError::ConfigurationConflict)
            }
            Agent::Dsh => dsh::validate_config(&dsh::home_dir(&self.home, self.tool_env), doc, prior)
                .map_err(AgentError::ConfigurationConflict),
            Agent::Codex if doc.contains(&["model_providers", "private_ai_proxy", "aws"]) => {
                Err(AgentError::AuthenticationConflict("Codex's private_ai_proxy provider has AWS authentication, which conflicts with command authentication. Remove that conflict in Codex; it will not be overwritten".to_string()))
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
                let provider = |key| ["providers", "private-ai-proxy", key];
                if doc.contains(scope) {
                    let owned = prior.is_some_and(|record| {
                        let fields: Vec<_> = record
                            .fields
                            .iter()
                            .filter(|field| {
                                field.path.starts_with(&owned(scope))
                                    && !is_model_selection_field(agent, field)
                            })
                            .collect();
                        !fields.is_empty()
                    });
                    if !owned {
                        return Err(AgentError::ConfigurationConflict("The Hermes private-ai-proxy provider already exists outside this connection; it will not be overwritten".to_string()));
                    }
                }
                if doc.contains(&provider("enabled")) && doc.get_value(&provider("enabled")) != Some(ConfigValue::Bool(true)) {
                    return Err(AgentError::ConfigurationConflict("The Hermes private-ai-proxy provider must have enabled: true or omit that field; resolve it in Hermes before connecting".to_string()));
                }
                if doc.contains(&provider("api_mode")) && doc.get_str(&provider("api_mode")).as_deref() != Some("chat_completions") {
                    return Err(AgentError::ConfigurationConflict("Hermes api_mode overrides the private-ai-proxy provider's chat_completions transport; resolve that conflict in Hermes".to_string()));
                }
                for key in ["api_key", "key_env", "api_key_env"] {
                    if doc.contains(&provider(key)) || doc.contains(&["model", key]) {
                        return Err(AgentError::AuthenticationConflict("Hermes has an explicit credential source that may override or seed a pool ahead of the helper; remove that conflict in Hermes".to_string()));
                    }
                }
                if doc.contains(&["fallback_model"]) {
                    return Err(AgentError::ConfigurationConflict("Hermes fallback_model is outside this verified connection; disable it in Hermes before connecting. It will not be erased".to_string()));
                }
                if doc
                    .get_value(&["fallback_providers"])
                    .is_some_and(|value| !matches!(value, ConfigValue::List(values) if values.is_empty()))
                {
                    return Err(AgentError::ConfigurationConflict("Hermes fallback_providers is outside this verified connection; disable it in Hermes before connecting. It will not be erased".to_string()));
                }
                let existing = doc.get_str(model_selection_paths(Agent::Hermes)[0].path);
                let selected = options.default_model.as_deref().map(str::trim).filter(|s| !s.is_empty())
                    .or(existing.as_deref());
                if selected.is_none_or(|id| id.is_empty() || catalog.is_some_and(|catalog| catalog.get(id).is_none_or(|model| !model.supports(agent.surface())))) {
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
                let name = doc.get_str(&provider("name")).unwrap_or_else(|| PRODUCT_NAME.to_string());
                for path in [directory.join("auth.json"), root.join("auth.json")] {
                    let auth = read_auth_document(&path)?;
                    if let Some(pool) = auth.get("credential_pool") {
                        let pool = pool.as_object().ok_or_else(|| AgentError::InvalidConfiguration("Cannot verify Hermes credential_pool; resolve its shape in Hermes".to_string()))?;
                        for key in ["private-ai-proxy".to_string(), format!("custom:{}", name.trim().to_lowercase().replace(' ', "-"))] {
                            if pool.get(&key).is_some_and(|entries| entries.as_array().is_none_or(|entries| !entries.is_empty())) {
                                return Err(AgentError::AuthenticationConflict("Hermes has a private-ai-proxy credential pool that takes priority over key_cmd; resolve it in Hermes. Native auth files are left unchanged".to_string()));
                            }
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// OpenCode 1.18.29+ deep-merges these process-level sources in order.
    /// OpenCode 2 reads the same V1 files and normalizes them in memory, but
    /// its native `providers` entry overlays the V1 `provider` one we write,
    /// so no source may define `providers.private-ai-proxy`. The projection
    /// keeps writing V1 keys for OpenCode 1 compatibility.
    /// Keep the original write/restore path: selecting JSONC instead would
    /// strand old connection journals, and our JSON writer edits strict JSON only.
    /// Project/managed/remote sources need CLI context; references stay opaque
    /// here so inspection never reads an API key file or executes anything.
    pub(super) fn check_opencode_merge(&self, doc: &ConfigDoc) -> Result<(), String> {
        let Some(projected) = doc.as_json() else {
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
                let unverified = |reason: &str| {
                    format!(
                        "Cannot verify OpenCode config merge: {} is {reason}. Access is disabled; \
                         fix that file or Disconnect to restore the original config.",
                        path.display(),
                    )
                };
                match fs::read_to_string(&path) {
                    Ok(text) => parse_jsonc(&text).map_err(|reason| unverified(&reason))?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(unverified("unreadable")),
                }
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
        let system_dir = if cfg!(target_os = "macos") {
            Some(PathBuf::from("/Library/Application Support/opencode"))
        } else if cfg!(windows) {
            env::var_os("ProgramData")
                .map(PathBuf::from)
                .map(|path| path.join("opencode"))
        } else {
            Some(PathBuf::from("/etc/opencode"))
        };
        if let Some(system_dir) = system_dir {
            for path in [
                system_dir.join("opencode.json"),
                system_dir.join("opencode.jsonc"),
            ] {
                let text = match fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        return Err(format!(
                            "Cannot verify OpenCode config merge: {} is unreadable. Access is disabled; fix that file or Disconnect to restore the original config.",
                            path.display()
                        ));
                    }
                };
                let layer = parse_jsonc(&text).map_err(|reason| {
                    format!(
                        "Cannot verify OpenCode config merge: {} is {reason}. Access is disabled; fix that file or Disconnect to restore the original config.",
                        path.display()
                    )
                })?;
                sources.push(path.display().to_string());
                merge_opencode_config(&mut merged, layer);
            }
        }
        if let Some(policies) = merged.get("policies") {
            let policies = policies.as_array().ok_or_else(|| {
                "OpenCode's merged policies are invalid. Resolve them in OpenCode; they will not be overwritten".to_string()
            })?;
            let mut denied = false;
            for policy in policies {
                let object = policy.as_object().ok_or_else(|| {
                    "OpenCode's merged policies are invalid. Resolve them in OpenCode; they will not be overwritten".to_string()
                })?;
                let matches = object.get("action").and_then(serde_json::Value::as_str)
                    == Some("provider.use")
                    && object
                        .get("resource")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|resource| Self::glob_matches(resource, "private-ai-proxy"));
                if matches {
                    denied =
                        object.get("effect").and_then(serde_json::Value::as_str) == Some("deny");
                }
            }
            if denied {
                return Err("OpenCode's merged policies deny provider.use for private-ai-proxy. Resolve those policies in OpenCode; they will not be overwritten".to_string());
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
            return Err("OpenCode's enabled_providers/disabled_providers exclude private-ai-proxy or are invalid. Resolve those filters in OpenCode; they will not be overwritten".to_string());
        }
        let v2 = projected.pointer("/providers/private-ai-proxy").is_some();
        let provider_pointer = if v2 {
            "/providers/private-ai-proxy"
        } else {
            "/provider/private-ai-proxy"
        };
        let other_pointer = if v2 {
            "/provider/private-ai-proxy"
        } else {
            "/providers/private-ai-proxy"
        };
        for (pointer, expected) in [
            (provider_pointer, projected.pointer(provider_pointer)),
            (other_pointer, None),
        ] {
            if merged.pointer(pointer) != expected {
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

    fn glob_matches(pattern: &str, value: &str) -> bool {
        fn matches(pattern: &[u8], value: &[u8]) -> bool {
            match pattern.split_first() {
                None => value.is_empty(),
                Some((b'*', rest)) => {
                    matches(rest, value)
                        || value
                            .split_first()
                            .is_some_and(|(_, rest)| matches(pattern, rest))
                }
                Some((expected, rest)) => {
                    value.split_first().is_some_and(|(actual, rest_value)| {
                        expected == actual && matches(rest, rest_value)
                    })
                }
            }
        }
        matches(pattern.as_bytes(), value.as_bytes())
    }

    fn opencode_v2_config(&self, doc: &ConfigDoc) -> bool {
        doc.contains(&["providers"])
            || doc.contains(&["policies"])
            || doc
                .get_str(&["$schema"])
                .is_some_and(|schema| schema.contains("/v2/"))
    }

    fn merged_opencode_selector(&self, doc: &ConfigDoc) -> Option<serde_json::Value> {
        let projected = doc.as_json()?;
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
        for path in paths {
            let layer = if path == target {
                projected.clone()
            } else {
                let Ok(text) = fs::read_to_string(path) else {
                    continue;
                };
                parse_jsonc(&text).ok()?
            };
            merge_opencode_config(&mut merged, layer);
        }
        if self.tool_env {
            if let Some(content) = env::var_os("OPENCODE_CONFIG_CONTENT") {
                merge_opencode_config(&mut merged, parse_jsonc(&content.into_string().ok()?).ok()?);
            }
        }
        let system_dir = if cfg!(target_os = "macos") {
            Some(PathBuf::from("/Library/Application Support/opencode"))
        } else if cfg!(windows) {
            env::var_os("ProgramData")
                .map(PathBuf::from)
                .map(|path| path.join("opencode"))
        } else {
            Some(PathBuf::from("/etc/opencode"))
        }?;
        for path in [
            system_dir.join("opencode.json"),
            system_dir.join("opencode.jsonc"),
        ] {
            if let Ok(text) = fs::read_to_string(path) {
                merge_opencode_config(&mut merged, parse_jsonc(&text).ok()?);
            }
        }
        let model = merged.get("model").cloned();
        model
    }

    pub(super) fn status(
        &self,
        agent: Agent,
        store: &Store,
        catalog: Option<&Catalog>,
    ) -> AgentStatus {
        let path = agent.config_path(&self.home, self.tool_env);
        let installed = detected(agent, &self.home, self.tool_env);
        let record = self.current_record(agent, store.get(agent.id()));
        let mut status = AgentStatus {
            id: agent.id().to_string(),
            name: agent.name().to_string(),
            config_path: path.display().to_string(),
            config_path_source: agent.config_path_source(self.tool_env).map(str::to_string),
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
            let valid = openclaw::validate_host(&self.home, self.tool_env)
                .and_then(|()| self.openclaw_helper());
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
        let (text, read_error) = self.config_text_at(&current_path);
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
        if !record.suspended {
            status.attention = record.attention.clone();
        }
        // A suspended connection's configuration is restored: it stays
        // recorded, to resume with protection, but is not connected.
        if record.suspended && !record.cleanup_pending {
            status.attention = record.attention.clone().or_else(|| {
                (!installed).then(|| {
                    "The agent's configuration folder was not found; its configuration stays \
                     restored until the agent is set up again"
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
            record
                .fields
                .iter()
                .all(|field| holds_managed_field(agent, doc, field))
        });
        let token = self.tokens.read(agent.id()).ok().flatten().is_some();
        let helper_stale = stale_helper(
            agent,
            record,
            self.active_helper(),
            self.file_credentials
                .then_some(self.tokens.path(agent.id()).as_path()),
        ) && helper_fields_hold(agent, record, doc.as_ref())
            && record
                .fields
                .iter()
                .filter(|field| !is_helper_field(agent, field))
                .all(|field| {
                    is_any_model_selection_field(field)
                        || holds_managed_field(agent, doc.as_ref().unwrap(), field)
                });
        let legacy_claude =
            agent == Agent::ClaudeCode && legacy_claude_journal(record, doc.as_ref());
        status.connected = managed && token;
        status.authorized = !record.disabled && managed && token && !legacy_claude;
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
        } else if helper_stale {
            status.repair_action = Some(AgentRepairAction::Reconnect);
            status.connected = false;
            status.authorized = false;
            status.attention = Some(
                "The configuration settings changed or this connection uses an outdated credential helper path or command. Disconnect, then Connect again to update it."
                    .to_string(),
            );
        } else if legacy_claude {
            status.repair_action = Some(AgentRepairAction::Reconnect);
            status.attention = Some("Legacy Claude Code provider-switch settings require migration; reconnecting will remove them and preserve their originals.".into());
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
        } else if status.connected {
            if agent == Agent::OpenCode
                && self
                    .merged_opencode_selector(doc.as_ref().unwrap())
                    .as_ref()
                    .and_then(ConfigValue::from_json)
                    .and_then(|value| match value {
                        ConfigValue::Str(value) => value.split('/').next().map(str::to_string),
                        ConfigValue::Json(value) => value
                            .get("providerID")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        _ => None,
                    })
                    .is_some_and(|provider| provider != "private-ai-proxy")
            {
                status.connected = false;
                status.attention = Some(
                    "OpenCode's selected model uses another provider, so this agent is not routed through Private AI Proxy. Choose a private-ai-proxy model to use the connection."
                        .to_string(),
                );
            } else if agent == Agent::OpenClaw
                && openclaw::entry_models(doc.as_ref().unwrap())
                    .iter()
                    .any(|(_, model)| !model.starts_with("private-ai-proxy/"))
            {
                status.connected = false;
                status.attention = Some(
                    "An OpenClaw agent entry selects another provider, so that agent is not routed through Private AI Proxy. Choose a private-ai-proxy model for the entry.".to_string(),
                );
            } else if let (Some(catalog), Some(model)) =
                (catalog, selected_model(agent, doc.as_ref()))
            {
                if catalog
                    .get(&model)
                    .is_none_or(|model| !model.supports(agent.surface()))
                {
                    status.attention = Some(format!(
                        "`{model}` is not available from the current profile. Choose an available model in the agent; the connection does not need to be recreated."
                    ));
                }
            }
        }
        // A managed connection has a readable config. Each check runs only
        // while no earlier one refused.
        if let (true, Some(doc)) = (status.authorized, &doc) {
            let (mut refusal, mut repair) = (None, None);
            if agent == Agent::OpenCode {
                refusal = self.check_opencode_merge(doc).err();
            }
            if refusal.is_none() {
                refusal = self
                    .validate_native_config(agent, doc, Some(record), &record.options, None)
                    .err()
                    .map(|error| error.to_string());
            }
            let dsh_home = dsh::home_dir(&self.home, self.tool_env);
            if refusal.is_none()
                && agent == Agent::Dsh
                && (self.tokens.read(agent.id()).ok().flatten())
                    .is_none_or(|token| !dsh::credential_holds(&dsh_home, &token))
            {
                refusal = Some(format!("dsh's credential store no longer holds this connection's token ({}). Reconnect this agent", dsh::TOKEN_REF));
                repair = Some(AgentRepairAction::Reconnect);
            }
            if refusal.is_none() && agent == Agent::OpenClaw {
                let token = self.tokens.path(agent.id());
                let file = self.file_credentials;
                refusal = openclaw::validate_config(doc, Some(record)).err();
                let staged = self.active_helper();
                if refusal.is_none() && openclaw::stale_credentials(record, staged, &token, file) {
                    refusal =
                        Some("The OpenClaw helper changed; Disconnect, then Connect again".into());
                }
            }
            if let Some(attention) = refusal {
                status.connected = false;
                status.authorized = false;
                status.attention = Some(attention);
                status.repair_action = repair.or(status.repair_action);
            }
        }
        status
    }

    /// Lenient read for flows that must survive a broken config (status,
    /// revisions, disconnect): the error is carried, never thrown.
    pub(super) fn config_text_at(
        &self,
        path: &Result<PathBuf, String>,
    ) -> (Option<String>, Option<String>) {
        match path {
            Ok(path) => match self.read_config_at(path) {
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
        self.read_config_at(&path)
    }

    pub(super) fn read_config_at(&self, path: &Path) -> Result<Option<String>, AgentError> {
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
        let path = agent.config_path(&self.home, self.tool_env);
        self.parse_config_at(agent, &path, text)
    }

    pub(super) fn parse_config_at(
        &self,
        agent: Agent,
        path: &Path,
        text: Option<&str>,
    ) -> Result<ConfigDoc, AgentError> {
        let jsonc = agent == Agent::OpenCode
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonc");
        if jsonc {
            parse_jsonc(text.unwrap_or_default()).map_err(|reason| {
                AgentError::InvalidConfiguration(self.parse_error(agent, &reason))
            })?;
        }
        let format = if jsonc { Format::Json5 } else { agent.format() };
        ConfigDoc::parse(format, text.unwrap_or_default())
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

fn helper_fields_hold(agent: Agent, record: &Connection, doc: Option<&ConfigDoc>) -> bool {
    let Some(doc) = doc else {
        return false;
    };
    record
        .fields
        .iter()
        .filter(|field| is_helper_field(agent, field))
        .all(|field| field.holds(owned_value(doc, field).as_ref()))
}

fn legacy_claude_journal(record: &Connection, doc: Option<&ConfigDoc>) -> bool {
    let Some(doc) = doc else { return false };
    let has_switch = [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_MANTLE",
    ]
    .iter()
    .any(|key| doc.contains(&["env", key]));
    has_switch
        && !record
            .fields
            .iter()
            .any(|field| field.path == owned(&["env", "CLAUDE_CODE_USE_BEDROCK"]))
}

fn is_helper_field(agent: Agent, field: &OwnedField) -> bool {
    match agent {
        Agent::Codex => {
            field.path == owned(&["model_providers", "private_ai_proxy", "auth", "command"])
                || field.path == owned(&["model_providers", "private_ai_proxy", "auth", "args"])
        }
        Agent::ClaudeCode => field.path == owned(&["apiKeyHelper"]),
        Agent::Hermes => field.path == owned(&["providers", "private-ai-proxy", "key_cmd"]),
        Agent::Pi | Agent::OhMyPi => field.path == owned(&["providers", "private-ai-proxy"]),
        Agent::OpenCode | Agent::OpenClaw | Agent::Dsh => false,
    }
}
