//! Platform-neutral desktop state and persistence.
//!
//! The Tauri shell owns window, menu, tray, clipboard, file-picker, and
//! autostart integration. This crate owns product state and policy.

pub mod cli_install;
pub mod client;
pub mod contracts;
pub mod controller;
pub mod gateway;
pub mod launch;
pub mod local_api;
pub mod maintenance;
pub mod preferences;
mod recovery;
pub mod process;
pub mod protocol;
pub mod server;
pub mod service_config;
pub mod transport;
pub mod usage;
