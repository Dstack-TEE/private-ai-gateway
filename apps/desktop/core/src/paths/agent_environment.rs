use std::env;

use super::{HOME_OVERRIDE_ENV, XDG_CONFIG_HOME};

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

/// Agent paths and validation depend on these process environment overrides.
pub fn environment_overridden() -> bool {
    !environment_overrides().is_empty()
}

/// Names of environment variables that move an agent's configuration away
/// from the login service's environment.
pub fn environment_overrides() -> Vec<&'static str> {
    let mut names = Vec::new();
    if super::xdg_home_overridden(XDG_CONFIG_HOME) {
        names.push(XDG_CONFIG_HOME);
    }
    if cfg!(windows) {
        names.extend(
            [LOCALAPPDATA, PROGRAM_DATA]
                .into_iter()
                .filter(|name| env::var_os(name).is_some()),
        );
    }
    names.extend(
        [
            CODEX_HOME,
            CLAUDE_CONFIG_DIR,
            OPENCODE_CONFIG,
            PI_CODING_AGENT_DIR,
            HERMES_HOME,
            OPENCODE_CONFIG_DIR,
            OPENCODE_CONFIG_CONTENT,
            OPENCLAW_CONFIG_PATH,
            OPENCLAW_STATE_DIR,
            OPENCLAW_HOME,
            OPENCLAW_PROFILE,
            OPENCLAW_NIX_MODE,
            PI_CONFIG_DIR,
            OMP_PROFILE,
            PI_PROFILE,
            DSH_HOME,
            HOME_OVERRIDE_ENV,
            DSH_TOKEN_REF,
        ]
        .into_iter()
        .filter(|name| env::var_os(name).is_some()),
    );
    names
}
