//! The move of `config.toml` and `credentials.toml` from where direct macOS
//! and Windows builds kept them up to 0.2.0-beta.8 (the `Config`
//! subdirectory of the app data directory; see
//! [`crate::paths::legacy_config_dir`]) to the settings directory,
//! `~/.config/private-ai-proxy`. State stays in the app data directory.
//!
//! The backend runs [`relocate`] on every start, before it opens the
//! settings; it does nothing once the old directory is gone. Both old files
//! are read before anything is placed. Each is then hard-linked into the new
//! directory (on the same volume, so it keeps its content and permissions,
//! including the owner-only DACL of `credentials.toml`) or else copied and
//! synced, never over an existing file. Only when every file is in place are
//! the old ones removed, `credentials.toml` last, so a crash leaves each file
//! in at least one of the directories, and the next start finishes the move:
//! an old file the new directory already holds (the same file at another
//! path, or a copy) is removed, unless the new path resolves through the old
//! one. A symlinked `config.toml` (a dotfiles link) becomes a link to the
//! same file. Files and directories are compared by identity (device and
//! inode, or volume serial number and file index), which needs only search
//! permission, not by path.
//!
//! Every check fails safe: whatever cannot be inspected counts as possibly
//! the same file, possibly reached through the old file and possibly in use,
//! so the old file stays. An old regular file is removed only when, right
//! before, the new path is a regular file of its own with the same content.
//!
//! A new file with other content wins. The old one is kept, never deleted,
//! and reported; the data directory records each reported file's size and
//! modification time, so it is reported again only after it changes, and an
//! old file whose new copy the user deleted after the move is not brought
//! back. An old directory that is a link elsewhere (a dotfiles directory) is
//! left alone: its files stay in effect while the new directory has none, and
//! that is reported once. If a file cannot be moved, what this start placed
//! is undone and the settings are used from the old directory until a later
//! start succeeds.
//!
//! The backend and the processes that read the settings without it (the
//! desktop shell's first paint, the CLI; see [`current_dir`]) choose the
//! directory in use by the same rule, [`in_use`].

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use crate::{
    config::{CONFIG_FILE, CREDENTIALS_FILE, SCHEMA_FILE},
    paths::{app_data_dir, config_dir, legacy_config_dir},
    private_fs::{self, Publish},
};

/// Moved in this order, so `credentials.toml` leaves the old directory last.
const FILES: [&str; 2] = [CONFIG_FILE, CREDENTIALS_FILE];
/// In the data directory: `<file> <size> <modified>` for each reported old
/// file (`directory` for a linked old directory).
const REPORTED_FILE: &str = "legacy-settings-reported";
/// How the backend's notice for a failed move starts.
const FAILED: &str = "Settings: The settings files could not be moved";

/// What the old directory still holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Leftover {
    /// Nothing the settings directory lacks or overrides.
    None,
    /// Files the move has yet to bring; they are the settings in effect.
    Pending,
    /// The old directory is a link the move leaves alone; its files are the
    /// settings in effect.
    Linked,
    /// Old files not used: the settings directory has its own.
    Kept(Vec<&'static str>),
}

/// The directory the settings are in now: [`config_dir`], or the old
/// directory while its files are in effect.
pub fn current_dir() -> Result<PathBuf, String> {
    let to = config_dir()?;
    Ok(match legacy_config_dir() {
        Some(from) if in_use(&from, &to, &reported(&app_data_dir()?)) => from,
        _ => to,
    })
}

/// A warning for `pap doctor` about files left in the old directory, given
/// the notices of the running backend, if any.
pub fn diagnostic(backend: Option<&[String]>) -> Option<String> {
    let (from, to) = (legacy_config_dir()?, config_dir().ok()?);
    describe(&from, &to, &app_data_dir().ok()?, backend)
}

