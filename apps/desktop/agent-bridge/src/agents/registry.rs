use super::*;

pub(super) use desktop_core::paths::XDG_CONFIG_HOME;

pub(super) const CODEX_HOME: &str = "CODEX_HOME";
pub(super) const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
pub(super) const OPENCODE_CONFIG: &str = "OPENCODE_CONFIG";
pub(super) const PI_CODING_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
pub(super) const HERMES_HOME: &str = "HERMES_HOME";
pub(super) const OPENCODE_CONFIG_DIR: &str = "OPENCODE_CONFIG_DIR";
pub(super) const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";
pub(super) const OPENCLAW_CONFIG_PATH: &str = "OPENCLAW_CONFIG_PATH";
pub(super) const OPENCLAW_STATE_DIR: &str = "OPENCLAW_STATE_DIR";
pub(super) const OPENCLAW_HOME: &str = "OPENCLAW_HOME";
pub(super) const OPENCLAW_PROFILE: &str = "OPENCLAW_PROFILE";
pub(super) const OPENCLAW_NIX_MODE: &str = "OPENCLAW_NIX_MODE";
pub(super) const PI_CONFIG_DIR: &str = "PI_CONFIG_DIR";
pub(super) const OMP_PROFILE: &str = "OMP_PROFILE";
pub(super) const PI_PROFILE: &str = "PI_PROFILE";
pub(super) const DSH_HOME: &str = "DSH_HOME";
pub(super) const LOCALAPPDATA: &str = "LOCALAPPDATA";
pub(super) const PROGRAM_DATA: &str = "ProgramData";

/// Agent paths and validation depend on these process environment overrides.
pub fn environment_overridden() -> bool {
    desktop_core::paths::xdg_home_overridden(XDG_CONFIG_HOME)
        || (cfg!(windows)
            && [LOCALAPPDATA, PROGRAM_DATA]
                .into_iter()
                .any(|name| env::var_os(name).is_some()))
        || [
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
            dsh::TOKEN_REF,
        ]
        .into_iter()
        .any(|name| env::var_os(name).is_some())
}

/// How the bridge reaches and configures each agent.
pub(crate) trait AgentIntegration {
    fn surface(self) -> Surface;
    fn format(self) -> Format;
    /// The live user-level config file. With `tool_env` each tool's own
    /// location override is honored.
    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf;
    fn config_path_source(self, tool_env: bool) -> Option<&'static str>;
    fn note(self, connect: bool) -> &'static str;
}

impl AgentIntegration for Agent {
    fn surface(self) -> Surface {
        match self {
            Self::Codex => Surface::Responses,
            Self::ClaudeCode => Surface::Messages,
            Self::OpenCode
            | Self::Pi
            | Self::Hermes
            | Self::OpenClaw
            | Self::OhMyPi
            | Self::Dsh => Surface::ChatCompletions,
        }
    }

    fn format(self) -> Format {
        match self {
            Agent::Codex => Format::Toml,
            Agent::ClaudeCode | Agent::OpenCode | Agent::Pi => Format::Json,
            Agent::Hermes => Format::Yaml,
            Agent::OpenClaw => Format::Json5,
            Agent::OhMyPi => Format::Yaml,
            Agent::Dsh => Format::YamlList,
        }
    }

    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf {
        let override_dir = |name: &str| tool_env.then(|| env_path(name)).flatten();
        match self {
            Agent::OpenClaw => openclaw::config_path(home, tool_env),
            Agent::OhMyPi => oh_my_pi::config_path(home, tool_env),
            Agent::Dsh => dsh::config_path(home, tool_env),
            Agent::Codex => override_dir(CODEX_HOME)
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
            Agent::ClaudeCode => override_dir(CLAUDE_CONFIG_DIR)
                .unwrap_or_else(|| home.join(".claude"))
                .join("settings.json"),
            Agent::OpenCode => override_dir(OPENCODE_CONFIG).unwrap_or_else(|| {
                override_dir(XDG_CONFIG_HOME)
                    .unwrap_or_else(|| home.join(".config"))
                    .join("opencode")
                    .join("opencode.json")
            }),
            Agent::Pi => override_dir(PI_CODING_AGENT_DIR)
                .map(|path| match path.strip_prefix("~") {
                    Ok(suffix) => home.join(suffix),
                    Err(_) => path,
                })
                .unwrap_or_else(|| home.join(".pi").join("agent"))
                .join("models.json"),
            Agent::Hermes => hermes_config_path(home, tool_env),
        }
    }

