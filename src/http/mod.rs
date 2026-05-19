//! HTTP wiring for the aggregator.

pub mod app;

pub use app::{build_router, build_router_with_admin};
