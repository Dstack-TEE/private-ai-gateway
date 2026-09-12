mod catalogs;
mod contracts;
mod discovery;
mod recovery;

use super::*;
use crate::secrets::MemoryStore;
use serde_json::json;

const ENDPOINT: &str = "http://127.0.0.1:4180";

fn apply_connect(
    sandbox: &Sandbox,
    agent: Agent,
    catalog: &Catalog,
    options: &ConnectOptions,
) -> AgentStatus {
    let preview = sandbox
        .projector
        .preview(agent, true, Some(catalog), options)
        .unwrap();
    sandbox
        .projector
        .apply(agent, true, &preview.revision, Some(catalog), options)
        .unwrap()
}

pub(super) fn catalog() -> Catalog {
    Catalog::from_remote(
        &json!({
            "data": [
                { "id": "openai/gpt-oss-20b", "name": "GPT OSS 20B", "context_length": 131072 },
                { "id": "phala/qwen" }
            ]
        }),
        1,
    )
    .unwrap()
}

pub(super) struct Sandbox {
    pub(super) home: PathBuf,
    pub(super) projector: Projector,
    secrets: Arc<MemoryStore>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

/// A fresh home directory under the system temp dir with a fake helper
/// binary; tool env overrides are ignored so no real config is
/// touched.
pub(super) fn sandbox(name: &str) -> Sandbox {
    let home = env::temp_dir().join(format!("pap-agents-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    let helper = home.join("Private AI Proxy.app").join(helper_binary_name());
    write(&helper, "#!/bin/sh\n");
    let data_dir = if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join(APP_IDENTIFIER)
    } else {
        home.join(APP_IDENTIFIER)
    };
    let staged = data_dir.join("helpers").join(helper_binary_name());
    write(&staged, "#!/bin/sh\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&helper, &staged] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let secrets = Arc::new(MemoryStore::default());
    let projector = Projector::at(
        home.clone(),
        data_dir,
        helper,
        ENDPOINT,
        false,
        secrets.clone(),
    );
    Sandbox {
        home,
        projector,
        secrets,
    }
}

pub(super) fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn claude_options() -> ConnectOptions {
    ConnectOptions {
        default_model: Some("openai/gpt-oss-20b".to_string()),
    }
}

fn connect(sandbox: &Sandbox) -> AgentStatus {
    let catalog = catalog();
    let preview = sandbox
        .projector
        .preview(Agent::ClaudeCode, true, Some(&catalog), &claude_options())
        .unwrap();
    sandbox
        .projector
        .apply(
            Agent::ClaudeCode,
            true,
            &preview.revision,
            Some(&catalog),
            &claude_options(),
        )
        .unwrap()
}

pub(super) fn disconnect(sandbox: &Sandbox, agent: Agent) -> AgentStatus {
    let options = ConnectOptions::default();
    let preview = sandbox
        .projector
        .preview(agent, false, None, &options)
        .unwrap();
    sandbox
        .projector
        .apply(agent, false, &preview.revision, None, &options)
        .unwrap()
}

fn doc(sandbox: &Sandbox, agent: Agent) -> ConfigDoc {
    let text = sandbox.projector.read_config(agent).unwrap();
    sandbox
        .projector
        .parse_config(agent, text.as_deref())
        .unwrap()
}