fn describe(from: &Path, to: &Path, data_dir: &Path, backend: Option<&[String]>) -> Option<String> {
    match leftover(from, to, data_dir) {
        Leftover::None => None,
        Leftover::Pending => Some(match backend {
            None => format!(
                "{} has settings files the backend moves to {} when it starts",
                from.display(),
                to.display()
            ),
            Some(notices) => notices
                .iter()
                .find(|notice| notice.starts_with(FAILED))
                .map(|notice| notice.trim_start_matches("Settings: ").to_string())
                .unwrap_or_else(|| {
                    format!(
                        "{} has settings files the running backend has not moved to {}; restart it to move them",
                        from.display(),
                        to.display()
                    )
                }),
        }),
        Leftover::Linked => Some(linked_notice(from, to)),
        Leftover::Kept(files) => Some(kept_notice(from, to, &files)),
    }
}

/// What `from` holds for `to`, without changing anything.
pub fn leftover(from: &Path, to: &Path, data_dir: &Path) -> Leftover {
    match fs::metadata(from) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Leftover::Pending,
        _ => return Leftover::None,
    }
    if same(from, to) == Some(true) {
        return Leftover::None;
    }
    let reported = reported(data_dir);
    let linked = is_symlink(from);
    if in_use(from, to, &reported) {
        return if linked {
            Leftover::Linked
        } else {
            Leftover::Pending
        };
    }
    let kept: Vec<_> = FILES
        .into_iter()
        .filter(|name| {
            let (old, new) = (from.join(name), to.join(name));
            if present(&old) == Some(false) {
                return false;
            }
            if linked || present(&new) == Some(false) {
                return true;
            }
            // Possibly the same file is not reported; files that cannot be read are.
            same(&old, &new) == Some(false)
                && !matches!((fs::read(&old), fs::read(&new)), (Ok(old), Ok(new)) if old == new)
        })
        .collect();
    if kept.is_empty() {
        Leftover::None
    } else {
        Leftover::Kept(kept)
    }
}

/// Whether the settings in effect are in `from`: a file there that `to`
/// lacks, unless it was reported (its new copy was deleted after the move).
/// A linked old directory is used while `to` has no settings file. What
/// cannot be inspected counts as possibly there, so it is never abandoned.
fn in_use(from: &Path, to: &Path, reported: &Reported) -> bool {
    match fs::metadata(from) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() != io::ErrorKind::NotFound => return true,
        _ => return false,
    }
    if same(from, to) == Some(true) {
        return false;
    }
    let (old, new) = (
        |name: &str| present(&from.join(name)),
        |name: &str| present(&to.join(name)),
    );
    if is_symlink(from) {
        return FILES.iter().any(|name| old(name) != Some(false))
            && FILES.iter().all(|name| new(name) == Some(false));
    }
    FILES.iter().any(|name| match old(name) {
        Some(false) => false,
        Some(true) => new(name) != Some(true) && !reported.matches(&from.join(name), name),
        None => new(name) != Some(true),
    })
}

/// Moves the settings files from `from` to `to`. Returns the directory to use
/// and notices for the user.
pub fn relocate(from: &Path, to: &Path, data_dir: &Path) -> (PathBuf, Vec<String>) {
    let reported = reported(data_dir);
    match fs::metadata(from) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            let error = format!("Cannot inspect {}: {error}", from.display());
            return failed(from, to, &reported, error);
        }
        _ => return (to.to_path_buf(), Vec::new()),
    }
    match same(from, to) {
        Some(true) => return (to.to_path_buf(), Vec::new()),
        Some(false) => {}
        None => {
            let error = format!(
                "Cannot tell whether {} and {} are the same directory",
                from.display(),
                to.display()
            );
            return failed(from, to, &reported, error);
        }
    }
    if is_symlink(from) {
        // Its target is the user's (a dotfiles directory); nothing is moved out of it.
        return if in_use(from, to, &reported) {
            let stamps = link_stamp(from).into_iter().collect();
            let notices = report(data_dir, &reported, stamps, linked_notice(from, to));
            (from.to_path_buf(), notices)
        } else {
            let kept: Vec<_> = FILES
                .into_iter()
                .filter(|name| present(&from.join(name)) != Some(false))
                .collect();
            let notices = report_kept(from, to, data_dir, &reported, &kept);
            (to.to_path_buf(), notices)
        };
    }
    match move_files(from, to, &reported) {
        Ok(kept) => {
            let notices = report_kept(from, to, data_dir, &reported, &kept);
            (to.to_path_buf(), notices)
        }
        Err(error) => failed(from, to, &reported, error),
    }
}

