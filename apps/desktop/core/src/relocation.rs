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
//! an old file whose content the new directory already holds at another path
//! is removed. A symlinked `config.toml` (a dotfiles link) becomes a link to
//! the same file. Nothing is removed when both paths resolve to the same file
//! or directory (a settings directory linked to the old one, or the reverse).
//!
//! A new file with other content wins. The old one is kept, never deleted,
//! and reported; the data directory records each reported file's size and
//! modification time, so it is reported again only after it changes, and an
//! old file whose new copy the user deleted after the move is not brought
//! back. If a file cannot be moved, what this start placed is undone and the
//! settings are used from the old directory until a later start succeeds.
//!
//! Until the move, processes that read the settings without the backend (the
//! desktop shell's first paint, the CLI) read them where [`current_dir`]
//! says: the old directory while it holds files the move has yet to bring.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};

use crate::{
    config::{CONFIG_FILE, CREDENTIALS_FILE, SCHEMA_FILE},
    paths::{app_data_dir, config_dir, legacy_config_dir},
    private_fs::{self, Publish},
};

/// Moved in this order, so `credentials.toml` leaves the old directory last.
const FILES: [&str; 2] = [CONFIG_FILE, CREDENTIALS_FILE];
/// In the data directory: the [`Stamp`] of each reported old file, as JSON.
const REPORTED_FILE: &str = "legacy-settings-reported";

/// What the old directory still holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Leftover {
    /// Nothing the settings directory lacks or overrides.
    None,
    /// Files the move has yet to bring; they are the settings in effect.
    Pending,
    /// Old files not used: the settings directory has its own.
    Kept(Vec<&'static str>),
}

/// The directory the settings are in now: [`config_dir`], or the old
/// directory while the move is pending.
pub fn current_dir() -> Result<PathBuf, String> {
    let to = config_dir()?;
    Ok(match legacy_config_dir() {
        Some(from) if leftover(&from, &to, &app_data_dir()?) == Leftover::Pending => from,
        _ => to,
    })
}

/// A warning for `pap doctor` about files left in the old directory.
pub fn diagnostic() -> Option<String> {
    let (from, to) = (legacy_config_dir()?, config_dir().ok()?);
    match leftover(&from, &to, &app_data_dir().ok()?) {
        Leftover::None => None,
        Leftover::Pending => Some(format!(
            "{} has settings files the backend moves to {} when it starts",
            from.display(),
            to.display()
        )),
        Leftover::Kept(files) => Some(kept_notice(&from, &to, &files)),
    }
}

/// What `from` holds for `to`, without changing anything.
pub fn leftover(from: &Path, to: &Path, data_dir: &Path) -> Leftover {
    if !from.is_dir() || same_path(from, to) {
        return Leftover::None;
    }
    let reported = reported(data_dir);
    let mut kept = Vec::new();
    for name in FILES {
        let (old, new) = (from.join(name), to.join(name));
        if !old.exists() {
            continue;
        }
        if !new.exists() {
            if !reported.matches(&old, name) {
                return Leftover::Pending;
            }
            kept.push(name);
        } else if !same_path(&old, &new) && fs::read(&old).ok() != fs::read(&new).ok() {
            kept.push(name);
        }
    }
    if kept.is_empty() {
        Leftover::None
    } else {
        Leftover::Kept(kept)
    }
}

/// Moves the settings files from `from` to `to`. Returns the directory to use
/// and notices for the user.
pub fn relocate(from: &Path, to: &Path, data_dir: &Path) -> (PathBuf, Vec<String>) {
    match move_files(from, to, data_dir) {
        Ok(kept) => (to.to_path_buf(), report(from, to, data_dir, &kept)),
        Err(error) => {
            // Never the new directory while a file exists only in the old one.
            let stay = FILES
                .iter()
                .any(|name| from.join(name).exists() && !to.join(name).exists());
            let dir = if stay { from } else { to };
            tracing::warn!(
                "Cannot move the settings files from {} to {}: {error}",
                from.display(),
                to.display()
            );
            let notice = format!(
                "Settings: The settings files could not be moved from {} to {}: {error}. Settings are used from {} for now, and the move is retried on the next start.",
                from.display(),
                to.display(),
                dir.display()
            );
            (dir.to_path_buf(), vec![notice])
        }
    }
}

