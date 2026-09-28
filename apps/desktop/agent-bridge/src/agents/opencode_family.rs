//! OpenCode and the CLIs forked from it share its config format: an
//! `@ai-sdk/openai-compatible` provider under `provider`, a `{file:}` key
//! reference, and user layers deep-merged in a fixed order. Only the names of
//! the directories, files and environment variables differ.
use super::*;

pub(super) struct Layers {
    /// Directory name under the XDG config home.
    pub(super) dir: &'static str,
    /// A variable naming a home whose `config` directory replaces it.
    pub(super) home_env: Option<&'static str>,
    /// The file this app writes in that directory.
    pub(super) file: &'static str,
    /// Global files in merge order (later ones win).
    pub(super) global: &'static [&'static str],
    /// Prefix of the `_CONFIG`, `_CONFIG_DIR` and `_CONFIG_CONTENT` overrides.
    pub(super) env: &'static str,
    /// Config directories under Home and the files each contributes, in order.
    pub(super) home_dirs: &'static [&'static str],
    pub(super) dir_files: &'static [&'static str],
}

pub(super) fn layers(agent: Agent) -> Option<Layers> {
    match agent {
        Agent::OpenCode => Some(Layers {
            dir: "opencode",
            home_env: None,
            file: "opencode.json",
            global: &["config.json", "opencode.json", "opencode.jsonc"],
            env: "OPENCODE",
            home_dirs: &[],
            dir_files: &["opencode.json", "opencode.jsonc"],
        }),
        Agent::KiloCli => Some(Layers {
            dir: "kilo",
            home_env: None,
            file: "kilo.json",
            global: &[
                "config.json",
                "kilo.json",
                "kilo.jsonc",
                "opencode.json",
                "opencode.jsonc",
            ],
            env: "KILO",
            home_dirs: &[".kilocode", ".kilo"],
            dir_files: &["kilo.jsonc", "kilo.json", "opencode.jsonc", "opencode.json"],
        }),
        Agent::MimoCode => Some(Layers {
            dir: "mimocode",
            home_env: Some("MIMOCODE_HOME"),
            file: "mimocode.json",
            global: &["config.json", "mimocode.json", "mimocode.jsonc"],
            env: "MIMOCODE",
            home_dirs: &[".mimocode"],
            dir_files: &["mimocode.json", "mimocode.jsonc"],
        }),
        _ => None,
    }
}

impl Layers {
    pub(super) fn var(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.env)
    }

    pub(super) fn global_dir(&self, home: &Path, tool_env: bool) -> PathBuf {
        if let Some(root) = self
            .home_env
            .and_then(|name| tool_env.then(|| env_path(name)).flatten())
        {
            return root.join("config");
        }
        tool_env
            .then(|| env_path("XDG_CONFIG_HOME"))
            .flatten()
            .unwrap_or_else(|| home.join(".config"))
            .join(self.dir)
    }

    pub(super) fn config_path(&self, home: &Path, tool_env: bool) -> PathBuf {
        tool_env
            .then(|| env_path(&self.var("CONFIG")))
            .flatten()
            .unwrap_or_else(|| self.global_dir(home, tool_env).join(self.file))
    }

    /// Every file layer in merge order, whether or not it exists.
    pub(super) fn paths(&self, home: &Path, tool_env: bool) -> Vec<PathBuf> {
        let global = self.global_dir(home, tool_env);
        let mut paths: Vec<PathBuf> = self.global.iter().map(|file| global.join(file)).collect();
        if tool_env {
            paths.extend(env_path(&self.var("CONFIG")));
        }
        let dir_files = |dir: PathBuf| self.dir_files.iter().map(move |file| dir.join(file));
        for dir in self.home_dirs {
            paths.extend(dir_files(home.join(dir)));
        }
        if let Some(dir) = tool_env
            .then(|| env_path(&self.var("CONFIG_DIR")))
            .flatten()
        {
            paths.extend(dir_files(dir));
        }
        paths
    }
}
