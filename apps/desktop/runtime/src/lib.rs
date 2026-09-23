//! Platform-neutral desktop state and persistence.
//!
//! The Tauri shell owns window, menu, tray, clipboard, file-picker, and
//! autostart integration. This crate owns product state and policy.

use std::io::Write;

pub mod account_login;
pub mod agent_access;
mod balance_cache;
pub mod cli_install;
pub mod client;
pub mod contracts;
pub mod controller;
mod endpoint_inventory;
#[cfg(all(unix, not(all(target_os = "macos", feature = "mac-app-store"))))]
mod helper_staging;
pub mod launch;
pub mod listen;
pub mod local_api;
pub mod maintenance;
pub mod power;
pub mod preferences;
pub mod protocol;
mod recovery;
pub mod server;
pub mod service_config;
pub mod transport;
pub mod ui_api;
pub mod updates;
pub mod usage;
pub mod verifier_session;
pub mod web_ui;

/// Write a best-effort backend diagnostic without letting a detached stderr
/// pipe turn an otherwise recoverable request error into a process panic.
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
