//! The per-user backend: the controller that owns the settings files,
//! profiles, verifier sessions, agent configuration, usage, account login and
//! the optional web UI, and the management API (`desktop_core::protocol`) it
//! serves on the local endpoint and the web UI listener.
//!
//! The Tauri shell owns window, menu, tray, clipboard, file-picker, and
//! autostart integration; clients reach this crate only through the API.

pub mod account_login;
mod api;
mod balance_cache;
pub mod controller;
mod dispatch;
mod endpoint_inventory;
mod error;
#[cfg(all(unix, not(all(target_os = "macos", feature = "mac-app-store"))))]
mod helper_staging;
pub mod local_state;
pub mod power;
mod recovery;
pub mod server;
pub mod settings;
pub mod usage;
pub mod verifier_session;
pub mod web_ui;

pub use error::Error;
