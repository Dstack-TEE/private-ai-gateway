//! The client side of Private AI Proxy, shared by the desktop shell, the CLI
//! and the backend: renderer and IPC contracts, the management protocol and
//! its client, the IPC transport, backend launch, the settings file, app paths and
//! owner-only file primitives. It links no server or database; its only HTTP
//! client is the release-channel update check.

use std::io::Write;

pub mod account;
pub mod agent_access;
pub mod agents;
pub mod brand;
pub mod client;
pub mod config;
pub mod contracts;
pub mod launch;
pub mod listen;
pub mod lock;
pub mod maintenance;
pub mod paths;
pub mod private_fs;
pub mod protocol;
pub mod transport;
pub mod ui_api;
pub mod updates;
pub mod usage;

/// Seconds since the Unix epoch; 0 if the system clock is set before it.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Write a best-effort diagnostic line to stderr, formatted like `eprintln!`.
///
/// Unlike `eprintln!`, a closed or detached stderr (a GUI process, a service
/// whose log pipe went away) never turns the write into a panic.
#[macro_export]
macro_rules! diagnostic {
    ($($arg:tt)*) => {
        $crate::diagnostic(::std::format_args!($($arg)*))
    };
}

/// The function behind [`diagnostic!`]; call the macro instead.
#[doc(hidden)]
pub fn diagnostic(args: std::fmt::Arguments<'_>) {
    let stderr = std::io::stderr();
    diagnostic_to(&mut stderr.lock(), args);
}

fn diagnostic_to(writer: &mut impl Write, args: std::fmt::Arguments<'_>) {
    let _ = writeln!(writer, "{args}");
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "reader closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn diagnostic_ignores_a_closed_stderr_pipe() {
        diagnostic_to(&mut ClosedPipe, format_args!("backend keeps serving"));
    }
}
