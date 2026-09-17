//! ACI protocol, transport, and verification implementation owned by the
//! Private AI Proxy package and reused by the gateway.

pub mod aci;
#[cfg(unix)]
pub use aci::dstack;