/// Returns the old files kept because the new directory has its own.
fn move_files(from: &Path, to: &Path, data_dir: &Path) -> Result<Vec<&'static str>, String> {
    if fs::symlink_metadata(from).is_err() || same_path(from, to) {
        return Ok(Vec::new());
    }
    if is_symlink(from) {
        // Its target is the user's (a dotfiles directory); nothing is moved out of it.
        return if FILES.iter().any(|name| from.join(name).exists()) {
            Err(format!(
                "{} is a symlink, so its files are left in place; move them to {} yourself",
                from.display(),
                to.display()
            ))
        } else {
            Ok(Vec::new())
        };
    }
    if !from.is_dir() {
        return Ok(Vec::new());
    }
    // Everything is read first: nothing is placed unless both files can be.
    let mut old = Vec::new();
    for name in FILES {
        let path = from.join(name);
        if let Some(content) =
            read(&path, name).map_err(|error| format!("Cannot read {name}: {error}"))?
        {
            old.push((name, path, content));
        }
    }
    let reported = reported(data_dir);
    let (mut moved, mut created, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    for (name, old, content) in old {
        let new = to.join(name);
        let placed = match read(&new, name) {
            // The same file (a link between the directories): nothing to move.
            Ok(Some(_)) if same_path(&old, &new) => continue,
            // Placed by an interrupted earlier move.
            Ok(Some(existing)) if existing == content => Ok(()),
            Ok(Some(_)) => {
                kept.push(name);
                continue;
            }
            // Deleted from the new directory after the move reported this file.
            Ok(None) if reported.matches(&old, name) => {
                kept.push(name);
                continue;
            }
            Ok(None) => private_fs::create_private_dir(to)
                .and_then(|()| link_or_copy(&old, &new, name, &content))
                .map(|()| created.push(new)),
            Err(error) => Err(error),
        };
        if let Err(error) = placed {
            for path in &created {
                let _ = fs::remove_file(path);
            }
            return Err(format!("Cannot move {name}: {error}"));
        }
        moved.push(old);
    }
    // Every file is in the new directory now; the old ones can go.
    for old in moved {
        match fs::remove_file(&old) {
            Ok(()) => tracing::info!(
                "Moved {} to {}",
                old.display(),
                to.join(old.file_name().unwrap_or_default()).display()
            ),
            // Its content is in place; the next start removes it.
            Err(error) => tracing::warn!("Cannot remove {}: {error}", old.display()),
        }
    }
    if !FILES.iter().any(|name| is_present(&from.join(name))) {
        // The schema is regenerated beside the new files; other files keep the directory.
        let _ = fs::remove_file(from.join(SCHEMA_FILE));
        if fs::remove_dir(from).is_ok() {
            tracing::info!("Removed the empty directory {}", from.display());
        }
    }
    Ok(kept)
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

/// Whether the path exists, even as a dangling link.
fn is_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Whether both paths resolve to the same file or directory.
fn same_path(left: &Path, right: &Path) -> bool {
    matches!(
        (fs::canonicalize(left), fs::canonicalize(right)),
        (Ok(left), Ok(right)) if left == right
    )
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

/// The old files reported so far, as they were then.
#[derive(Default)]
struct Reported {
    stamps: Vec<Stamp>,
    /// A record from an earlier version: `<file> <size> <modified>` lines, which
    /// are only compared with [`Stamp::legacy_line`]; the next check rewrites
    /// it as JSON.
    legacy: Option<String>,
}

fn reported(data_dir: &Path) -> Reported {
    let Ok(text) = fs::read_to_string(data_dir.join(REPORTED_FILE)) else {
        return Reported::default();
    };
    match serde_json::from_str(&text) {
        Ok(stamps) => Reported {
            stamps,
            legacy: None,
        },
        Err(_) => Reported {
            stamps: Vec::new(),
            legacy: Some(text),
        },
    }
}

impl Reported {
    fn matches(&self, path: &Path, name: &str) -> bool {
        stamp(path, name).is_some_and(|stamp| {
            self.stamps.contains(&stamp)
                || self
                    .legacy
                    .as_deref()
                    .is_some_and(|text| text.lines().any(|line| line == stamp.legacy_line()))
        })
    }
}

/// An old file's size and modification time, which change when it is written.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    file: String,
    size: u64,
    /// Nanoseconds since the Unix epoch.
    modified: u128,
}

impl Stamp {
    fn legacy_line(&self) -> String {
        format!("{} {} {}", self.file, self.size, self.modified)
    }
}

fn stamp(path: &Path, name: &str) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(Stamp {
        file: name.to_string(),
        size: metadata.len(),
        modified,
    })
}

