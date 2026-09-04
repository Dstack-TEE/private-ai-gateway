//! Platform-neutral desktop state and persistence.
//!
//! Native clients and the Tauri migration shell share these modules. Window,
//! menu, tray, clipboard, file-picker, and autostart behavior stays in each
//! platform adapter.

pub mod contracts;
pub mod controller;
pub mod gateway;
pub mod local_api;
pub mod process;
pub mod protocol;
pub mod server;
pub mod service_config;
pub mod usage;
