//! Owner-only files and atomic replacement, shared by every process that
//! writes app state: preferences, profiles, locks, tokens and agent configs.

use std::{fs, io, path::Path};

use rand::RngCore;

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

pub fn symlink_refused() -> io::Error {
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

/// Persist Unix directory entries through an O_DIRECTORY, O_NOFOLLOW handle.
#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc_directory() | libc_nofollow());
    options.open(dir)?.sync_all()
}

/// Windows has no directory handle to flush; token revocation clears the file
/// before unlinking it instead.
#[cfg(windows)]
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Replace `path` atomically: refuse symlinks, re-check that the file still
/// holds `expected` right before the swap, write a random owner-only temp file
/// (never following links), keep the target's permissions when it exists,
/// rename, then fsync the directory on Unix. Callers that need cross-process
/// exclusion wrap this in `lock::with_apply_lock`.
pub fn write_atomic(path: &Path, content: &str, expected: Option<Option<&str>>) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    fs::create_dir_all(dir)?;
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to replace a symlink",
            ))
        }
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(expected) = expected {
        let current = match fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if current.as_deref() != expected {
            return Err(io::Error::other(
                "the file changed on disk since it was read",
            ));
        }
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let mut nonce = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let temp = dir.join(format!(".{name}.{}.tmp", hex(&nonce)));
    let result = (|| {
        write_private(&temp, content)?;
        if let Some(metadata) = &existing {
            fs::set_permissions(&temp, metadata.permissions())?;
        }
        fs::rename(&temp, path)?;
        sync_parent(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn sync_parent(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_dir: &Path) -> io::Result<()> {
    Ok(())
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
