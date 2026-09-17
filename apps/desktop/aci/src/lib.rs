//! ACI protocol, transport, and verification primitives for Private AI Proxy.
//!
//! This crate is independent of the gateway service and desktop shell. Both
//! consume it as the authoritative source of ACI wire types, cryptography,
//! upstream transports, and verifier contracts.

pub mod digest;
#[cfg(unix)]
pub mod dstack;
pub mod e2ee;
pub mod identity;
pub mod keys;
pub mod receipt;
pub mod types;
pub mod upstream;
pub mod verifier;
