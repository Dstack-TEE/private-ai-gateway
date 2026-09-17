use super::*;
use crate::sidecar_protocol::{
    IdentityEvent, RequestCompleteEvent, ServeEvent, ServeEventKind, EVENT_SCHEMA_VERSION,
};

impl GatewayManager {
    pub(super) fn handle_stdout(
        self: &Arc<Self>,
        generation: u64,
        bytes: &[u8],
    ) -> Result<(), String> {
        let lines = {
            let mut runtime = self.lock()?;
            if runtime.generation != generation {
                return Ok(());
            }
            if runtime.stdout.len().saturating_add(bytes.len()) > MAX_EVENT_BYTES {
                drop(runtime);
                self.fail(
                    generation,
                    "Verifier emitted an oversized event".to_string(),
                )?;
                return Ok(());
            }
            runtime.stdout.extend_from_slice(bytes);

            let mut lines = Vec::new();
            while let Some(position) = runtime.stdout.iter().position(|byte| *byte == b'\n') {
                let line = runtime.stdout.drain(..=position).collect::<Vec<_>>();
                lines.push(line);
            }
            lines
        };

        for line in lines {
            let line = String::from_utf8(line)
                .map_err(|_| "Verifier emitted non-UTF-8 event data".to_string())?;
            let line = line.trim();
            if !line.is_empty() {
                self.handle_line(generation, line)?;
            }
        }
        Ok(())
    }

    pub(super) fn handle_line(self: &Arc<Self>, generation: u64, line: &str) -> Result<(), String> {
        let event: ServeEvent = serde_json::from_str(line)
            .map_err(|_| "Verifier emitted invalid JSON event data".to_string())?;
        if event.schema_version != EVENT_SCHEMA_VERSION {
            return Err("Verifier emitted an unknown event schema".to_string());
        }

        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }

