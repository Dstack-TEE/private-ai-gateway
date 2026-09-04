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
    match path {
        "/v1/models" => true,
        "/v1/responses" | "/v1/responses/compact" => matches!(agent, "codex" | "pi"),
        "/v1/messages" | "/v1/messages/count_tokens" => agent == "claude-code",
        "/v1/chat/completions" => matches!(agent, "opencode" | "hermes"),
        _ => false,
    }
}

pub struct TokenFiles {
    dir: PathBuf,
    /// Persists a directory-entry removal (see `sync_dir`); swappable in
    /// tests to prove revocation order and fail-closed behaviour without
    /// tracing syscalls.
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
        self.issue(agent)
    }

    /// Replace whatever token exists with a fresh one (never reuse a leftover).
    pub fn rotate(&self, agent: &str) -> Result<String, String> {
        self.revoke(agent)?;
        self.issue(agent)
    }

    fn issue(&self, agent: &str) -> Result<String, String> {
        let token = if agent == LOCAL_TOOLS_AGENT {
            format!("pag_{}", generate())
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

    /// Durable revocation: the token file is removed and the removal is
    /// persisted (parent-directory sync) before this returns, so a crash or
    /// power loss right after cannot resurrect the token. A missing file
    /// changed no directory entry and needs no sync.
    pub fn revoke(&self, agent: &str) -> Result<(), String> {
        match fs::remove_file(self.path(agent)) {
            Ok(()) => (self.sync_parent)(&self.dir).map_err(|error| {
                format!(
                    "The {agent} token file was deleted but the removal could not be \
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
#[derive(Clone, Debug, Default)]
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

/// Owner-only (0700) directory on Unix; on Windows the per-user app data
/// directory already carries the profile's ACL.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Create an owner-only file that must not exist yet (never follows a symlink).
pub fn write_private(path: &Path, content: &str) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc_nofollow());
    }
    use std::io::Write;
    let mut file = options.open(path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()
}

/// Open a private file read-only without following symlinks (`O_NOFOLLOW`
/// on Unix). `Ok(None)` when the file does not exist.
fn open_private(path: &Path) -> io::Result<Option<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc_nofollow());
    }
    #[cfg(not(unix))]
    if fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(symlink_refused());
    }
    match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            // The open already refused; the metadata only names the reason.
            if fs::symlink_metadata(path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(symlink_refused());
            }
            Err(error)
        }
    }
}

fn symlink_refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "the path is a symlink; refusing to use it",
    )
}

/// Read a private file, refusing symlinks; `Ok(None)` when absent. A pure
/// read: permissions are never touched here.
pub fn read_private_text(path: &Path) -> io::Result<Option<String>> {
    use std::io::Read;
    match open_private(path)? {
        None => Ok(None),
        Some(mut file) => {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            Ok(Some(text))
        }
    }
}

/// Restore owner-only permissions on an existing private file, through an
/// `O_NOFOLLOW` descriptor so the path cannot be swapped for a symlink
/// between check and change. Missing files are fine.
pub fn tighten_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(file) = open_private(path)? {
        use std::os::unix::fs::PermissionsExt;
        let permissions = file.metadata()?.permissions();
        if permissions.mode() & 0o077 != 0 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Durably persist a change to a directory's entries (a file removal). On
/// Unix this is the mature pattern: open the directory itself
/// (`O_DIRECTORY`, `O_NOFOLLOW`; std adds `O_CLOEXEC`) and `fsync` the
/// handle. On Windows a `FlushFileBuffers` is issued on a
/// `FILE_FLAG_BACKUP_SEMANTICS` directory handle via `File::sync_all`;
/// beyond that there is no POSIX-style directory fsync — NTFS journals
/// metadata operations, so the flush plus the journal is the strongest
/// guarantee available without raw volume access.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc_directory() | libc_nofollow());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        // FlushFileBuffers needs write access on the handle.
        options.write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(dir)?.sync_all()
}

#[cfg(unix)]
fn libc_directory() -> i32 {
    // O_DIRECTORY; the constant is stable across the Unix targets we build.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        0x0010_0000
    }
    #[cfg(target_os = "freebsd")]
    {
        0x0002_0000
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    {
        0o200000
    }
}

#[cfg(unix)]
fn libc_nofollow() -> i32 {
    // O_NOFOLLOW; the constant is stable across the Unix targets we build.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        0x0100
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    {
        0o400000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_private_per_agent_and_revocable() {
        let dir = std::env::temp_dir().join(format!("pag-tokens-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
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
        assert!(client.starts_with("pag_"));
        files.revoke("codex").unwrap();
        assert!(files.read("codex").unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
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
        assert!(agent_allows("pi", "/v1/responses"));
        assert!(agent_allows(LOCAL_TOOLS_AGENT, "/v1/messages"));
        assert!(agent_allows(LOCAL_TOOLS_AGENT, "/v1/responses"));
        assert!(agent_allows(LOCAL_TOOLS_AGENT, "/v1/chat/completions"));
        assert!(agent_allows("opencode", "/v1/models"));
        assert!(!agent_allows("opencode", "/v1/responses"));
    }
}
