//! The client side of Private AI Proxy, shared by the desktop shell, the CLI
//! and the backend: renderer contracts, the management API and its client,
//! the local endpoint, backend launch, the settings file, app paths and
//! owner-only file primitives. It links no server or database; its HTTP
//! clients are the management API client and the release-channel update check.

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
pub mod logging;
pub mod maintenance;
pub mod paths;
pub mod private_fs;
pub mod protocol;
pub mod sse;
pub mod transport;
pub mod ui_api;
pub mod updates;
pub mod usage;
#[cfg(windows)]
pub mod windows_acl;

/// Seconds since the Unix epoch; 0 if the system clock is set before it.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
