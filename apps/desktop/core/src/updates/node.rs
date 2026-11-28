use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use super::{tool_output, Installation, PACKAGE_NAME};

/// Global Node installations can replace their own launcher. Show the command
/// for the manager that owns its wrapper, which may differ from the one on PATH.
#[derive(Debug)]
pub(super) struct NodeInstallation {
    pub(super) manager: Installation,
    prefix: Option<PathBuf>,
}

impl NodeInstallation {
    pub(super) fn current() -> Option<&'static Self> {
        static INSTALLATION: OnceLock<Option<NodeInstallation>> = OnceLock::new();
        INSTALLATION
            .get_or_init(|| {
                let wrapper = fs::canonicalize(std::env::var_os("PAP_NODE_PACKAGE_ROOT")?).ok()?;
                Self::discover(&wrapper, tool_output)
            })
            .as_ref()
    }

    fn discover(wrapper: &Path, output: impl Fn(&str, &[&str]) -> Option<String>) -> Option<Self> {
        let matches = |directory: &Path| {
            fs::canonicalize(directory).is_ok_and(|directory| directory == wrapper)
        };
        let global_wrapper = |root: &Path| matches(&root.join("node_modules").join(PACKAGE_NAME));
        // pnpm's isolated global store can place the resolved package outside
        // its global root. Its dependency listing gives the path to resolve.
        if let Some(path) = output("pnpm", &["list", "--global", "--json"])
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
        if output("yarn", &["global", "dir"]).is_some_and(|root| global_wrapper(Path::new(&root))) {
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
            && !output("npm", &["prefix", "--global"]).is_some_and(|active| {
                matches(&Path::new(&active).join("node_modules").join(PACKAGE_NAME))
            })
        {
            return None;
        }
        Some(Self {
            manager: Installation::Npm,
            prefix: Some(prefix.to_owned()),
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
                self.prefix.as_deref()?.to_str()?,
                "--include=optional",
                spec,
            ],
            _ => return None,
        })
    }
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

    #[test]
    fn manager_listing_owns_only_its_package_location() {
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
            let install = NodeInstallation::discover(&wrapper, |program, _| {
                (tool == program).then(|| response.clone())
            })
            .unwrap();
            assert_eq!(install.manager, manager);
            assert_eq!(
                install.words(PACKAGE_NAME).unwrap(),
                [command, vec![PACKAGE_NAME]].concat()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn npm_upgrades_the_running_prefix_even_when_another_node_is_active() {
        let wrapper = Path::new("/example/nvm/versions/node/v22/lib/node_modules/private-ai-proxy");
        let install = NodeInstallation::discover(wrapper, |program, _| {
            (program == "npm").then(|| "/example/nvm/versions/node/v24".into())
        })
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
            |_, _| None
        )
        .is_none());
    }
}