/// The directory to use and the notice when the move did not happen.
fn failed(from: &Path, to: &Path, reported: &Reported, error: String) -> (PathBuf, Vec<String>) {
    let dir = if in_use(from, to, reported) { from } else { to };
    tracing::warn!(
        "Cannot move the settings files from {} to {}: {error}",
        from.display(),
        to.display()
    );
    let notice = format!(
        "{FAILED} from {} to {}: {error}. Settings are used from {} for now, and the move is retried on the next start.",
        from.display(),
        to.display(),
        dir.display()
    );
    (dir.to_path_buf(), vec![notice])
}

/// What happens to one old file.
enum Step {
    /// Placed in the new directory, then removed if [`removable`].
    Place(Vec<u8>),
    /// The new directory holds it, or possibly does (the same file, or a
    /// copy): removed if [`removable`], else left alone.
    Settle,
    /// The new directory has other content, or deleted its copy after this
    /// file was reported: kept.
    Keep,
}

/// Returns the old files kept because the new directory has its own. An old
/// file that cannot be inspected or read fails the move before anything is
/// placed.
fn move_files(from: &Path, to: &Path, reported: &Reported) -> Result<Vec<&'static str>, String> {
    let mut steps = Vec::new();
    for name in FILES {
        let (old, new) = (from.join(name), to.join(name));
        if entry(&old)
            .map_err(|error| format!("Cannot read {name}: {error}"))?
            .is_none()
        {
            continue;
        }
        let new_entry =
            entry(&new).map_err(|error| format!("Cannot read the new {name}: {error}"))?;
        let step = if new_entry.is_none() {
            match read(&old, name).map_err(|error| format!("Cannot read {name}: {error}"))? {
                // A dangling link: nothing to move.
                None => continue,
                // Deleted from the new directory after the move reported this file.
                Some(_) if reported.matches(&old, name) => Step::Keep,
                Some(content) => Step::Place(content),
            }
        } else if same(&old, &new) == Some(false) {
            let old_content =
                read(&old, name).map_err(|error| format!("Cannot read {name}: {error}"))?;
            let new_content =
                read(&new, name).map_err(|error| format!("Cannot read the new {name}: {error}"))?;
            match (old_content, new_content) {
                (None, _) => continue,
                (Some(old), Some(new)) if old == new => Step::Settle,
                _ => Step::Keep,
            }
        } else {
            // The same file under both paths (a link, or an interrupted move), or
            // one that cannot be told apart from it.
            Step::Settle
        };
        steps.push((name, old, new, step));
    }
    let mut created = Vec::new();
    for (name, old, new, step) in &steps {
        let Step::Place(content) = step else {
            continue;
        };
        let placed =
            private_fs::create_private_dir(to).and_then(|()| link_or_copy(old, new, name, content));
        if let Err(error) = placed {
            for path in &created {
                let _ = fs::remove_file(path);
            }
            return Err(format!("Cannot move {name}: {error}"));
        }
        created.push(new.clone());
    }
    // Every file is in the new directory now; the old ones can go.
    let mut kept = Vec::new();
    for (name, old, new, step) in steps {
        if let Step::Keep = step {
            kept.push(name);
        } else if removable(&old, &new) {
            match fs::remove_file(&old) {
                Ok(()) => tracing::info!("Moved {} to {}", old.display(), new.display()),
                // Its content is in place; the next start removes it.
                Err(error) => tracing::warn!("Cannot remove {}: {error}", old.display()),
            }
        }
    }
    if FILES
        .iter()
        .all(|name| matches!(entry(&from.join(name)), Ok(None)))
    {
        // The schema is regenerated beside the new files; other files keep the directory.
        let _ = fs::remove_file(from.join(SCHEMA_FILE));
        if fs::remove_dir(from).is_ok() {
            tracing::info!("Removed the empty directory {}", from.display());
        }
    }
    Ok(kept)
}

