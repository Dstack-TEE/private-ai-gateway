//! Compare the shell's agent paths with those of the backend it will use.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use agent_bridge::agents::{
    agent_path_overrides, agent_validation_overrides, config_path_sources, shell_config_path,
    validation_sources,
};
use desktop_core::{
    agents::{Agent, AgentStatus},
    client::{agent_environment_hint, CallError, Client},
    protocol::{rpc, Version},
};

pub(super) fn check(client: &Client, agent: Agent) -> Result<Version, CallError> {
    let version = client.version()?;
    if let Some(message) = client.agent_secret_mismatch(agent, &version) {
        return Err(message.into());
    }
    if version.accepts_agent_environment == Some(true) {
        return Ok(version);
    }
    let supervised = version.supervised == Some(true);
    match ShellEnvironment::read().difference(agent, &version) {
        Some(Difference::Validation(source)) => {
            return Err(validation_mismatch(agent, source, supervised).into());
        }
        Some(Difference::Path(source)) => {
            let statuses = client.call_in_instance(rpc::ListAgents, &version.instance_id)?;
            let status = statuses
                .iter()
                .find(|status| status.id == agent.id())
                .ok_or_else(|| {
                    format!("Cannot check {}'s config path for {source}.", agent.name())
                })?;
            if let Some(message) = path_mismatch(agent, source, status, supervised)? {
                return Err(message.into());
            }
        }
        None => {}
    }
    Ok(version)
}

pub(super) fn warn_if_unreported(version: &Version) {
    if version.agent_path_overrides.is_none() || version.agent_validation_overrides.is_none() {
        eprintln!("warning: This backend does not report all agent overrides. Check the agent's configuration or update the backend.");
    }
}

pub(super) fn warnings(client: &Client, version: &Version) -> Vec<String> {
    if version.accepts_agent_environment == Some(true) {
        let warnings: BTreeMap<_, _> = Agent::ALL
            .into_iter()
            .filter_map(|agent| {
                client
                    .agent_secret_mismatch(agent, version)
                    .map(|warning| (agent.id(), warning))
            })
            .collect();
        if warnings.is_empty() {
            return Vec::new();
        }
        return match client.call_in_instance(rpc::ListAgents, &version.instance_id) {
            Ok(statuses) => statuses
                .iter()
                .filter(|status| status.connected)
                .filter_map(|status| warnings.get(status.id.as_str()).cloned())
                .collect(),
            Err(error) => vec![format!("Cannot check connected agent paths: {error}")],
        };
    }
    let local = ShellEnvironment::read();
    if !Agent::ALL
        .into_iter()
        .any(|agent| local.difference(agent, version).is_some())
    {
        return Vec::new();
    }
    let statuses = match client.call_in_instance(rpc::ListAgents, &version.instance_id) {
        Ok(statuses) => statuses,
        Err(error) => return vec![format!("Cannot check connected agent paths: {error}")],
    };
    let supervised = version.supervised == Some(true);
    statuses
        .iter()
        .filter(|status| status.connected)
        .filter_map(|status| {
            let agent = Agent::from_id(&status.id).ok()?;
            match local.difference(agent, version)? {
                Difference::Path(source) => {
                    path_mismatch(agent, source, status, supervised).unwrap_or_else(Some)
                }
                Difference::Validation(source) => {
                    Some(validation_mismatch(agent, source, supervised))
                }
            }
        })
        .collect()
}

enum Difference {
    Path(&'static str),
    Validation(&'static str),
}

struct ShellEnvironment {
    paths: BTreeMap<String, String>,
    validation: BTreeSet<String>,
}

impl ShellEnvironment {
    fn read() -> Self {
        Self {
            paths: agent_path_overrides(),
            validation: agent_validation_overrides(),
        }
    }

    fn difference(&self, agent: Agent, version: &Version) -> Option<Difference> {
        if let Some(backend) = &version.agent_validation_overrides {
            if let Some(name) = validation_sources(agent)
                .iter()
                .copied()
                .find(|name| backend.contains(*name) != self.validation.contains(*name))
            {
                return Some(Difference::Validation(name));
            }
        }
        let backend = version.agent_path_overrides.as_ref()?;
        changed_path_source(agent, backend, &self.paths).map(Difference::Path)
    }
}

fn changed_path_source(
    agent: Agent,
    backend: &BTreeMap<String, String>,
    local: &BTreeMap<String, String>,
) -> Option<&'static str> {
    config_path_sources(agent).iter().copied().find(|source| {
        let shell = local.get(*source);
        // A lossy match may still represent two different native paths.
        shell != backend.get(*source) || shell.is_some_and(|path| path.contains('\u{fffd}'))
    })
}

fn path_mismatch(
    agent: Agent,
    source: &str,
    status: &AgentStatus,
    supervised: bool,
) -> Result<Option<String>, String> {
    let shell = shell_config_path(agent).map_err(|error| {
        format!(
            "Cannot check {}'s config path for {source}: {error}",
            agent.name()
        )
    })?;
    let backend = Path::new(&status.config_path);
    if shell.is_absolute()
        && backend.is_absolute()
        && (shell == backend
            || canonical_config_path(&shell)
                .is_some_and(|shell| canonical_config_path(backend).as_ref() == Some(&shell)))
    {
        return Ok(None);
    }
    let fix = agent_environment_hint(source, supervised);
    Ok(Some(format!(
        "{source} selects a different config for {}. Your shell's agent reads {shell:?}. PAP uses {backend:?}. {fix}",
        agent.name()
    )))
}

fn canonical_config_path(path: &Path) -> Option<PathBuf> {
    // Connect can create the file or its parents. Resolve any existing symlink
    // before comparing the part of the path that has yet to be created.
    path.ancestors().find_map(|ancestor| {
        let resolved = ancestor.canonicalize().ok()?;
        Some(resolved.join(path.strip_prefix(ancestor).ok()?))
    })
}

fn validation_mismatch(agent: Agent, source: &str, supervised: bool) -> String {
    let fix = agent_environment_hint(source, supervised);
    format!(
        "{source} is set in one environment only. PAP cannot verify that {} uses the same configuration in this shell. {fix}",
        agent.name()
    )
}
