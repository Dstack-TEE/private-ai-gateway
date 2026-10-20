//! Per-user OS file locks (`std::fs::File::lock`: flock / LockFileEx).
//!
//! `instance` decides which process is the primary app instance before the
//! endpoint is claimed, so a process that lost the port can never become the
//! primary. The startup gate keeps installers from replacing a backend that a
//! client is starting. `with_apply_lock` serializes agent-config transactions across
//! processes: the lock is held from the revision check through the final
//! rename and manifest update.

use std::{
    fs, io,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::private_fs::{create_or_repair_private_dir, create_private_dir};

/// The wait for a just-released instance lock, needed on Windows.
pub const INSTANCE_RELEASE_WAIT: Duration = Duration::from_secs(2);

fn open(dir: &Path, name: &str) -> io::Result<fs::File> {
    create_private_dir(dir)?;
    let path = dir.join(name);
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let result = options.open(&path);
    #[cfg(unix)]
    let result = result.map_err(|error| {
        if error.raw_os_error() == Some(libc::ELOOP) {
            return io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("lock file {} must not be a symlink", path.display()),
            );
        }
        error
    });
    let file = result?;
    #[cfg(unix)]
    validate_lock_file(&file, &path)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_lock_file(file: &fs::File, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "lock file {} must be a regular file owned by the current user",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// The primary-instance lock, held for the rest of the process lifetime.
#[derive(Debug)]
pub struct InstanceLock {
    _file: Arc<fs::File>,
}

#[cfg(unix)]
impl InstanceLock {
    /// Keep ownership for the listener's lifetime, including socket recovery.
    pub(crate) fn retain(&self) -> Self {
        Self {
            _file: self._file.clone(),
        }
    }
}

#[cfg(unix)]
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct InstanceOwner {
    pub process_id: u32,
    pub endpoint: String,
}

/// Diagnostic metadata only; the OS lock remains the authority for ownership.
#[cfg(unix)]
pub(crate) fn instance_owner(data_dir: &Path) -> io::Result<Option<InstanceOwner>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let path = data_dir.join("instance.lock");
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    validate_lock_file(&file, &path)?;
    if taken(file.try_lock_shared())? {
        return Ok(None);
    }
    serde_json::from_reader(file.take(4096))
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The startup gate, a reader-writer lock (`flock(2)` `LOCK_SH`/`LOCK_EX`,
/// `LockFileEx`): clients starting the backend hold it shared, so they never
/// wait for each other (the instance lock picks one backend, as gpg-agent's
/// socket does); an installer or updater replacing the backend holds it
/// exclusively and starts only once no client is starting one.
pub struct StartupLock {
    _file: fs::File,
}

/// Whether a `try_lock` took the lock; `false` while another holder has it.
fn taken(result: Result<(), fs::TryLockError>) -> io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Take the gate to start the backend; `None` while an installer holds it.
pub fn startup_shared(data_dir: &Path) -> io::Result<Option<StartupLock>> {
    let file = open(data_dir, "startup.lock")?;
    Ok(taken(file.try_lock_shared())?.then_some(StartupLock { _file: file }))
}

/// Take the gate to replace the backend; `None` while anyone else holds it.
pub fn startup(data_dir: &Path) -> io::Result<Option<StartupLock>> {
    let file = open(data_dir, "startup.lock")?;
    Ok(taken(file.try_lock())?.then_some(StartupLock { _file: file }))
}

/// Try to become the primary instance; `None` when another process holds it.
/// Closing the owned file releases the lock, including on initialization failure.
pub fn instance(data_dir: &Path) -> io::Result<Option<InstanceLock>> {
    create_or_repair_private_dir(data_dir)?;
    let file = open(data_dir, "instance.lock")?;
    if !taken(file.try_lock())? {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        let mut writer = &file;

        let owner = InstanceOwner {
            process_id: std::process::id(),
            endpoint: crate::transport::endpoint_path_in(data_dir)?
                .display()
                .to_string(),
        };
        let metadata = serde_json::to_vec(&owner)?;
        file.set_len(0)?;
        writer.write_all(&metadata)?;
    }
    Ok(Some(InstanceLock {
        _file: Arc::new(file),
    }))
}

/// [`instance`], waiting up to `wait` for a holder to release it: Windows may
/// release a `LockFileEx` lock shortly after its process exits.
pub fn instance_within(data_dir: &Path, wait: Duration) -> io::Result<Option<InstanceLock>> {
    let deadline = Instant::now() + wait;
    loop {
        let lock = instance(data_dir)?;
        if lock.is_some() || Instant::now() >= deadline {
            return Ok(lock);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Failure to open or acquire the transaction lock, separate from operation errors.
#[derive(Debug, thiserror::Error)]
#[error("Cannot lock the agent configurations: {0}")]
pub struct ApplyLockError(io::Error);

impl From<ApplyLockError> for String {
    fn from(error: ApplyLockError) -> Self {
        error.to_string()
    }
}

/// Run `f` while holding the exclusive apply lock; blocks until it is free.
pub fn with_apply_lock<T, E: From<ApplyLockError>>(
    data_dir: &Path,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    // Closing the file releases the lock.
    let file = open(data_dir, "apply.lock").map_err(ApplyLockError)?;
    file.lock().map_err(ApplyLockError)?;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn only_the_backend_instance_repairs_the_private_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        drop(startup_shared(temp.path()).unwrap().unwrap());
        drop(eventually(|| startup(temp.path()).unwrap()));
        with_apply_lock(temp.path(), || Ok::<_, String>(())).unwrap();
        assert_eq!(
            fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let _owner = instance(temp.path()).unwrap().unwrap();
        assert_eq!(
            fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_instance_lock_never_changes_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let victim = temp.path().join("victim");
        fs::write(&victim, "untouched").unwrap();
        symlink(&victim, temp.path().join("instance.lock")).unwrap();
        let error = instance(temp.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("must not be a symlink"));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(instance_owner(temp.path()).is_err());
    }

    /// Waits for `acquire` to succeed. Files are opened `O_CLOEXEC`, but a
    /// child another test is spawning shares their locks between its fork and
    /// exec (`flock(2)` locks belong to the open file description), so a
    /// released lock may be taken again only after a moment.
    fn eventually<T>(mut acquire: impl FnMut() -> Option<T>) -> T {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(lock) = acquire() {
                return lock;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "lock was not released"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn instance_lock_is_exclusive_across_handles() {
        let dir = tempfile::tempdir().unwrap();
        let first = instance(dir.path()).unwrap();
        assert!(first.is_some());
        // A second independent handle (as a second process would open) loses.
        assert!(instance(dir.path()).unwrap().is_none());
        drop(first);
        eventually(|| instance(dir.path()).unwrap());
    }

    #[test]
    fn instance_within_waits_for_a_release() {
        let dir = tempfile::tempdir().unwrap();
        let held = instance(dir.path()).unwrap().unwrap();
        assert!(instance_within(dir.path(), Duration::from_millis(100))
            .unwrap()
            .is_none());
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        assert!(instance_within(dir.path(), Duration::from_secs(5))
            .unwrap()
            .is_some());
        releaser.join().unwrap();
    }

    #[test]
    fn clients_share_the_startup_gate_and_installers_exclude_them() {
        let dir = tempfile::tempdir().unwrap();
        let first = eventually(|| startup_shared(dir.path()).unwrap());
        let second = startup_shared(dir.path()).unwrap().unwrap();
        assert!(startup(dir.path()).unwrap().is_none());
        drop((first, second));
        let installer = eventually(|| startup(dir.path()).unwrap());
        assert!(startup_shared(dir.path()).unwrap().is_none());
        drop(installer);
    }

    #[test]
    fn apply_lock_serializes_writers() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("data");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder_dir = dir.clone();
        let holder = std::thread::spawn(move || {
            with_apply_lock(&holder_dir, || {
                release_rx.recv().unwrap();
                Ok::<_, String>(())
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let waiter_dir = dir.clone();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            with_apply_lock(&waiter_dir, || Ok::<_, String>(started.elapsed()))
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        release_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();
        assert!(waiter.join().unwrap().unwrap() >= std::time::Duration::from_millis(100));
    }
}
