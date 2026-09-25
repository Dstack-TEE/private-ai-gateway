//! The authenticated, per-user local endpoint of the management API: a Unix
//! domain socket or a Windows named pipe. The service serves HTTP on the
//! [`Listener`]; clients [`connect`] and speak HTTP/1.1 over the stream.

use std::{io, path::PathBuf};

use crate::paths::app_data_dir;

#[cfg(unix)]
#[path = "transport/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "transport/windows.rs"]
mod platform;

#[cfg(not(any(unix, windows)))]
compile_error!("desktop local IPC is supported only on Unix and Windows");

pub use platform::{connect, ClientStream, Listener, Stream};

const ENDPOINT_HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const ENDPOINT_HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Resolve the endpoint used by both the backend and its local clients.
pub fn endpoint_path() -> io::Result<PathBuf> {
    platform::endpoint_path(&data_dir()?, platform::SOCKET_FILE)
}

/// The endpoint where a 0.1.4 to 0.2 beta backend answers its NDJSON protocol.
pub fn legacy_endpoint_path() -> io::Result<PathBuf> {
    platform::endpoint_path(&data_dir()?, platform::LEGACY_SOCKET_FILE)
}

fn data_dir() -> io::Result<PathBuf> {
    let data_dir = app_data_dir().map_err(io::Error::other)?;
    if !data_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the app data directory must be absolute",
        ));
    }
    Ok(data_dir)
}

fn endpoint_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(ENDPOINT_HASH_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(ENDPOINT_HASH_PRIME)
    })
}
