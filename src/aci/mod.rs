//! Gateway-side ACI implementation.
//!
//! Wire types and deterministic encoding come from the neutral
//! `aci-protocol` crate. This module adds Gateway-owned key custody, receipt
//! production, upstream verification, and transport behavior.

pub use aci_protocol::{digest, types};
pub mod e2ee;
pub mod identity;
pub mod keys;
pub mod receipt;
pub mod upstream;
pub mod verifier;
