use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};
use tokio::sync::OnceCell;

use super::{tool_output, without_windows_verbatim_prefix, Installation, PACKAGE_NAME};

/// Global Node installations can replace their own launcher. Show the command
/// for the manager that owns its wrapper, which may differ from the one on PATH.
#[derive(Debug)]
pub(super) struct NodeInstallation {
    pub(super) manager: Installation,
    prefix: Option<String>,
}

impl NodeInstallation {
    pub(super) async fn current() -> Option<&'static Self> {
        static INSTALLATION: OnceCell<Option<NodeInstallation>> = OnceCell::const_new();
        INSTALLATION
            .get_or_init(|| async {
                let executable = std::env::current_exe().and_then(fs::canonicalize).ok()?;
                let wrapper = wrapper_directory(
                    &executable,
                    std::env::var_os("PAP_NODE_PACKAGE_ROOT").as_deref(),
                )?;
                Self::discover(&wrapper, tool_output).await
            })
            .await
            .as_ref()
    }

    async fn discover(
        wrapper: &Path,
        output: impl AsyncFn(&str, &[&str]) -> Option<String>,
    ) -> Option<Self> {
        let matches = |directory: &Path| {
            fs::canonicalize(directory).is_ok_and(|directory| directory == wrapper)
        };
        let global_wrapper = |root: &Path| matches(&root.join("node_modules").join(PACKAGE_NAME));
        // pnpm's isolated global store can place the resolved package outside
        // its global root. Its dependency listing gives the path to resolve.
        if let Some(path) = output("pnpm", &["list", "--global", "--json"])
            .await
            .as_deref()
            .and_then(pnpm_package_path)
        {
            if matches(&path) {
                return Some(Self {
                    manager: Installation::Pnpm,
                    prefix: None,
                });
            }
        }
        if output("bun", &["pm", "ls", "--global"])
            .await
            .and_then(|output| {
                let (root, _) = output.lines().next()?.rsplit_once(" node_modules (")?;
                Some(global_wrapper(Path::new(root)))
            })
            .unwrap_or_default()
        {
            return Some(Self {
                manager: Installation::Bun,
                prefix: None,
            });
        }
        if output("yarn", &["global", "dir"])
            .await
            .is_some_and(|root| global_wrapper(Path::new(&root)))
        {
            return Some(Self {
                manager: Installation::Yarn,
                prefix: None,
            });
        }
        let modules = wrapper.parent()?;
        if wrapper.file_name()? != PACKAGE_NAME || modules.file_name()? != "node_modules" {
            return None;
        }
        let prefix = npm_prefix(wrapper)?;
        if cfg!(windows)
            && !prefix.join("npm.cmd").is_file()
            && !output("npm", &["prefix", "--global"])
                .await
                .is_some_and(|active| {
                    matches(&Path::new(&active).join("node_modules").join(PACKAGE_NAME))
                })
        {
            return None;
        }
        Some(Self {
            manager: Installation::Npm,
            prefix: Some(without_windows_verbatim_prefix(prefix.to_str()?).into_owned()),
        })
    }

    pub(super) fn words<'a>(&'a self, spec: &'a str) -> Option<Vec<&'a str>> {
        Some(match self.manager {
            Installation::Pnpm => vec!["pnpm", "add", "--global", spec],
            Installation::Bun => vec!["bun", "add", "--global", spec],
            Installation::Yarn => vec!["yarn", "global", "add", spec],
            Installation::Npm => vec![
                "npm",
                "install",
                "--global",
                "--prefix",
                self.prefix.as_deref()?,
                "--include=optional",
                spec,
            ],
            _ => return None,
        })
    }
}

fn wrapper_directory(executable: &Path, launcher_root: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(wrapper) = launcher_root.and_then(|root| fs::canonicalize(root).ok()) {
        return Some(wrapper);
    }
    // A login service runs the native sidecar directly. npm installs the
    // native package either under the wrapper or alongside it when hoisted.
    executable
        .ancestors()
        .filter(|directory| directory.file_name() == Some(OsStr::new("node_modules")))
        .map(|modules| modules.join(PACKAGE_NAME))
        .find(|wrapper| wrapper.join("package.json").is_file())
        .and_then(|wrapper| fs::canonicalize(wrapper).ok())
}

