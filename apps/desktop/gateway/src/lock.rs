//! Per-user OS file locks (flock / LockFileEx via `fd-lock`).
//!
//! `instance` decides which process is the primary app instance before the
//! endpoint is claimed, so a process that lost the port can never become the
//! primary. `with_apply_lock` serializes agent-config transactions across
//! processes: the lock is held from the revision check through the final
//! rename and manifest update.

use std::{fs, io, path::Path};

use fd_lock::RwLock;

use crate::tokens::create_private_dir;

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

/// Run `f` while holding the exclusive apply lock; blocks until it is free.
pub fn with_apply_lock<T>(
    data_dir: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let mut lock = RwLock::new(
        open(data_dir, "apply.lock")
            .map_err(|error| format!("Cannot open the agent config lock: {error}"))?,
    );
    let _guard = lock
        .write()
        .map_err(|error| format!("Cannot take the agent config lock: {error}"))?;
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
        assert!(instance(dir.path()).unwrap().is_some());
    }

    #[test]
    fn instance_lock_excludes_legacy_fd_lock_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let mut legacy = RwLock::new(open(dir.path(), "instance.lock").unwrap());
        let guard = legacy.try_write().unwrap();
        assert!(instance(dir.path()).unwrap().is_none());
        drop(guard);
        let current = instance(dir.path()).unwrap().unwrap();
        assert_eq!(
            legacy.try_write().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(current);
        assert!(legacy.try_write().is_ok());
    }

    #[test]
    fn apply_lock_serializes_writers() {
        let dir = std::env::temp_dir().join(format!("pap-apply-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder_dir = dir.clone();
        let holder = std::thread::spawn(move || {
            with_apply_lock(&holder_dir, || {
                release_rx.recv().unwrap();
                Ok(())
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let waiter_dir = dir.clone();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            with_apply_lock(&waiter_dir, || Ok(started.elapsed()))
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        release_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();
        assert!(waiter.join().unwrap().unwrap() >= std::time::Duration::from_millis(100));
        let _ = fs::remove_dir_all(&dir);
    }
}
