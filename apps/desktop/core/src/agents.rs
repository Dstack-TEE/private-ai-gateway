//! Supported coding agents and the agent shapes shared by every client. How
//! each agent is detected and configured lives in `agent_bridge::agents`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct AgentStatus {
    pub id: String,
    pub name: String,
    pub config_path: String,
    pub installed: bool,
    /// A connected link, including one suspended until protection resumes.
    pub connected: bool,
    /// A connection record exists (whatever the config now says).
    pub recorded: bool,
    /// The proxy would authorize this agent's token right now: recorded,
    /// enabled, config readable and its routing/authentication still managed.
    pub authorized: bool,
    /// Something the user must act on (removed model, incomplete disconnect).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_action: Option<AgentRepairAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentRepairAction {
    Reconnect,
    Disconnect,
}

/// One config field a connection changes. Sensitive fields never show their
/// values; `None` means absent.
#[derive(Clone, Debug, Deserialize, Serialize, ts_rs::TS)]
pub struct ConfigChange {
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentPreview {
    pub agent: AgentStatus,
    pub connect: bool,
    pub changes: Vec<ConfigChange>,
    pub note: String,
    /// Fingerprint of the inputs the preview was computed from; `apply`
    /// refuses when it no longer matches.
    pub revision: String,
}

/// User choices a connection is projected with.
#[derive(Clone, Debug, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct ConnectOptions {
    /// Optional default selected from the verified catalog. The full model
    /// list is discovered natively or generated from that catalog.
    #[serde(default)]
    pub default_model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Codex,
    ClaudeCode,
    OpenCode,
    Pi,
    Hermes,
    OpenClaw,
    OhMyPi,
}

impl Agent {
    pub const ALL: [Agent; 7] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::Hermes,
        Agent::Pi,
        Agent::OhMyPi,
        Agent::OpenCode,
        Agent::OpenClaw,
    ];

    pub fn from_id(id: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|agent| agent.id() == id)
            .ok_or_else(|| "Unknown agent".to_string())
    }

    pub fn id(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::ClaudeCode => "claude-code",
            Agent::OpenCode => "opencode",
            Agent::Pi => "pi",
            Agent::Hermes => "hermes",
            Agent::OpenClaw => "openclaw",
            Agent::OhMyPi => "oh-my-pi",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::ClaudeCode => "Claude Code",
            Agent::OpenCode => "OpenCode",
            Agent::Pi => "Pi",
            Agent::Hermes => "Hermes Agent",
            Agent::OpenClaw => "OpenClaw",
            Agent::OhMyPi => "Oh My Pi",
        }
    }

    pub fn website(self) -> &'static str {
        match self {
            Agent::Codex => "https://developers.openai.com/codex/cli/",
            Agent::ClaudeCode => "https://code.claude.com",
            Agent::OpenCode => "https://opencode.ai",
            Agent::Pi => "https://pi.dev",
            Agent::Hermes => "https://hermes-agent.nousresearch.com",
            Agent::OpenClaw => "https://openclaw.ai",
            Agent::OhMyPi => "https://omp.sh",
        }
    }
}
