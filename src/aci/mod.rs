//! ACI core: protocol-bearing math and types.
//!
//! Nothing in this module depends on the HTTP framework, the upstream
//! client, or the dstack SDK. The launcher and any other host should
//! be able to consume the public re-exports as the authoritative
//! source of ACI digest formulas, attestation binding, and receipt
//! construction.

pub mod canonical;
pub mod e2ee;
pub mod identity;
pub mod keys;
pub mod receipt;
pub mod types;
pub mod upstream;
pub mod verifier;