/// Whether removing `old` loses nothing, checked right before it is removed.
/// An old link may go when `new` leads to the same file without passing
/// through it. An old file may go only when `new` is a regular file of its
/// own (not a link; hard links count, their directories differ) with the same
/// content: identities alone are not trusted with data (ReFS file indexes are
/// not unique). Anything that cannot be checked keeps the old file.
fn removable(old: &Path, new: &Path) -> bool {
    let (Ok(Some(old_entry)), Ok(Some(new_entry))) = (entry(old), entry(new)) else {
        return false;
    };
    if old_entry.file_type().is_symlink() {
        return same(old, new) == Some(true) && resolves_through(new, old) == Some(false);
    }
    old_entry.is_file()
        && new_entry.is_file()
        && matches!((fs::read(old), fs::read(new)), (Ok(old), Ok(new)) if old == new)
}

/// A settings file's content, `None` when it does not exist. As in the
/// settings store, a symlinked `config.toml` (a dotfiles link) is followed
/// and a symlinked `credentials.toml` refused.
fn read(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    if name == CREDENTIALS_FILE && is_symlink(path) {
        return Err(private_fs::symlink_refused());
    }
    match fs::read(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// The entry itself (a link is not followed), `None` when there is none.
fn entry(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether the path leads to something (links followed); `None` when that
/// cannot be told.
fn present(path: &Path) -> Option<bool> {
    match fs::metadata(path) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// A file's or directory's identity: device and inode on Unix, volume serial
/// number and file index on Windows.
#[derive(Debug, PartialEq, Eq)]
struct Identity(u64, u128);

/// The identity of what `path` leads to (`follow`) or of the entry itself.
/// Only the parent directories must be searchable: nothing is opened for
/// reading.
#[cfg(unix)]
fn identity(path: &Path, follow: bool) -> io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = if follow {
        fs::metadata(path)?
    } else {
        fs::symlink_metadata(path)?
    };
    Ok(Identity(metadata.dev(), metadata.ino().into()))
}

/// On Windows the handle is opened with no access rights, as the standard
/// library's `fs::metadata` does, so neither read nor list permission is
/// needed; backup semantics lets it open directories.
#[cfg(windows)]
fn identity(path: &Path, follow: bool) -> io::Result<Identity> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let flags = if follow {
        FILE_FLAG_BACKUP_SEMANTICS
    } else {
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT
    };
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(flags)
        .open(path)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow);
    Ok(Identity(info.dwVolumeSerialNumber.into(), index))
}

/// Whether both paths lead to the same file or directory, however they are
/// spelled (case, `..`, links). A missing path is never the same as another;
/// `None` when either cannot be inspected.
fn same(left: &Path, right: &Path) -> Option<bool> {
    match (identity(left, true), identity(right, true)) {
        (Ok(left), Ok(right)) => Some(left == right),
        (Err(error), _) | (_, Err(error)) if error.kind() == io::ErrorKind::NotFound => Some(false),
        _ => None,
    }
}

/// Whether following the links from `path` passes through the entry `via`
/// itself (compared by identity, so a differently spelled or cased path to it
/// counts), so removing `via` would break `path`. `None` when a step cannot
/// be inspected.
fn resolves_through(path: &Path, via: &Path) -> Option<bool> {
    let via = identity(via, false).ok()?;
    let mut path = path.to_path_buf();
    // Deeper than the kernel follows (40 on Linux): treated as unknown.
    for _ in 0..40 {
        let entry = match entry(&path) {
            Ok(Some(entry)) => entry,
            // The chain ends before reaching anything.
            Ok(None) => return Some(false),
            Err(_) => return None,
        };
        if identity(&path, false).ok()? == via {
            return Some(true);
        }
        if !entry.file_type().is_symlink() {
            return Some(false);
        }
        let target = fs::read_link(&path).ok()?;
        path = path.parent().unwrap_or(Path::new(".")).join(target);
    }
    None
}

/// Places `content`, read from `old`, at `new`, failing if `new` exists.
fn link_or_copy(old: &Path, new: &Path, name: &str, content: &[u8]) -> io::Result<()> {
    let dir = new.parent().unwrap_or(Path::new("."));
    if is_symlink(old) {
        // The new link points at the same file, not through the old directory.
        let target = fs::canonicalize(old)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, new)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(target, new).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("{error} (creating a symbolic link needs Developer Mode or the Create symbolic links privilege)"),
            )
        })?;
        return private_fs::sync_dir(dir);
    }
    match fs::hard_link(old, new) {
        Ok(()) => private_fs::sync_dir(dir),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        // Another volume, or a file system without hard links.
        Err(_) => copy(old, new, name, content),
    }
}