/// Reports old files the new directory overrides, unless they were reported
/// as they are now.
fn report(from: &Path, to: &Path, data_dir: &Path, kept: &[&'static str]) -> Vec<String> {
    let reported = reported(data_dir);
    let known = kept
        .iter()
        .all(|name| reported.matches(&from.join(name), name));
    if known && reported.legacy.is_none() {
        return Vec::new();
    }
    let stamps: Vec<Stamp> = kept
        .iter()
        .filter_map(|name| stamp(&from.join(name), name))
        .collect();
    let written = serde_json::to_string(&stamps)
        .map_err(io::Error::other)
        .and_then(|record| private_fs::write_atomic(&data_dir.join(REPORTED_FILE), &record, None));
    if let Err(error) = written {
        tracing::warn!(
            "Cannot record that {} was reported: {error}",
            from.display()
        );
    }
    if known {
        return Vec::new();
    }
    let notice = kept_notice(from, to, kept);
    tracing::warn!("{notice}");
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

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "# Mine\nappearance = \"dark\" # night owl\n";
    const CREDENTIALS: &str = "# Keys\n[profiles.home]\napi-key = \"sk-home\"\n";

    struct Dirs {
        root: tempfile::TempDir,
        old: PathBuf,
        new: PathBuf,
        data: PathBuf,
    }

    fn dirs() -> Dirs {
        let root = tempfile::tempdir().unwrap();
        let data = root
            .path()
            .join("Application Support/org.dstack.private-ai-proxy");
        let dirs = Dirs {
            old: data.join("Config"),
            new: root.path().join(".config/private-ai-proxy"),
            data,
            root,
        };
        fs::create_dir_all(&dirs.old).unwrap();
        fs::write(dirs.old.join(CONFIG_FILE), CONFIG).unwrap();
        private_fs::write_private(&dirs.old.join(CREDENTIALS_FILE), CREDENTIALS).unwrap();
        fs::write(dirs.old.join(SCHEMA_FILE), "{}").unwrap();
        dirs
    }

    fn relocate(dirs: &Dirs) -> (PathBuf, Vec<String>) {
        super::relocate(&dirs.old, &dirs.new, &dirs.data)
    }

    fn leftover(dirs: &Dirs) -> Leftover {
        super::leftover(&dirs.old, &dirs.new, &dirs.data)
    }

    fn text(path: PathBuf) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn moves_both_files_unchanged_and_removes_the_old_directory() {
        let dirs = dirs();
        assert_eq!(leftover(&dirs), Leftover::Pending);
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
        assert_eq!(
            private_fs::readable_by_others(&dirs.new.join(CREDENTIALS_FILE)).unwrap(),
            Some(false)
        );
        assert!(!dirs.old.exists());
        assert!(dirs.data.exists(), "state stays");
        assert_eq!(leftover(&dirs), Leftover::None);

        // Done: later starts change nothing.
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
    }

    #[test]
    fn a_copy_keeps_the_content_and_credentials_owner_only() {
        let dirs = dirs();
        fs::create_dir_all(&dirs.new).unwrap();
        for (name, content) in [(CONFIG_FILE, CONFIG), (CREDENTIALS_FILE, CREDENTIALS)] {
            let (old, new) = (dirs.old.join(name), dirs.new.join(name));
            copy(&old, &new, name, content.as_bytes()).unwrap();
            assert_eq!(text(new.clone()), content);
            assert_eq!(
                copy(&old, &new, name, b"other").unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(text(new), content, "never overwritten");
        }
        assert_eq!(
            private_fs::readable_by_others(&dirs.new.join(CREDENTIALS_FILE)).unwrap(),
            Some(false)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode(dirs.new.join(CONFIG_FILE)),
                mode(dirs.old.join(CONFIG_FILE))
            );
        }
    }

    #[test]
    fn existing_new_files_win_and_old_ones_are_reported_until_they_change() {
        let dirs = dirs();
        fs::create_dir_all(&dirs.new).unwrap();
        fs::write(dirs.new.join(CONFIG_FILE), "appearance = \"light\"\n").unwrap();

        let (dir, notices) = relocate(&dirs);
        assert_eq!(dir, dirs.new);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains(&dirs.old.display().to_string()));
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), "appearance = \"light\"\n");
        // The file the new directory lacked still moves.
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
        assert!(!dirs.old.join(CREDENTIALS_FILE).exists());
        // The old config.toml is kept, with the directory.
        assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);
        assert_eq!(leftover(&dirs), Leftover::Kept(vec![CONFIG_FILE]));

        // Reported once while it stays as it is...
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        // ...and again once it changes (an earlier version wrote it).
        fs::write(dirs.old.join(CONFIG_FILE), "appearance = \"system\"\n").unwrap();
        let (_, notices) = relocate(&dirs);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), "appearance = \"light\"\n");
    }

    #[test]
    fn a_record_from_an_earlier_version_is_honored_and_rewritten_as_json() {
        let dirs = dirs();
        fs::create_dir_all(&dirs.new).unwrap();
        fs::write(dirs.new.join(CONFIG_FILE), "appearance = \"light\"\n").unwrap();
        let old = stamp(&dirs.old.join(CONFIG_FILE), CONFIG_FILE).unwrap();
        let record = dirs.data.join(REPORTED_FILE);
        fs::write(&record, format!("{}\n", old.legacy_line())).unwrap();

        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        let rewritten: Vec<Stamp> = serde_json::from_str(&text(record)).unwrap();
        assert_eq!(rewritten, [old]);
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
    }

    #[test]
    fn a_reported_old_file_is_not_brought_back_after_the_new_one_is_deleted() {
        let dirs = dirs();
        fs::create_dir_all(&dirs.new).unwrap();
        fs::write(dirs.new.join(CONFIG_FILE), "appearance = \"light\"\n").unwrap();
        assert_eq!(relocate(&dirs).1.len(), 1);

        fs::remove_file(dirs.new.join(CONFIG_FILE)).unwrap();
        assert_eq!(leftover(&dirs), Leftover::Kept(vec![CONFIG_FILE]));
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert!(!dirs.new.join(CONFIG_FILE).exists());
        assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);
    }

    #[test]
    fn an_interrupted_move_is_finished() {
        // A crash after both files were placed, before the old ones were removed.
        let dirs = dirs();
        fs::create_dir_all(&dirs.new).unwrap();
        fs::hard_link(dirs.old.join(CONFIG_FILE), dirs.new.join(CONFIG_FILE)).unwrap();
        fs::copy(
            dirs.old.join(CREDENTIALS_FILE),
            dirs.new.join(CREDENTIALS_FILE),
        )
        .unwrap();
        assert_eq!(leftover(&dirs), Leftover::None);
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert!(!dirs.old.exists());
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
    }

    #[test]
    fn an_unreadable_old_file_places_nothing_and_keeps_the_old_directory() {
        let dirs = dirs();
        // A directory where credentials.toml should be cannot be read.
        fs::remove_file(dirs.old.join(CREDENTIALS_FILE)).unwrap();
        fs::create_dir(dirs.old.join(CREDENTIALS_FILE)).unwrap();
        let (dir, notices) = relocate(&dirs);
        assert_eq!(dir, dirs.old);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(!dirs.new.join(CONFIG_FILE).exists(), "nothing placed");
        assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_move_is_undone_and_the_old_directory_used() {
        let dirs = dirs();
        // A symlink where credentials.toml should go makes placing it fail.
        fs::create_dir_all(&dirs.new).unwrap();
        std::os::unix::fs::symlink(dirs.data.join("elsewhere"), dirs.new.join(CREDENTIALS_FILE))
            .unwrap();
        let (dir, notices) = relocate(&dirs);
        assert_eq!(dir, dirs.old);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(!dirs.new.join(CONFIG_FILE).exists(), "undone");
        assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.old.join(CREDENTIALS_FILE)), CREDENTIALS);
    }

    #[cfg(unix)]
    #[test]
    fn a_settings_directory_linked_to_the_old_one_is_left_alone() {
        let dirs = dirs();
        fs::create_dir_all(dirs.new.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&dirs.old, &dirs.new).unwrap();
        assert_eq!(leftover(&dirs), Leftover::None);
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_linked_to_the_old_one_is_left_alone() {
        let dirs = dirs();
        fs::create_dir_all(&dirs.new).unwrap();
        std::os::unix::fs::symlink(dirs.old.join(CONFIG_FILE), dirs.new.join(CONFIG_FILE)).unwrap();
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
        assert!(!dirs.old.join(CREDENTIALS_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_old_directory_linked_to_the_settings_directory_is_done() {
        let dirs = dirs();
        fs::create_dir_all(dirs.new.parent().unwrap()).unwrap();
        fs::rename(&dirs.old, &dirs.new).unwrap();
        std::os::unix::fs::symlink(&dirs.new, &dirs.old).unwrap();
        assert_eq!(leftover(&dirs), Leftover::None);
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_config_file_stays_linked_to_its_target() {
        let dirs = dirs();
        let target = dirs.root.path().join("dotfiles/config.toml");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, CONFIG).unwrap();
        fs::remove_file(dirs.old.join(CONFIG_FILE)).unwrap();
        std::os::unix::fs::symlink("../../../dotfiles/config.toml", dirs.old.join(CONFIG_FILE))
            .unwrap();
        assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);

        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        let link = dirs.new.join(CONFIG_FILE);
        assert!(is_symlink(&link));
        assert_eq!(
            fs::canonicalize(link).unwrap(),
            fs::canonicalize(&target).unwrap()
        );
        assert!(!dirs.old.exists());
    }

    #[test]
    fn nothing_to_move_without_an_old_directory() {
        let dirs = dirs();
        fs::remove_dir_all(&dirs.old).unwrap();
        assert_eq!(leftover(&dirs), Leftover::None);
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert!(!dirs.new.exists());
    }
}
