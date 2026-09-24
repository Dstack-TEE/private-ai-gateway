use super::*;

impl SessionManager {
    pub(super) fn handle_event(
        self: &Arc<Self>,
        generation: u64,
        event: VerifierEvent,
    ) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }

        let mut load_catalog = false;
        let mut end_session = false;
        let mut retired_task = None;
        match event {
            // Identity in (or rotated): a new epoch; the session stays closed
            // until the catalog read through this identity is in too.
            VerifierEvent::Ready {
                identity,
                remote_url,
                service,
            } => {
                apply_identity_event(&mut runtime.state, &identity);
                runtime.state.remote_url = Some(remote_url);
                runtime.service = Some(service);
                runtime.identity_ready = true;
                runtime.epoch += 1;
                runtime.state.status = "verifying".to_string();
                runtime.state.progress = Some("Reading the verified model list".to_string());
                runtime.state.catalog = None;
                load_catalog = true;
            }
            VerifierEvent::IdentityUpdated { identity } => {
                apply_identity_event(&mut runtime.state, &identity);
                runtime.identity_ready = true;
                runtime.epoch += 1;
                runtime.state.status = "verifying".to_string();
                runtime.state.progress = Some("Reading the verified model list".to_string());
                runtime.state.catalog = None;
                load_catalog = true;
            }
            // Verification lost: one atomic barrier. The epoch moves so a
            // read still in flight can neither publish nor clear this error,
            // and the identity must be reported again before anything opens.
            VerifierEvent::Blocked { code, reason } => {
                let rotating =
                    code.as_deref() == Some("keyset_changed") && runtime.state.status != "blocked";
                end_session = !rotating && !runtime.verification_only;
                runtime.epoch += 1;
                runtime.identity_ready = false;
                runtime.state.status = if rotating { "error" } else { "blocked" }.to_string();
                runtime.state.reconnecting = crate::recovery::connection_intended(&runtime.state);
                if rotating {
                    retired_task = runtime.task.take();
                }
                runtime.state.progress = None;
                runtime.state.catalog = None;
                runtime.state.error = Some(reason);
            }
            VerifierEvent::Fatal { message } => {
                runtime.epoch += 1;
                runtime.identity_ready = false;
                if runtime.state.status != "blocked" {
                    runtime.state.status = "error".to_string();
                }
                runtime.state.reconnecting = crate::recovery::connection_intended(&runtime.state);
                runtime.state.progress = None;
                runtime.state.catalog = None;
                runtime.state.error = Some(message);
            }
            VerifierEvent::Terminated { error } => {
                drop(runtime);
                return self.terminated(generation, error);
            }
        }

        let epoch = runtime.epoch;
        if runtime.state.status != "verified" {
            // Any state other than verified revokes the session at once.
            self.proxy.publish(Session {
                generation,
                epoch,
                session_id: Some(runtime.session_id.clone()),
                service: runtime.service.clone(),
                ..Session::default()
            });
        }
        // Use the same runtime -> usage lock order as start_inner, so this
        // blocked event cannot delete the resume marker of a newer Start.
        if end_session && self.usage.end_session().is_err() {
            crate::diagnostic(format_args!(
                "Could not persist the end of a blocked protection session"
            ));
        }
        drop(runtime);
        if let Some(mut task) = retired_task {
            task.stop()
                .map_err(|_| "Could not stop the previous verifier")?;
        }
        self.publish();
        if load_catalog {
            let manager = Arc::clone(self);
            self.task_runtime.spawn(async move {
                let _ = manager.load_catalog(generation, epoch).await;
            });
        }
        Ok(())
    }
}

/// Record the verifier's identity and checks; the status is decided by the
/// caller once the catalog is in.
pub(super) fn apply_identity_event(state: &mut AppState, event: &IdentityEvent) {
    state.identity = Some(parse_identity(event));
    state.checks = parse_checks(Some(&event.verification));
    state.error = None;
}

pub(super) fn parse_identity(event: &IdentityEvent) -> ServiceIdentity {
    ServiceIdentity {
        tee_type: event.tee_type.clone(),
        trust_level: event.trust_level.clone(),
        keyset_digest: event.keyset_digest.clone(),
        keyset_not_after: event.keyset_not_after,
        tls_spki: event.tls_spki.clone(),
        source: SourceProvenance {
            repo_url: event.source_provenance.repo_url.clone(),
            repo_commit: event.source_provenance.repo_commit.clone(),
            image_digest: event.source_provenance.image_digest.clone(),
        },
        serving: if event.service_capabilities.serving.is_empty() {
            "aggregator".to_string()
        } else {
            event.service_capabilities.serving.clone()
        },
        supported_e2ee_versions: event.service_capabilities.supported_e2ee_versions.clone(),
    }
}

pub(super) fn parse_checks(value: Option<&Value>) -> Vec<VerificationCheck> {
    value
        .and_then(Value::as_object)
        .and_then(|verification| verification.get("checks"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let item = item.as_object()?;
            let status = optional_string(item, "status")?;
            if !matches!(status.as_str(), "pass" | "fail" | "skip" | "info") {
                return None;
            }
            Some(VerificationCheck {
                id: optional_string(item, "id")?,
                section: optional_string(item, "section")?,
                title: optional_string(item, "title")?,
                status,
                detail: optional_string(item, "detail").unwrap_or_default(),
            })
        })
        .collect()
}

pub(super) fn merge_activity(state: &mut AppState, mut incoming: RequestActivity) {
    if incoming.path == "/v1/models" {
        return;
    }
    if let Some(existing) = state
        .activity
        .iter_mut()
        .find(|item| item.id == incoming.id)
    {
        incoming.at = existing.at.min(incoming.at);
        incoming.agent = incoming.agent.or_else(|| existing.agent.clone());
        incoming.model = incoming.model.or_else(|| existing.model.clone());
        incoming.receipt_id = incoming.receipt_id.or_else(|| existing.receipt_id.clone());
        incoming.verified = incoming.verified.or(existing.verified);
        let failed = existing.status != 0 && !(200..300).contains(&existing.status);
        if failed {
            incoming.status = existing.status;
        }
        if incoming.detail.is_empty() || (failed && !existing.detail.is_empty()) {
            incoming.detail = existing.detail.clone();
        }
        incoming.local_policy_applied = incoming
            .local_policy_applied
            .or(existing.local_policy_applied);
        incoming.rewritten = incoming.rewritten.or(existing.rewritten);
        incoming.left_device |= existing.left_device;
        incoming.input_tokens = incoming.input_tokens.or(existing.input_tokens);
        incoming.output_tokens = incoming.output_tokens.or(existing.output_tokens);
        incoming.cache_read_tokens = incoming.cache_read_tokens.or(existing.cache_read_tokens);
        incoming.cache_write_tokens = incoming.cache_write_tokens.or(existing.cache_write_tokens);
        incoming.cost_usd = incoming.cost_usd.or(existing.cost_usd);
        *existing = incoming;
    } else {
        state.activity.insert(0, incoming);
    }
    state
        .activity
        .sort_by_key(|item| std::cmp::Reverse(item.at));
    state.activity.truncate(MAX_ACTIVITY);
}

pub(super) fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