/// Writes a synced copy of `old` at `new`, failing if `new` exists. The copy
/// starts owner-only; only `config.toml` takes the permissions of the old file.
fn copy(old: &Path, new: &Path, name: &str, content: &[u8]) -> io::Result<()> {
    let permissions = fs::metadata(old)?.permissions();
    private_fs::publish(new, Publish::NoClobber, |file| {
        use std::io::Write;
        if name == CREDENTIALS_FILE {
            #[cfg(windows)]
            crate::windows_acl::restrict_to_current_user(file)?;
        } else {
            file.set_permissions(permissions)?;
        }
        file.write_all(content)
    })
}

/// What was reported so far, as it was then.
struct Reported(String);

fn reported(data_dir: &Path) -> Reported {
    Reported(fs::read_to_string(data_dir.join(REPORTED_FILE)).unwrap_or_default())
}

impl Reported {
    fn matches(&self, path: &Path, name: &str) -> bool {
        stamp(path, name).is_some_and(|stamp| self.has(&stamp))
    }

    fn has(&self, stamp: &str) -> bool {
        self.0.lines().any(|line| line == stamp)
    }
}

/// `<file> <size> <modified>` of an old file, which changes when it is written.
fn stamp(path: &Path, name: &str) -> Option<String> {
    stamp_of(&fs::metadata(path).ok()?, name)
}

/// The same for a linked old directory's link, which changes when it is replaced.
fn link_stamp(path: &Path) -> Option<String> {
    stamp_of(&fs::symlink_metadata(path).ok()?, "directory")
}

fn stamp_of(metadata: &fs::Metadata, name: &str) -> Option<String> {
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{name} {} {modified}", metadata.len()))
}

/// Reports old files the new directory overrides, unless they were reported
/// as they are now.
fn report_kept(
    from: &Path,
    to: &Path,
    data_dir: &Path,
    reported: &Reported,
    kept: &[&'static str],
) -> Vec<String> {
    let stamps = kept
        .iter()
        .filter_map(|name| stamp(&from.join(name), name))
        .collect();
    report(data_dir, reported, stamps, kept_notice(from, to, kept))
}

/// Reports `notice` unless every stamp was reported; records the stamps with
/// the others.
fn report(
    data_dir: &Path,
    reported: &Reported,
    stamps: Vec<String>,
    notice: String,
) -> Vec<String> {
    if stamps.iter().all(|stamp| reported.has(stamp)) {
        return Vec::new();
    }
    tracing::warn!("{notice}");
    // Merged: a new stamp replaces the one recorded for the same file.
    let name = |stamp: &str| stamp.split(' ').next().unwrap_or_default().to_string();
    let names: Vec<_> = stamps.iter().map(|stamp| name(stamp)).collect();
    let record: String = reported
        .0
        .lines()
        .filter(|line| !names.contains(&name(line)))
        .map(str::to_string)
        .chain(stamps)
        .map(|stamp| format!("{stamp}\n"))
        .collect();
    if let Err(error) = private_fs::write_atomic(&data_dir.join(REPORTED_FILE), &record, None) {
        tracing::warn!("Cannot record the settings report: {error}");
    }
    vec![format!("Settings: {notice}")]
}

fn kept_notice(from: &Path, to: &Path, kept: &[&str]) -> String {
    format!(
        "{} still has {} from an earlier version. The settings in {} are used; copy anything you still need from the old files, then delete them.",
        from.display(),
        kept.join(" and "),
        to.display()
    )
}

fn linked_notice(from: &Path, to: &Path) -> String {
    format!(
        "{} is a symlink, so its settings files are not moved and stay in use; move them to {} (or link {} to them) to use the new location.",
        from.display(),
        to.display(),
        to.display()
    )
}

#[cfg(test)]
mod tests;
