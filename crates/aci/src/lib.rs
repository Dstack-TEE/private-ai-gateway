//! Shared ACI implementation used by the gateway and Private AI Proxy.

pub mod aci;
#[cfg(unix)]
pub mod dstack;
