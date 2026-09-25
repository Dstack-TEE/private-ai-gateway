//! Shared ACI wire types and deterministic encoding rules.
//!
//! This crate deliberately excludes producer logic and relying-party
//! verification; the §9.1 verifier both products run is the sibling
//! `aci-verifier` crate.

pub mod digest;
pub mod identity;
pub mod receipt;
pub mod types;