    fn config_path_source(self, tool_env: bool) -> Option<&'static str> {
        if !tool_env {
            return None;
        }
        let vars = match self {
            Agent::Codex => &[CODEX_HOME][..],
            Agent::ClaudeCode => &[CLAUDE_CONFIG_DIR][..],
            Agent::OpenCode => &[OPENCODE_CONFIG, XDG_CONFIG_HOME][..],
            Agent::Pi | Agent::OhMyPi => &[PI_CODING_AGENT_DIR, PI_CONFIG_DIR][..],
            Agent::Hermes => &[HERMES_HOME][..],
            Agent::OpenClaw => &[OPENCLAW_CONFIG_PATH, OPENCLAW_STATE_DIR, OPENCLAW_HOME][..],
            Agent::Dsh => &[DSH_HOME][..],
        };
        vars.iter().find(|name| env_path(name).is_some()).copied()
    }

    fn note(self, connect: bool) -> &'static str {
        if !connect {
            return "Proxy provider definitions are retained. Default routing and credentials taken \
                    over at connect are restored; edits made \
                    since are left in place. The agent's local token is revoked.";
        }
        match self {
            Agent::OpenClaw => "OpenClaw uses a native-host provider and an executable SecretRef for its local token. Restart OpenClaw after applying.",
            Agent::Dsh => "DeepSeek Harness applies $DSH_HOME/cordis.patch.yml (~/.dsh by default) to its web, headless and acp profiles and the dsh desktop app, and reads the token from dsh's own credential store. Change the default model with `pap agents connect dsh --model <id>`. This app reads DSH_HOME only from its own environment, never in the Mac App Store build, so a DSH_HOME exported in your shell points dsh at a folder this connection does not cover. Also not protected: runs started with --patch, a PRIVATE_AI_PROXY_DSH_TOKEN exported in your shell on older dsh releases, and the sdk and sdk-minimal profiles for embedding dsh, which choose their provider in code. Only new sessions go through the proxy: existing sessions, resumed acp sessions included, keep the provider and model they were recorded with. dsh web and the dsh desktop app apply the change at once; restart running dsh headless or acp processes. DeepSeek web search is turned off while connected; search providers you installed yourself, such as Exa or Perplexity, still use your own keys.",
            Agent::OhMyPi => "Oh My Pi uses its own local token and native models YAML. Connect selects a compatible default; Disconnect restores the previous selection while keeping the provider. Restart omp after applying. Named profiles and conflicting overrides are not modified.",
            Agent::Codex if super::codex_service::AVAILABLE => {
                "Codex will use its official custom model provider with the Responses API, the \
                 selected model from the verified catalog, command-backed authentication, and \
                 the app-owned model catalog. The app does not check the Codex version. Codex keeps a background \
                 service with the previous settings. The app offers to stop it; otherwise run \
                 \"codex app-server daemon restart\" in your terminal. Either stops running \
                 Codex sessions. Then quit and reopen the Codex app."
            }
            Agent::Codex => {
                "Codex will use its official custom model provider with the Responses API, the \
                 selected model from the verified catalog, command-backed authentication, and \
                 the app-owned model catalog. The app does not check the Codex version. Codex keeps a background \
                 server, so after applying run \"codex app-server daemon restart\" (this stops \
                 running Codex sessions), then quit and reopen the Codex app."
            }
            Agent::OpenCode => {
                "OpenCode will use an app-owned provider catalog generated from the verified \
                 service and a file-backed machine-local token. Restart OpenCode after applying."
            }
            Agent::ClaudeCode => {
                if cfg!(windows) {
                    return "Claude Code uses a verified model picker and apiKeyHelper with a local token. Use 2.1.299+ for upstream fixes that keep gateway tokens out of Anthropic background requests. Nonessential traffic and auto-updates are disabled while connected. Responses stream immediately; receipt audits run after delivery. Windows shell compatibility has not been verified with the real Claude CLI. Shell credentials and managed settings may override this projection. Anthropic does not officially support non-Claude models.";
                }
                "Claude Code will authenticate through apiKeyHelper with a machine-local token \
                 and use a Messages-compatible model picker generated from the verified service. Use Claude Code 2.1.299+ for upstream fixes that keep gateway tokens out of Anthropic background requests. Nonessential traffic and auto-updates are disabled while connected. Restart Claude Code after applying. Responses stream immediately; receipt audits run after delivery. Credentials set in this settings file are taken over and restored on \
                 disconnect; a token exported in your shell would still take priority, so unset \
                 ANTHROPIC_AUTH_TOKEN and ANTHROPIC_API_KEY there. A claude.ai login is not \
                 used through the gateway. Anthropic does not officially support non-Claude models."
            }
            Agent::Pi => {
                "Pi will load an app-owned provider catalog generated from the verified service. Choose a model in Pi after applying. Restart Pi after applying."
            }
            Agent::Hermes => {
                "Hermes uses a machine-local token command. Start a new session without --api-key or --base-url overrides; existing native credentials and fallbacks are never erased."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_ignores_default_xdg_config_and_checks_windows_paths_only_on_windows() {
        const CHILD: &str = "PAP_TEST_AGENT_ENVIRONMENT";
        if let Ok(case) = env::var(CHILD) {
            assert_eq!(environment_overridden(), case == "custom" || cfg!(windows));
            return;
        }
        let home = tempfile::tempdir().unwrap();
        for (case, config) in [
            ("default", home.path().join(".config")),
            ("custom", home.path().join("config-override")),
        ] {
            let output = std::process::Command::new(env::current_exe().unwrap())
                .env_clear()
                .env(CHILD, case)
                .env("HOME", home.path())
                .env(XDG_CONFIG_HOME, config)
                .env(LOCALAPPDATA, home.path())
                .env(PROGRAM_DATA, home.path())
                .args(["--exact", "agents::registry::tests::enrollment_ignores_default_xdg_config_and_checks_windows_paths_only_on_windows"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}
