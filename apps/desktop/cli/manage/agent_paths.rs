//! Compare the shell's agent paths with those of the backend it will use.

use std::{collections::BTreeMap, ffi::OsString, path::Path};

use agent_bridge::agents::{agent_path_overrides, config_path_sources, shell_config_path};
use desktop_core::{
    agents::{Agent, AgentStatus},
    client::{CallError, Client},
    protocol::{rpc, Version},
};

pub(super) fn check(client: &Client, agent: Agent) -> Result<Version, CallError> {
    let version = client.version()?;
    let Some(backend) = &version.agent_path_overrides else {
        return Ok(version);
    };
    if let Some(source) = changed_source(agent, backend, &agent_path_overrides()) {
        let statuses = client.call_in_instance(rpc::ListAgents, &version.instance_id)?;
        let status = statuses
            .iter()
            .find(|status| status.id == agent.id())
            .ok_or_else(|| format!("Cannot check {}'s config path for {source}.", agent.name()))?;
        if let Some(message) = mismatch(agent, source, status)? {
            return Err(message.into());
        }
    }
    Ok(version)
}

pub(super) fn warn_if_unreported(version: &Version) {
    if version.agent_path_overrides.is_none() {
        eprintln!("warning: This backend does not report agent path overrides. Check the agent's config path or update the backend.");
    }
}

pub(super) fn warnings(client: &Client, version: &Version) -> Vec<String> {
    let Some(backend) = &version.agent_path_overrides else {
        return Vec::new();
    };
    let local = agent_path_overrides();
    if !Agent::ALL
        .into_iter()
        .any(|agent| changed_source(agent, backend, &local).is_some())
    {
        return Vec::new();
    }
    let statuses = match client.call_in_instance(rpc::ListAgents, &version.instance_id) {
        Ok(statuses) => statuses,
        Err(error) => return vec![format!("Cannot check connected agent paths: {error}")],
    };
    statuses
        .iter()
        .filter(|status| status.connected)
        .filter_map(|status| {
            let agent = Agent::from_id(&status.id).ok()?;
            let source = changed_source(agent, backend, &local)?;
            mismatch(agent, source, status).unwrap_or_else(Some)
        })
        .collect()
}

fn changed_source(
    agent: Agent,
    backend: &BTreeMap<String, OsString>,
    local: &BTreeMap<String, OsString>,
) -> Option<&'static str> {
    let sources = config_path_sources(agent);
    let selected = |values: &BTreeMap<String, OsString>| {
        sources
            .iter()
            .copied()
            .find(|source| values.contains_key(*source))
    };
    let shell_source = selected(local);
    let backend_source = selected(backend);
    if shell_source == backend_source
        && shell_source.is_none_or(|source| local.get(source) == backend.get(source))
    {
        return None;
    }
    shell_source.or(backend_source)
}

fn mismatch(agent: Agent, source: &str, status: &AgentStatus) -> Result<Option<String>, String> {
    let shell = shell_config_path(agent).map_err(|error| {
        format!(
            "Cannot check {}'s config path for {source}: {error}",
            agent.name()
        )
    })?;
    let backend = Path::new(&status.config_path);
    if shell.is_absolute() && shell == backend {
        return Ok(None);
    }
    let fix = if cfg!(target_os = "linux") {
        format!("Match {source} in the systemd user environment (environment.d or systemctl --user set-environment), then run pap service restart. To use the current shell's environment, run pap service disable and retry from this shell.")
    } else if cfg!(target_os = "macos") {
        format!("Match {source} in the launchd environment (launchctl setenv), then run pap service restart. To use the current shell's environment, run pap service disable and retry from this shell.")
    } else {
        format!(
            "Run pap service restart from a shell with the desired {source} setting, then retry."
        )
    };
    Ok(Some(format!(
        "{source} selects a different config for {}. Your shell's agent reads {shell:?}. PAP uses {backend:?}. {fix}",
        agent.name()
    )))
}
