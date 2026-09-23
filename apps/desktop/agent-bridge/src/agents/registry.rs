use super::*;

/// How the bridge reaches and configures each agent.
pub(crate) trait AgentIntegration {
    fn surface(self) -> Surface;
    /// The official CLI executable name, for install detection on PATH.
    fn cli_names(self) -> &'static [&'static str];
    fn format(self) -> Format;
    /// The live user-level config file. With `tool_env` each tool's own
    /// location override is honored.
    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf;
    fn note(self, connect: bool) -> &'static str;
}

impl AgentIntegration for Agent {
    fn surface(self) -> Surface {
        match self {
            Self::Codex => Surface::Responses,
            Self::ClaudeCode => Surface::Messages,
            Self::OpenCode | Self::Pi | Self::Hermes | Self::OpenClaw | Self::OhMyPi => {
                Surface::ChatCompletions
            }
        }
    }

    fn cli_names(self) -> &'static [&'static str] {
        match self {
            Agent::Codex => &["codex"],
            Agent::ClaudeCode => &["claude"],
            Agent::OpenCode => &["opencode"],
            Agent::Pi => &["pi"],
            Agent::Hermes => &["hermes"],
            Agent::OpenClaw => &["openclaw"],
            Agent::OhMyPi => &["omp"],
        }
    }

    fn format(self) -> Format {
        match self {
            Agent::Codex => Format::Toml,
            Agent::ClaudeCode | Agent::OpenCode | Agent::Pi => Format::Json,
            Agent::Hermes => Format::Yaml,
            Agent::OpenClaw => Format::Json5,
            Agent::OhMyPi => Format::Yaml,
        }
    }

    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf {
        let override_dir = |name: &str| tool_env.then(|| env_path(name)).flatten();
        match self {
            Agent::OpenClaw => openclaw::config_path(home, tool_env),
            Agent::OhMyPi => oh_my_pi::config_path(home, tool_env),
            Agent::Codex => override_dir("CODEX_HOME")
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
            Agent::ClaudeCode => override_dir("CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|| home.join(".claude"))
                .join("settings.json"),
            Agent::OpenCode => override_dir("OPENCODE_CONFIG").unwrap_or_else(|| {
                override_dir("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| home.join(".config"))
                    .join("opencode")
                    .join("opencode.json")
            }),
            Agent::Pi => override_dir("PI_CODING_AGENT_DIR")
                .map(|path| match path.strip_prefix("~") {
                    Ok(suffix) => home.join(suffix),
                    Err(_) => path,
                })
                .unwrap_or_else(|| home.join(".pi").join("agent"))
                .join("models.json"),
            Agent::Hermes => override_dir("HERMES_HOME")
                .and_then(|path| {
                    path.to_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(|| hermes_native_dir(home, tool_env))
                .join("config.yaml"),
        }
    }

    fn note(self, connect: bool) -> &'static str {
        if !connect {
            return "Proxy provider definitions are retained. Default routing and credentials taken \
                    over at connect come back from the system credential store; edits made \
                    since are left in place. The agent's local token is revoked.";
        }
        match self {
            Agent::OpenClaw => "OpenClaw uses a native-host provider and an executable SecretRef for its local gateway token. Restart OpenClaw after applying.",
            Agent::OhMyPi => "Oh My Pi uses its own local token and native models YAML. Connect selects a compatible default; Disconnect restores the previous selection while keeping the provider. Restart omp after applying. Named profiles and conflicting overrides are not modified.",
            Agent::Codex => {
                "Codex will use its official custom model provider with the Responses API, the \
                 selected model from the verified catalog, command-backed authentication, and \
                 the app-owned model catalog. Only the Codex baseline pinned by this app is \
                 supported; other versions are not checked or supported. Restart Codex after applying."
            }
            Agent::OpenCode => {
                "OpenCode will use an app-owned provider catalog generated from the verified \
                 service and a file-backed machine-local token. Restart OpenCode after applying."
            }
            Agent::ClaudeCode => {
                if cfg!(windows) {
                    return "Claude Code 2.1.242+ uses a verified model picker and apiKeyHelper with a local token. Responses stream immediately; receipt audits run after delivery. Windows shell compatibility has not been verified with the real Claude CLI. Shell credentials and managed settings may override this projection. Anthropic does not officially support non-Claude models.";
                }
                "Claude Code will authenticate through apiKeyHelper with a machine-local token \
                 and use a Messages-compatible model picker generated from the verified service (Claude Code 2.1.242+). Restart Claude Code after applying. Responses stream immediately; receipt audits run after delivery. Credentials set in this settings file are taken over and restored on \
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
