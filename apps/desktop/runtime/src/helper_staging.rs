//! A durable, current-user-owned helper for Unix agent integrations.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub fn stage(bundled: &Path, app_data: &Path) -> io::Result<PathBuf> {
    let uid = rustix::process::getuid().as_raw();
    let mut source = File::open(bundled)?;
    let metadata = source.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the bundled credential helper is not a nonempty executable file",
        ));
    }

    let directory = app_data.join("helpers");
    desktop_gateway::tokens::create_private_dir(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 || metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the helper directory must be private, owned by the current user, and not a symlink",
        ));
    }
    let destination = directory.join(desktop_gateway::agents::helper_binary_name());
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the helper destination must be a regular file, not a symlink",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // One same-filesystem temporary file, removed on failure by RAII. Rename
    // also lets an already-running helper finish using the previous inode.
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    if temporary.as_file().metadata()?.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the staged helper must be owned by the current user",
        ));
    }
    io::copy(&mut source, temporary.as_file_mut())?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o700))?;
    temporary.as_file().sync_all()?;
    temporary.persist(&destination)?;
    File::open(&directory)?.sync_all()?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::symlink;
    use std::process::Command;

    fn bundled(root: &Path, name: &str, output: &str) -> PathBuf {
        let directory = root.join(name);
        fs::create_dir(&directory).unwrap();
        let path = directory.join(desktop_gateway::agents::helper_binary_name());
        fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{output}'\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn survives_unmount_and_replaces_at_the_same_private_path() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("app data");
        let first = bundled(root.path(), "mount-one", "first");
        let stable = stage(&first, &data).unwrap();
        assert_eq!(
            stable,
            data.join("helpers").join(first.file_name().unwrap())
        );
        fs::remove_dir_all(first.parent().unwrap()).unwrap();
        let output = Command::new(&stable).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"first");
        let mut previous = File::open(&stable).unwrap();
        let second = bundled(root.path(), "mount-two", "second");
        assert_eq!(stage(&second, &data).unwrap(), stable);
        let mut old = String::new();
        previous.read_to_string(&mut old).unwrap();
        assert!(old.contains("first"));
        assert_eq!(Command::new(&stable).output().unwrap().stdout, b"second");
        for path in [&stable, stable.parent().unwrap()] {
            assert_eq!(
                fs::metadata(path).unwrap().uid(),
                rustix::process::getuid().as_raw()
            );
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(fs::read_dir(stable.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn invalid_bundle_preserves_the_installed_helper() {
        let root = tempfile::tempdir().unwrap();
        let source = bundled(root.path(), "mount", "original");
        let data = root.path().join("data");
        let stable = stage(&source, &data).unwrap();
        let original = fs::read(&stable).unwrap();
        fs::write(&source, "").unwrap();
        assert!(stage(&source, &data).is_err());
        fs::remove_file(&source).unwrap();
        assert!(stage(&source, &data).is_err());
        assert_eq!(fs::read(&stable).unwrap(), original);
        assert_eq!(fs::read_dir(stable.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn refuses_redirected_or_shared_destinations() {
        let root = tempfile::tempdir().unwrap();
        let source = bundled(root.path(), "mount", "helper");
        let data = root.path().join("data");
        fs::create_dir(&data).unwrap();
        let external = root.path().join("external");
        fs::create_dir(&external).unwrap();
        symlink(&external, data.join("helpers")).unwrap();
        assert!(stage(&source, &data).is_err());
        assert_eq!(fs::read_dir(&external).unwrap().count(), 0);
        fs::remove_file(data.join("helpers")).unwrap();
        let stable = stage(&source, &data).unwrap();
        fs::remove_file(&stable).unwrap();
        let external_file = external.join("user-file");
        fs::write(&external_file, "untouched").unwrap();
        symlink(&external_file, &stable).unwrap();
        assert!(stage(&source, &data).is_err());
        assert_eq!(fs::read_to_string(external_file).unwrap(), "untouched");
        fs::remove_file(stable).unwrap();
        fs::set_permissions(data.join("helpers"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(stage(&source, &data).is_err());
    }

    #[test]
    #[ignore = "requires root in a disposable Unix container to exercise distinct file owners"]
    fn stages_root_owned_source_but_refuses_root_owned_directory() {
        use std::os::unix::{fs::chown, process::CommandExt};

        const CHILD_ROOT: &str = "HELPER_STAGING_OWNERSHIP_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let source = root
                .join("mount")
                .join(desktop_gateway::agents::helper_binary_name());
            assert_eq!(fs::metadata(&source).unwrap().uid(), 0);
            let stable = stage(&source, &root.join("user-data")).unwrap();
            assert_eq!(
                fs::metadata(stable).unwrap().uid(),
                rustix::process::getuid().as_raw()
            );
            let error = stage(&source, &root.join("root-data")).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("owned by the current user"));
            return;
        }

        assert_eq!(rustix::process::getuid().as_raw(), 0);
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        bundled(root.path(), "mount", "root-owned");
        let user_data = root.path().join("user-data");
        fs::create_dir(&user_data).unwrap();
        chown(&user_data, Some(65534), Some(65534)).unwrap();
        let root_directory = root.path().join("root-data").join("helpers");
        desktop_gateway::tokens::create_private_dir(&root_directory).unwrap();
        fs::set_permissions(
            root_directory.parent().unwrap(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "stages_root_owned_source_but_refuses_root_owned_directory",
            ])
            .env(CHILD_ROOT, root.path())
            .uid(65534)
            .gid(65534)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(fs::read_dir(root_directory).unwrap().count(), 0);
    }
}
