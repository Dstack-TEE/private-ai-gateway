//! `private-ai-gateway` is a Rust implementation of the
//! Attested Confidential Inference (ACI) gateway.
//!
//! The crate is organised so the protocol-bearing math (digests,
//! identity, receipts) lives in the [`aci`] module and is reusable.
//! The service composition and HTTP wiring live in [`aggregator`] and
//! [`http`] respectively. dstack-specific key custody and TEE quoting
//! live in [`dstack`] and use the Rust dstack SDK.

pub mod aci;
pub mod aggregator;
pub mod dstack;
pub mod http;
pub mod middleware;
