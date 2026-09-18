//! Shared ACI wire types and deterministic encoding rules.
//!
//! This crate deliberately excludes producer and relying-party verification
//! policy. Private AI Gateway and Private AI Proxy use the same bytes on the
//! wire while retaining independent security decisions.

pub mod digest;
pub mod identity;
pub mod receipt;
pub mod types;