fn pnpm_package_path(output: &str) -> Option<PathBuf> {
    let listings: Vec<serde_json::Value> = serde_json::from_str(output).ok()?;
    listings.iter().find_map(|entry| {
        entry["dependencies"][PACKAGE_NAME]["path"]
            .as_str()
            .map(PathBuf::from)
    })
}

fn npm_prefix(wrapper: &Path) -> Option<&Path> {
    let modules = wrapper.parent()?;
    if wrapper.file_name()? != PACKAGE_NAME || modules.file_name()? != "node_modules" {
        return None;
    }
    let parent = modules.parent()?;
    if cfg!(windows) {
        Some(parent)
    } else if parent.file_name()? == "lib" {
        parent.parent()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manager_listing_owns_only_its_package_location() {
        let temporary = tempfile::tempdir().unwrap();
        let global = temporary.path();
        let wrapper = global.join("node_modules").join(PACKAGE_NAME);
        fs::create_dir_all(&wrapper).unwrap();
        let wrapper = fs::canonicalize(wrapper).unwrap();
        let pnpm = serde_json::json!([{ "dependencies": {
            (PACKAGE_NAME): { "path": wrapper }
        }}])
        .to_string();
        let bun = format!(
            "{} node_modules (1 installed)\n└── {PACKAGE_NAME}@1.0.0",
            global.display()
        );
        let yarn = global.display().to_string();
        for (tool, response, manager, command) in [
            (
                "pnpm",
                pnpm,
                Installation::Pnpm,
                vec!["pnpm", "add", "--global"],
            ),
            (
                "bun",
                bun,
                Installation::Bun,
                vec!["bun", "add", "--global"],
            ),
            (
                "yarn",
                yarn,
                Installation::Yarn,
                vec!["yarn", "global", "add"],
            ),
        ] {
            let install = NodeInstallation::discover(&wrapper, async |program, _| {
                (tool == program).then(|| response.clone())
            })
            .await
            .unwrap();
            assert_eq!(install.manager, manager);
            assert_eq!(
                install.words(PACKAGE_NAME).unwrap(),
                [command, vec![PACKAGE_NAME]].concat()
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn npm_upgrades_the_running_prefix_even_when_another_node_is_active() {
        let wrapper = Path::new("/example/nvm/versions/node/v22/lib/node_modules/private-ai-proxy");
        let install = NodeInstallation::discover(wrapper, async |program, _| {
            (program == "npm").then(|| "/example/nvm/versions/node/v24".into())
        })
        .await
        .unwrap();
        assert_eq!(install.manager, Installation::Npm);
        assert_eq!(
            install.words("private-ai-proxy@1.1.0").unwrap(),
            [
                "npm",
                "install",
                "--global",
                "--prefix",
                "/example/nvm/versions/node/v22",
                "--include=optional",
                "private-ai-proxy@1.1.0"
            ]
        );
        assert!(NodeInstallation::discover(
            Path::new("/example/project/node_modules/private-ai-proxy"),
            async |_, _| None
        )
        .await
        .is_none());
    }
    #[test]
    fn login_service_recovers_hoisted_npm_wrapper() {
        let temp = tempfile::tempdir().unwrap();
        let modules = temp.path().join("lib/node_modules");
        let wrapper = modules.join(PACKAGE_NAME);
        fs::create_dir_all(&wrapper).unwrap();
        fs::write(wrapper.join("package.json"), "{}").unwrap();
        let executable = modules.join("private-ai-proxy-linux-x64/vendor/private-ai-proxy-service");
        assert_eq!(
            wrapper_directory(&executable, None),
            Some(fs::canonicalize(wrapper).unwrap())
        );
    }

    #[tokio::test]
    async fn windows_upgrade_stops_before_replacing_node_binaries() {
        let node = NodeInstallation {
            manager: Installation::Npm,
            prefix: Some(without_windows_verbatim_prefix(r"\\?\C:\Node Prefix").into_owned()),
        };
        assert_eq!(
            super::super::node_steps(&node, "private-ai-proxy@1.1.0", true).unwrap(),
            [
                super::super::STOP_STEP,
                r"npm 'install' '--global' '--prefix' 'C:\Node Prefix' '--include=optional' 'private-ai-proxy@1.1.0'",
            ]
        );
    }
}
