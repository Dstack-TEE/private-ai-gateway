use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::private_fs::{create_private_dir, publish, write_private_atomic, Publish};

pub(super) fn foreign(path: &Path) -> String {
    format!(
        "{} belongs to another installation or was customized",
        path.display()
    )
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Ownership {
    path: PathBuf,
    device: u64,
    inode: u64,
    changed: (i64, i64),
}

impl Ownership {
    fn new(path: &Path, metadata: &fs::Metadata) -> Self {
        Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

pub(super) fn record_path() -> Result<PathBuf, String> {
    Ok(crate::paths::app_data_dir()?.join("login-service-registration.json"))
}

/// Ownership requires a matching receipt and the exact generated contents.
pub(super) fn exists(record: &Path, path: &Path, contents: &str) -> Result<bool, String> {
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
    let owned = fs::read(record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Ownership>(&bytes).ok())
        .is_some_and(|receipt| receipt == Ownership::new(path, &metadata));
    if !metadata.is_file() || !owned {
        return Err(format!(
            "{} has no PAP ownership record for this file",
            path.display()
        ));
    }
    let mut current = Vec::new();
    file.read_to_end(&mut current)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    if current == contents.as_bytes() {
        Ok(true)
    } else {
        Err(foreign(path))
    }
}

pub(super) fn create(record: &Path, path: &Path, contents: &str) -> Result<(), String> {
    let directory = path.parent().ok_or("The registration has no directory")?;
    let mut created = None;
    create_private_dir(directory)
        .and_then(|()| {
            publish(path, Publish::NoClobber, |file| {
                file.write_all(contents.as_bytes())?;
                created = Some(file.try_clone()?);
                Ok(())
            })
        })
        .map_err(|error| format!("Cannot create {}: {error}", path.display()))?;

    let metadata = created
        .ok_or("Cannot identify the newly created registration")?
        .metadata()
        .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
    let ownership = Ownership::new(path, &metadata);
    let receipt = serde_json::to_string(&ownership)
        .map_err(io::Error::other)
        .and_then(|receipt| write_private_atomic(record, &receipt, None));
    if let Err(error) = receipt {
        if fs::symlink_metadata(path)
            .is_ok_and(|metadata| Ownership::new(path, &metadata) == ownership)
        {
            let _ = fs::remove_file(path);
        }
        return Err(format!(
            "Cannot record ownership of {}: {error}",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn remove(record: &Path, path: &Path, contents: &str) -> Result<(), String> {
    if exists(record, path, contents)? {
        fs::remove_file(path)
            .map_err(|error| format!("Cannot remove {}: {error}", path.display()))?;
        match fs::remove_file(record) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot remove {}: {error}", record.display())),
        }
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
        let record = directory.path().join("receipt");
        let other = "PAP generated\n/previous-prefix/bin/pap service run\n";
        let expected = "PAP generated\n/new-prefix/bin/pap service run\n";
        create(&record, &path, other).unwrap();
        assert!(create(&record, &path, expected).is_err());
        assert!(remove(&record, &path, expected).is_err());
        assert_eq!(fs::read(&path).unwrap(), other.as_bytes());
        remove(&record, &path, other).unwrap();
        assert!(!path.exists());
        assert!(!record.exists());
    }

    #[test]
    fn an_identical_registration_without_its_own_receipt_is_foreign() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registration");
        let record = directory.path().join("receipt");
        let contents = "PAP generated\n/path/to/pap service run\n";
        fs::write(&path, contents).unwrap();
        assert!(exists(&record, &path, contents).is_err());
        assert!(create(&record, &path, contents).is_err());
        assert!(remove(&record, &path, contents).is_err());
        assert!(!record.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), contents);

        fs::remove_file(&path).unwrap();
        create(&record, &path, contents).unwrap();
        let replacement = directory.path().join("replacement");
        fs::write(&replacement, contents).unwrap();
        fs::rename(replacement, &path).unwrap();
        assert!(remove(&record, &path, contents).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), contents);
    }
}
