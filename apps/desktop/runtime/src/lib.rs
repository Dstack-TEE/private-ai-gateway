//! Platform-neutral desktop state and persistence.
//!
//! The Tauri shell owns window, menu, tray, clipboard, file-picker, and
//! autostart integration. This crate owns product state and policy.

pub mod contracts;
pub mod controller;
pub mod gateway;
pub mod local_api;
pub mod preferences;
pub mod service_config;
pub mod usage;
