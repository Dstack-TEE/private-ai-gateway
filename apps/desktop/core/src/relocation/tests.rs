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

#[test]
fn the_backend_and_early_readers_agree_when_a_move_fails_beside_a_kept_file() {
    let dirs = dirs();
    fs::create_dir_all(&dirs.new).unwrap();
    fs::write(dirs.new.join(CONFIG_FILE), "appearance = \"light\"\n").unwrap();
    assert_eq!(
        relocate(&dirs).1.len(),
        1,
        "the old config.toml is reported"
    );
    // The new copy is deleted, and an old credentials.toml cannot be read.
    fs::remove_file(dirs.new.join(CONFIG_FILE)).unwrap();
    fs::create_dir(dirs.old.join(CREDENTIALS_FILE)).unwrap();

    let (dir, notices) = relocate(&dirs);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with(FAILED));
    // The kept config.toml does not pull the backend back to the old directory...
    assert_eq!(dir, dirs.new);
    assert!(!in_use(&dirs.old, &dirs.new, &reported(&dirs.data)));
    assert_ne!(leftover(&dirs), Leftover::Pending);
    // ...and is never brought back.
    assert!(!dirs.new.join(CONFIG_FILE).exists());
    assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
}

#[test]
fn doctor_names_a_failed_move_while_the_backend_runs() {
    let dirs = dirs();
    fs::remove_file(dirs.old.join(CREDENTIALS_FILE)).unwrap();
    fs::create_dir(dirs.old.join(CREDENTIALS_FILE)).unwrap();
    let (dir, notices) = relocate(&dirs);
    assert_eq!(dir, dirs.old);
    let describe =
        |backend: Option<&[String]>| describe(&dirs.old, &dirs.new, &dirs.data, backend).unwrap();
    let failed = describe(Some(&notices));
    assert!(failed.starts_with("The settings files could not be moved"));
    assert!(failed.contains("Cannot read credentials.toml"), "{failed}");
    assert!(describe(None).contains("when it starts"));
    assert!(describe(Some(&[])).contains("restart it"));
}

#[test]
fn the_same_directory_spelled_differently_is_left_alone() {
    let dirs = dirs();
    let same = dirs.old.join("..").join("Config");
    assert_eq!(
        super::leftover(&dirs.old, &same, &dirs.data),
        Leftover::None
    );
    assert_eq!(
        super::relocate(&dirs.old, &same, &dirs.data),
        (same.clone(), Vec::new())
    );
    assert_eq!(text(dirs.old.join(CONFIG_FILE)), CONFIG);
    assert_eq!(text(dirs.old.join(CREDENTIALS_FILE)), CREDENTIALS);
}

#[cfg(unix)]
#[test]
fn a_new_credentials_file_linked_to_the_old_one_is_not_reported() {
    let dirs = dirs();
    fs::create_dir_all(&dirs.new).unwrap();
    std::os::unix::fs::symlink(
        dirs.old.join(CREDENTIALS_FILE),
        dirs.new.join(CREDENTIALS_FILE),
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
        assert_eq!(leftover(&dirs), Leftover::None);
    }
    assert_eq!(text(dirs.old.join(CREDENTIALS_FILE)), CREDENTIALS);
    assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
    assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
}

#[cfg(unix)]
#[test]
fn an_old_directory_linked_elsewhere_stays_in_use_and_is_reported_once() {
    let dirs = dirs();
    let dotfiles = dirs.root.path().join("dotfiles/private-ai-proxy");
    fs::create_dir_all(dotfiles.parent().unwrap()).unwrap();
    fs::rename(&dirs.old, &dotfiles).unwrap();
    std::os::unix::fs::symlink(&dotfiles, &dirs.old).unwrap();

    let (dir, notices) = relocate(&dirs);
    assert_eq!(dir, dirs.old);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(leftover(&dirs), Leftover::Linked);
    // Edits to the files in use do not report it again.
    fs::write(dotfiles.join(CONFIG_FILE), "appearance = \"light\"\n").unwrap();
    assert_eq!(relocate(&dirs), (dirs.old.clone(), Vec::new()));
    assert!(!dirs.new.exists(), "nothing is moved out of it");

    // Once the new directory has settings, they are used and the old files reported once.
    fs::create_dir_all(&dirs.new).unwrap();
    fs::write(dirs.new.join(CONFIG_FILE), CONFIG).unwrap();
    let (dir, notices) = relocate(&dirs);
    assert_eq!(dir, dirs.new);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
    assert_eq!(
        leftover(&dirs),
        Leftover::Kept(vec![CONFIG_FILE, CREDENTIALS_FILE])
    );
    assert_eq!(text(dotfiles.join(CREDENTIALS_FILE)), CREDENTIALS);
}

#[cfg(unix)]
#[test]
fn a_redundant_old_link_to_the_same_dotfile_is_removed() {
    // Both config.toml files link to the dotfile, as an interrupted move leaves them.
    let dirs = dirs();
    let target = dirs.root.path().join("dotfiles/config.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, CONFIG).unwrap();
    fs::remove_file(dirs.old.join(CONFIG_FILE)).unwrap();
    std::os::unix::fs::symlink(&target, dirs.old.join(CONFIG_FILE)).unwrap();
    fs::create_dir_all(&dirs.new).unwrap();
    std::os::unix::fs::symlink(&target, dirs.new.join(CONFIG_FILE)).unwrap();

    assert_eq!(relocate(&dirs), (dirs.new.clone(), Vec::new()));
    assert!(
        !dirs.old.exists(),
        "the old link and the empty directory are gone"
    );
    assert!(is_symlink(&dirs.new.join(CONFIG_FILE)));
    assert_eq!(text(dirs.new.join(CONFIG_FILE)), CONFIG);
    assert_eq!(text(target), CONFIG);
    assert_eq!(text(dirs.new.join(CREDENTIALS_FILE)), CREDENTIALS);
}

#[cfg(unix)]
#[test]
fn only_links_that_go_through_the_old_file_count_as_resolving_through_it() {
    let dirs = dirs();
    let old = dirs.old.join(CONFIG_FILE);
    fs::create_dir_all(&dirs.new).unwrap();
    let direct = dirs.new.join("direct.toml");
    std::os::unix::fs::symlink(&old, &direct).unwrap();
    // Through a linked directory and a relative hop.
    let linked_dir = dirs.root.path().join("linked");
    std::os::unix::fs::symlink(&dirs.old, &linked_dir).unwrap();
    let indirect = dirs.new.join("indirect.toml");
    std::os::unix::fs::symlink("../../linked/config.toml", &indirect).unwrap();
    let hard = dirs.new.join("hard.toml");
    fs::hard_link(&old, &hard).unwrap();

    assert!(resolves_through(&direct, &old));
    assert!(resolves_through(&indirect, &old));
    assert!(!resolves_through(&hard, &old));
    assert!(same_file(&hard, &old) && same_file(&indirect, &old));
}
