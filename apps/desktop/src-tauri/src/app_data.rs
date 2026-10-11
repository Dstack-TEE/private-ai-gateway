use std::{fs, io, os::unix::fs::DirBuilderExt, path::Path};

pub fn prepare(directory: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    restrict_to_owner(directory)
}

fn restrict_to_owner(directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(directory)?;
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the app data directory belongs to another user",
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn prepares_existing_app_data_as_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("app-data");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();

        prepare(&directory).unwrap();

        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
