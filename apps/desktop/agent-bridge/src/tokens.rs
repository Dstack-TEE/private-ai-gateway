//! Machine-local agent tokens. Each connected agent gets its own random
//! token in an owner-only file the app owns; agent configs reference the file
//! or the bundled helper, never the RedPill key. A token is a capability plus
//! an attribution label for one agent's surfaces: it does not defend against
//! other code running as the same OS user, which can read the same files.
//! Revoking a token deletes the file, which cuts that agent off.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use rand::RngCore;

#[cfg(windows)]
use desktop_core::private_fs::symlink_refused;
use desktop_core::private_fs::{
    create_private_dir, read_private_text, sync_dir, tighten_private, write_private,
};

use desktop_core::agents::Agent;

use crate::{agents::AgentIntegration, catalog::Surface};

const TOKEN_BYTES: usize = 32;
pub const LOCAL_TOOLS_AGENT: &str = "local-tools";

/// Which local paths a token issued to `agent` may call. `/v1/models` is
/// shared; inference and its helpers are per agent.
pub fn agent_allows(agent: &str, path: &str) -> bool {
    if agent == LOCAL_TOOLS_AGENT {
        return matches!(
            path,
            "/v1/models"
                | "/v1/responses"
                | "/v1/responses/compact"
                | "/v1/messages"
                | "/v1/messages/count_tokens"
                | "/v1/chat/completions"
        );
    }
    let Ok(agent) = Agent::from_id(agent) else {
        return false;
    };
    let surface = match path {
        "/v1/models" => return true,
        "/v1/responses" | "/v1/responses/compact" => Surface::Responses,
        "/v1/messages" | "/v1/messages/count_tokens" => Surface::Messages,
        "/v1/chat/completions" => Surface::ChatCompletions,
        _ => return false,
    };
    agent.surface() == surface
}

pub struct TokenFiles {
    dir: PathBuf,
    /// Completes the platform revocation barrier after removal (see
    /// `sync_dir`); swappable in tests to prove fail-closed behaviour.
    sync_parent: fn(&Path) -> io::Result<()>,
}

