//! Aggregator service composition.
//!
//! [`service::AciService`] is the orchestrator that joins
//! [`crate::aci`] (digests, identity, receipts) with a configured
//! upstream backend. The HTTP layer ([`crate::http`]) dispatches
//! request work to a single `AciService` instance.

pub mod keyset_epoch;
pub mod metrics;
pub mod revocation_store;
pub mod service;
pub mod session;
pub mod session_store;
pub mod upstream_config;
