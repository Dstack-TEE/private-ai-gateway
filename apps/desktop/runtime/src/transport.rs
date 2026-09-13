//! Authenticated, per-user local IPC for the desktop backend.
//!
//! The listener is synchronous so the service can run it in `spawn_blocking`.
//! Callers should set it nonblocking and poll `accept` with their shutdown
//! signal. Streams implement `Read` and `Write`; frame code must set finite
//! read and write timeouts before exchanging authenticated control messages.

use std::{io, path::PathBuf};

use desktop_gateway::agents::app_data_dir;

#[cfg(unix)]
#[path = "transport/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "transport/windows.rs"]
mod platform;

#[cfg(not(any(unix, windows)))]
compile_error!("desktop local IPC is supported only on Unix and Windows");

pub use platform::{Listener, Stream};

const ENDPOINT_HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const ENDPOINT_HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Resolve the endpoint used by both the backend and its local clients.
pub fn endpoint_path() -> io::Result<PathBuf> {
    let data_dir = app_data_dir().map_err(io::Error::other)?;
    if !data_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the app data directory must be absolute",
        ));
    }
    platform::endpoint_path(&data_dir)
}

fn endpoint_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(ENDPOINT_HASH_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(ENDPOINT_HASH_PRIME)
    })
}
