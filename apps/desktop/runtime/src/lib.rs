//! The per-user backend: the controller that owns profiles, verifier
//! sessions, agent configuration, usage, account login and the optional web
//! UI, and the management server that answers `desktop_core::protocol`.
//!
//! The Tauri shell owns window, menu, tray, clipboard, file-picker, and
//! autostart integration; clients reach this crate only over IPC.

pub use desktop_core::diagnostic;

pub mod account_login;
mod balance_cache;
pub mod controller;
mod dispatch;
mod endpoint_inventory;
#[cfg(all(unix, not(all(target_os = "macos", feature = "mac-app-store"))))]
mod helper_staging;
pub mod power;
mod recovery;
pub mod server;
pub mod usage;
pub mod verifier_session;
pub mod web_ui;
