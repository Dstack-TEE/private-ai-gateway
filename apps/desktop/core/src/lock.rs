//! Per-user OS file locks (flock / LockFileEx via `fd-lock`).
//!
//! `instance` decides which process is the primary app instance before the
//! endpoint is claimed, so a process that lost the port can never become the
//! primary. `with_apply_lock` serializes agent-config transactions across
//! processes: the lock is held from the revision check through the final
//! rename and manifest update.

use std::{fmt, fs, io, path::Path};

use fd_lock::RwLock;

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

/// Coordinates client-driven startup with installer replacement before spawn.
pub struct StartupLock {
    _file: fs::File,
}

pub fn startup(data_dir: &Path) -> io::Result<Option<StartupLock>> {
    let file = open(data_dir, "startup.lock")?;
    match file.try_lock() {
        Ok(()) => Ok(Some(StartupLock { _file: file })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Held beside the startup lock by a client that is starting the backend, so
/// clients waiting on the startup lock can tell a slow start (worth waiting
/// for) from an installer or updater holding the gate (reported at once).
pub struct StartingLock {
    _file: fs::File,
}

/// Mark this startup-lock holder as starting the backend. Blocks only while
/// another client briefly probes `start_in_progress`.
pub fn starting(data_dir: &Path) -> io::Result<StartingLock> {
    let file = open(data_dir, "starting.lock")?;
    file.lock()?;
    Ok(StartingLock { _file: file })
}

/// Whether the startup lock is held by a client starting the backend rather
/// than by an installer or updater, which never take `starting`.
pub fn start_in_progress(data_dir: &Path) -> io::Result<bool> {
    let file = open(data_dir, "starting.lock")?;
    match file.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(fs::TryLockError::WouldBlock) => Ok(true),
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
    let mut lock = RwLock::new(open(data_dir, "apply.lock").map_err(ApplyLockError)?);
    let _guard = lock.write().map_err(ApplyLockError)?;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_lock_is_exclusive_across_handles() {
        let dir = tempfile::tempdir().unwrap();
        let first = instance(dir.path()).unwrap();
        assert!(first.is_some());
        // A second independent handle (as a second process would open) loses.
        assert!(instance(dir.path()).unwrap().is_none());
        drop(first);
        // A child that another test is spawning holds an inherited copy of the
        // descriptor until its exec closes it, so release may lag briefly.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while instance(dir.path()).unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "lock was not released"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn starting_marks_only_client_starts() {
        let dir = tempfile::tempdir().unwrap();
        let gate = startup(dir.path()).unwrap().unwrap();
        assert!(!start_in_progress(dir.path()).unwrap());
        let marker = starting(dir.path()).unwrap();
        assert!(start_in_progress(dir.path()).unwrap());
        // Probes are shared, so they never block each other.
        assert!(start_in_progress(dir.path()).unwrap());
        drop(marker);
        drop(gate);
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
