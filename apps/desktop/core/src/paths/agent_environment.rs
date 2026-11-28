use std::{collections::BTreeMap, env, ffi::OsString};

use crate::agents::Agent;

use super::XDG_CONFIG_HOME;

pub const CODEX_HOME: &str = "CODEX_HOME";
pub const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
pub const OPENCODE_CONFIG: &str = "OPENCODE_CONFIG";
pub const PI_CODING_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
pub const HERMES_HOME: &str = "HERMES_HOME";
pub const OPENCODE_CONFIG_DIR: &str = "OPENCODE_CONFIG_DIR";
pub const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";
pub const OPENCLAW_CONFIG_PATH: &str = "OPENCLAW_CONFIG_PATH";
pub const OPENCLAW_STATE_DIR: &str = "OPENCLAW_STATE_DIR";
pub const OPENCLAW_HOME: &str = "OPENCLAW_HOME";
pub const OPENCLAW_PROFILE: &str = "OPENCLAW_PROFILE";
pub const OPENCLAW_NIX_MODE: &str = "OPENCLAW_NIX_MODE";
pub const PI_CONFIG_DIR: &str = "PI_CONFIG_DIR";
pub const OMP_PROFILE: &str = "OMP_PROFILE";
pub const PI_PROFILE: &str = "PI_PROFILE";
pub const DSH_HOME: &str = "DSH_HOME";
pub const LOCALAPPDATA: &str = "LOCALAPPDATA";
pub const PROGRAM_DATA: &str = "ProgramData";

pub const DSH_TOKEN_REF: &str = "PRIVATE_AI_PROXY_DSH_TOKEN";

pub const PATH: &str = "PATH";

/// Location selectors in native discovery order.
pub fn config_path_sources(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Codex => &[CODEX_HOME],
        Agent::ClaudeCode => &[CLAUDE_CONFIG_DIR],
        Agent::OpenCode => &[OPENCODE_CONFIG, XDG_CONFIG_HOME],
        Agent::Pi => &[PI_CODING_AGENT_DIR],
        Agent::OhMyPi => &[PI_CODING_AGENT_DIR, PI_CONFIG_DIR],
        Agent::Hermes if cfg!(windows) => &[HERMES_HOME, LOCALAPPDATA],
        Agent::Hermes => &[HERMES_HOME],
        Agent::OpenClaw => &[OPENCLAW_CONFIG_PATH, OPENCLAW_STATE_DIR, OPENCLAW_HOME],
        Agent::Dsh => &[DSH_HOME],
    }
}

/// Other variables that affect how the agent uses its configuration.
pub fn validation_sources(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::OpenCode if cfg!(windows) => {
            &[OPENCODE_CONFIG_DIR, OPENCODE_CONFIG_CONTENT, PROGRAM_DATA]
        }
        Agent::OpenCode => &[OPENCODE_CONFIG_DIR, OPENCODE_CONFIG_CONTENT],
        Agent::OhMyPi => &[OMP_PROFILE, PI_PROFILE],
        Agent::OpenClaw => &[OPENCLAW_PROFILE, OPENCLAW_NIX_MODE],
        Agent::Dsh => &[DSH_TOKEN_REF],
        _ => &[],
    }
}

pub fn sensitive(name: &str) -> bool {
    matches!(name, OPENCODE_CONFIG_CONTENT | DSH_TOKEN_REF)
}

pub fn override_sources(agent: Agent) -> impl Iterator<Item = &'static str> {
    config_path_sources(agent)
        .iter()
        .chain(validation_sources(agent))
        .copied()
}

pub fn forwarded_sources(agent: Agent) -> impl Iterator<Item = &'static str> {
    std::iter::once(PATH).chain(override_sources(agent).filter(|name| !sensitive(name)))
}

/// An explicit snapshot also represents variables absent from the client's shell.
pub type AgentEnvironment = BTreeMap<String, String>;

pub(crate) type ClientEnvironment = BTreeMap<String, OsString>;

pub(crate) fn capture(mut read: impl FnMut(&'static str) -> Option<OsString>) -> ClientEnvironment {
    std::iter::once(PATH)
        .chain(Agent::ALL.into_iter().flat_map(override_sources))
        .filter_map(|name| {
            let value = read(name)?;
            Some((
                name.to_string(),
                if sensitive(name) {
                    OsString::new()
                } else {
                    value
                },
            ))
        })
        .collect()
}

pub(crate) fn forwarded(
    agent: Agent,
    environment: &ClientEnvironment,
) -> Result<AgentEnvironment, String> {
    forwarded_sources(agent)
        .filter_map(|name| environment.get(name).map(|value| (name, value)))
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.to_string(), value.to_string()))
                .ok_or_else(|| format!("{name} must be valid Unicode to connect {}.", agent.name()))
        })
        .collect()
}

/// Only variables that still need to be set outside the agent connection
/// can prevent automatic supervision.
pub(crate) fn supervision_overrides() -> Vec<&'static str> {
    Agent::ALL
        .into_iter()
        .flat_map(override_sources)
        .filter(|name| sensitive(name) && env::var_os(name).is_some())
        .collect()
}
