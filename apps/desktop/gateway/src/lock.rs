//! Per-user OS file locks (flock / LockFileEx via `fd-lock`).
//!
//! `instance` decides which process is the primary app instance before the
//! endpoint is claimed, so a process that lost the port can never become the
//! primary. `with_apply_lock` serializes agent-config transactions across
//! processes: the lock is held from the revision check through the final
//! rename and manifest update.

use std::{fs, io, path::Path};

use fd_lock::{RwLock, RwLockWriteGuard};

use crate::tokens::create_private_dir;

fn open(dir: &Path, name: &str) -> io::Result<RwLock<fs::File>> {
    create_private_dir(dir)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(name))?;
    Ok(RwLock::new(file))
}

/// The primary-instance lock, held for the rest of the process lifetime.
pub struct InstanceLock {
    _guard: RwLockWriteGuard<'static, fs::File>,
}

/// Try to become the primary instance; `None` when another process holds it.
/// The lock file handle is intentionally leaked so the guard can live as long
/// as the process.
pub fn instance(data_dir: &Path) -> io::Result<Option<InstanceLock>> {
    let lock: &'static mut RwLock<fs::File> = Box::leak(Box::new(open(data_dir, "instance.lock")?));
    match lock.try_write() {
        Ok(guard) => Ok(Some(InstanceLock { _guard: guard })),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

/// Run `f` while holding the exclusive apply lock; blocks until it is free.
pub fn with_apply_lock<T>(
    data_dir: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let mut lock = open(data_dir, "apply.lock")
        .map_err(|error| format!("Cannot open the agent config lock: {error}"))?;
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
        let dir = std::env::temp_dir().join(format!("pag-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let first = instance(&dir).unwrap();
        assert!(first.is_some());
        // A second independent handle (as a second process would open) loses.
        assert!(instance(&dir).unwrap().is_none());
        drop(first);
        assert!(instance(&dir).unwrap().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_lock_serializes_writers() {
        let dir = std::env::temp_dir().join(format!("pag-apply-lock-{}", std::process::id()));
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
