//! Per-user OS file locks (`std::fs::File::lock`: flock / LockFileEx).
//!
//! `instance` decides which process is the primary app instance before the
//! endpoint is claimed, so a process that lost the port can never become the
//! primary. The startup gate keeps installers from replacing a backend that a
//! client is starting. `with_apply_lock` serializes agent-config transactions across
//! processes: the lock is held from the revision check through the final
//! rename and manifest update.

use std::{fmt, fs, io, path::Path};

use crate::private_fs::create_private_dir;

fn open(dir: &Path, name: &str) -> io::Result<fs::File> {
    create_private_dir(dir)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(name))?;
    Ok(file)
}

/// The primary-instance lock, held for the rest of the process lifetime.
pub struct InstanceLock {
    _file: fs::File,
}

/// The startup gate, a reader-writer lock (`flock(2)` `LOCK_SH`/`LOCK_EX`,
/// `LockFileEx`): clients starting the backend hold it shared, so they never
/// wait for each other (the instance lock picks one backend, as gpg-agent's
/// socket does); an installer or updater replacing the backend holds it
/// exclusively and starts only once no client is starting one.
pub struct StartupLock {
    _file: fs::File,
}

/// Take the gate to start the backend; `None` while an installer holds it.
pub fn startup_shared(data_dir: &Path) -> io::Result<Option<StartupLock>> {
    let file = open(data_dir, "startup.lock")?;
    match file.try_lock_shared() {
        Ok(()) => Ok(Some(StartupLock { _file: file })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Take the gate to replace the backend; `None` while anyone else holds it.
pub fn startup(data_dir: &Path) -> io::Result<Option<StartupLock>> {
    let file = open(data_dir, "startup.lock")?;
    match file.try_lock() {
        Ok(()) => Ok(Some(StartupLock { _file: file })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Try to become the primary instance; `None` when another process holds it.
/// Closing the owned file releases the lock, including on initialization failure.
pub fn instance(data_dir: &Path) -> io::Result<Option<InstanceLock>> {
    let file = open(data_dir, "instance.lock")?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Failure to open or acquire the transaction lock, separate from operation errors.
#[derive(Debug)]
pub struct ApplyLockError(io::Error);

impl fmt::Display for ApplyLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cannot lock the agent configurations: {}", self.0)
    }
}

impl std::error::Error for ApplyLockError {}

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
