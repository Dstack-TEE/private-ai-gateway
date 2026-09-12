use super::*;

impl Projector {
    /// Apply the previewed edits under the cross-process config lock.
    pub fn apply(
        &self,
        agent: Agent,
        connect: bool,
        revision_seen: &str,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<AgentStatus, String> {
        lock::with_apply_lock(&self.data_dir, || {
            self.maintain_store_permissions()?;
            let mut store = self.load_store()?;
            let path = self.action_path(agent, store.get(agent.id()), connect);
            let (text, read_error) = self.config_text_at(agent, &path);
            if revision(
                agent,
                connect,
                path.as_deref().ok(),
                text.as_deref(),
                store.get(agent.id()),
                catalog,
                options,
            ) != revision_seen
            {
                return Err(format!(
                    "The {} config changed since the preview; review the changes again",
                    agent.name()
                ));
            }
            if connect {
                let path = path?;
                if let Some(error) = read_error {
                    return Err(error);
                }
                if catalog.is_some() {
                    self.require_helper()?;
                    self.connect(agent, &mut store, text, &path, catalog, options)
                        .map_err(ConnectFailure::message)?;
                } else {
                    if self.current_record(agent, store.get(agent.id())).is_some() {
                        self.suspend(agent, &mut store)?;
                    }
                    let prior = self.current_record(agent, store.get(agent.id()));
                    let saved_options = if options.default_model.is_none() {
                        prior
                            .map(|record| record.options.clone())
                            .unwrap_or_default()
                    } else {
                        options.clone()
                    };
                    let fields = prior
                        .map(|record| record.fields.clone())
                        .unwrap_or_default();
                    store.insert(
                        agent.id().to_string(),
                        Connection {
                            config_path: Some(path),
                            fields,
                            suspended: true,
                            options: saved_options,
                            ..Connection::default()
                        },
                    );
                    self.save_store(&store)?;
                }
            } else {
                self.disconnect(agent, &mut store)?;
            }
            Ok(self.status(agent, &store, catalog))
        })
    }

    /// Emergency restore: disconnect every recorded agent, whether or not
    /// this version supports it, the endpoint is bound, or the gateway runs.
    /// Every agent's token file is deleted before any manifest or config is
    /// touched — revoking the capability itself is durable, so no later
    /// failure (not even across a restart) can leave an agent authorized.
    /// `Err` means revocation or the tombstone step failed and callers must
    /// keep the in-memory token set empty; per-agent cleanup failures keep
    /// their tombstone for an idempotent retry.
    pub fn disconnect_all(&self) -> Result<Vec<(String, String)>, String> {
        lock::with_apply_lock(&self.data_dir, || {
            self.maintain_store_permissions()?;
            let mut store = self.load_store()?;
            let targets: Vec<Agent> = Agent::ALL
                .iter()
                .copied()
                .filter(|agent| store.contains_key(agent.id()))
                .collect();
            if targets.is_empty() {
                return Ok(Vec::new());
            }
            for agent in &targets {
                self.tokens.revoke(agent.id())?;
            }
            for agent in &targets {
                if let Some(record) = store.get_mut(agent.id()) {
                    record.disabled = true;
                    record.cleanup_pending = true;
                }
            }
            self.save_store(&store)?;
            let mut failures = Vec::new();
            for agent in targets {
                if let Err(error) = self.cleanup(agent, &mut store) {
                    failures.push((agent.id().to_string(), error));
                }
            }
            Ok(failures)
        })
    }

    /// Reconcile saved links with protection. All edits use the same lock and
    /// restoration journal as explicit connect/disconnect operations.
    pub fn reconcile(&self, catalog: Option<&Catalog>) -> Result<Vec<(String, String)>, String> {
        lock::with_apply_lock(&self.data_dir, || {
            let mut store = self.load_store()?;
            let mut failures = Vec::new();
            for agent in Agent::ALL {
                let Some(record) = store.get(agent.id()).cloned() else {
                    continue;
                };
                if record.disconnected() {
                    continue;
                }
                let status = self.status(agent, &store, catalog);
                let result = if record.cleanup_pending {
                    self.cleanup(agent, &mut store)
                        .map_err(ConnectFailure::Unavailable)
                } else if catalog.is_none()
                    || !status.installed
                    || status.error.is_some()
                    || (!record.suspended && !status.authorized)
                {
                    if catalog.is_some()
                        && status.installed
                        && status.error.is_none()
                        && !record.suspended
                        && !status.authorized
                    {
                        if let Some(record) = store.get_mut(agent.id()) {
                            record.attention = status.attention.clone().or_else(|| Some("Configuration changed outside the app; reconnect to apply it again".to_string()));
                        }
                    }
                    self.suspend(agent, &mut store)
                        .map_err(ConnectFailure::Unavailable)
                } else if record.suspended && record.attention.is_none() {
                    self.suspend(agent, &mut store)
                        .map_err(ConnectFailure::Unavailable)
                        .and_then(|()| {
                            self.require_helper()?;
                            let text = self.read_config(agent)?;
                            let path = self
                                .action_path(agent, store.get(agent.id()), true)
                                .map_err(ConnectFailure::Conflict)?;
                            self.connect(agent, &mut store, text, &path, catalog, &record.options)
                        })
                } else if !record.suspended
                    && catalog.is_some_and(|catalog| {
                        record.catalog_revision.as_deref() != Some(catalog.revision.as_str())
                            || (record.attention.is_none()
                                && (record.options.default_model.is_none()
                                    || (matches!(agent, Agent::Pi | Agent::OhMyPi)
                                        && record.selection.is_none())))
                    })
                {
                    (|| {
                        self.require_helper()?;
                        let text = self.read_config(agent)?;
                        let doc = self
                            .parse_config(agent, text.as_deref())
                            .map_err(ConnectFailure::Conflict)?;
                        let options = ConnectOptions {
                            default_model: selected_model(agent, Some(&doc))
                                .or_else(|| record.options.default_model.clone()),
                        };
                        let path = self
                            .action_path(agent, store.get(agent.id()), true)
                            .map_err(ConnectFailure::Conflict)?;
                        self.connect(agent, &mut store, text, &path, catalog, &options)
                    })()
                } else {
                    Ok(())
                };
                if let Err(error) = result {
                    if let ConnectFailure::Conflict(message) = &error {
                        if let Some(record) = store.get_mut(agent.id()) {
                            record.attention = Some(message.clone());
                            record.catalog_revision =
                                catalog.map(|catalog| catalog.revision.clone());
                        }
                        self.save_store(&store)?;
                    }
                    failures.push((agent.id().to_string(), error.message()));
                }
            }
            Ok(failures)
        })
    }

    pub(super) fn suspend(&self, agent: Agent, store: &mut Store) -> Result<(), String> {
        let Some(record) = store.get_mut(agent.id()) else {
            return Ok(());
        };
        if record.restored() {
            return Ok(());
        }
        self.tokens.revoke(agent.id())?;
        record.suspended = true;
        self.save_store(store)?;
        let record = store
            .get(agent.id())
            .cloned()
            .ok_or("Missing agent restore record")?;
        record.validate_recovery()?;
        if record.fields.is_empty() {
            return Ok(());
        }
        let path = record.restore_path()?;
        if let Some(journal) = &record.selection {
            if let Some(selection) =
                selection::restoration(agent, path, journal, self.secrets.as_ref())?
            {
                if !selection.changes.is_empty() {
                    write_atomic(
                        &selection.path,
                        &selection.after,
                        Some(selection.before.as_deref()),
                    )
                    .map_err(|error| format!("Cannot restore native model settings: {error}"))?;
                }
            }
        }
        let text = self.read_config_at(agent, path)?;
        let mut doc = ConfigDoc::parse(agent.format(), text.as_deref().unwrap_or_default())
            .map_err(|reason| {
                format!(
                    "Cannot restore {} at {}: {reason}",
                    agent.name(),
                    path.display()
                )
            })?;
        let default_model =
            selected_model(agent, Some(&doc)).or_else(|| record.options.default_model.clone());
        let edit = if text.is_some() {
            restore(&mut doc, &record, self.secrets.as_ref())?
        } else {
            // A removed config is an uninstall/user action, not a request to
            // recreate fields the gateway previously removed.
            Edit {
                selection: None,
                changes: Vec::new(),
                record: None,
                pending_secrets: Vec::new(),
                consumed_secrets: record
                    .fields
                    .iter()
                    .filter_map(|field| match &field.previous {
                        Some(Previous::Secret { secret_ref }) => Some(secret_ref.clone()),
                        _ => None,
                    })
                    .collect(),
            }
        };
        if !edit.changes.is_empty() {
            write_atomic(path, &doc.render()?, Some(text.as_deref()))
                .map_err(|error| format!("Cannot restore {}: {error}", agent.name()))?;
        }
        for entry in &edit.consumed_secrets {
            self.secrets.delete(entry)?;
        }
        if let Some(record) = store.get_mut(agent.id()) {
            let fields = record.fields.clone();
            record
                .fields
                .retain(|field| retain_provider_field(field, &fields));
            for field in &mut record.fields {
                let inactive = inactive_provider_value(field);
                if doc.get_value(&refs(&field.path)) == inactive {
                    field.value = inactive;
                }
            }
            record.options.default_model = default_model;
            record.selection = None;
        }
        self.save_store(store)
    }

    /// Token, parked secrets, config, and record land together or are rolled
    /// back together.
    pub(super) fn connect(
        &self,
        agent: Agent,
        store: &mut Store,
        text: Option<String>,
        path: &Path,
        catalog: Option<&Catalog>,
        options: &ConnectOptions,
    ) -> Result<(), ConnectFailure> {
        let mut doc = self
            .parse_config(agent, text.as_deref())
            .map_err(ConnectFailure::Conflict)?;
        let options = connection_options(
            agent,
            &doc,
            self.current_record(agent, store.get(agent.id())),
            catalog,
            options,
        );
        if store.get(agent.id()).is_some_and(|record| {
            record.cleanup_pending || (record.disabled && !record.disconnected())
        }) {
            return Err(ConnectFailure::Conflict(format!(
                "{} has a disconnect in progress; finish it before connecting again",
                agent.name()
            )));
        }
        let edit = self
            .edit(agent, true, &mut doc, store, catalog, &options)
            .map_err(ConnectFailure::Conflict)?;
        if agent == Agent::Codex {
            self.sync_codex_catalog(
                catalog.ok_or_else(|| "The verified model list is not available".to_string())?,
            )?;
        }
        let previous_record = store.get(agent.id()).cloned();
        let mut next_record = edit.record.clone().ok_or("Missing connection journal")?;
        next_record.config_path = Some(path.to_path_buf());
        next_record.options = options.clone();
        next_record.catalog_revision = catalog.map(|catalog| catalog.revision.clone());
        let mut guard = Rollback::default();
        let result = (|| -> Result<(), String> {
            // A fresh token on every new connection; a leftover file from an
            // incomplete disconnect is never reused.
            if store
                .get(agent.id())
                .is_some_and(|record| !record.suspended)
            {
                self.tokens.ensure(agent.id())?;
            } else {
                self.tokens.rotate(agent.id())?;
                guard.revoke_token = true;
            }
            for secret in &edit.pending_secrets {
                let previous = self.secrets.get(&secret.entry)?;
                self.secrets.set(&secret.entry, &secret.value)?;
                guard.secrets.push((secret.entry.clone(), previous));
            }
            // Persist recovery before either file changes. An interrupted apply
            // is never authorized and must restore before another connection.
            let mut pending = next_record.clone();
            pending.suspended = true;
            pending.cleanup_pending = true;
            pending.disabled = true;
            store.insert(agent.id().to_string(), pending);
            self.save_store(store)?;
            if !edit.changes.is_empty() {
                write_atomic(path, &doc.render()?, Some(text.as_deref())).map_err(|error| {
                    format!("Cannot write the {} config: {error}", agent.name())
                })?;
                guard.configs.push((path.to_path_buf(), text.clone()));
            }
            if let Some(selection) = &edit.selection {
                if !selection.changes.is_empty() {
                    write_atomic(
                        &selection.path,
                        &selection.after,
                        Some(selection.before.as_deref()),
                    )
                    .map_err(|error| format!("Cannot write native model settings: {error}"))?;
                    guard
                        .configs
                        .push((selection.path.clone(), selection.before.clone()));
                }
            }
            store.insert(agent.id().to_string(), next_record);
            self.save_store(store)
        })();
        if let Err(error) = result {
            let rollback = self.rollback(agent, guard);
            if rollback.is_ok() {
                match previous_record {
                    Some(record) => {
                        store.insert(agent.id().to_string(), record);
                    }
                    None => {
                        store.remove(agent.id());
                    }
                }
                self.save_store(store)?;
            }
            return Err(ConnectFailure::Unavailable(match rollback {
                Ok(()) => format!("{error}; nothing was changed"),
                Err(rollback) => format!("{error}; rolling back also failed: {rollback}"),
            }));
        }
        Ok(())
    }

    /// Disconnect revokes the capability itself first: the token file is
    /// deleted before the record or any config is touched, so whatever fails
    /// afterwards — even the tombstone save — the agent can never be
    /// authorized again, not even across a restart. Cleanup never needs the
    /// old token; a new connection always issues a fresh one.
    pub(super) fn disconnect(&self, agent: Agent, store: &mut Store) -> Result<(), String> {
        let Some(record) = store.get_mut(agent.id()) else {
            return Err(format!("{} is not connected", agent.name()));
        };
        self.tokens.revoke(agent.id())?;
        record.disabled = true;
        record.cleanup_pending = true;
        self.save_store(store)?;
        self.cleanup(agent, store)
    }

    /// Keep the journal on failure so cleanup can be retried without losing
    /// the original settings or parked credentials.
    pub(super) fn cleanup(&self, agent: Agent, store: &mut Store) -> Result<(), String> {
        self.suspend(agent, store)?;
        self.tokens.revoke(agent.id())?;
        if let Some(record) = store.get_mut(agent.id()) {
            if record.fields.is_empty() {
                store.remove(agent.id());
            } else {
                record.disabled = true;
                record.cleanup_pending = false;
                record.attention = None;
            }
        }
        self.save_store(store)
    }

    pub(super) fn rollback(&self, agent: Agent, guard: Rollback) -> Result<(), String> {
        let mut first_error = None;
        for (path, original) in guard.configs.into_iter().rev() {
            let restored = match original {
                Some(original) => write_atomic(&path, &original, None),
                None => fs::remove_file(&path),
            };
            if let Err(error) = restored {
                first_error.get_or_insert(format!("cannot restore the config: {error}"));
            }
        }
        for (entry, previous) in guard.secrets.into_iter().rev() {
            let restored = match previous {
                Some(value) => self.secrets.set(&entry, &value),
                None => self.secrets.delete(&entry),
            };
            if let Err(error) = restored {
                first_error.get_or_insert(error);
            }
        }
        if guard.revoke_token {
            if let Err(error) = self.tokens.revoke(agent.id()) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn save_store(&self, store: &Store) -> Result<(), String> {
        for record in store.values() {
            record.validate_recovery()?;
        }
        let text = serde_json::to_string_pretty(store).map_err(|error| error.to_string())?;
        tokens::create_private_dir(&self.data_dir)
            .map_err(|error| format!("Cannot create the app data directory: {error}"))?;
        write_atomic(&self.store_path(), &text, None)
            .map_err(|error| format!("Cannot save the agent connection record: {error}"))
    }
}
