use super::*;

/// How the bridge reaches and configures each agent.
pub(crate) trait AgentIntegration {
    fn surface(self) -> Surface;
    fn format(self) -> Format;
    /// The live user-level config file. With `tool_env` each tool's own
    /// location override is honored.
    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf;
    /// The folder whose presence means the agent has run; by default the
    /// folder holding the config file.
    fn detection_dir(self, home: &Path, tool_env: bool) -> PathBuf;
    fn note(self, connect: bool) -> &'static str;
    /// The agent reads its credential only as a literal value, so its config
    /// holds the agent's local token itself rather than a command or file.
    fn static_token(self) -> bool;
    /// Owned fields the agent rewrites when the user picks another model in
    /// it; a change there does not pause access.
    fn user_selection(self, path: &[String]) -> bool;
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
            | Self::QwenCode
            | Self::KiloCli
            | Self::ClineCli
            | Self::Crush
            | Self::MimoCode => Surface::ChatCompletions,
        }
    }

    fn format(self) -> Format {
        match self {
            Agent::Codex => Format::Toml,
            Agent::ClaudeCode
            | Agent::OpenCode
            | Agent::Pi
            | Agent::QwenCode
            | Agent::KiloCli
            | Agent::ClineCli
            | Agent::Crush
            | Agent::MimoCode => Format::Json,
            Agent::Hermes => Format::Yaml,
            Agent::OpenClaw => Format::Json5,
            Agent::OhMyPi => Format::Yaml,
        }
    }

    fn config_path(self, home: &Path, tool_env: bool) -> PathBuf {
        let override_dir = |name: &str| tool_env.then(|| env_path(name)).flatten();
        match self {
            Agent::OpenClaw => openclaw::config_path(home, tool_env),
            Agent::ClineCli => cline::config_path(home, tool_env),
            Agent::Crush => crush::config_path(home, tool_env),
            Agent::OhMyPi => oh_my_pi::config_path(home, tool_env),
            Agent::Codex => override_dir("CODEX_HOME")
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
            Agent::ClaudeCode => override_dir("CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|| home.join(".claude"))
                .join("settings.json"),
            Agent::OpenCode | Agent::KiloCli | Agent::MimoCode => opencode_family::layers(self)
                .map(|layers| layers.config_path(home, tool_env))
                .unwrap_or_default(),
            Agent::QwenCode => override_dir("QWEN_HOME")
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| match path.strip_prefix("~") {
                    Ok(suffix) => home.join(suffix),
                    Err(_) => path,
                })
                .unwrap_or_else(|| home.join(".qwen"))
                .join("settings.json"),
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

    fn detection_dir(self, home: &Path, tool_env: bool) -> PathBuf {
        match self {
            Agent::ClineCli => cline::detection_dir(home, tool_env),
            _ => self
                .config_path(home, tool_env)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
        }
    }

    fn note(self, connect: bool) -> &'static str {
        if !connect {
            return "Proxy provider definitions are retained. Default routing and credentials taken \
                    over at connect are restored; edits made \
                    since are left in place. The agent's local token is revoked.";
        }
        match self {
            Agent::OpenClaw => "OpenClaw uses a native-host provider and an executable SecretRef for its local token. Restart OpenClaw after applying.",
            Agent::OhMyPi => "Oh My Pi uses its own local token and native models YAML. Connect selects a compatible default; Disconnect restores the previous selection while keeping the provider. Restart omp after applying. Named profiles and conflicting overrides are not modified.",
            Agent::Codex if super::codex_service::AVAILABLE => {
                "Codex will use its official custom model provider with the Responses API, the \
                 selected model from the verified catalog, command-backed authentication, and \
                 the app-owned model catalog. Only the Codex baseline pinned by this app is \
                 supported; other versions are not checked or supported. Codex keeps a background \
                 service with the previous settings. The app offers to stop it; otherwise run \
                 \"codex app-server daemon restart\" in your terminal. Either stops running \
                 Codex sessions. Then quit and reopen the Codex app."
            }
            Agent::Codex => {
                "Codex will use its official custom model provider with the Responses API, the \
                 selected model from the verified catalog, command-backed authentication, and \
                 the app-owned model catalog. Only the Codex baseline pinned by this app is \
                 supported; other versions are not checked or supported. Codex keeps a background \
                 server, so after applying run \"codex app-server daemon restart\" (this stops \
                 running Codex sessions), then quit and reopen the Codex app."
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
            Agent::MimoCode => {
                "MiMo Code will use an app-owned provider catalog generated from the verified \
                 service and a file-backed machine-local token. Restart MiMo Code after applying."
            }
            Agent::KiloCli => {
                "Kilo CLI will use an app-owned provider catalog generated from the verified \
                 service and a file-backed machine-local token. Restart Kilo after applying."
            }
            Agent::ClineCli => {
                "Cline CLI will use its built-in OpenAI-compatible provider pointed at the verified service, and lists the service's models itself. Cline only accepts that built-in provider, so the one you had there is saved and restored on disconnect. Cline reads keys only as values, so its settings hold this agent's machine-local token, which is revoked on disconnect. Run \"cline hub stop\" or restart Cline after applying. The Cline editor extensions keep their own settings and are not changed."
            }
            Agent::Crush => {
                "Crush will use an app-owned provider catalog generated from the verified service, with a machine-local token command, written to its data file where it also saves the models you pick. Restart Crush after applying. Project crush.json files are not inspected and may override the provider."
            }
            Agent::QwenCode => {
                "Qwen Code will use an app-owned OpenAI-compatible model provider generated from the verified service. Qwen Code reads keys only as values, so its settings hold this agent's machine-local token, which is revoked on disconnect. Restart Qwen Code after applying; a project .qwen/settings.json that defines modelProviders replaces this catalog."
            }
        }
    }

    fn static_token(self) -> bool {
        matches!(self, Agent::QwenCode | Agent::ClineCli)
    }

    fn user_selection(self, path: &[String]) -> bool {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        match self {
            Agent::ClineCli => cline::user_selection(&path),
            Agent::Crush => {
                path == ["models", "large", "provider"] || path == ["models", "large", "model"]
            }
            Agent::Codex => path == ["model"],
            Agent::QwenCode => {
                path == ["model", "name"] || path == ["security", "auth", "selectedType"]
            }
            _ => false,
        }
    }
}