        let mut load_catalog = false;
        let mut persist = None;
        let mut end_session = false;
        let mut retired_child = None;
        match event.kind {
            // Identity in (or rotated): a new epoch; the session stays closed
            // until the catalog read through this identity is in too.
            ServeEventKind::Ready {
                identity,
                remote_url,
                proxy_url,
                ..
            } => {
                apply_identity_event(&mut runtime.state, &identity);
                runtime.state.remote_url = Some(remote_url);
                runtime.sidecar_url = Some(proxy_url);
                runtime.identity_ready = true;
                runtime.epoch += 1;
                runtime.state.status = "verifying".to_string();
                runtime.state.progress = Some("Reading the verified model list".to_string());
                runtime.state.catalog = None;
                load_catalog = true;
            }
            ServeEventKind::IdentityUpdated { identity } => {
                apply_identity_event(&mut runtime.state, &identity);
                runtime.identity_ready = true;
                runtime.epoch += 1;
                runtime.state.status = "verifying".to_string();
                runtime.state.progress = Some("Reading the verified model list".to_string());
                runtime.state.catalog = None;
                load_catalog = true;
            }
            ServeEventKind::RequestComplete { request } => {
                if request.path != "/v1/models" {
                    persist = Some(apply_request_event(&mut runtime.state, &request)?);
                }
            }
            // Verification lost: one atomic barrier. The epoch moves so a
            // read still in flight can neither publish nor clear this error,
            // and the identity must be reported again before anything opens.
            ServeEventKind::Blocked { code, reason } => {
                let rotating =
                    code.as_deref() == Some("keyset_changed") && runtime.state.status != "blocked";
                end_session = !rotating && !runtime.verification_only;
                runtime.epoch += 1;
                runtime.identity_ready = false;
                runtime.state.status = if rotating { "error" } else { "blocked" }.to_string();
                runtime.state.reconnecting = crate::recovery::connection_intended(&runtime.state);
                if rotating {
                    retired_child = runtime.child.take();
                }
                runtime.state.progress = None;
                runtime.state.catalog = None;
                runtime.state.error = Some(reason);
            }
            ServeEventKind::Fatal { message } => {
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
            ServeEventKind::Unknown => return Ok(()),
        }

        let epoch = runtime.epoch;
        if runtime.state.status != "verified" {
            // Any state other than verified revokes the session at once.
            self.proxy.publish(Session {
                generation,
                epoch,
                session_id: Some(runtime.session_id.clone()),
                base_url: runtime.sidecar_url.clone(),
                ..Session::default()
            });
        }
        // Use the same runtime -> usage lock order as start_inner, so this
        // blocked event cannot delete the resume marker of a newer Start.
        if end_session && self.usage.end_session().is_err() {
            eprintln!("Could not persist the end of a blocked protection session");
        }
        drop(runtime);
        if let Some(mut child) = retired_child {
            child
                .kill()
                .map_err(|_| "Could not stop the previous verifier")?;
        }

        if let Some(activity) = persist {
            let summary = self
                .usage
                .upsert(&activity)
                .and_then(|()| self.usage.session_summary(&activity.session_id));
            let mut runtime = self.lock()?;
            if runtime.generation == generation {
                runtime.state.usage_revision = runtime.state.usage_revision.wrapping_add(1);
                match summary {
                    Ok(summary)
                        if runtime.state.session_id.as_deref()
                            == Some(activity.session_id.as_str()) =>
                    {
                        runtime.state.session_usage = summary;
                    }
                    Err(error) => runtime.state.error = Some(error),
                    _ => {}
                }
            }
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

    pub(super) fn append_diagnostic(&self, generation: u64, bytes: &[u8]) -> Result<(), String> {
        let mut runtime = self.lock()?;
        if runtime.generation != generation {
            return Ok(());
        }
        for byte in bytes {
            if runtime.diagnostic.len() == MAX_DIAGNOSTIC_BYTES {
                runtime.diagnostic.pop_front();
            }
            runtime.diagnostic.push_back(*byte);
        }
        Ok(())
    }
}

pub(super) fn spawn_event_reader(
    manager: Arc<GatewayManager>,
    generation: u64,
    mut receiver: Receiver<SidecarEvent>,
) {
    let task_runtime = manager.task_runtime.clone();
    task_runtime.spawn(async move {
        while let Some(event) = receiver.recv().await {
            let result = match event {
                SidecarEvent::Stdout(bytes) => manager.handle_stdout(generation, &bytes),
                SidecarEvent::Stderr(bytes) => manager.append_diagnostic(generation, &bytes),
                SidecarEvent::Error(error) => {
                    manager.fail(generation, format!("Verifier process error: {error}"))
                }
                SidecarEvent::Terminated => manager.terminated(generation),
            };
            if let Err(error) = result {
                let _ = manager.fail(generation, error);
            }
        }
    });
}

/// Record the sidecar's identity and checks; the status is decided by the
/// caller once the catalog is in.
pub(super) fn apply_identity_event(state: &mut GatewayState, event: &IdentityEvent) {
    state.identity = Some(parse_identity(event));
    state.checks = parse_checks(Some(&event.verification));
    state.error = None;
}

pub(super) fn parse_identity(event: &IdentityEvent) -> GatewayIdentity {
    GatewayIdentity {
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

pub(super) fn apply_request_event(
    state: &mut GatewayState,
    event: &RequestCompleteEvent,
) -> Result<RequestActivity, String> {
    let (request_id, session_id, agent) = parse_request_tag(
        event
            .tag
            .as_deref()
            .ok_or_else(|| "Verifier event is missing tag".to_string())?,
    )?;
    let activity = RequestActivity {
        id: request_id,
        session_id,
        method: event.method.clone(),
        path: event.path.clone(),
        model: None,
        status: event.status,
        streamed: event.streamed,
        receipt_id: event.receipt_id.clone(),
        verified: event.verified,
        detail: event.detail.clone(),
        at: now_secs(),
        agent: Some(agent),
        local_policy_applied: event.local_policy_applied,
        rewritten: event.rewritten,
        left_device: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    };
    merge_activity(state, activity.clone());
    Ok(activity)
}

pub(super) fn merge_activity(state: &mut GatewayState, mut incoming: RequestActivity) {
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

fn parse_request_tag(tag: &str) -> Result<(String, String, String), String> {
    let mut parts = tag.splitn(4, ':');
    if parts.next() == Some("pap") {
        if let (Some(request), Some(session), Some(agent)) =
            (parts.next(), parts.next(), parts.next())
        {
            if !request.is_empty() && !session.is_empty() && !agent.is_empty() {
                return Ok((request.to_string(), session.to_string(), agent.to_string()));
            }
        }
    }
    Err("Verifier emitted an invalid request attribution tag".to_string())
}

pub(super) fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
