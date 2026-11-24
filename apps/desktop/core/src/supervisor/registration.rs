use std::{fs, io, io::Write, path::Path};

use crate::private_fs::{create_private_dir, publish, Publish};

pub(super) fn foreign(path: &Path) -> String {
    format!(
        "{} belongs to another installation or was customized",
        path.display()
    )
}

/// Generated registrations must match this installation, including their
/// command path. In particular, a marker alone never grants ownership.
pub(super) fn exists(path: &Path, contents: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(foreign(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
    }
    let current =
        fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    if current == contents.as_bytes() {
        Ok(true)
    } else {
        Err(foreign(path))
    }
}

pub(super) fn create(path: &Path, contents: &str) -> Result<(), String> {
    let directory = path.parent().ok_or("The registration has no directory")?;
    create_private_dir(directory)
        .and_then(|()| {
            publish(path, Publish::NoClobber, |file| {
                file.write_all(contents.as_bytes())
            })
        })
        .map_err(|error| format!("Cannot create {}: {error}", path.display()))
}

pub(super) fn remove(path: &Path, contents: &str) -> Result<(), String> {
    if exists(path, contents)? {
        fs::remove_file(path)
            .map_err(|error| format!("Cannot remove {}: {error}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registration_for_a_different_command_is_never_replaced_or_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registration");
        let other = "PAP generated\n/previous-prefix/bin/pap service run\n";
        let expected = "PAP generated\n/new-prefix/bin/pap service run\n";
        create(&path, other).unwrap();
        assert!(create(&path, expected).is_err());
        assert!(remove(&path, expected).is_err());
        assert_eq!(fs::read(&path).unwrap(), other.as_bytes());
        remove(&path, other).unwrap();
        assert!(!path.exists());
    }
}
