//! The move of `config.toml` and `credentials.toml` from where direct macOS
//! and Windows builds kept them up to 0.2.0-beta.8 (the `Config`
//! subdirectory of the app data directory; see
//! `desktop_core::paths::legacy_config_dir`) to the settings directory,
//! `~/.config/private-ai-proxy`. State stays in the app data directory.
//!
//! It runs on every start, before the settings are opened, and does nothing
//! once the old directory is gone. Each old file is hard-linked into the new
//! directory (on the same volume, so it keeps its content and permissions,
//! including the owner-only DACL of `credentials.toml`) or else copied and
//! synced, never over an existing file. Only when every file is in place are
//! the old ones removed, `credentials.toml` last, so a crash leaves each file
//! in at least one of the directories, and the next start finishes the move:
//! an old file whose content the new directory already holds is removed. A
//! symlinked `config.toml` (a dotfiles link) becomes a link to the same file;
//! a symlinked old directory is left alone and reported.
//!
//! A new file with other content wins. The old one is kept, never deleted,
//! and reported once. If a file cannot be moved, what this start moved is
//! undone and the settings are used from the old directory until a later
//! start succeeds.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use desktop_core::{
    config::{CONFIG_FILE, CREDENTIALS_FILE, SCHEMA_FILE},
    private_fs::{self, Publish},
};

/// Moved in this order, so `credentials.toml` leaves the old directory last.
const FILES: [&str; 2] = [CONFIG_FILE, CREDENTIALS_FILE];
/// In the data directory once old files the new directory overrides are reported.
const REPORTED_FILE: &str = "legacy-settings-reported";

/// Moves the settings files from `from` to `to`. Returns the directory to use
/// and notices for the user.
pub(crate) fn relocate(from: &Path, to: &Path, data_dir: &Path) -> (PathBuf, Vec<String>) {
    match move_files(from, to) {
        Ok(kept) => (to.to_path_buf(), report_kept(from, to, data_dir, &kept)),
        Err(error) => {
            let present = |dir: &Path| FILES.iter().any(|name| dir.join(name).exists());
            let dir = if present(from) && !present(to) {
                from
            } else {
                to
            };
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

/// Returns the old files kept because the new directory has other ones.
fn move_files(from: &Path, to: &Path) -> Result<Vec<&'static str>, String> {
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
    let (mut moved, mut created, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    for name in FILES {
        let (old, new) = (from.join(name), to.join(name));
        let Some(content) =
            read(&old, name).map_err(|error| format!("Cannot read {name}: {error}"))?
        else {
            continue;
        };
        let placed = match read(&new, name) {
            Ok(Some(existing)) if existing == content => Ok(()),
            Ok(Some(_)) => {
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
    let mut left = !kept.is_empty();
    for old in moved {
        match fs::remove_file(&old) {
            Ok(()) => tracing::info!(
                "Moved {} to {}",
                old.display(),
                to.join(old.file_name().unwrap_or_default()).display()
            ),
            Err(error) => {
                // Its content is in place; the next start removes it.
                tracing::warn!("Cannot remove {}: {error}", old.display());
                left = true;
            }
        }
    }
    if !left {
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

/// Places `content`, read from `old`, at `new`, failing if `new` exists.
fn link_or_copy(old: &Path, new: &Path, name: &str, content: &[u8]) -> io::Result<()> {
    let dir = new.parent().unwrap_or(Path::new("."));
    if is_symlink(old) {
        // The new link points at the same file, not through the old directory.
        let target = fs::canonicalize(old)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, new)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(target, new)?;
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
            desktop_core::windows_acl::restrict_to_current_user(file)?;
        } else {
            file.set_permissions(permissions)?;
        }
        file.write_all(content)
    })
}

/// Reports old files the new directory overrides, once per data directory.
fn report_kept(from: &Path, to: &Path, data_dir: &Path, kept: &[&str]) -> Vec<String> {
    let marker = data_dir.join(REPORTED_FILE);
    if kept.is_empty() || marker.exists() {
        return Vec::new();
    }
    let files = kept.join(" and ");
    tracing::warn!(
        "{} still has {files} from an earlier version; {} has its own, which are used",
        from.display(),
        to.display()
    );
    let record = format!("{}\n", from.display());
    if let Err(error) = private_fs::write_atomic(&marker, &record, None) {
        tracing::warn!(
            "Cannot record that {} was reported: {error}",
            from.display()
        );
    }
    vec![format!(
        "Settings: {} still has {files} from an earlier version. The settings in {} are used; copy anything you still need from the old files, then delete them.",
        from.display(),
        to.display()
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "# Mine\nappearance = \"dark\" # night owl\n";
    const CREDENTIALS: &str = "# Keys\n[profiles.home]\napi-key = \"sk-home\"\n";

    struct Dirs {
        _root: tempfile::TempDir,
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
            _root: root,
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

    fn text(path: PathBuf) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn moves_both_files_unchanged_and_removes_the_old_directory() {
        let dirs = dirs();
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
        assert_eq!(
            private_fs::readable_by_others(&dirs.new.join(CREDENTIALS_FILE)).unwrap(),
            Some(false)
        );
        assert!(!dirs.old.exists());
        assert!(dirs.data.exists(), "state stays");

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
    fn existing_new_files_win_and_the_old_ones_are_reported_once() {
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

        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
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
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert!(!dirs.old.exists());
        assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
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
    fn a_linked_config_file_stays_linked_to_its_target() {
        let dirs = dirs();
        let target = dirs.data.join("dotfiles/config.toml");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, CONFIG).unwrap();
        fs::remove_file(dirs.old.join(CONFIG_FILE)).unwrap();
        std::os::unix::fs::symlink("../dotfiles/config.toml", dirs.old.join(CONFIG_FILE)).unwrap();

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
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert!(!dirs.new.exists());
    }
}
