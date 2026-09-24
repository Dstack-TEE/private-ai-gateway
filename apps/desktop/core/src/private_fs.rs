//! Owner-only files and atomic replacement, shared by every process that
//! writes app state: preferences, profiles, locks, tokens and agent configs.

use std::{fs, io, path::Path};

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
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
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
        options.custom_flags(libc::O_NOFOLLOW);
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
    options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    options.open(dir)?.sync_all()
}

/// Windows has no directory handle to flush; token revocation clears the file
/// before unlinking it instead.
#[cfg(windows)]
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Replace `path` atomically: refuse symlinks, re-check that the file still
/// holds `expected` right before the swap, and keep the target's permissions
/// when it exists; see [`publish`]. Callers that need cross-process exclusion
/// wrap this in `lock::with_apply_lock`.
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
            return Err(io::Error::other(ChangedOnDisk));
        }
    }
    publish(path, Publish::Replace, |file| {
        use std::io::Write;
        file.write_all(content.as_bytes())?;
        if let Some(metadata) = &existing {
            file.set_permissions(metadata.permissions())?;
        }
        Ok(())
    })
}

/// [`write_atomic`] found other content than expected: someone else wrote the file.
#[derive(Debug)]
pub struct ChangedOnDisk;

impl std::fmt::Display for ChangedOnDisk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the file changed on disk since it was read")
    }
}

impl std::error::Error for ChangedOnDisk {}

impl ChangedOnDisk {
    pub fn is(error: &io::Error) -> bool {
        error.get_ref().is_some_and(|inner| inner.is::<Self>())
    }
}

/// What [`publish`] does when the destination already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish {
    Replace,
    /// Fail with `AlreadyExists` instead, atomically.
    NoClobber,
}

/// Write a file so that readers only ever see the complete result: `write`
/// fills a new owner-only temporary file (`.<name>.<random>.tmp`) beside
/// `path`, which is synced, closed and then moved into place; the directory
/// entry is synced on Unix. Nothing is left behind on failure.
pub fn publish(
    path: &Path,
    mode: Publish,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(dir)?;
    write(temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    // Publish through a closed handle: nothing can observe the destination
    // while it is still open for writing.
    let temporary = temporary.into_temp_path();
    match mode {
        Publish::Replace => temporary.persist(path),
        Publish::NoClobber => temporary.persist_noclobber(path),
    }
    .map_err(|error| error.error)?;
    sync_parent(dir)
}

#[cfg(unix)]
fn sync_parent(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;

    // Regression: the open(2) flags once came from hand-written constants
    // that were only O_DIRECTORY/O_NOFOLLOW on x86_64 Linux.
    #[test]
    fn private_reads_refuse_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::write(&target, "secret").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let link = temp.path().join("link");
        symlink(&target, &link).unwrap();

        let error = read_private_text(&link).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(tighten_private(&link).is_err());
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        assert_eq!(
            read_private_text(&target).unwrap().as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn sync_dir_opens_only_real_directories() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("dir");
        create_private_dir(&dir).unwrap();
        sync_dir(&dir).unwrap();

        let file = temp.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(sync_dir(&file).is_err());
        let link = temp.path().join("link");
        symlink(&dir, &link).unwrap();
        assert!(sync_dir(&link).is_err());
    }
}
