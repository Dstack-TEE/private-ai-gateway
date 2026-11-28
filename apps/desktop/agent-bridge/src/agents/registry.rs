use super::*;

pub(super) use desktop_core::paths::XDG_CONFIG_HOME;

pub(super) use desktop_core::paths::agent_environment::{
    CLAUDE_CONFIG_DIR, CODEX_HOME, DSH_HOME, HERMES_HOME, LOCALAPPDATA, OMP_PROFILE,
    OPENCLAW_CONFIG_PATH, OPENCLAW_HOME, OPENCLAW_NIX_MODE, OPENCLAW_PROFILE, OPENCLAW_STATE_DIR,
    OPENCODE_CONFIG, OPENCODE_CONFIG_CONTENT, OPENCODE_CONFIG_DIR, PI_CODING_AGENT_DIR,
    PI_CONFIG_DIR, PI_PROFILE, PROGRAM_DATA,
};

/// How the bridge reaches and configures each agent.
pub(crate) trait AgentIntegration {
    fn surface(self) -> Surface;
    /// Whether the agent can work with the model: the surface serves it, it
    /// takes the tool definitions every coding agent sends, and the service
    /// has not retired it.
    fn can_use(self, catalog: &Catalog, model: &CatalogModel) -> bool;
    /// The catalog's models the agent can work with.
    fn catalog(self, catalog: &Catalog) -> Catalog;
    fn format(self) -> Format;
    /// The live user-level config file. Uses the connection's environment when available.
    fn config_path(self, home: &Path, tool_env: &ToolEnvironment) -> PathBuf;
    fn config_path_source(self, tool_env: &ToolEnvironment) -> Option<&'static str>;
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

    fn can_use(self, catalog: &Catalog, model: &CatalogModel) -> bool {
        model.supports(self.surface())
            && model.supports_tools() != Some(false)
            && !model.deprecated_at(catalog.fetched_at)
    }

    fn catalog(self, catalog: &Catalog) -> Catalog {
        let mut usable = catalog.clone();
        usable.models.retain(|model| self.can_use(catalog, model));
        usable
    }

    fn format(self) -> Format {
        match self {
            Agent::Codex => Format::Toml,
            Agent::ClaudeCode | Agent::Pi => Format::Json,
            // OpenCode v1 and v2 load both .json and .jsonc as JSONC.
            Agent::OpenCode => Format::Jsonc,
            Agent::Hermes => Format::Yaml,
            Agent::OpenClaw => Format::Json5,
            Agent::OhMyPi => Format::Yaml,
            Agent::Dsh => Format::YamlList,
        }
    }

    fn config_path(self, home: &Path, tool_env: &ToolEnvironment) -> PathBuf {
        let override_dir = |name: &str| tool_env.path(name);
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
            Agent::Pi => pi_agent_dir(home, tool_env).join("models.json"),
            Agent::Hermes => hermes_config_path(home, tool_env),
        }
    }

    fn config_path_source(self, tool_env: &ToolEnvironment) -> Option<&'static str> {
        super::config_path_sources(self)
            .iter()
            .find(|name| tool_env.path(name).is_some())
            .copied()
    }

    fn note(self, connect: bool) -> &'static str {
        if !connect {
            return "Proxy provider definitions are retained. Default routing and credentials taken \
                    over at connect are restored; edits made \
                    since are left in place. The agent's local token is revoked.";
        }
        match self {
            Agent::OpenClaw => "OpenClaw uses a native-host provider and an executable SecretRef for its local token. Restart OpenClaw after applying.",
            Agent::Dsh => "DeepSeek Harness applies $DSH_HOME/cordis.patch.yml (~/.dsh by default) to its web, headless and acp profiles and the dsh desktop app, and reads the token from dsh's own credential store. Change the default model with `pap agents connect dsh --model <id>`. The CLI and desktop app use and save the client's DSH_HOME when connecting. The Mac App Store build uses its authorized Home folder. Also not protected: runs started with --patch, a PRIVATE_AI_PROXY_DSH_TOKEN exported in your shell on older dsh releases, and the sdk and sdk-minimal profiles for embedding dsh, which choose their provider in code. Only new sessions go through the proxy: existing sessions, resumed acp sessions included, keep the provider and model they were recorded with. dsh web and the dsh desktop app apply the change at once; restart running dsh headless or acp processes. DeepSeek web search is turned off while connected; search providers you installed yourself, such as Exa or Perplexity, still use your own keys.",
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

pub(super) fn pi_agent_dir(home: &Path, tool_env: &ToolEnvironment) -> PathBuf {
    tool_env
        .path(PI_CODING_AGENT_DIR)
        .map(|path| match path.strip_prefix("~") {
            Ok(suffix) => home.join(suffix),
            Err(_) => path,
        })
        .unwrap_or_else(|| home.join(".pi").join("agent"))
}
