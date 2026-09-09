//! Platform-neutral desktop state and persistence.
//!
//! The Tauri shell owns window, menu, tray, clipboard, file-picker, and
//! autostart integration. This crate owns product state and policy.

pub mod account_login;
pub mod cli;
pub mod cli_install;
pub mod client;
pub mod contracts;
pub mod controller;
pub mod gateway;
#[cfg(unix)]
mod helper_staging;
pub mod launch;
pub mod local_api;
pub mod maintenance;
pub mod power;
pub mod preferences;
pub mod process;
pub mod protocol;
mod recovery;
pub mod server;
pub mod service_config;
pub mod transport;
pub mod usage;