impl TokenFiles {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("agent-tokens"),
            sync_parent: sync_dir,
        }
    }

    #[cfg(test)]
    pub fn set_sync_parent(&mut self, sync_parent: fn(&Path) -> io::Result<()>) {
        self.sync_parent = sync_parent;
    }

    pub fn path(&self, agent: &str) -> PathBuf {
        self.dir.join(agent)
    }

    /// The agent's current token, issuing a fresh one when none exists.
    pub fn ensure(&self, agent: &str) -> Result<String, String> {
        if let Some(existing) = self.read(agent)? {
            return Ok(existing);
        }
        // An interrupted Windows revocation can leave a durably empty tombstone.
        self.revoke(agent)?;
        self.issue(agent)
    }

    /// Replace whatever token exists with a fresh one (never reuse a leftover).
    pub fn rotate(&self, agent: &str) -> Result<String, String> {
        self.revoke(agent)?;
        self.issue(agent)
    }

    fn issue(&self, agent: &str) -> Result<String, String> {
        // OpenClaw's text exec resolver first attempts JSON parsing. A prefix
        // prevents a randomly all-numeric token from being treated as a number.
        let token = if agent == LOCAL_TOOLS_AGENT || agent == "openclaw" {
            format!("sk-pap-{}", generate())
        } else {
            generate()
        };
        create_private_dir(&self.dir)
            .and_then(|()| write_private(&self.path(agent), &token))
            .map_err(|error| format!("Cannot store the {agent} token: {error}"))?;
        Ok(token)
    }

    /// Read a token, refusing symlinks. Reading never changes permissions;
    /// `maintain` does, under the caller's lock.
    pub fn read(&self, agent: &str) -> Result<Option<String>, String> {
        let text = read_private_text(&self.path(agent))
            .map_err(|error| format!("Cannot read the {agent} token: {error}"))?;
        Ok(text.and_then(|text| {
            let token = text.trim();
            (!token.is_empty()).then(|| token.to_string())
        }))
    }

    /// Restore owner-only permissions on existing token files. Called only
    /// under the apply lock (startup and transactions).
    pub fn maintain(&self, agents: &[&str]) -> Result<(), String> {
        for agent in agents {
            tighten_private(&self.path(agent))
                .map_err(|error| format!("Cannot secure the {agent} token: {error}"))?;
        }
        Ok(())
    }

    /// Unix persists removal through a parent-directory sync; Windows first
    /// persists an empty file so a rolled-back deletion cannot restore the key.
    /// A missing file needs no persistence barrier.
    pub fn revoke(&self, agent: &str) -> Result<(), String> {
        let path = self.path(agent);
        #[cfg(windows)]
        match neutralize_private(&path) {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(error) => return Err(format!("Cannot revoke the {agent} token: {error}")),
        }
        match fs::remove_file(path) {
            Ok(()) => (self.sync_parent)(&self.dir).map_err(|error| {
                format!(
                    "The {agent} token file was deleted but durable revocation could not be \
                     persisted: {error}"
                )
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Cannot revoke the {agent} token: {error}")),
        }
    }

    /// Every issued token for `agents`, keyed by value, for the authenticator.
    pub fn load(&self, agents: &[&str]) -> Result<TokenSet, String> {
        let mut set = TokenSet::default();
        for agent in agents {
            if let Some(token) = self.read(agent)? {
                set.0.insert(token, agent.to_string());
            }
        }
        Ok(set)
    }
}

/// Issued tokens mapped to the agent they authenticate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenSet(HashMap<String, String>);

impl TokenSet {
    pub fn agent_for(&self, token: &str) -> Option<&str> {
        self.0.get(token).map(String::as_str)
    }

    pub fn insert(&mut self, token: String, agent: String) {
        self.0.insert(token, agent);
    }

    pub fn without(&self, agent: &str) -> TokenSet {
        TokenSet(
            self.0
                .iter()
                .filter(|(_, owner)| owner.as_str() != agent)
                .map(|(token, owner)| (token.clone(), owner.clone()))
                .collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn generate() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// FlushFileBuffers supports regular writable files, not directory handles.
/// Clear and flush before unlinking so a recovered entry cannot revive a key.
#[cfg(windows)]
fn neutralize_private(path: &Path) -> io::Result<bool> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let file = match fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(symlink_refused());
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the token path is not a regular file",
        ));
    }
    file.set_len(0)?;
    file.sync_all()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_replaces_an_empty_revocation_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let files = TokenFiles::new(dir.path());
        let old = files.ensure(LOCAL_TOOLS_AGENT).unwrap();
        #[cfg(windows)]
        assert!(neutralize_private(&files.path(LOCAL_TOOLS_AGENT)).unwrap());
        #[cfg(not(windows))]
        fs::write(files.path(LOCAL_TOOLS_AGENT), "").unwrap();
        assert!(files.read(LOCAL_TOOLS_AGENT).unwrap().is_none());
        let replacement = files.ensure(LOCAL_TOOLS_AGENT).unwrap();
        assert_ne!(replacement, old);
        assert_eq!(files.read(LOCAL_TOOLS_AGENT).unwrap(), Some(replacement));
    }

    #[test]
    fn rotate_sync_failure_leaves_no_readable_old_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = TokenFiles::new(dir.path());
        let old = files.ensure(LOCAL_TOOLS_AGENT).unwrap();
        files.set_sync_parent(|_| Err(io::Error::other("injected sync failure")));
        assert!(files.rotate(LOCAL_TOOLS_AGENT).is_err());
        assert!(files.read(LOCAL_TOOLS_AGENT).unwrap().is_none());
        files.set_sync_parent(sync_dir);
        assert_ne!(files.ensure(LOCAL_TOOLS_AGENT).unwrap(), old);
    }

    #[test]
    fn tokens_are_private_per_agent_and_revocable() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("tokens");
        let files = TokenFiles::new(&dir);
        let codex = files.ensure("codex").unwrap();
        assert_eq!(codex.len(), TOKEN_BYTES * 2);
        assert_eq!(files.ensure("codex").unwrap(), codex);
        let opencode = files.ensure("opencode").unwrap();
        assert_ne!(codex, opencode);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = files.path("codex");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            // Reading never changes permissions; maintenance (run under the
            // apply lock) tightens a loosened file through its descriptor.
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            files.read("codex").unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644
            );
            files.maintain(&["codex"]).unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            // A symlink in place of a token is refused.
            std::os::unix::fs::symlink(&path, files.path("claude-code")).unwrap();
            assert!(files.read("claude-code").unwrap_err().contains("symlink"));
        }
        let set = files.load(&["codex", "opencode"]).unwrap();
        assert_eq!(set.agent_for(&codex), Some("codex"));
        assert_eq!(set.agent_for("nope"), None);
        assert_eq!(set.without("codex").agent_for(&codex), None);
        let client = files.ensure(LOCAL_TOOLS_AGENT).unwrap();
        assert!(client.starts_with("sk-pap-"));
        assert_eq!(client.len(), "sk-pap-".len() + TOKEN_BYTES * 2);
        let openclaw = files.ensure("openclaw").unwrap();
        assert!(openclaw.starts_with("sk-pap-"));
        assert_eq!(openclaw.len(), "sk-pap-".len() + TOKEN_BYTES * 2);
        assert!(serde_json::from_str::<serde_json::Value>(&openclaw).is_err());
        assert_eq!(files.ensure("openclaw").unwrap(), openclaw);
        let rotated = files.rotate("openclaw").unwrap();
        assert_ne!(rotated, openclaw);
        assert!(rotated.starts_with("sk-pap-"));
        files.revoke("codex").unwrap();
        assert!(files.read("codex").unwrap().is_none());
    }

    #[test]
    fn tokens_are_scoped_to_their_agent_surfaces() {
        assert!(agent_allows("codex", "/v1/responses"));
        assert!(agent_allows("codex", "/v1/responses/compact"));
        assert!(!agent_allows("codex", "/v1/messages"));
        assert!(agent_allows("claude-code", "/v1/messages/count_tokens"));
        assert!(!agent_allows("claude-code", "/v1/chat/completions"));
        assert!(agent_allows("opencode", "/v1/chat/completions"));
        assert!(agent_allows("hermes", "/v1/chat/completions"));
        assert!(agent_allows("openclaw", "/v1/chat/completions"));
        assert!(agent_allows("openclaw", "/v1/models"));
        assert!(!agent_allows("openclaw", "/v1/responses"));
        assert!(!agent_allows("openclaw", "/v1/messages"));
        assert!(!agent_allows("openclaw", "/v1/responses/compact"));
        assert!(agent_allows("oh-my-pi", "/v1/models"));
        assert!(agent_allows("oh-my-pi", "/v1/chat/completions"));
        assert!(!agent_allows("oh-my-pi", "/v1/responses"));
        assert!(!agent_allows("oh-my-pi", "/v1/messages"));
        assert!(agent_allows("pi", "/v1/chat/completions"));
        assert!(!agent_allows("pi", "/v1/responses"));
        assert!(!agent_allows("pi", "/v1/responses/compact"));
        assert!(agent_allows(LOCAL_TOOLS_AGENT, "/v1/messages"));
        assert!(agent_allows(LOCAL_TOOLS_AGENT, "/v1/responses"));
        assert!(agent_allows(LOCAL_TOOLS_AGENT, "/v1/chat/completions"));
        assert!(agent_allows("opencode", "/v1/models"));
        assert!(!agent_allows("opencode", "/v1/responses"));
    }
}
