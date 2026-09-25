//! Gateway-side ACI implementation.
//!
//! Wire types and deterministic encoding come from the neutral
//! `aci-protocol` crate, and the §9.1 report appraisal from `aci-verifier`.
//! This module adds Gateway-owned key custody, receipt production, upstream
//! provider verifiers, and transport behavior.

pub use aci_protocol::{digest, types};
pub mod e2ee;
pub mod identity;
pub mod keys;
pub mod receipt;
pub mod upstream;
pub mod verifier;
